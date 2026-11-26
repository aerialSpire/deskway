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
use crate::workspace::{Conflict, Download, FileTransfers, Upload};

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
}

impl Partial {
    fn at_path(&self) -> io::Result<bool> {
        Ok(native::same_file(&self.file.metadata()?, &fs::symlink_metadata(&self.path)?))
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if self.at_path().unwrap_or(false) {
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

impl Staged {
    fn destination(&self) -> Result<Option<fs::Metadata>, Error> {
        match fs::symlink_metadata(&self.destination) {
            Ok(meta) if meta.is_file() => Ok(Some(meta)),
            Ok(_) => Err(Error::NotRegular),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(e)),
        }
    }
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
    fn conflict(&self, seated: &dyn Fn() -> bool) -> Result<Option<Conflict>, Error> {
        check(seated)?;
        Ok(self.destination()?.map(|metadata| Conflict { modified: native::modified(&metadata), metadata }))
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
        let partial = Partial { file, path };
        check(seated)?;
        native::lock_exclusive(&partial.file).map_err(io_error)?;
        self.partial = Some(partial);
        Ok(0)
    }

    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        self.partial.as_mut().ok_or(Error::Protocol)?.file.write_all(bytes).map_err(io_error)
    }

    fn verify(&self, sha256: &str, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        let partial = self.partial.as_ref().ok_or(Error::Protocol)?;
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
        partial.file.sync_all().map_err(io_error)
    }

    fn publish(&self, approval: Option<&Conflict>) -> Result<bool, Error> {
        let partial = self.partial.as_ref().ok_or(Error::Protocol)?;
        if !partial.at_path().unwrap_or(false) {
            return Err(Error::Integrity);
        }
        let replacing = if let Some(approval) = approval {
            if let Some(current) = self.destination()? {
                let approved = &approval.metadata;
                if !native::same_file(&current, approved) || current.len() != approved.len()
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
            fs::rename(&partial.path, &self.destination)
        } else {
            native::rename_new(&partial.path, &self.destination)
        }.map_err(io_error)?;
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
            let result = staged.verify(&digest(data), &|| seated).and_then(|()| staged.publish(None));
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
            let mut approval = staged.conflict(&|| true).unwrap();
            let mtime = approval.as_ref().map(|old| old.metadata.modified().unwrap());
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
            let result = staged.publish(approval.as_ref());
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
                approval = staged.conflict(&|| true).unwrap();
                assert_eq!(staged.publish(approval.as_ref()), Ok(true), "{change}");
            }
            assert_eq!(fs::read(&dest).unwrap(), b"new");
            drop(staged);
            assert!(!dir.names().iter().any(|name| partial_name(name)));
        }
    }

    #[test]
    fn publishing_requires_the_staging_path_to_still_name_the_verified_file() {
        for replacement in ["missing", "file", "link"] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            fs::write(&dest, b"old").unwrap();
            let mut staged = upload(&dest, 3, &|| true).unwrap();
            let approval = staged.conflict(&|| true).unwrap();
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
            assert_eq!(staged.publish(approval.as_ref()), Err(Error::Integrity));
            drop(staged);
            assert_eq!(fs::read(dest).unwrap(), b"old");
            assert_eq!(fs::symlink_metadata(path).is_ok(), replacement != "missing");
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
                file: File::open(&path).unwrap(), path,
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
        for (path, expected) in [(&empty, Ok(())), (&PathBuf::from("/proc/meminfo"), Err(Error::Changed))] {
            let mut reading = download(path, &|| true).unwrap();
            assert_eq!(reading.size(), 0);
            assert_eq!(reading.finish(&|| true), expected);
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
