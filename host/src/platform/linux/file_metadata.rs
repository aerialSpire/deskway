//! Linux filesystem details the workspace uses for the browser.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

struct Directory(*mut libc::DIR);

impl Drop for Directory {
    fn drop(&mut self) {
        // This handle came from opendir and is owned by Directory.
        unsafe { libc::closedir(self.0) };
    }
}

/// Read only the directory's names and type hints. DT_UNKNOWN stays in name order
/// with the other entries instead of requiring a metadata call during the scan.
pub fn read_names(path: &Path, mut visit: impl FnMut(&OsStr, bool)) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let dir = unsafe { libc::opendir(path.as_ptr()) };
    if dir.is_null() {
        return Err(io::Error::last_os_error());
    }
    let dir = Directory(dir);
    loop {
        // readdir leaves errno unchanged at EOF. Its entry is valid until the next call.
        let errno = unsafe { libc::__errno_location() };
        let entry = unsafe {
            *errno = 0;
            libc::readdir(dir.0)
        };
        if entry.is_null() {
            return match unsafe { *errno } {
                0 => Ok(()),
                code => Err(io::Error::from_raw_os_error(code)),
            };
        }
        let name = unsafe { CStr::from_ptr(std::ptr::addr_of!((*entry).d_name).cast()) }.to_bytes();
        if !matches!(name, b"." | b"..") {
            visit(OsStr::from_bytes(name), unsafe { (*entry).d_type } == libc::DT_DIR);
        }
    }
}

pub fn mode(meta: &Metadata) -> Option<u32> {
    Some(meta.mode() & 0o7777)
}

pub type Owners = HashMap<u32, String>;

pub fn owner_for(meta: &Metadata, owners: &mut Owners) -> Option<String> {
    Some(owners.entry(meta.uid()).or_insert_with_key(|&uid| owner(uid)).clone())
}

pub fn attributes(_: &Metadata) -> Option<Vec<String>> { None }

fn owner(uid: u32) -> String {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    let mut buf = vec![0u8; 1024];
    loop {
        // getpwuid_r stores the entry and its strings in the supplied buffers.
        let rc = unsafe { libc::getpwuid_r(uid, entry.as_mut_ptr(), buf.as_mut_ptr().cast(), buf.len(), &mut found) };
        if rc == 0 && !found.is_null() {
            // On success the entry is filled in; its name points into `buf`.
            let name = unsafe { entry.assume_init() }.pw_name;
            if name.is_null() {
                return uid.to_string();
            }
            return unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        }
        if rc != libc::ERANGE {
            return uid.to_string();
        }
        buf.resize(buf.len() * 2, 0);
    }
}

pub fn modified(meta: &Metadata) -> i64 {
    meta.mtime()
}

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

fn pinned_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

pub fn lock_exclusive(file: &File) -> io::Result<()> {
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

pub type Identity = (u64, u64);

pub fn identity(file: &File) -> io::Result<Identity> {
    let meta = file.metadata()?;
    Ok((meta.dev(), meta.ino()))
}

pub fn owned(_: &File, meta: &Metadata) -> bool {
    meta.uid() == unsafe { libc::geteuid() }
}

pub fn name_bytes(name: &OsStr) -> Cow<'_, [u8]> { Cow::Borrowed(name.as_bytes()) }

pub fn name_from_bytes(bytes: Vec<u8>) -> Option<OsString> { Some(OsString::from_vec(bytes)) }

pub fn valid_component(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    !matches!(bytes, b"" | b"." | b"..") && !bytes.contains(&b'/') && !bytes.contains(&0)
}

pub fn canonical_name(name: &OsStr) -> Cow<'_, OsStr> { Cow::Borrowed(name) }

pub fn entry_name(path: &Path) -> io::Result<OsString> {
    path.file_name().map(OsStr::to_os_string).ok_or(io::ErrorKind::InvalidInput.into())
}

pub fn valid_name(name: &str) -> bool {
    valid_component(OsStr::new(name)) && !name.contains('\\') && name.len() <= 255
}

pub fn directory(parts: &[OsString]) -> Option<PathBuf> {
    let mut path = PathBuf::from("/");
    for part in parts {
        if !valid_component(part) { return None; }
        path.push(part);
    }
    Some(path)
}

pub fn path_parts(path: &Path) -> Vec<OsString> {
    path.components().filter_map(|part| match part {
        Component::Normal(name) => Some(name.to_os_string()),
        _ => None,
    }).collect()
}

pub fn roots() -> io::Result<Vec<OsString>> { Err(io::ErrorKind::Unsupported.into()) }

pub fn is_link(meta: &Metadata) -> bool { meta.is_symlink() }
pub fn is_regular(meta: &Metadata) -> bool { meta.is_file() }

pub fn remove_entry(path: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() { std::fs::remove_dir(path) }
    else { std::fs::remove_file(path) }
}

pub fn pin_directory(path: &Path) -> io::Result<(File, PathBuf)> {
    let folder = pin(path)?;
    if !folder.metadata()?.is_dir() { return Err(io::ErrorKind::NotADirectory.into()); }
    let resolved = pinned_path(&folder);
    Ok((folder, resolved))
}

pub fn open_pinned(file: &File) -> io::Result<File> { File::open(pinned_path(file)) }

pub fn create_partial(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(path)
}

pub fn replace(from: &Path, to: &Path) -> io::Result<()> { std::fs::rename(from, to) }
