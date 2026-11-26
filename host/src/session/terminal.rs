//! The desk seat's data-only role, with one session-owned PTY.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use anyhow::Context;
use peer::state::Signal;
use str0m::change::SdpOffer;
use str0m::channel::{Channel, ChannelData, ChannelId, Reliability};
use str0m::Event;
use wire::control::Control;
use wire::terminal::{Terminal as Msg, TerminalError, CHANNEL, FRAME, INPUT_WINDOW, OUTPUT_WINDOW};

use crate::platform::{OpenPty, Pty};
use super::{Cx, Role};

const HIGH: usize = 64 * 1024;
const LOW: usize = 16 * 1024;
const POLL: Duration = Duration::from_millis(10);

/// A data-only offer has a single application section. The original SDP is covered by the
/// same proof as a picture offer.
pub fn data_only(sdp: &str) -> bool {
    SdpOffer::from_sdp_string(sdp).is_ok_and(|offer| {
        matches!(&offer.media_lines[..], [media] if media.typ.is_channel() && !media.disabled)
    })
}

#[derive(Default)]
struct OutputCredit {
    paused: bool,
    outstanding: usize,
}

impl OutputCredit {
    fn capacity(&mut self, buffered: usize) -> usize {
        self.paused = if self.paused { buffered >= LOW } else { buffered >= HIGH };
        if self.paused { 0 } else { FRAME.min(HIGH - buffered).min(OUTPUT_WINDOW - self.outstanding) }
    }

    fn acknowledge(&mut self, bytes: usize) -> bool {
        if bytes == 0 || bytes > self.outstanding {
            return false;
        }
        self.outstanding -= bytes;
        true
    }
}

#[derive(Default)]
struct Input {
    queued: VecDeque<u8>,
    written: usize,
}

impl Input {
    fn receive(&mut self, bytes: Vec<u8>) -> bool {
        if self.queued.len() + self.written + bytes.len() > INPUT_WINDOW {
            return false;
        }
        self.queued.extend(bytes);
        true
    }

    fn write(&mut self, pty: &mut (impl io::Write + ?Sized)) -> io::Result<()> {
        if !self.queued.is_empty() {
            match pty.write(self.queued.as_slices().0) {
                Ok(n) => {
                    self.queued.drain(..n);
                    self.written += n;
                }
                Err(e) if retry(&e) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn acknowledge(&mut self, send: impl FnOnce(usize) -> anyhow::Result<bool>) -> anyhow::Result<bool> {
        if self.written == 0 || !send(self.written)? {
            return Ok(false);
        }
        self.written = 0;
        Ok(true)
    }
}

struct Stream {
    pty: Box<dyn Pty>,
    input: Input,
    output: Vec<u8>,
    credit: OutputCredit,
    eof: bool,
}

impl Stream {
    fn new(pty: Box<dyn Pty>) -> Self {
        Self { pty, input: Input::default(), output: Vec::new(), credit: OutputCredit::default(), eof: false }
    }

    /// At most one PTY read and write per pass, so continuous output still yields to
    /// incoming control messages and terminal input.
    fn pump(&mut self, ch: &mut Channel<'_>) -> anyhow::Result<(bool, Option<i32>)> {
        let code = self.pty.exit().context("check PTY exit status")?;
        let capacity = self.credit.capacity(ch.buffered_amount());
        if self.output.is_empty() && !self.eof && capacity > 0 {
            self.output.resize(capacity, 0);
            let len = match self.pty.read(&mut self.output) {
                Ok(n) => { self.eof = n == 0; n }
                Err(e) if retry(&e) => 0,
                Err(e) => return Err(e).context("read PTY"),
            };
            self.output.truncate(len);
        }
        let sent = !self.output.is_empty() && self.output.len() <= capacity && ch.write(true, &self.output)?;
        if sent {
            self.credit.outstanding += self.output.len();
            self.output.clear();
        }
        if !self.eof && code.is_none() {
            self.input.write(&mut *self.pty).context("write PTY")?;
        }
        let acked = self.input.acknowledge(|bytes| {
            Ok(ch.write(false, &serde_json::to_vec(&Msg::Ack { bytes })?)?)
        })?;
        Ok((sent || acked, code.filter(|_| self.eof)))
    }
}

fn retry(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

pub struct Terminal {
    name: String,
    open: Option<OpenPty>,
    channel: Option<ChannelId>,
    stream: Option<Stream>,
    reply: Option<Msg>,
}

impl Terminal {
    pub fn new(name: String, open: OpenPty) -> Self {
        eprintln!("terminal session started by {name} (Desk seat)");
        Self { name, open: Some(open), channel: None, stream: None, reply: None }
    }

    fn close(&mut self, reply: Option<Msg>) {
        self.open = None;
        self.stream = None;
        self.reply = reply;
    }

    fn error(&mut self, why: TerminalError) {
        self.close(Some(Msg::Error { why }));
    }

    fn message(&mut self, cx: &Cx<'_>, d: ChannelData) {
        if self.open.is_none() && self.stream.is_none() {
            return;
        }
        if d.data.len() > FRAME {
            self.error(TerminalError::Protocol);
            return;
        }
        if d.binary {
            if let Some(stream) = &mut self.stream {
                if !stream.input.receive(d.data) {
                    self.error(TerminalError::Protocol);
                }
            }
            return;
        }
        match serde_json::from_slice(&d.data) {
            Ok(Msg::Open { rows, cols }) if cx.control().is_some() && rows > 0 && cols > 0 => {
                let Some(open) = self.open.take() else { return self.error(TerminalError::Protocol) };
                match open(rows, cols) {
                    Ok(pty) => {
                        self.stream = Some(Stream::new(pty));
                        self.reply = Some(Msg::Opened {});
                    }
                    Err(e) => {
                        eprintln!("open PTY: {e}");
                        self.error(TerminalError::Open);
                    }
                }
            }
            Ok(Msg::Resize { rows, cols }) if rows > 0 && cols > 0 => {
                if let Some(stream) = &mut self.stream {
                    if let Err(e) = stream.pty.resize(rows, cols) {
                        eprintln!("resize PTY: {e}");
                        self.error(TerminalError::Io);
                    }
                }
            }
            Ok(Msg::Ack { bytes }) => {
                if let Some(stream) = &mut self.stream {
                    if !stream.credit.acknowledge(bytes) {
                        self.error(TerminalError::Protocol);
                    }
                }
            }
            Ok(Msg::Close {}) => self.close(None),
            _ => self.error(TerminalError::Protocol),
        }
    }

    fn pump(&mut self, ch: &mut Channel<'_>) -> anyhow::Result<Option<Instant>> {
        let replied = if let Some(reply) = &self.reply {
            if !ch.write(false, &serde_json::to_vec(reply)?)? {
                return Ok(Some(Instant::now() + POLL));
            }
            self.reply = None;
            true
        } else {
            false
        };
        let Some(stream) = &mut self.stream else { return Ok(replied.then(Instant::now)) };
        let (progress, exit) = stream.pump(ch)?;
        if let Some(code) = exit {
            self.close(Some(Msg::Exit { code }));
        }
        Ok(Some(if progress || exit.is_some() { Instant::now() } else { Instant::now() + POLL }))
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close(None);
        eprintln!("terminal session ended for {} (Desk seat)", self.name);
    }
}

impl Role for Terminal {
    fn releaser(&self) -> Box<dyn Fn() + Send> {
        Box::new(|| {})
    }

    fn accepts(&self, sdp: &str) -> bool {
        data_only(sdp)
    }

    fn channel_open(&mut self, cx: &mut Cx, id: ChannelId, label: &str) -> bool {
        let reliable = cx.rtc.channel(id).is_some_and(|ch| {
            ch.config().is_some_and(|c| c.ordered && matches!(c.reliability, Reliability::Reliable))
        });
        match label {
            "control" if reliable && cx.control().is_none() => return true,
            CHANNEL if reliable && self.channel.is_none() => self.channel = Some(id),
            _ => cx.rtc.direct_api().close_data_channel(id),
        }
        false
    }

    fn channel_data(&mut self, cx: &mut Cx, d: ChannelData) -> bool {
        if cx.control() == Some(d.id) && !d.binary {
            if let Ok(Control::End { reason, .. }) = serde_json::from_slice(&d.data) {
                return cx.signal(Signal::PeerEnded(reason));
            }
        } else if self.channel == Some(d.id) {
            self.message(cx, d);
        }
        false
    }

    fn rtc_event(&mut self, _: &mut Cx, e: Event) {
        if matches!(e, Event::ChannelClose(id) if self.channel == Some(id)) {
            self.close(None);
        }
    }

    fn before_wait(&mut self, cx: &mut Cx, _: usize) -> Option<Instant> {
        if !cx.active() {
            return None;
        }
        let mut ch = cx.rtc.channel(self.channel?)?;
        match self.pump(&mut ch) {
            Ok(next) => next,
            Err(e) => {
                eprintln!("terminal stream failed: {e:#}");
                self.error(TerminalError::Io);
                // If the channel cannot send even an error, its close reports the failure.
                if self.pump(&mut ch).is_err() {
                    cx.rtc.direct_api().close_data_channel(self.channel?);
                    self.close(None);
                }
                Some(Instant::now())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_credit_returns_only_after_a_pty_write_and_a_sent_ack() {
        let mut input = Input::default();
        for _ in 0..INPUT_WINDOW / FRAME {
            assert!(input.receive(vec![3; FRAME]));
        }
        assert!(!input.receive(vec![4]));
        assert!(!input.acknowledge(|_| panic!("no input was written")).unwrap());

        let mut short = [0; 7];
        input.write(&mut short.as_mut_slice()).unwrap();
        assert_eq!(short, [3; 7]);
        assert!(!input.acknowledge(|bytes| { assert_eq!(bytes, 7); Ok(false) }).unwrap());
        assert!(!input.receive(vec![4]));

        assert!(input.acknowledge(|bytes| { assert_eq!(bytes, 7); Ok(true) }).unwrap());
        assert!(!input.acknowledge(|_| panic!("credit already returned")).unwrap());
        assert!(!input.receive(vec![4; 8]));
        assert!(input.receive(vec![4; 7]));
        let mut rest = Vec::new();
        while !input.queued.is_empty() {
            input.write(&mut rest).unwrap();
        }
        assert_eq!(rest.len(), INPUT_WINDOW);
        assert!(rest[..INPUT_WINDOW - 7].iter().all(|&byte| byte == 3));
        assert_eq!(rest[INPUT_WINDOW - 7..], [4; 7]);
    }

    #[test]
    fn output_uses_channel_hysteresis_and_emulator_credit() {
        let mut flow = OutputCredit::default();
        for (buffered, expected) in [(0, FRAME), (HIGH - 1, 1), (HIGH, 0), (LOW, 0), (LOW - 1, FRAME)] {
            assert_eq!(flow.capacity(buffered), expected);
        }
        flow.outstanding = OUTPUT_WINDOW;
        assert_eq!(flow.capacity(0), 0);
        for invalid in [0, OUTPUT_WINDOW + 1] {
            assert!(!flow.acknowledge(invalid));
            assert_eq!(flow.outstanding, OUTPUT_WINDOW);
        }
        assert!(flow.acknowledge(7));
        assert_eq!(flow.capacity(0), 7);
    }
}
