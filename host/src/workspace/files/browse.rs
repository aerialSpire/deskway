//! Directory operations for the browser, with opaque entry names.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use wire::browse::{Browse, BrowseError as Error, FileEntry, FileKind, MAX_BYTES};

use crate::platform::file_metadata as native;

const PAGE_ENTRIES: usize = 128;
// Leave room in the message for the path, cursor and envelope.
const ENTRY_BYTES: usize = MAX_BYTES * 3 / 4;

fn file_error(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => Error::NotFound,
        io::ErrorKind::PermissionDenied => Error::Denied,
        io::ErrorKind::AlreadyExists => Error::Exists,
        io::ErrorKind::DirectoryNotEmpty => Error::NotEmpty,
        io::ErrorKind::NotADirectory => Error::InvalidPath,
        _ => Error::Io,
    }
}


pub(super) fn new_name(name: &str) -> Result<&OsStr, Error> {
    if !native::valid_name(name) || super::transfer::partial_name(&native::canonical_name(OsStr::new(name))) {
        return Err(Error::InvalidName);
    }
    Ok(OsStr::new(name))
}

fn encode(name: &OsStr) -> String {
    URL_SAFE_NO_PAD.encode(native::name_bytes(name))
}

pub(super) fn component(encoded: &str) -> Result<OsString, Error> {
    let name = decode(encoded)?;
    native::valid_component(&name).then_some(name).ok_or(Error::InvalidPath)
}

fn decode(encoded: &str) -> Result<OsString, Error> {
    native::name_from_bytes(URL_SAFE_NO_PAD.decode(encoded).map_err(|_| Error::InvalidPath)?)
        .ok_or(Error::InvalidPath)
}

pub(super) fn directory(path: Option<&[String]>) -> Result<PathBuf, Error> {
    match path {
        None => std::env::home_dir().filter(|p| p.is_absolute()).ok_or(Error::NotFound),
        Some(parts) => {
            let parts = parts.iter().map(|part| decode(part)).collect::<Result<Vec<_>, _>>()?;
            native::directory(&parts).ok_or(Error::InvalidPath)
        }
    }
}

type Key = (bool, OsString);

fn cursor_for(key: &Key) -> String {
    let (not_folder, name) = key;
    let bytes: Vec<_> = std::iter::once(u8::from(*not_folder)).chain(native::name_bytes(name).into_owned()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn after(cursor: &str) -> Result<Key, Error> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| Error::InvalidPath)?;
    match bytes.split_first() {
        Some((rank @ (0 | 1), bytes)) => {
            let name = native::name_from_bytes(bytes.to_vec()).filter(|name| native::valid_component(name)).ok_or(Error::InvalidPath)?;
            Ok((*rank == 1, name))
        }
        _ => Err(Error::InvalidPath),
    }
}

fn describe(path: &Path, name: &OsStr, owners: &mut native::Owners) -> io::Result<FileEntry> {
    let path = path.join(name);
    let meta = fs::symlink_metadata(&path)?;
    let kind = if native::is_link(&meta) { FileKind::Link }
        else if meta.is_dir() { FileKind::Folder }
        else if meta.is_file() { FileKind::File }
        else { FileKind::Special };
    let (target, opens) = if kind == FileKind::Link {
        let target = fs::read_link(&path).ok().map(|target| encode(target.as_os_str()));
        let opens = fs::metadata(path).is_ok_and(|m| m.is_dir());
        (target, Some(opens))
    } else {
        (None, None)
    };
    Ok(FileEntry {
        name: encode(name), kind, size: meta.len(), modified: Some(native::modified(&meta)),
        mode: native::mode(&meta), owner: native::owner_for(&meta, owners), attributes: native::attributes(&meta),
        target, opens,
    })
}

fn list(path: &Path, cursor: Option<&str>, limit: usize, seated: &dyn Fn() -> bool) -> Result<Browse, Error> {
    let after = cursor.map(after).transpose()?;
    // Keep only the next sorted slice, regardless of the directory's iteration order.
    let mut page = BTreeSet::new();
    let mut more = false;
    let now = std::time::SystemTime::now();
    native::read_names(path, |name, folder| {
        if super::transfer::partial_name(name) {
            super::transfer::sweep_entry(path, name, now, seated);
        }
        let key = (!folder, name.to_os_string());
        if after.as_ref().is_some_and(|after| &key <= after) {
            return;
        }
        page.insert(key);
        if page.len() > limit {
            more = true;
            page.pop_last();
        }
    }).map_err(file_error)?;
    let mut owners = native::Owners::default();
    let mut entries = Vec::new();
    let mut last = None;
    let mut bytes = 0;
    for key in page {
        match describe(path, &key.1, &mut owners) {
            Ok(entry) => {
                bytes += serde_json::to_vec(&entry).map_err(|_| Error::Io)?.len() + 1;
                if bytes > ENTRY_BYTES && !entries.is_empty() {
                    more = true;
                    break;
                }
                entries.push(entry);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(file_error(e)),
        }
        last = Some(key);
    }
    let path = native::path_parts(path).iter().map(|name| encode(name)).collect();
    Ok(Browse::Listing { path, entries, roots: None, cursor: last.filter(|_| more).as_ref().map(cursor_for) })
}

fn lock_partial(path: &Path) -> Result<Option<fs::File>, Error> {
    let name = native::entry_name(path).map_err(file_error)?;
    if !super::transfer::partial_name(&native::canonical_name(&name)) {
        return Ok(None);
    }
    let pinned = native::pin(path).map_err(file_error)?;
    if !native::is_regular(&pinned.metadata().map_err(file_error)?) {
        return Ok(None);
    }
    let file = native::open_pinned(&pinned).map_err(file_error)?;
    native::lock_exclusive(&file).map_err(|err| {
        if err.kind() == io::ErrorKind::WouldBlock { Error::InUse } else { file_error(err) }
    })?;
    Ok(Some(file))
}

fn request(op: Browse, seated: &dyn Fn() -> bool) -> (PathBuf, Result<Browse, Error>) {
    let path = match &op {
        Browse::List { path, .. } => directory(path.as_deref()),
        Browse::Mkdir { path, .. } | Browse::Rename { path, .. } | Browse::Delete { path, .. } => directory(Some(path)),
        _ => Err(Error::InvalidPath),
    };
    let path = match path {
        Ok(path) => path,
        Err(why) => return (PathBuf::from("<invalid path>"), Err(why)),
    };
    if !seated() {
        return (path, Err(Error::Denied));
    }
    if path.as_os_str().is_empty() {
        let result = if matches!(op, Browse::List { cursor: None, .. }) {
            native::roots().map(|drives| Browse::Listing {
                path: vec![], roots: Some(true), cursor: None,
                entries: drives.iter().map(|name| FileEntry {
                    name: encode(name), kind: FileKind::Folder, size: 0, modified: None,
                    mode: None, owner: None, attributes: None, target: None, opens: None,
                }).collect(),
            }).map_err(file_error)
        } else {
            Err(Error::InvalidPath)
        };
        return (path, result);
    }
    match op {
        Browse::List { cursor, .. } => {
            let path = if cursor.is_none() {
                match path.canonicalize() {
                    Ok(resolved) => resolved,
                    Err(e) => return (path, Err(file_error(e))),
                }
            } else {
                path
            };
            let response = list(&path, cursor.as_deref(), PAGE_ENTRIES, seated);
            (path, response)
        }
        Browse::Mkdir { name, .. } => match new_name(&name) {
            Ok(name) => {
                let path = path.join(name);
                let result = if seated() { fs::create_dir(&path).map(|()| Browse::Changed {}).map_err(file_error) } else { Err(Error::Denied) };
                (path, result)
            }
            Err(why) => (path, Err(why)),
        },
        Browse::Rename { entry, name, .. } => match component(&entry) {
            Ok(entry) => {
                let from = path.join(entry);
                let result = new_name(&name).and_then(|name| {
                    let _partial = lock_partial(&from)?;
                    if !seated() { return Err(Error::Denied); }
                    native::rename_new(&from, &path.join(name)).map_err(file_error)
                });
                (from, result.map(|()| Browse::Changed {}))
            }
            Err(why) => (path, Err(why)),
        },
        Browse::Delete { entry, .. } => match component(&entry) {
            Ok(entry) => {
                let path = path.join(entry);
                let result = lock_partial(&path).and_then(|_partial| {
                    if !seated() { return Err(Error::Denied); }
                    native::remove_entry(&path).map_err(file_error)
                });
                (path, result.map(|()| Browse::Changed {}))
            }
            Err(why) => (path, Err(why)),
        },
        _ => unreachable!(),
    }
}

pub fn call(op: Browse, seated: &dyn Fn() -> bool) -> (PathBuf, Browse) {
    let (path, result) = request(op, seated);
    (path, result.unwrap_or_else(|why| Browse::Error { why }))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::ffi::OsStrExt;
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn names_are_single_linux_components() {
        for (name, valid) in [
            ("", false), (".", false), ("..", false), ("/a", false), ("a/b", false), ("a\\b", false),
            ("a\0b", false), (".hidden", true), (" x ", true), ("å\n\u{202e}", true),
        ] {
            assert_eq!(new_name(name).is_ok(), valid, "{name:?}");
        }
        assert!(new_name(&"é".repeat(128)).is_err());
        for name in [b"..".as_slice(), b"a/b", b"a\0b"] {
            assert!(component(&URL_SAFE_NO_PAD.encode(name)).is_err());
        }
        assert_eq!(component("_w").unwrap().as_bytes(), &[255]);
    }

    #[test]
    fn pages_resume_after_the_key_even_if_the_directory_changes() {
        let dir = std::env::temp_dir().join(format!("deskway-pages-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        fs::create_dir(&dir).unwrap();
        fs::create_dir(dir.join("z-folder")).unwrap();
        for name in ["z", ".hidden", "b", "a"] {
            fs::write(dir.join(name), []).unwrap();
        }
        let page = |cursor| {
            let Browse::Listing { entries, cursor, .. } = list(&dir, cursor, 3, &|| true).unwrap() else { panic!("expected listing") };
            (entries.into_iter().map(|e| component(&e.name).unwrap()).collect::<Vec<_>>(), cursor)
        };
        let (names, cursor) = page(None);
        assert_eq!(names, ["z-folder", ".hidden", "a"]);
        fs::remove_file(dir.join("a")).unwrap();
        fs::write(dir.join("0"), []).unwrap();
        fs::write(dir.join("c"), []).unwrap();
        let (names, cursor) = page(cursor.as_deref());
        assert_eq!(names, ["b", "c", "z"]);
        assert_eq!(cursor, None);
        assert!(matches!(list(&dir, Some("invalid!"), 3, &|| true), Err(Error::InvalidPath)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn operations_use_byte_exact_names_and_delete_only_the_link_or_empty_directory() {
        let dir = std::env::temp_dir().join(format!("deskway-browse-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        fs::create_dir(&dir).unwrap();
        let path: Vec<_> = dir.iter().skip(1).map(encode).collect();
        let raw = OsStr::from_bytes(b"x\xff");
        assert_eq!(call(Browse::Mkdir { path: path.clone(), name: "folder".into() }, &|| true).1, Browse::Changed {});
        fs::write(dir.join("folder/content"), []).unwrap();
        symlink("folder", dir.join(raw)).unwrap();
        let Browse::Listing { entries, .. } = list(&dir, None, 16, &|| true).unwrap() else { panic!("expected listing") };
        assert_eq!(entries[1].name, encode(raw));
        assert_eq!(entries[1].target, Some(encode(OsStr::new("folder"))));
        assert_eq!(entries[1].opens, Some(true));
        let rename = |name: &str| call(Browse::Rename { path: path.clone(), entry: encode(raw), name: name.into() }, &|| true).1;
        assert_eq!(rename("folder"), Browse::Error { why: Error::Exists });
        assert_eq!(rename("link"), Browse::Changed {});
        let delete = |name: &str| call(Browse::Delete { path: path.clone(), entry: encode(OsStr::new(name)) }, &|| true).1;
        assert_eq!(delete("link"), Browse::Changed {});
        assert!(dir.join("folder/content").is_file());
        assert_eq!(delete("folder"), Browse::Error { why: Error::NotEmpty });
        fs::remove_file(dir.join("folder/content")).unwrap();
        assert_eq!(delete("folder"), Browse::Changed {});
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn browse_lists_live_partials_and_refuses_to_rename_or_delete_them() {
        let dir = std::env::temp_dir().join(format!("deskway-browse-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        fs::create_dir(&dir).unwrap();
        let path: Vec<_> = dir.iter().skip(1).map(encode).collect();
        let name = format!(".deskway-upload-{}.partial", "a".repeat(32));
        let entry = encode(OsStr::new(&name));
        for op in [
            Browse::Rename { path: path.clone(), entry: entry.clone(), name: "renamed".into() },
            Browse::Delete { path: path.clone(), entry: entry.clone() },
        ] {
            let file = fs::File::create(dir.join(&name)).unwrap();
            native::lock_exclusive(&file).unwrap();
            let Browse::Listing { entries, .. } = list(&dir, None, 16, &|| true).unwrap() else { panic!("expected listing") };
            assert!(entries.iter().any(|item| item.name == entry));
            assert_eq!(call(op.clone(), &|| true).1, Browse::Error { why: Error::InUse });
            drop(file);
            assert_eq!(call(op, &|| true).1, Browse::Changed {});
            assert!(!dir.join(&name).exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn operations_use_the_directory_that_was_listed_through_a_link() {
        let dir = std::env::temp_dir().join(format!("deskway-browse-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        let old = dir.join("old");
        let new = dir.join("new");
        let link = dir.join("current");
        for folder in [&old, &new] {
            fs::create_dir_all(folder).unwrap();
            for name in ["delete", "rename"] {
                fs::write(folder.join(name), []).unwrap();
            }
        }
        symlink("old", &link).unwrap();
        let Browse::Listing { path, .. } = call(Browse::List {
            path: Some(link.iter().skip(1).map(encode).collect()), cursor: None,
        }, &|| true).1 else { panic!("expected listing") };
        assert_eq!(directory(Some(&path)).unwrap(), old.canonicalize().unwrap());
        symlink("new", dir.join("replacement")).unwrap();
        fs::rename(dir.join("replacement"), &link).unwrap();
        for (name, op) in [
            ("delete", Browse::Delete { path: path.clone(), entry: encode(OsStr::new("delete")) }),
            ("rename", Browse::Rename { path, entry: encode(OsStr::new("rename")), name: "renamed".into() }),
        ] {
            assert_eq!(call(op, &|| true).1, Browse::Changed {});
            assert!(!old.join(name).exists());
            assert!(new.join(name).is_file());
        }
        assert!(old.join("renamed").is_file());
        assert!(!new.join("renamed").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
