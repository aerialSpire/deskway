use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};

pub const UNIX_SOCKETS: bool = cfg!(unix);
const UNIX_SOCKET_UNSUPPORTED: &str = "Unix socket listeners are unavailable on Windows.";

pub fn watch_shutdown() -> io::Result<Arc<AtomicBool>> {
    let stopped = Arc::new(AtomicBool::new(false));
    watch_signals(stopped.clone())?;
    let parent = stopped.clone();
    std::thread::Builder::new().spawn(move || {
        // The supervisor alone holds the write end. The kernel closes it on exit.
        let _ = io::copy(&mut io::stdin().lock(), &mut io::sink());
        request_shutdown(&parent);
    })?;
    Ok(stopped)
}

fn request_shutdown(stopped: &AtomicBool) {
    if !stopped.swap(true, Ordering::AcqRel) {
        // A request or startup operation may be blocked in I/O. Bound the time to exit.
        std::thread::sleep(Duration::from_secs(3));
        std::process::exit(1);
    }
}

#[cfg(unix)]
fn block_shutdown_signals() -> io::Result<libc::sigset_t> {
    let signals = unsafe {
        let mut set = std::mem::MaybeUninit::uninit();
        libc::sigemptyset(set.as_mut_ptr());
        libc::sigaddset(set.as_mut_ptr(), libc::SIGINT);
        libc::sigaddset(set.as_mut_ptr(), libc::SIGTERM);
        set.assume_init()
    };
    let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(signals)
}

#[cfg(unix)]
fn watch_signals(stopped: Arc<AtomicBool>) -> io::Result<()> {
    // Spawned workers inherit the mask. Only the signal waiter consumes these signals.
    let signals = block_shutdown_signals()?;
    std::thread::Builder::new().spawn(move || {
        let mut received = 0;
        unsafe { libc::sigwait(&signals, &mut received) };
        request_shutdown(&stopped);
    })?;
    Ok(())
}

#[cfg(windows)]
fn watch_signals(_: Arc<AtomicBool>) -> io::Result<()> {
    Ok(())
}

pub struct HttpServer(tiny_http::Server);

impl HttpServer {
    pub fn new(listener: impl Into<tiny_http::Listener>) -> Result<Self> {
        let server = tiny_http::Server::from_listener(listener, None).map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Self(server))
    }

    pub fn server(&self) -> &tiny_http::Server {
        &self.0
    }

    #[cfg(windows)]
    pub fn unix(_: &Path) -> Result<Self> {
        bail!(UNIX_SOCKET_UNSUPPORTED);
    }
}

#[cfg(unix)]
mod unix;

pub fn validate_socket_path(path: &Path) -> Result<()> {
    if !UNIX_SOCKETS {
        bail!(UNIX_SOCKET_UNSUPPORTED);
    }
    if !path.is_absolute() || path.file_name().is_none() || path.to_string_lossy().contains(['\r', '\n']) {
        bail!("--listen unix:PATH requires an absolute path with a socket filename and no line breaks.");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
