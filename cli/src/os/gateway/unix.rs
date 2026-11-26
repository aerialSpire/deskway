use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixListener;
use std::path::Path;

use anyhow::{bail, Context, Result};
use socket2::{Domain, SockAddr, Socket, Type};

use super::HttpServer;

const PRIVATE_DIRECTORY: &str = "The Unix socket's parent must be a directory owned by this user, with no group or other access.";

impl HttpServer {
    pub fn unix(path: &Path) -> Result<Self> {
        const OPEN_PARENT: &str = "Open the Unix socket's parent directory.";
        let directory = path.parent().expect("validated socket path").canonicalize().context(OPEN_PARENT)?;
        let path = directory.join(path.file_name().expect("validated socket path"));
        let parent = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY)
            .open(directory).context(OPEN_PARENT)?;
        let uid = unsafe { libc::geteuid() };
        let directory = parent.metadata().context(OPEN_PARENT)?;
        if directory.uid() != uid || directory.mode() & 0o077 != 0 || has_acl(&parent)? {
            bail!(PRIVATE_DIRECTORY);
        }
        // bind creates the path. No connection can succeed until after chmod and listen.
        let address = SockAddr::unix(&path)?;
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        if let Err(e) = socket.bind(&address) {
            if e.kind() == ErrorKind::AddrInUse {
                bail!("The Unix socket path already exists.");
            }
            return Err(e.into());
        }
        let start = || -> Result<_> {
            crate::os::set_unix_mode(&path, 0o600)?;
            socket.listen(128)?;
            Self::new(UnixListener::from(OwnedFd::from(socket)))
        };
        let result = start();
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result
    }
}

#[cfg(target_os = "linux")]
fn has_acl(_: &File) -> Result<bool> {
    // Linux's group mode bits also mask access for users and groups named by a POSIX ACL.
    Ok(false)
}

#[cfg(target_os = "macos")]
fn has_acl(directory: &File) -> Result<bool> {
    use std::os::fd::AsRawFd;

    extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    // Darwin's extended ACLs can grant access independently of the group and other mode bits.
    let acl = unsafe { acl_get_fd(directory.as_raw_fd()) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        return match error.kind() {
            ErrorKind::NotFound | ErrorKind::Unsupported => Ok(false),
            _ => Err(error).context(PRIVATE_DIRECTORY),
        };
    }
    unsafe { acl_free(acl) };
    Ok(true)
}
