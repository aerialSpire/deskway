//! The terminal driver for local streams. I/O and the renderer run on the other end of
//! a bounded connection, so the driver never blocks on a write under the registry lock.

use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::sync::{mpsc::{self, SyncSender}, Arc, Condvar, Mutex};

use super::{registry::{Lease, LocalTerminals}, Port, PumpError, Terminal};

#[derive(Clone, Default)]
pub(crate) struct LocalSlot(Arc<Mutex<Option<LocalTerminals>>>);

impl LocalSlot {
    pub fn publish(&self, terminals: Option<LocalTerminals>) {
        *self.0.lock().unwrap() = terminals;
    }

    pub fn get(&self) -> Option<LocalTerminals> {
        self.0.lock().unwrap().clone()
    }
}

pub(crate) struct Frame {
    pub binary: bool,
    pub bytes: Vec<u8>,
}

const QUEUE_FRAMES: usize = 32;

#[derive(Default)]
struct Queue {
    frames: VecDeque<Frame>,
    bytes: usize,
}

#[derive(Default)]
struct Queues {
    incoming: [Queue; 2],
    closed: bool,
}

struct Link {
    queues: Mutex<Queues>,
    changed: Condvar,
    driver: Option<SyncSender<()>>,
}

pub(crate) struct Connection {
    link: Arc<Link>,
    side: usize,
}

impl Connection {
    fn pair(driver: Option<SyncSender<()>>) -> (Self, Self) {
        let link = Arc::new(Link { queues: Mutex::default(), changed: Condvar::new(), driver });
        (Self { link: link.clone(), side: 0 }, Self { link, side: 1 })
    }

    pub fn cancel(&self) {
        let mut queues = self.link.queues.lock().unwrap();
        *queues = Queues { closed: true, ..Queues::default() };
        self.changed();
        self.wake_driver();
    }

    pub fn finish(&self) {
        self.link.queues.lock().unwrap().closed = true;
        self.changed();
        self.wake_driver();
    }

    fn discard_output(&self) {
        let mut queues = self.link.queues.lock().unwrap();
        let output = &mut queues.incoming[1 - self.side];
        output.frames.retain(|frame| !frame.binary);
        output.bytes = output.frames.iter().map(|frame| frame.bytes.len()).sum();
        self.changed();
    }

    fn closed(&self) -> bool {
        self.link.queues.lock().unwrap().closed
    }

    fn changed(&self) {
        self.link.changed.notify_all();
    }

    fn wake_driver(&self) {
        if let Some(driver) = &self.link.driver {
            let _ = driver.try_send(());
        }
    }

    fn send_with(&self, frame: Frame, wait: bool) -> io::Result<bool> {
        let mut queues = self.link.queues.lock().unwrap();
        loop {
            if queues.closed { return Err(ErrorKind::BrokenPipe.into()); }
            if frame.bytes.len() > wire::terminal::FRAME {
                return Err(io::Error::new(ErrorKind::InvalidInput, "frame too large"));
            }
            let to = &mut queues.incoming[1 - self.side];
            if to.frames.len() < QUEUE_FRAMES {
                to.bytes += frame.bytes.len();
                to.frames.push_back(frame);
                self.changed();
                if self.side == 1 { self.wake_driver(); }
                return Ok(true);
            }
            if !wait { return Ok(false); }
            queues = self.link.changed.wait(queues).unwrap();
        }
    }

    /// Ignore late window input while the window drains the driver's final frames.
    pub fn send(&self, frame: Frame) -> io::Result<()> {
        match self.send_with(frame, false) {
            Ok(true) => Ok(()),
            Ok(false) => Err(ErrorKind::WouldBlock.into()),
            Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
            Err(e) => Err(e),
        }
    }

    #[cfg(windows)]
    pub fn send_wait(&self, frame: Frame) -> io::Result<()> {
        self.send_with(frame, true).map(|_| ())
    }

    fn receive_with(&self, wait: bool) -> Option<Frame> {
        let mut queues = self.link.queues.lock().unwrap();
        loop {
            let from = &mut queues.incoming[self.side];
            if let Some(frame) = from.frames.pop_front() {
                from.bytes -= frame.bytes.len();
                self.changed();
                if self.side == 1 { self.wake_driver(); }
                return Some(frame);
            }
            if queues.closed || !wait { return None; }
            queues = self.link.changed.wait(queues).unwrap();
        }
    }

    pub fn receive(&self) -> Option<Frame> {
        self.receive_with(true)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Port for Connection {
    fn write(&mut self, binary: bool, data: &[u8]) -> anyhow::Result<bool> {
        Ok(self.send_with(Frame { binary, bytes: data.into() }, false)?)
    }

    fn buffered(&mut self) -> usize {
        self.link.queues.lock().unwrap().incoming[1 - self.side].bytes
    }
}

#[cfg(windows)]
pub(crate) fn connection_pair() -> (Connection, Connection) {
    Connection::pair(None)
}

pub(crate) fn connect(lease: Lease) -> Connection {
    let (wake, ready) = mpsc::sync_channel(1);
    let (mut driver, client) = Connection::pair(Some(wake.clone()));
    std::thread::spawn(move || {
        let mut terminal = Terminal::new(lease.clone(), move || { let _ = wake.try_send(()); });
        terminal.negotiate(&[wire::caps::TERMINALS.into(), wire::caps::TERMINAL_TAKEOVER.into()], true);
        while !driver.closed() {
            if !lease.accessible() { driver.discard_output(); }
            let frame = driver.receive_with(false);
            let received = frame.is_some();
            if let Some(frame) = frame {
                terminal.message(true, frame.binary, frame.bytes);
            }
            match terminal.pump(&mut driver) {
                Ok(_) if terminal.finished() => break,
                Ok(true) => continue,
                Ok(false) if received => continue,
                Ok(false) => { if ready.recv().is_err() { break; } }
                Err(failure) => {
                    let broken = matches!(failure, PumpError::Transport(_));
                    terminal.pump_failed(failure);
                    if broken { break; }
                }
            }
        }
        terminal.transport_closed();
    });
    client
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::terminal::{DetachReason, Terminal as Msg};

    #[test]
    fn a_late_ack_leaves_the_final_frame_for_the_window() {
        for message in [Msg::Exit { code: 17 }, Msg::Detached { reason: DetachReason::Taken }] {
            let (mut driver, client) = Connection::pair(None);
            let bytes = serde_json::to_vec(&message).unwrap();
            assert!(driver.write(false, &bytes).unwrap());
            drop(driver);

            let ack = serde_json::to_vec(&Msg::Ack { bytes: 1 }).unwrap();
            client.send(Frame { binary: false, bytes: ack }).unwrap();
            let final_frame = client.receive().unwrap();
            assert!(!final_frame.binary);
            assert_eq!(final_frame.bytes, bytes);
            assert!(client.receive().is_none());
        }
    }
}
