//! Staged writes and consistent reads for the Linux file view.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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

fn path(folder: &[String], name: &str, upload: bool) -> Result<PathBuf, Error> {
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

struct Staged {
    file: File,
    partial: Option<PathBuf>,
    destination: PathBuf,
    _folder: File,
    size: u64,
}

fn upload(path: &Path, size: u64, seated: &dyn Fn() -> bool) -> Result<Box<dyn Upload>, Error> {
    check(seated)?;
    let folder = native::pin(path.parent().ok_or(Error::InvalidPath)?).map_err(io_error)?;
    check(seated)?;
    if !folder.metadata().map_err(io_error)?.is_dir() {
        return Err(Error::InvalidPath);
    }
    let directory = native::pinned_path(&folder);
    let destination = directory.join(path.file_name().ok_or(Error::InvalidPath)?);
    check(seated)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(Error::Exists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    let id = wire::crypto::hex(&wire::crypto::random::<16>());
    let partial = directory.join(format!("{PREFIX}{id}{SUFFIX}"));
    check(seated)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .map_err(io_error)?;
    let staged = Staged {
        file,
        partial: Some(partial),
        destination,
        _folder: folder,
        size,
    };
    check(seated)?;
    native::lock_exclusive(&staged.file).map_err(io_error)?;
    Ok(Box::new(staged))
}

impl Upload for Staged {
    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        self.file.write_all(bytes).map_err(io_error)
    }

    fn commit(mut self: Box<Self>, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        check(seated)?;
        if self.file.metadata().map_err(io_error)?.len() != self.size {
            return Err(Error::Size);
        }
        check(seated)?;
        self.file.sync_all().map_err(io_error)?;
        check(seated)?;
        native::rename_new(
            self.partial.as_ref().expect("staged path"),
            &self.destination,
        )
        .map_err(io_error)?;
        self.partial = None;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(path) = &self.partial {
            let _ = fs::remove_file(path);
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

    #[test]
    fn staged_uploads_commit_or_clean_up() {
        enum End {
            Commit,
            Cancel,
            SeatLost,
            Appeared,
            WrongSize,
        }
        for (end, size, expected, contents) in [
            (End::Commit, 3, Ok(()), Some(b"new".as_slice())),
            (End::Cancel, 3, Ok(()), None),
            (End::SeatLost, 3, Err(Error::Cancelled), None),
            (
                End::Appeared,
                3,
                Err(Error::Exists),
                Some(b"old".as_slice()),
            ),
            (End::WrongSize, 4, Err(Error::Size), None),
        ] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            let mut staged = upload(&dest, size, &|| true).ok().unwrap();
            staged.write(b"new", &|| true).unwrap();
            assert!(!dest.exists());
            assert!(dir.names().iter().all(|name| partial_name(name)));
            let result = match end {
                End::Cancel => {
                    drop(staged);
                    Ok(())
                }
                End::SeatLost => staged.commit(&|| false),
                End::Appeared => {
                    fs::write(&dest, b"old").unwrap();
                    staged.commit(&|| true)
                }
                _ => staged.commit(&|| true),
            };
            assert_eq!(result, expected);
            assert_eq!(fs::read(&dest).ok().as_deref(), contents);
            assert!(!dir.names().iter().any(|name| partial_name(name)));
        }
    }

    #[test]
    fn existing_files_and_dangling_links_are_refused_before_staging() {
        for link in [false, true] {
            let dir = Folder::new();
            let dest = dir.0.join("file");
            if link {
                symlink("missing", &dest).unwrap();
            } else {
                fs::write(&dest, b"unchanged").unwrap();
            }
            assert_eq!(upload(&dest, 1, &|| true).err(), Some(Error::Exists));
            assert_eq!(dir.names(), [OsString::from("file")]);
        }
    }

    #[test]
    fn a_write_failure_drops_the_partial() {
        let dir = Folder::new();
        let destination = dir.0.join("file");
        let partial = dir.0.join(format!("{PREFIX}{}{SUFFIX}", "a".repeat(32)));
        fs::write(&partial, []).unwrap();
        let mut staged = Staged {
            file: OpenOptions::new().write(true).open("/dev/full").unwrap(),
            partial: Some(partial),
            destination,
            _folder: File::open(&dir.0).unwrap(),
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
                assert_eq!(reading.verify(&|| true), Ok(()));
            } else {
                assert_eq!(reading.verify(&|| true), Err(Error::Changed));
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
