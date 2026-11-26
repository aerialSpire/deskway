//! Terminal and state-directory operations shared by the Unix hosts.
pub(super) mod programs;

use std::fs::{File, OpenOptions};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

/// Hold this file open for as long as the host owns its state directory.
pub(super) fn lock_host() -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(crate::config::dir()?.join("host.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(file)
}

/// Turn off echo for one read from terminal stdin and restore its settings afterwards.
pub fn without_echo<T>(read: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let fd = libc::STDIN_FILENO;
    let mut original = MaybeUninit::uninit();
    let original = unsafe {
        if libc::tcgetattr(fd, original.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        original.assume_init()
    };
    let mut hidden = original;
    hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let result = read();
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) } != 0 {
        return Err(io::Error::last_os_error());
    }
    result
}
