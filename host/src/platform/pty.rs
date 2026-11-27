//! The browser PTY's ordered output and exit, and bounded input to its native writer.

use std::io::{self, Write};
#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
#[cfg(windows)]
use std::thread;

use wire::terminal::{FRAME, OUTPUT_WINDOW};

pub(crate) const OUTPUT_FRAMES: usize = OUTPUT_WINDOW / FRAME;

#[derive(Debug)]
pub(crate) enum PtyEvent {
    Bytes(Vec<u8>),
    Exit(i32),
    Error(io::Error),
}

pub struct PtyEvents {
    pub(crate) output: Box<dyn FnMut(PtyEvent) -> bool + Send>,
    pub(crate) writable: Arc<dyn Fn() + Send + Sync>,
}

#[cfg(test)]
pub(crate) fn test_events() -> (PtyEvents, std::sync::mpsc::Receiver<PtyEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    (PtyEvents { output: Box::new(move |event| tx.send(event).is_ok()), writable: Arc::new(|| {}) }, rx)
}

#[cfg(windows)]
impl PtyEvents {
    /// Owns the blocking reader. Report the status after the last bytes, even if the
    /// process waiter finished before the browser had room for all the output.
    pub(crate) fn read(mut self, mut reader: impl Read, status: impl FnOnce() -> io::Result<i32>) {
        let mut bytes = [0; FRAME];
        let result = loop {
            match reader.read(&mut bytes) {
                Ok(0) => break status(),
                Ok(len) => if !(self.output)(PtyEvent::Bytes(bytes[..len].to_vec())) { return; },
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            }
        };
        (self.output)(match result {
            Ok(code) => PtyEvent::Exit(code),
            Err(e) => PtyEvent::Error(e),
        });
    }
}

/// A live PTY with nonblocking input and cleanup on drop.
pub trait Pty: Write + Send {
    fn listen(&mut self, events: PtyEvents) -> io::Result<()>;
    fn consumed(&mut self) {}
    fn set_attached(&mut self, _attached: bool) {}
    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()>;
    /// Apply the size and notify the foreground program even if the size is unchanged.
    fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()>;
    /// End the shell and its jobs. Returns false if the shell was already exiting.
    fn terminate(&mut self) -> io::Result<bool>;
}

#[cfg(windows)]
pub(crate) struct PtyInput(SyncSender<Vec<u8>>);

#[cfg(windows)]
impl PtyInput {
    pub(crate) fn start(mut write: impl FnMut(&[u8]) -> io::Result<()> + Send + 'static, writable: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let (tx, rx) = sync_channel::<Vec<u8>>(1);
        thread::Builder::new().name("pty-input".into()).spawn(move || {
            for bytes in rx {
                writable();
                if write(&bytes).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self(tx))
    }

    pub(crate) fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        self.0.try_send(bytes[..bytes.len().min(FRAME)].to_vec()).map_err(|error| match error {
            TrySendError::Full(_) => io::ErrorKind::WouldBlock,
            TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
        })?;
        Ok(bytes.len().min(FRAME))
    }
}
