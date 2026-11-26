//! The desk seat's data-only role, with an attachment to the host's PTY.

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

use crate::i18n::t;
use wire::types::Reason;
use registry::{Closed, Lease, Shell};
use replay::History;

pub mod registry;
mod replay;
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

struct Replay {
    prefix: Vec<u8>,
    len: usize,
    sent: usize,
}

impl Replay {
    fn new(history: &History) -> Self {
        let prefix = history.mode_prefix();
        Self { len: prefix.len() + history.bytes.len(), prefix, sent: 0 }
    }
}

struct Stream {
    size: (u16, u16),
    replay: Option<Replay>,
    input: Input,
    output: Vec<u8>,
    credit: OutputCredit,
}

impl Stream {
    fn new(rows: u16, cols: u16, replay: Option<Replay>) -> Self {
        Self { size: (rows, cols), replay, input: Input::default(), output: Vec::new(), credit: OutputCredit::default() }
    }

    fn resize(&mut self, shell: &mut Shell, rows: u16, cols: u16) -> io::Result<()> {
        self.size = (rows, cols);
        if self.replay.is_none() {
            shell.pty.resize(rows, cols)?;
        }
        Ok(())
    }

    fn pump(&mut self, shell: &mut Shell, buffered: usize, mut send: impl FnMut(bool, &[u8]) -> anyhow::Result<bool>) -> anyhow::Result<(bool, Option<i32>)> {
        let code = shell.pty.exit().context("check PTY exit status")?;
        let capacity = self.credit.capacity(buffered);
        if let Some(replay) = &mut self.replay {
            if replay.sent < replay.len {
                self.output.clear();
                self.output.extend(replay.prefix.iter().chain(&shell.replay.bytes).skip(replay.sent).take(capacity));
                let sent = !self.output.is_empty() && send(true, &self.output)?;
                if sent {
                    replay.sent += self.output.len();
                    self.credit.outstanding += self.output.len();
                }
                self.output.clear();
                return Ok((sent, None));
            }
            // Refresh the foreground program only after the emulator consumed the replay.
            if self.credit.outstanding != 0 {
                return Ok((false, None));
            }
            if code.is_none() {
                if let Err(e) = shell.pty.redraw(self.size.0, self.size.1) {
                    eprintln!("{}: {e}", t("terminal.redraw_failed"));
                }
            }
            self.replay = None;
        }
        // At most one read and write per pass, even with a busy PTY.
        if self.output.is_empty() && capacity > 0 {
            self.output.resize(capacity, 0);
            let len = shell.read(&mut self.output).context("read PTY")?;
            self.output.truncate(len);
        }
        let sent = !self.output.is_empty() && self.output.len() <= capacity && send(true, &self.output)?;
        if sent {
            self.credit.outstanding += self.output.len();
            self.output.clear();
        }
        if !shell.eof && code.is_none() {
            self.input.write(&mut *shell.pty).context("write PTY")?;
        }
        let acked = self.input.acknowledge(|bytes| {
            send(false, &serde_json::to_vec(&Msg::Ack { bytes })?)
        })?;
        Ok((sent || acked, code.filter(|_| shell.eof && self.output.is_empty())))
    }
}

fn retry(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

pub struct Terminal {
    lease: Lease,
    channel: Option<ChannelId>,
    stream: Option<Stream>,
    reply: Option<Msg>,
    done: bool,
}

impl Terminal {
    pub fn new(lease: Lease) -> Self {
        Self { lease, channel: None, stream: None, reply: None, done: false }
    }

    fn finish(&mut self, reply: Option<Msg>) {
        self.stream = None;
        self.reply = reply;
        self.done = true;
    }

    fn error(&mut self, why: TerminalError) {
        self.lease.detach(why.as_str());
        self.finish(Some(Msg::Error { why }));
    }

    fn open(&mut self, rows: u16, cols: u16) {
        match self.lease.attach(rows, cols) {
            Ok(Some((replay, dropped))) => {
                self.reply = Some(Msg::Opened {
                    replay: replay.as_ref().map(|replay| replay.len),
                    dropped: dropped.then_some(true),
                });
                self.stream = Some(Stream::new(rows, cols, replay));
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("{}: {e}", t("terminal.open_failed"));
                self.error(TerminalError::Open);
            }
        }
    }

    fn message(&mut self, active: bool, d: ChannelData) {
        if self.done {
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
            Ok(Msg::Open { rows, cols }) if active && rows > 0 && cols > 0 && self.stream.is_none() => self.open(rows, cols),
            Ok(Msg::Resize { rows, cols }) if rows > 0 && cols > 0 => {
                if let Some(stream) = &mut self.stream {
                    if let Some(Err(e)) = self.lease.with_shell(|shell| stream.resize(shell, rows, cols)) {
                        eprintln!("{}: {e}", t("terminal.io"));
                        self.lease.close(Closed::Io);
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
            Ok(Msg::Close {}) => {
                self.lease.detach(Reason::Left.as_str());
                self.finish(None);
            }
            Ok(Msg::Terminate {}) if active => match self.lease.terminate() {
                Some(Ok(true)) => self.finish(Some(Msg::Closed {})),
                Some(Ok(false)) => {} // The stream will drain the shell's output and send exit.
                Some(Err(e)) => {
                    eprintln!("{}: {e}", t("terminal.close_failed"));
                    self.reply = Some(Msg::Error { why: TerminalError::Close });
                }
                None => self.error(TerminalError::Protocol),
            },
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
        let Some(result) = self.lease.with_shell(|shell| {
            stream.pump(shell, ch.buffered_amount(), |binary, data| Ok(ch.write(binary, data)?))
        }) else { return Ok(None) };
        let (progress, exit) = result?;
        if let Some(code) = exit {
            self.lease.close(Closed::Exit(code));
            self.finish(Some(Msg::Exit { code }));
        }
        Ok(Some(if progress || exit.is_some() { Instant::now() } else { Instant::now() + POLL }))
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
            self.message(cx.active(), d);
        }
        false
    }

    fn rtc_event(&mut self, _: &mut Cx, e: Event) {
        if matches!(e, Event::ChannelClose(id) if self.channel == Some(id)) {
            self.lease.detach(Reason::Failed.as_str());
            self.finish(None);
        }
    }

    fn ended(&mut self, reason: Reason) {
        self.lease.detach(reason.as_str());
    }

    fn before_wait(&mut self, cx: &mut Cx, _: usize) -> Option<Instant> {
        if !cx.active() {
            return None;
        }
        let mut ch = cx.rtc.channel(self.channel?)?;
        match self.pump(&mut ch) {
            Ok(next) => next,
            Err(e) => {
                eprintln!("{}: {e:#}", t("terminal.io"));
                if e.is::<io::Error>() {
                    self.lease.close(Closed::Io);
                }
                self.error(TerminalError::Io);
                // If the channel cannot send even an error, its close reports the failure.
                if self.pump(&mut ch).is_err() {
                    cx.rtc.direct_api().close_data_channel(self.channel?);
                    self.finish(None);
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
