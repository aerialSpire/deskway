//! TCP connections over data channels, one channel each (`tunnel`): the same at both ends. A
//! connection's bytes are read on a thread of its own and handed to the end's loop
//! (`PipeEvent`), which sends them on the channel (`Pipes::to_channel`, through an `Outbox`);
//! what the channel brings is written to the connection on another thread
//! (`Pipes::from_channel`), so the loop never waits on a socket. A connection whose channel
//! holds `HIGH` bytes not yet sent stops being read until it is down to `LOW`; one that closes
//! closes its channel once all it gave has gone.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use str0m::channel::ChannelId;
use str0m::Rtc;

use crate::outbox::Outbox;

const HIGH: usize = 4 << 20;
const LOW: usize = 1 << 20;
const READ: usize = 16 * 1024;

/// What a connection gives its end's loop.
pub enum PipeEvent {
    Data(ChannelId, Vec<u8>),
    /// The connection is closed (or failed): so is its channel.
    Closed(ChannelId),
}

struct Pipe {
    to_socket: Sender<Vec<u8>>,
    socket: TcpStream,
    paused: Arc<AtomicBool>,
    /// The connection has closed: its channel closes once what waits has gone.
    closing: bool,
}

/// The connections of one session, by channel.
#[derive(Default)]
pub struct Pipes {
    pipes: HashMap<ChannelId, Pipe>,
    outbox: Outbox,
}

impl Pipes {
    /// Joins `socket` to channel `id`, which is open: its bytes go to `events`.
    pub fn add<E: From<PipeEvent> + Send + 'static>(&mut self, id: ChannelId, socket: TcpStream, events: Sender<E>) -> std::io::Result<()> {
        let (mut reader, mut writer) = (socket.try_clone()?, socket.try_clone()?);
        let paused = Arc::new(AtomicBool::new(false));
        let wait = paused.clone();
        std::thread::Builder::new().name("pipe-read".into()).spawn(move || {
            let mut buf = vec![0u8; READ];
            loop {
                while wait.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if events.send(PipeEvent::Data(id, buf[..n].to_vec()).into()).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = events.send(PipeEvent::Closed(id).into());
        })?;
        let (to_socket, from_channel) = channel::<Vec<u8>>();
        std::thread::Builder::new().name("pipe-write".into()).spawn(move || {
            for data in from_channel {
                if writer.write_all(&data).is_err() {
                    break;
                }
            }
            // The channel closed: the other side reads to its end.
            let _ = writer.shutdown(Shutdown::Write);
        })?;
        self.pipes.insert(id, Pipe { to_socket, socket, paused, closing: false });
        Ok(())
    }

    /// Whether channel `id` is a connection's.
    pub fn has(&self, id: ChannelId) -> bool {
        self.pipes.contains_key(&id)
    }

    /// What channel `id` brought, for its connection. False when it is no connection's.
    pub fn from_channel(&mut self, id: ChannelId, data: &[u8]) -> bool {
        match self.pipes.get(&id) {
            Some(p) => {
                let _ = p.to_socket.send(data.to_vec());
                true
            }
            None => false,
        }
    }

    /// Channel `id` closed: its connection is done once what it brought is written.
    pub fn channel_closed(&mut self, id: ChannelId) {
        self.pipes.remove(&id);
        self.outbox.forget(id);
    }

    /// What a connection gave, onto its channel; a closed connection closes it.
    pub fn to_channel(&mut self, rtc: &mut Rtc, ev: PipeEvent) {
        match ev {
            PipeEvent::Data(id, data) => {
                if !self.pipes.contains_key(&id) {
                    return;
                }
                self.outbox.send(rtc, id, data);
                if self.unsent(rtc, id) > HIGH {
                    self.pipes[&id].paused.store(true, Ordering::Relaxed);
                }
            }
            PipeEvent::Closed(id) => {
                if let Some(p) = self.pipes.get_mut(&id) {
                    p.closing = true;
                }
            }
        }
        self.drained(rtc);
    }

    /// Bytes given for channel `id` that have not left yet.
    fn unsent(&self, rtc: &mut Rtc, id: ChannelId) -> usize {
        self.outbox.waiting(id) + rtc.channel(id).map_or(0, |mut ch| ch.buffered_amount())
    }

    /// What waits goes as the channels take it; connections paused while their channel was
    /// full read again once it has drained, and closed ones close their channel once empty.
    pub fn drained(&mut self, rtc: &mut Rtc) {
        self.outbox.flush(rtc);
        let mut done = Vec::new();
        for (id, p) in &self.pipes {
            if p.closing && self.outbox.waiting(*id) == 0 {
                done.push(*id);
            } else if p.paused.load(Ordering::Relaxed) && self.unsent(rtc, *id) < LOW {
                p.paused.store(false, Ordering::Relaxed);
            }
        }
        for id in done {
            self.close(rtc, id);
        }
    }

    fn close(&mut self, rtc: &mut Rtc, id: ChannelId) {
        if let Some(p) = self.pipes.remove(&id) {
            let _ = p.socket.shutdown(Shutdown::Both);
            self.outbox.forget(id);
            rtc.direct_api().close_data_channel(id);
        }
    }

    /// Every connection ends with the session.
    pub fn close_all(&mut self) {
        for (_, p) in self.pipes.drain() {
            let _ = p.socket.shutdown(Shutdown::Both);
        }
    }
}

/// The label of the channel for a connection to port `port` of the computer's loopback.
pub fn label(port: u16) -> String {
    format!("tunnel:{port}")
}

/// The port a channel's label names, if it is a connection's.
pub fn port(label: &str) -> Option<u16> {
    label.strip_prefix("tunnel:")?.parse().ok().filter(|p| *p != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connections_channel_names_its_port() {
        assert_eq!(port(&label(8080)), Some(8080));
        for l in ["tunnel:0", "tunnel:70000", "tunnel:x", "ai", "control"] {
            assert_eq!(port(l), None, "{l}");
        }
    }
}
