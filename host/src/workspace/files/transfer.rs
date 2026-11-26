//! Staged writes and consistent reads for the Linux file view.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use wire::file_transfer::Error;

use super::browse::{component, directory, new_name};
use crate::platform::file_metadata as native;
use crate::workspace::{Download, FileTransfers, Upload};

const PREFIX: &str = ".deskway-upload-";
const SUFFIX: &str = ".partial";
const MAX_AGE: Duration = Duration::from_secs(3600);

pub const FILES: FileTransfers = FileTransfers {
    path,
    upload,
    download,
};

fn path(folder: &[String], name: &str, upload: bool, seated: &dyn Fn() -> bool) -> Result<PathBuf, Error> {
    check(seated)?;
    let folder = directory(Some(folder)).map_err(|_| Error::InvalidPath)?;
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
            meta.is_file()
                && native::owned(meta)
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
        if !old(&meta) {
            return Ok(());
        }
        check(seated)?;
        let file = File::open(native::pinned_path(&pinned)).map_err(io_error)?;
        check(seated)?;
        native::lock_exclusive(&file).map_err(io_error)?;
        check(seated)?;
        let current = fs::symlink_metadata(&path).map_err(io_error)?;
        if old(&current) && native::same_file(&current, &meta) {
            check(seated)?;
            fs::remove_file(path).map_err(io_error)?;
        }
        Ok(())
    })();
}

pub(crate) fn sweep_home() {
    if let Ok(home) = directory(None) {
        let now = SystemTime::now();
        let _ = native::read_names(&home, |name, _| {
            if partial_name(name) {
                sweep_entry(&home, name, now, &|| true);
            }
        });
    }
}

struct Partial {
    file: File,
    path: PathBuf,
    keep: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct Staged {
    partial: Option<Partial>,
    destination: PathBuf,
    folder: File,
    size: u64,
}

fn upload(path: &Path, size: u64, seated: &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error> {
    check(seated)?;
    let folder = native::pin(path.parent().ok_or(Error::InvalidPath)?).map_err(io_error)?;
    check(seated)?;
    if !folder.metadata().map_err(io_error)?.is_dir() {
        return Err(Error::InvalidPath);
    }
    let destination = native::pinned_path(&folder).join(path.file_name().ok_or(Error::InvalidPath)?);
    Ok(Box::new(Staged { partial: None, destination, folder, size }))
}

impl Upload for Staged {
    fn conflict(&self, seated: &dyn Fn() -> bool) -> Result<Option<(u64, i64)>, Error> {
        check(seated)?;
        match fs::symlink_metadata(&self.destination) {
            Ok(meta) if meta.is_file() => Ok(Some((meta.len(), native::modified(&meta)))),
            Ok(_) => Err(Error::NotRegular),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(e)),
        }
    }

    fn prepare(&mut self, seated: &dyn Fn() -> bool) -> Result<u64, Error> {
        if let Some(partial) = &mut self.partial {
            check(seated)?;
            let offset = partial.file.stream_position().map_err(io_error)?;
            return Ok(offset);
        }
        let id = wire::crypto::hex(&wire::crypto::random::<16>());
        let path = native::pinned_path(&self.folder).join(format!("{PREFIX}{id}{SUFFIX}"));
        check(seated)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(io_error)?;
        let partial = Partial { file, path, keep: false };
        check(seated)?;
        native::lock_exclusive(&partial.file).map_err(io_error)?;
        self.partial = Some(partial);
        Ok(0)
    }

    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        self.partial.as_mut().ok_or(Error::Protocol)?.file.write_all(bytes).map_err(io_error)
    }

    fn commit(&mut self, sha256: &str, replace: bool, seated: &dyn Fn() -> bool) -> Result<bool, Error> {
        let partial = self.partial.as_mut().ok_or(Error::Protocol)?;
        check(seated)?;
        if partial.file.metadata().map_err(io_error)?.len() != self.size {
            return Err(Error::Size);
        }
        check(seated)?;
        let mut reader = File::open(native::pinned_path(&partial.file)).map_err(io_error)?;
        let mut hash = Sha256::new();
        let mut buf = vec![0; 64 * 1024];
        loop {
            check(seated)?;
            let n = reader.read(&mut buf).map_err(io_error)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        if wire::crypto::hex(&hash.finalize()) != sha256 {
            return Err(Error::Integrity);
        }
        check(seated)?;
        partial.file.sync_all().map_err(io_error)?;
        let replacing = replace && self.conflict(seated)?.is_some();
        let partial = self.partial.as_mut().unwrap();
        check(seated)?;
        if replacing {
            fs::rename(&partial.path, &self.destination)
        } else {
            native::rename_new(&partial.path, &self.destination)
        }.map_err(io_error)?;
        partial.keep = true;
        Ok(replacing)
    }

    fn abandon(&mut self) {
        if let Some(partial) = &mut self.partial {
            partial.keep = true;
        }
    }
}
struct Reading {
    file: File,
    size: u64,
    modified: SystemTime,
}

fn download(path: &Path, seated: &dyn Fn() -> bool) -> Result<Box<dyn Download>, Error> {
    check(seated)?;
    let pinned = native::pin(path).map_err(io_error)?;
    check(seated)?;
    let meta = pinned.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(Error::NotRegular);
    }
    check(seated)?;
    let file = File::open(native::pinned_path(&pinned)).map_err(io_error)?;
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

#[cfg(test)]
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

    use std::ffi::OsString;

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
            assert_eq!(staged.commit(&digest(data), false, &|| seated), expected);
            drop(staged);
            assert_eq!(fs::read(&dest).ok().as_deref(), expected.ok().map(|_| b"new".as_slice()));
            assert!(!dir.names().iter().any(|name| partial_name(name)));
        }
    }

    #[test]
    fn replacement_checks_the_kind_and_preserves_the_destination_until_commit() {
        for (kind, late) in [("regular", false), ("regular", true), ("directory", false), ("link", false)] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            let put_destination = || match kind {
                "regular" => fs::write(&dest, b"old").unwrap(),
                "directory" => fs::create_dir(&dest).unwrap(),
                _ => symlink("missing", &dest).unwrap(),
            };
            if !late {
                put_destination();
            }
            let mut staged = upload(&dest, 3, &|| true).ok().unwrap();
            let found = staged.conflict(&|| true);
            if kind != "regular" {
                assert_eq!(found, Err(Error::NotRegular));
            } else if late {
                assert_eq!(found, Ok(None));
            } else {
                assert_eq!(found.unwrap().unwrap().0, 3);
            }
            staged.prepare(&|| true).unwrap();
            staged.write(b"new", &|| true).unwrap();
            if late {
                put_destination();
                assert_eq!(staged.commit(&digest(b"new"), false, &|| true), Err(Error::Exists));
                assert_eq!(staged.conflict(&|| true).unwrap().unwrap().0, 3);
            }
            if kind == "regular" {
                assert_eq!(fs::read(&dest).unwrap(), b"old");
                assert_eq!(staged.commit(&digest(b"bad"), true, &|| true), Err(Error::Integrity));
                assert_eq!(fs::read(&dest).unwrap(), b"old");
                assert_eq!(staged.commit(&digest(b"new"), true, &|| true), Ok(true));
                assert_eq!(fs::read(&dest).unwrap(), b"new");
            } else {
                assert_eq!(staged.commit(&digest(b"new"), true, &|| true), Err(Error::NotRegular));
                let meta = fs::symlink_metadata(&dest).unwrap();
                assert_eq!(meta.is_dir(), kind == "directory");
                assert_eq!(meta.is_symlink(), kind == "link");
                assert!(!dir.0.join("missing").exists());
            }
            drop(staged);
            assert_eq!(dir.names(), [OsString::from("file")]);
        }
    }

    #[test]
    fn a_write_failure_drops_the_partial() {
        let dir = Folder::new();
        let destination = dir.0.join("file");
        let path = dir.0.join(format!("{PREFIX}{}{SUFFIX}", "a".repeat(32)));
        fs::write(&path, []).unwrap();
        let mut staged = Staged {
            partial: Some(Partial {
                file: OpenOptions::new().write(true).open("/dev/full").unwrap(), path, keep: false,
            }),
            destination,
            folder: File::open(&dir.0).unwrap(),
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
    fn an_abandoned_upload_unlocks_for_the_age_sweep() {
        let dir = Folder::new();
        let mut staged = upload(&dir.0.join("file"), 2, &|| true).ok().unwrap();
        staged.prepare(&|| true).unwrap();
        staged.write(b"a", &|| true).unwrap();
        let name = dir.names().pop().unwrap();
        let path = dir.0.join(&name);
        let now = fs::metadata(&path).unwrap().modified().unwrap() + MAX_AGE + Duration::from_secs(1);
        sweep_entry(&dir.0, &name, now, &|| true);
        assert!(path.exists(), "a retained upload remains locked");
        staged.abandon();
        drop(staged);
        assert!(path.exists(), "abandonment leaves the staged file for the sweep");
        sweep_entry(&dir.0, &name, now, &|| true);
        assert!(!path.exists());
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
                assert_eq!(reading.verify(&|| true), Ok(()));
            } else {
                assert_eq!(reading.verify(&|| true), Err(Error::Changed));
                assert_eq!(reading.rewind(&|| true), Err(Error::Changed));
                assert_eq!(reading.read(&mut byte, &|| true), Err(Error::Changed));
            }
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
