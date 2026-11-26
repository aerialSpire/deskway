//! Staged writes and consistent reads for the browser file view.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use wire::file_transfer::Error;

use super::browse::{component, directory, new_name};
use crate::platform::file_metadata as native;
use crate::workspace::{Conflict, Download, FileTransfers, Upload};

const PREFIX: &str = ".deskway-upload-";
const SUFFIX: &str = ".partial";
const MAX_AGE: Duration = Duration::from_secs(3600);

pub fn files() -> FileTransfers {
    FileTransfers {
        path: std::sync::Arc::new(path),
        upload: std::sync::Arc::new(upload),
        download: std::sync::Arc::new(download),
    }
}

fn path(folder: &[String], name: &str, upload: bool, seated: &dyn Fn() -> bool) -> Result<PathBuf, Error> {
    check(seated)?;
    let folder = directory(Some(folder)).map_err(|_| Error::InvalidPath)?;
    if folder.as_os_str().is_empty() { return Err(Error::InvalidPath); }
    let name = if upload {
        new_name(name)
            .map_err(|_| Error::InvalidName)?
            .to_os_string()
    } else {
        component(name).map_err(|_| Error::InvalidPath)?
    };
    Ok(folder.join(name))
}

fn io_error(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => Error::NotFound,
        io::ErrorKind::PermissionDenied => Error::Denied,
        io::ErrorKind::AlreadyExists => Error::Exists,
        io::ErrorKind::NotADirectory => Error::InvalidPath,
        _ => Error::Io,
    }
}

fn check(seated: &dyn Fn() -> bool) -> Result<(), Error> {
    if seated() {
        Ok(())
    } else {
        Err(Error::Cancelled)
    }
}

pub(super) fn partial_name(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX))
        .is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

pub(super) fn sweep_entry(folder: &Path, name: &OsStr, now: SystemTime, seated: &dyn Fn() -> bool) {
    let path = folder.join(name);
    let _ = (|| -> Result<(), Error> {
        let old = |meta: &fs::Metadata| {
            native::is_regular(meta)
                && meta
                    .modified()
                    .ok()
                    .and_then(|mtime| now.duration_since(mtime).ok())
                    .is_some_and(|age| age > MAX_AGE)
        };
        check(seated)?;
        let pinned = native::pin(&path).map_err(io_error)?;
        check(seated)?;
        let meta = pinned.metadata().map_err(io_error)?;
        if !old(&meta) || !native::owned(&pinned, &meta) {
            return Ok(());
        }
        check(seated)?;
        let file = native::open_pinned(&pinned).map_err(io_error)?;
        check(seated)?;
        native::lock_exclusive(&file).map_err(io_error)?;
        check(seated)?;
        let current = native::pin(&path).map_err(io_error)?;
        let current_meta = current.metadata().map_err(io_error)?;
        if old(&current_meta) && native::owned(&current, &current_meta)
            && native::identity(&current).map_err(io_error)? == native::identity(&pinned).map_err(io_error)? {
            check(seated)?;
            fs::remove_file(path).map_err(io_error)?;
        }
        Ok(())
    })();
}

pub(crate) fn sweep_home(seated: &dyn Fn() -> bool) {
    if let Ok(home) = directory(None) {
        let now = SystemTime::now();
        let _ = native::read_names(&home, |name, _| {
            if partial_name(name) {
                sweep_entry(&home, name, now, seated);
            }
        });
    }
}

fn regular(path: &Path) -> Result<(), Error> {
    if native::is_regular(&fs::symlink_metadata(path).map_err(io_error)?) {
        Ok(())
    } else {
        Err(Error::NotRegular)
    }
}

struct Partial {
    file: File,
    name: OsString,
}

impl Partial {
    fn at_path(&self, path: &Path) -> io::Result<bool> {
        Ok(native::identity(&self.file)? == native::identity(&native::pin(path)?)?)
    }
}

type Snapshot = (native::Identity, fs::Metadata);

struct Staged {
    partial: Option<Partial>,
    destination: OsString,
    folder: File,
    pending: Option<Snapshot>,
    approval: Option<Snapshot>,
    size: u64,
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(partial) = &self.partial {
            native::discard_partial(&partial.file, &self.folder, &partial.name);
        }
    }
}

fn destination(path: &Path) -> Result<Option<Snapshot>, Error> {
    match regular(path) {
        Ok(()) => {}
        Err(Error::NotFound) => return Ok(None),
        Err(why) => return Err(why),
    }
    match native::pin(path) {
        Ok(file) => {
            let name = native::entry_name(path).map_err(io_error)?;
            if partial_name(&native::canonical_name(&name)) { return Err(Error::InvalidName); }
            let meta = file.metadata().map_err(io_error)?;
            if !native::is_regular(&meta) { return Err(Error::NotRegular); }
            Ok(Some((native::identity(&file).map_err(io_error)?, meta)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(e)),
    }
}

fn upload(path: &Path, size: u64, seated: &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error> {
    check(seated)?;
    let folder = native::pin_directory(path.parent().ok_or(Error::InvalidPath)?).map_err(io_error)?;
    let destination = path.file_name().ok_or(Error::InvalidPath)?.to_os_string();
    Ok(Box::new(Staged { partial: None, destination, folder, pending: None, approval: None, size }))
}

impl Upload for Staged {
    fn conflict(&mut self, seated: &dyn Fn() -> bool) -> Result<Option<Conflict>, Error> {
        check(seated)?;
        self.pending = destination(&native::pinned_path(&self.folder).map_err(io_error)?.join(&self.destination))?;
        Ok(self.pending.as_ref().map(|(_, metadata)| Conflict {
            modified: native::modified(metadata), size: metadata.len(),
        }))
    }

    fn accept_conflict(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        self.approval = Some(self.pending.take().ok_or(Error::Protocol)?);
        Ok(())
    }

    fn prepare(&mut self, seated: &dyn Fn() -> bool) -> Result<u64, Error> {
        if let Some(partial) = &mut self.partial {
            check(seated)?;
            let offset = partial.file.stream_position().map_err(io_error)?;
            return Ok(offset);
        }
        let id = wire::crypto::hex(&wire::crypto::random::<16>());
        let name = OsString::from(format!("{PREFIX}{id}{SUFFIX}"));
        check(seated)?;
        let path = native::pinned_path(&self.folder).map_err(io_error)?.join(&name);
        let file = native::create_partial(&path).map_err(io_error)?;
        let partial = Partial { file, name };
        if let Err(why) = check(seated).and_then(|()| native::lock_exclusive(&partial.file).map_err(io_error)) {
            native::discard_partial(&partial.file, &self.folder, &partial.name);
            return Err(why);
        }
        self.partial = Some(partial);
        Ok(0)
    }

    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        self.partial.as_mut().ok_or(Error::Protocol)?.file.write_all(bytes).map_err(io_error)
    }

    fn verify(&mut self, sha256: &str, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        let partial = self.partial.as_mut().ok_or(Error::Protocol)?;
        check(seated)?;
        if partial.file.metadata().map_err(io_error)?.len() != self.size {
            return Err(Error::Size);
        }
        check(seated)?;
        let saved = partial.file.stream_position().map_err(io_error)?;
        partial.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let result = (|| {
            let mut hash = Sha256::new();
            let mut buf = vec![0; 64 * 1024];
            loop {
                check(seated)?;
                let n = partial.file.read(&mut buf).map_err(io_error)?;
                if n == 0 {
                    return Ok(hash);
                }
                hash.update(&buf[..n]);
            }
        })();
        partial.file.seek(SeekFrom::Start(saved)).map_err(io_error)?;
        let hash = result?;
        if wire::crypto::hex(&hash.finalize()) != sha256 {
            return Err(Error::Integrity);
        }
        check(seated)?;
        partial.file.sync_all().map_err(io_error)
    }

    fn publish(&mut self) -> Result<bool, Error> {
        let partial = self.partial.as_ref().ok_or(Error::Protocol)?;
        let folder = native::pinned_path(&self.folder).map_err(io_error)?;
        let source = folder.join(&partial.name);
        if !partial.at_path(&source).unwrap_or(false) {
            return Err(Error::Integrity);
        }
        let target = folder.join(&self.destination);
        let replacing = if let Some((approved_id, approved)) = &self.approval {
            if let Some((current_id, current)) = destination(&target)? {
                if current_id != *approved_id || current.len() != approved.len()
                    || current.modified().map_err(io_error)? != approved.modified().map_err(io_error)? {
                    return Err(Error::Exists);
                }
                true
            } else {
                false
            }
        } else {
            false
        };
        if replacing {
            native::replace(&source, &target)
        } else {
            native::rename_new(&source, &target)
        }.map_err(io_error)?;
        self.partial = None;
        Ok(replacing)
    }
}
struct Reading {
    file: File,
    size: u64,
    modified: SystemTime,
}

fn download(path: &Path, seated: &dyn Fn() -> bool) -> Result<Box<dyn Download>, Error> {
    check(seated)?;
    regular(path)?;
    check(seated)?;
    let pinned = native::pin(path).map_err(io_error)?;
    check(seated)?;
    let meta = pinned.metadata().map_err(io_error)?;
    if !native::is_regular(&meta) {
        return Err(Error::NotRegular);
    }
    check(seated)?;
    let file = native::open_pinned(&pinned).map_err(io_error)?;
    Ok(Box::new(Reading {
        file,
        size: meta.len(),
        modified: meta.modified().map_err(io_error)?,
    }))
}

impl Download for Reading {
    fn size(&self) -> u64 {
        self.size
    }

    fn rewind(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.verify(seated)?;
        check(seated)?;
        self.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        Ok(())
    }

    fn read(&mut self, bytes: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.verify(seated)?;
        check(seated)?;
        self.file.read_exact(bytes).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                Error::Changed
            } else {
                io_error(e)
            }
        })?;
        self.verify(seated)
    }

    fn finish(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        if self.file.read(&mut [0]).map_err(io_error)? != 0 {
            return Err(Error::Changed);
        }
        self.verify(seated)
    }
}

impl Reading {
    fn verify(&self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        let meta = self.file.metadata().map_err(io_error)?;
        if meta.len() == self.size && meta.modified().map_err(io_error)? == self.modified {
            Ok(())
        } else {
            Err(Error::Changed)
        }
    }
}

#[cfg(all(test, windows))]
#[test]
fn sharing_violation_leaves_the_destination_and_discards_the_open_partial() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Foundation::ERROR_SHARING_VIOLATION;
    use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let dir = std::env::temp_dir().join(format!("deskway-sharing-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
    fs::create_dir(&dir).unwrap();
    let dest = dir.join("file");
    fs::write(&dest, b"old").unwrap();
    let exclusive = |path: &Path| fs::OpenOptions::new().read(true).share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0).open(path);
    let held_destination = exclusive(&dest).unwrap();
    let mut staged = upload(&dest, 3, &|| true).unwrap();
    assert!(staged.conflict(&|| true).unwrap().is_some());
    staged.accept_conflict(&|| true).unwrap();
    staged.prepare(&|| true).unwrap();
    staged.write(b"new", &|| true).unwrap();
    staged.verify(&wire::crypto::hex(&wire::crypto::sha256(b"new")), &|| true).unwrap();
    let path = fs::read_dir(&dir).unwrap().map(|entry| entry.unwrap().path())
        .find(|path| partial_name(path.file_name().unwrap())).unwrap();
    assert_eq!(exclusive(&path).unwrap_err().raw_os_error(), Some(ERROR_SHARING_VIOLATION.0 as i32));
    let held_partial = File::open(&path).unwrap();
    assert_eq!(staged.publish(), Err(Error::Denied));
    assert_eq!(fs::read(&dest).unwrap(), b"old");
    drop(staged);
    assert!(!path.exists());
    assert_eq!(held_partial.metadata().unwrap().len(), 3);
    drop(held_partial);
    drop(held_destination);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::remove_dir_all(dir).unwrap();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::{fs::symlink, net::UnixListener};

    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "deskway-transfer-{}",
                wire::crypto::hex(&wire::crypto::random::<8>())
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn names(&self) -> Vec<OsString> {
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn digest(bytes: &[u8]) -> String {
        wire::crypto::hex(&wire::crypto::sha256(bytes))
    }

    #[test]
    fn staged_uploads_commit_or_clean_up() {
        for (size, data, resume, seated, expected) in [
            (3, b"new", false, true, Ok(false)),
            (3, b"new", true, true, Ok(false)),
            (3, b"bad", true, true, Err(Error::Integrity)),
            (4, b"new", false, true, Err(Error::Size)),
            (3, b"new", false, false, Err(Error::Cancelled)),
        ] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            let mut staged = upload(&dest, size, &|| true).ok().unwrap();
            assert_eq!(staged.prepare(&|| true), Ok(0));
            staged.write(b"n", &|| true).unwrap();
            if resume {
                assert_eq!(staged.prepare(&|| true), Ok(1));
            }
            staged.write(b"ew", &|| true).unwrap();
            assert!(!dest.exists());
            let result = staged.verify(&digest(data), &|| seated).and_then(|()| staged.publish());
            assert_eq!(result, expected);
            drop(staged);
            assert_eq!(fs::read(&dest).ok().as_deref(), expected.ok().map(|_| b"new".as_slice()));
            assert!(!dir.names().iter().any(|name| partial_name(name)));
        }
    }

    #[test]
    fn replacement_requires_approval_for_the_current_file() {
        for change in ["none", "late", "identity", "size", "mtime", "directory", "link"] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            if change != "late" {
                fs::write(&dest, b"old").unwrap();
            }
            let mut staged = upload(&dest, 3, &|| true).ok().unwrap();
            let approval = staged.conflict(&|| true).unwrap();
            if approval.is_some() { staged.accept_conflict(&|| true).unwrap(); }
            let mtime = approval.as_ref().map(|_| fs::metadata(&dest).unwrap().modified().unwrap());
            staged.prepare(&|| true).unwrap();
            staged.write(b"new", &|| true).unwrap();
            match change {
                "identity" | "directory" | "link" => {
                    fs::rename(&dest, dir.0.join("old")).unwrap();
                    match change {
                        "identity" => {
                            fs::write(&dest, b"alt").unwrap();
                            File::open(&dest).unwrap().set_modified(mtime.unwrap()).unwrap();
                        }
                        "directory" => fs::create_dir(&dest).unwrap(),
                        _ => symlink("missing", &dest).unwrap(),
                    }
                }
                "size" | "mtime" => {
                    fs::write(&dest, if change == "size" { b"long".as_slice() } else { b"alt" }).unwrap();
                    let old = mtime.unwrap();
                    File::open(&dest).unwrap().set_modified(if change == "size" { old } else { old + Duration::from_secs(1) }).unwrap();
                }
                "late" => fs::write(&dest, b"alt").unwrap(),
                _ => {}
            }
            staged.verify(&digest(b"new"), &|| true).unwrap();
            let result = staged.publish();
            if matches!(change, "directory" | "link") {
                assert_eq!(result, Err(Error::NotRegular));
                assert_eq!(staged.conflict(&|| true).err(), Some(Error::NotRegular));
                continue;
            }
            if change == "none" {
                assert_eq!(result, Ok(true));
            } else {
                assert_eq!(result, Err(Error::Exists), "{change}");
                assert_eq!(fs::read(&dest).unwrap(), if change == "size" { b"long".as_slice() } else { b"alt" });
                assert!(staged.conflict(&|| true).unwrap().is_some());
                staged.accept_conflict(&|| true).unwrap();
                assert_eq!(staged.publish(), Ok(true), "{change}");
            }
            assert_eq!(fs::read(&dest).unwrap(), b"new");
            drop(staged);
            assert!(!dir.names().iter().any(|name| partial_name(name)));
        }
    }

    #[test]
    fn an_upload_stays_in_its_open_directory_after_a_rename() {
        let dir = Folder::new();
        let original = dir.0.join("original");
        let moved = dir.0.join("moved");
        fs::create_dir(&original).unwrap();
        let mut staged = upload(&original.join("file"), 3, &|| true).unwrap();
        staged.prepare(&|| true).unwrap();
        staged.write(b"new", &|| true).unwrap();
        fs::rename(&original, &moved).unwrap();
        fs::create_dir(&original).unwrap();
        staged.verify(&digest(b"new"), &|| true).unwrap();
        assert_eq!(staged.publish(), Ok(false));
        assert_eq!(fs::read(moved.join("file")).unwrap(), b"new");
        assert!(!original.join("file").exists());
    }

    #[test]
    fn publishing_requires_the_staging_path_to_still_name_the_verified_file() {
        for replacement in ["missing", "file", "link"] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            fs::write(&dest, b"old").unwrap();
            let mut staged = upload(&dest, 3, &|| true).unwrap();
            assert!(staged.conflict(&|| true).unwrap().is_some());
            staged.accept_conflict(&|| true).unwrap();
            staged.prepare(&|| true).unwrap();
            staged.write(b"new", &|| true).unwrap();
            staged.verify(&digest(b"new"), &|| true).unwrap();
            let path = dir.0.join(dir.names().into_iter().find(|name| partial_name(name)).unwrap());
            let moved = dir.0.join("moved");
            fs::rename(&path, &moved).unwrap();
            match replacement {
                "file" => fs::write(&path, b"bad").unwrap(),
                "link" => symlink(&moved, &path).unwrap(),
                _ => {}
            }
            assert_eq!(staged.publish(), Err(Error::Integrity));
            drop(staged);
            assert_eq!(fs::read(dest).unwrap(), b"old");
            assert_eq!(fs::symlink_metadata(path).is_ok(), replacement != "missing");
        }
    }

    #[test]
    fn a_write_failure_drops_the_partial() {
        let dir = Folder::new();
        let name = OsString::from(format!("{PREFIX}{}{SUFFIX}", "a".repeat(32)));
        let path = dir.0.join(&name);
        fs::write(&path, []).unwrap();
        let mut staged = Staged {
            partial: Some(Partial {
                file: File::open(&path).unwrap(), name,
            }),
            destination: "file".into(),
            folder: native::pin_directory(&dir.0).unwrap(),
            pending: None,
            approval: None,
            size: 3,
        };
        assert_eq!(staged.write(b"new", &|| true), Err(Error::Io));
        drop(staged);
        assert!(dir.names().is_empty());
    }

    #[test]
    fn sweep_removes_only_old_unlocked_partials() {
        let dir = Folder::new();
        let start = SystemTime::now();
        for (id, age, locked, remains) in [
            ("a", MAX_AGE + Duration::from_secs(1), false, false),
            ("b", MAX_AGE, false, true),
            ("c", MAX_AGE - Duration::from_secs(1), false, true),
            ("d", MAX_AGE + Duration::from_secs(1), true, true),
        ] {
            let name = format!("{PREFIX}{}{SUFFIX}", id.repeat(32));
            let path = dir.0.join(&name);
            let file = File::create(&path).unwrap();
            file.set_modified(start).unwrap();
            if locked {
                native::lock_exclusive(&file).unwrap();
            }
            sweep_entry(
                &dir.0,
                OsStr::new(&name),
                file.metadata().unwrap().modified().unwrap() + age,
                &|| true,
            );
            assert_eq!(path.exists(), remains, "{name}");
        }
        for name in [
            ".deskway-upload-not-an-id.partial",
            ".other.partial",
            ".deskway-upload-0123456789ABCDEF0123456789ABCDEF.partial",
        ] {
            assert!(!partial_name(OsStr::new(name)));
        }
    }

    #[test]
    fn download_keeps_its_open_file_and_detects_size_or_mtime_changes() {
        for change in ["rename", "size", "mtime"] {
            let dir = Folder::new();
            let path = dir.0.join("file");
            fs::write(&path, b"abc").unwrap();
            let mut reading = download(&path, &|| true).ok().unwrap();
            let mut byte = [0];
            reading.read(&mut byte, &|| true).unwrap();
            assert_eq!(&byte, b"a");
            match change {
                "rename" => {
                    fs::rename(&path, dir.0.join("old")).unwrap();
                    fs::write(&path, b"new").unwrap();
                    reading.read(&mut byte, &|| true).unwrap();
                    assert_eq!(&byte, b"b");
                }
                "size" => File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(4)
                    .unwrap(),
                _ => File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(SystemTime::now() + Duration::from_secs(10))
                    .unwrap(),
            }
            if change == "rename" {
                reading.read(&mut byte, &|| true).unwrap();
                assert_eq!(&byte, b"c");
                assert_eq!(reading.finish(&|| true), Ok(()));
            } else {
                assert_eq!(reading.rewind(&|| true), Err(Error::Changed));
                assert_eq!(reading.read(&mut byte, &|| true), Err(Error::Changed));
            }
        }
    }

    #[test]
    fn a_zero_size_download_must_reach_eof() {
        let dir = Folder::new();
        let empty = dir.0.join("empty");
        fs::write(&empty, []).unwrap();
        for change in [false, true] {
            let mut reading = download(&empty, &|| true).unwrap();
            assert_eq!(reading.size(), 0);
            if change {
                fs::write(&empty, b"extra bytes").unwrap();
            }
            assert_eq!(reading.finish(&|| true), if change { Err(Error::Changed) } else { Ok(()) });
        }
    }

    #[test]
    fn download_refuses_non_regular_files() {
        let dir = Folder::new();
        let socket = dir.0.join("socket");
        let _listener = UnixListener::bind(&socket).unwrap();
        let link = dir.0.join("link");
        symlink("/dev/null", &link).unwrap();
        for path in [&dir.0, &socket, &link, &PathBuf::from("/dev/null")] {
            assert_eq!(download(path, &|| true).err(), Some(Error::NotRegular));
        }
    }
}
