//! Directory operations for the Linux browser, with byte-exact entry names.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

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

fn valid_component(bytes: &[u8]) -> bool {
    !matches!(bytes, b"" | b"." | b"..") && !bytes.contains(&b'/') && !bytes.contains(&0)
}

fn new_name(name: &str) -> Result<&OsStr, Error> {
    if !valid_component(name.as_bytes()) || name.contains('\\') || name.len() > 255 {
        return Err(Error::InvalidName);
    }
    Ok(OsStr::new(name))
}

fn encode(name: &OsStr) -> String {
    URL_SAFE_NO_PAD.encode(name.as_bytes())
}

fn component(encoded: &str) -> Result<OsString, Error> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| Error::InvalidPath)?;
    if !valid_component(&bytes) {
        return Err(Error::InvalidPath);
    }
    Ok(OsString::from_vec(bytes))
}

fn directory(path: Option<&[String]>) -> Result<PathBuf, Error> {
    match path {
        None => std::env::home_dir().filter(|p| p.is_absolute()).ok_or(Error::NotFound),
        Some(parts) => {
            let mut path = PathBuf::from("/");
            for part in parts {
                path.push(component(part)?);
            }
            Ok(path)
        }
    }
}

type Key = (bool, OsString);

fn cursor_for(key: &Key) -> String {
    let (not_folder, name) = key;
    let bytes: Vec<_> = std::iter::once(u8::from(*not_folder)).chain(name.as_bytes().iter().copied()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn after(cursor: &str) -> Result<Key, Error> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| Error::InvalidPath)?;
    match bytes.split_first() {
        Some((rank @ (0 | 1), name)) if valid_component(name) => Ok((*rank == 1, OsString::from_vec(name.to_vec()))),
        _ => Err(Error::InvalidPath),
    }
}

fn describe(path: &Path, name: &OsStr, owners: &mut HashMap<u32, String>) -> io::Result<FileEntry> {
    let path = path.join(name);
    let meta = fs::symlink_metadata(&path)?;
    let kind = if meta.is_dir() { FileKind::Folder }
        else if meta.is_symlink() { FileKind::Link }
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
        mode: native::mode(&meta), owner: owners.entry(native::owner_id(&meta)).or_insert_with_key(|&uid| native::owner(uid)).clone(),
        target, opens,
    })
}

fn list(path: &Path, cursor: Option<&str>, limit: usize) -> Result<Browse, Error> {
    let after = cursor.map(after).transpose()?;
    // Keep only the next sorted slice, regardless of the directory's iteration order.
    let mut page = BTreeSet::new();
    let mut more = false;
    native::read_names(path, |name, folder| {
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
    let mut owners = HashMap::new();
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
    let path = path.components().filter_map(|c| match c {
        Component::Normal(name) => Some(encode(name)),
        _ => None,
    }).collect();
    Ok(Browse::Listing { path, entries, cursor: last.filter(|_| more).as_ref().map(cursor_for) })
}

fn request(op: Browse) -> (PathBuf, Result<Browse, Error>) {
    let path = match &op {
        Browse::List { path, .. } => directory(path.as_deref()),
        Browse::Mkdir { path, .. } | Browse::Rename { path, .. } | Browse::Delete { path, .. } => directory(Some(path)),
        _ => Err(Error::InvalidPath),
    };
    let path = match path {
        Ok(path) => path,
        Err(why) => return (PathBuf::from("<invalid path>"), Err(why)),
    };
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
            let response = list(&path, cursor.as_deref(), PAGE_ENTRIES);
            (path, response)
        }
        Browse::Mkdir { name, .. } => match new_name(&name) {
            Ok(name) => {
                let path = path.join(name);
                let result = fs::create_dir(&path).map(|()| Browse::Changed {}).map_err(file_error);
                (path, result)
            }
            Err(why) => (path, Err(why)),
        },
        Browse::Rename { entry, name, .. } => match component(&entry) {
            Ok(entry) => {
                let from = path.join(entry);
                let result = new_name(&name).and_then(|name| native::rename_new(&from, &path.join(name)).map_err(file_error));
                (from, result.map(|()| Browse::Changed {}))
            }
            Err(why) => (path, Err(why)),
        },
        Browse::Delete { entry, .. } => match component(&entry) {
            Ok(entry) => {
                let path = path.join(entry);
                let result = super::remove(&path, false).map(|()| Browse::Changed {}).map_err(file_error);
                (path, result)
            }
            Err(why) => (path, Err(why)),
        },
        _ => unreachable!(),
    }
}

pub fn call(op: Browse) -> (PathBuf, Browse) {
    let (path, result) = request(op);
    (path, result.unwrap_or_else(|why| Browse::Error { why }))
}

#[cfg(test)]
mod tests {
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
            let Browse::Listing { entries, cursor, .. } = list(&dir, cursor, 3).unwrap() else { panic!("expected listing") };
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
        assert!(matches!(list(&dir, Some("invalid!"), 3), Err(Error::InvalidPath)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn operations_use_byte_exact_names_and_delete_only_the_link_or_empty_directory() {
        let dir = std::env::temp_dir().join(format!("deskway-browse-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        fs::create_dir(&dir).unwrap();
        let path: Vec<_> = dir.iter().skip(1).map(encode).collect();
        let raw = OsStr::from_bytes(b"x\xff");
        assert_eq!(call(Browse::Mkdir { path: path.clone(), name: "folder".into() }).1, Browse::Changed {});
        fs::write(dir.join("folder/content"), []).unwrap();
        symlink("folder", dir.join(raw)).unwrap();
        let Browse::Listing { entries, .. } = list(&dir, None, 16).unwrap() else { panic!("expected listing") };
        assert_eq!(entries[1].name, encode(raw));
        assert_eq!(entries[1].target, Some(encode(OsStr::new("folder"))));
        assert_eq!(entries[1].opens, Some(true));
        let rename = |name: &str| call(Browse::Rename { path: path.clone(), entry: encode(raw), name: name.into() }).1;
        assert_eq!(rename("folder"), Browse::Error { why: Error::Exists });
        assert_eq!(rename("link"), Browse::Changed {});
        let delete = |name: &str| call(Browse::Delete { path: path.clone(), entry: encode(OsStr::new(name)) }).1;
        assert_eq!(delete("link"), Browse::Changed {});
        assert!(dir.join("folder/content").is_file());
        assert_eq!(delete("folder"), Browse::Error { why: Error::NotEmpty });
        fs::remove_file(dir.join("folder/content")).unwrap();
        assert_eq!(delete("folder"), Browse::Changed {});
        fs::remove_dir(dir).unwrap();
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
        }).1 else { panic!("expected listing") };
        assert_eq!(directory(Some(&path)).unwrap(), old.canonicalize().unwrap());
        symlink("new", dir.join("replacement")).unwrap();
        fs::rename(dir.join("replacement"), &link).unwrap();
        for (name, op) in [
            ("delete", Browse::Delete { path: path.clone(), entry: encode(OsStr::new("delete")) }),
            ("rename", Browse::Rename { path, entry: encode(OsStr::new("rename")), name: "renamed".into() }),
        ] {
            assert_eq!(call(op).1, Browse::Changed {});
            assert!(!old.join(name).exists());
            assert!(new.join(name).is_file());
        }
        assert!(old.join("renamed").is_file());
        assert!(!new.join("renamed").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
