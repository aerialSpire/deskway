//! Linux filesystem details the workspace uses for the browser.

use std::ffi::{CStr, CString, OsStr};
use std::fs::Metadata;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

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

pub fn mode(meta: &Metadata) -> u32 {
    meta.mode() & 0o7777
}

pub fn owner_id(meta: &Metadata) -> u32 {
    meta.uid()
}

pub fn owner(uid: u32) -> String {
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
