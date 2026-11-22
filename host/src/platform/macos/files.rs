//! What file transfer needs from macOS: the Downloads folder and private files.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSSearchPathDirectory, NSSearchPathDomainMask};

use crate::platform::Desk;
use crate::transfer::Host;

pub struct Files;

impl Files {
    pub fn new() -> Self {
        Files
    }
}

impl Host for Files {
    fn locked(&mut self) -> bool {
        super::desk::current() != Desk::Default
    }

    fn downloads(&mut self) -> Option<PathBuf> {
        autoreleasepool(|_| {
            NSFileManager::defaultManager()
                .URLsForDirectory_inDomains(NSSearchPathDirectory::DownloadsDirectory, NSSearchPathDomainMask::UserDomainMask)
                .firstObject()?
                .to_file_path()
        })
    }

    fn create(&mut self, path: &Path) -> io::Result<File> {
        let dir = path.parent().ok_or_else(|| io::Error::other("no destination directory"))?;
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(dir)?.is_dir() => {}
            Err(e) => return Err(e),
        }
        OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
    }

    fn remove(&mut self, path: &Path) {
        let _ = fs::remove_file(path);
    }
}
