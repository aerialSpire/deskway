//! Darwin file handles and exclusive rename for the browser workspace.

pub use crate::platform::unix::file_metadata::*;

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // Darwin resolves aliases and checks for collisions in the same operation as the rename.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn entry_name(path: &Path) -> io::Result<OsString> {
    #[repr(C)]
    struct Name {
        length: u32,
        name: libc::attrreference_t,
        bytes: [u8; libc::MAXPATHLEN as usize],
    }
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT, reserved: 0, commonattr: libc::ATTR_CMN_NAME,
        volattr: 0, dirattr: 0, fileattr: 0, forkattr: 0,
    };
    let mut name: Name = unsafe { std::mem::zeroed() };
    if unsafe { libc::getattrlist(path.as_ptr(), (&mut attrs as *mut libc::attrlist).cast(),
        (&mut name as *mut Name).cast(), size_of::<Name>(), libc::FSOPT_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // attr_dataoffset is relative to the reference, not the start of the response.
    let offset = name.name.attr_dataoffset as isize - size_of::<libc::attrreference_t>() as isize;
    let start = usize::try_from(offset).map_err(io::Error::other)?;
    let end = start.checked_add(name.name.attr_length as usize).ok_or(io::ErrorKind::InvalidData)?;
    let bytes = name.bytes.get(start..end).ok_or(io::ErrorKind::InvalidData)?;
    let stored = CStr::from_bytes_with_nul(bytes).map_err(io::Error::other)?;
    Ok(OsStr::from_bytes(stored.to_bytes()).to_os_string())
}

pub fn pin(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_EVTONLY | libc::O_SYMLINK | libc::O_NONBLOCK | libc::O_NOCTTY).open(path)
}

pub fn pin_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_EVTONLY).open(path)
}

pub fn pinned_path(file: &File) -> io::Result<PathBuf> {
    let mut path = [0; libc::MAXPATHLEN as usize];
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&path).map_err(io::Error::other)?;
    Ok(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

pub fn open_pinned(file: &File) -> io::Result<File> {
    let path = pinned_path(file)?;
    let opened = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY).open(path)?;
    if identity(&opened)? != identity(file)? {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(opened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn names_and_renames_follow_the_volumes_case_rules() {
        let dir = std::env::temp_dir().join(format!("deskway-mac-files-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        fs::create_dir(&dir).unwrap();
        let spelling = format!(".deskway-upload-{}.partial", "a".repeat(32));
        let dest = dir.join(&spelling);
        let alias = dir.join(spelling.to_ascii_uppercase());
        fs::write(&dest, b"old").unwrap();
        let source = dir.join("source");
        fs::write(&source, b"new").unwrap();
        let from = dir.join("SOURCE");
        rename_new(&source, &from).unwrap();
        assert_eq!(entry_name(&from).unwrap(), "SOURCE");

        if alias.exists() {
            assert_eq!(entry_name(&alias).unwrap(), spelling.as_str());
            assert_eq!(rename_new(&from, &alias).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&dest).unwrap(), b"old");
            replace(&from, &alias).unwrap();
            assert_eq!(fs::read(&dest).unwrap(), b"new");
        } else {
            rename_new(&from, &alias).unwrap();
            assert_eq!(fs::read(dest).unwrap(), b"old");
            assert_eq!(fs::read(alias).unwrap(), b"new");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
