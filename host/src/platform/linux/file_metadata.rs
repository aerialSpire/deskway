//! Linux no-replace rename and file descriptors pinned through procfs.

pub use crate::platform::unix::file_metadata::*;

use std::ffi::{CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_encoded_bytes())?;
    let to = CString::new(to.as_os_str().as_encoded_bytes())?;
    // Linux checks destination existence and renames in one operation.
    let rc = unsafe {
        libc::renameat2(libc::AT_FDCWD, from.as_ptr(), libc::AT_FDCWD, to.as_ptr(), libc::RENAME_NOREPLACE)
    };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Pin the object without opening a device, following a link, or waiting for a FIFO.
pub fn pin(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_PATH | libc::O_NOFOLLOW).open(path)
}

pub fn pinned_path(file: &File) -> io::Result<PathBuf> {
    Ok(PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd())))
}


pub fn entry_name(path: &Path) -> io::Result<OsString> {
    path.file_name().map(OsStr::to_os_string).ok_or(io::ErrorKind::InvalidInput.into())
}


pub fn pin_directory(path: &Path) -> io::Result<File> {
    let folder = pin(path)?;
    if !folder.metadata()?.is_dir() { return Err(io::ErrorKind::NotADirectory.into()); }
    Ok(folder)
}

pub fn open_pinned(file: &File) -> io::Result<File> { File::open(pinned_path(file)?) }
