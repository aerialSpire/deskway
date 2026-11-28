//! A terminal attachment to the host's PTY.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::Arc;

use anyhow::Context;
use wire::terminal::{DetachReason, Terminal as Msg, TerminalError, FRAME, INPUT_WINDOW, OUTPUT_WINDOW};

use crate::i18n::t;
use wire::types::Reason;
use registry::{AttachmentId, Closed, Lease, Selection, Shell};
use replay::History;

pub mod registry;
#[cfg(any(windows, target_os = "macos"))]
pub(crate) mod local;
mod replay;
use super::credit::Credit;

const HIGH: usize = 64 * 1024;
pub(super) const LOW: usize = 16 * 1024;

type Notify = Arc<dyn Fn() + Send + Sync>;

pub(super) trait Port {
    /// Returns false if the transport did not accept the frame.
    fn write(&mut self, binary: bool, data: &[u8]) -> anyhow::Result<bool>;
    fn buffered(&mut self) -> usize;
}

#[derive(Debug)]
pub(super) enum PumpError {
    Terminal(anyhow::Error),
    Transport(anyhow::Error),
}

// Shell::output passes through send errors alongside PTY errors.
#[derive(Debug)]
struct PortWrite(anyhow::Error);

impl fmt::Display for PortWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for PortWrite {}

#[derive(Default)]
struct OutputCredit {
    paused: bool,
    window: Credit,
}

impl OutputCredit {
    fn capacity(&mut self, buffered: usize) -> usize {
        self.paused = if self.paused { buffered >= LOW } else { buffered >= HIGH };
        if self.paused { 0 } else { FRAME.min(HIGH - buffered).min(self.window.available(OUTPUT_WINDOW)) }
    }

    fn acknowledge(&mut self, bytes: usize) -> bool {
        self.window.acknowledge(bytes)
    }
}

#[derive(Default)]
struct Input {
    queued: VecDeque<u8>,
    written: usize,
    credit: Credit,
}

impl Input {
    fn receive(&mut self, bytes: Vec<u8>) -> bool {
        if !bytes.is_empty() && !self.credit.take(bytes.len(), INPUT_WINDOW) {
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
        self.credit.acknowledge(self.written);
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
        let capacity = self.credit.capacity(buffered);
        if let Some(replay) = &mut self.replay {
            if replay.sent < replay.len {
                self.output.clear();
                self.output.extend(replay.prefix.iter().chain(&shell.replay.bytes).skip(replay.sent).take(capacity));
                let sent = !self.output.is_empty() && send(true, &self.output)?;
                if sent {
                    replay.sent += self.output.len();
                    self.credit.window.take(self.output.len(), OUTPUT_WINDOW);
                }
                self.output.clear();
                return Ok((sent, None));
            }
            // Refresh the foreground program only after the emulator consumed the replay.
            if self.credit.window.outstanding() != 0 {
                return Ok((false, None));
            }
            if let Err(e) = shell.pty.redraw(self.size.0, self.size.1) {
                eprintln!("{}: {e}", t("terminal.redraw_failed"));
            }
            self.replay = None;
        }
        // At most one slice of output and one write per pass, even with a busy PTY.
        let (sent, exit) = shell.output(capacity, |data| send(true, data))?;
        self.credit.window.take(sent, OUTPUT_WINDOW);
        if exit.is_some() { return Ok((false, exit)); }
        let before = self.input.queued.len();
        self.input.write(&mut *shell.pty).context("write PTY")?;
        let writing = self.input.queued.len() < before && !self.input.queued.is_empty();
        let acked = self.input.acknowledge(|bytes| {
            send(false, &serde_json::to_vec(&Msg::Ack { bytes })?)
        })?;
        Ok((sent > 0 || writing || acked, exit))
    }
}

fn retry(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

fn selection(id: Option<u64>, new: Option<bool>, take: Option<bool>, terminals: bool, takeover: bool) -> Option<Selection> {
    match (id, new, take) {
        (None, None, None) => Some(Selection::LatestOrNew),
        (Some(id), None, None) if terminals => Some(Selection::Id { id, take: false }),
        (Some(id), None, Some(take)) if terminals && takeover => Some(Selection::Id { id, take }),
        (None, Some(new), None) if terminals => Some(if new { Selection::New } else { Selection::LatestOrNew }),
        _ => None,
    }
}

pub struct Terminal {
    lease: Lease,
    notify: Notify,
    peer_has_terminals: bool,
    takeover: bool,
    attachment: Option<AttachmentId>,
    stream: Option<Stream>,
    reply: Option<Msg>,
    done: bool,
}

impl Terminal {
    pub fn new(lease: Lease, notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self { lease, notify: Arc::new(notify), peer_has_terminals: false, takeover: false, attachment: None, stream: None, reply: None, done: false }
    }

    pub(super) fn negotiate(&mut self, caps: &[String], offers_takeover: bool) {
        self.peer_has_terminals = caps.iter().any(|cap| cap == wire::caps::TERMINALS);
        self.takeover = offers_takeover && caps.iter().any(|cap| cap == wire::caps::TERMINAL_TAKEOVER);
    }

    fn finish(&mut self, reply: Option<Msg>) {
        self.stream = None;
        self.reply = reply;
        self.done = true;
    }

    fn detach(&self, why: &str) {
        if let Some(key) = &self.attachment { self.lease.detach(key, why); }
    }

    fn check_revocation(&mut self) -> bool {
        let Some(reason) = self.attachment.as_ref().and_then(AttachmentId::revoked) else { return false };
        // The receiver needs the terminal ID before it can report the loss.
        if !matches!(self.reply, Some(Msg::Opened { .. })) {
            self.attachment = None;
            let reply = if self.takeover && reason == Reason::Taken {
                Msg::Detached { reason: DetachReason::Taken }
            } else {
                Msg::Error { why: TerminalError::Io }
            };
            self.finish(Some(reply));
        }
        true
    }

    fn error(&mut self, why: TerminalError) {
        self.detach(why.as_str());
        if !self.check_revocation() {
            self.finish(Some(Msg::Error { why }));
        }
    }

    fn open(&mut self, rows: u16, cols: u16, target: Selection) {
        let result = self.lease.attach(target, rows, cols, self.notify.clone());
        match result {
            Ok(Some(registry::Attachment { key, replay, dropped })) => {
                self.reply = Some(Msg::Opened {
                    id: self.peer_has_terminals.then_some(key.terminal),
                    replay: replay.as_ref().map(|replay| replay.len),
                    dropped: dropped.then_some(true),
                });
                self.attachment = Some(key);
                self.stream = Some(Stream::new(rows, cols, replay));
            }
            Ok(None) => self.finish(Some(Msg::Error { why: TerminalError::Open })),
            Err(e) => {
                eprintln!("{}: {e}", t("terminal.open_failed"));
                self.error(TerminalError::Open);
            }
        }
    }

    pub(super) fn message(&mut self, active: bool, binary: bool, bytes: Vec<u8>) {
        if self.check_revocation() || self.done {
            return;
        }
        if bytes.len() > FRAME {
            self.error(TerminalError::Protocol);
            return;
        }
        if binary {
            if let Some(stream) = &mut self.stream {
                if !stream.input.receive(bytes) {
                    self.error(TerminalError::Protocol);
                }
            }
            return;
        }
        match serde_json::from_slice(&bytes) {
            Ok(Msg::Open { rows, cols, id, new, take }) if active && rows > 0 && cols > 0 && self.stream.is_none() => {
                match selection(id, new, take, self.peer_has_terminals, self.takeover) {
                    Some(target) => self.open(rows, cols, target),
                    None => self.error(TerminalError::Protocol),
                }
            }
            Ok(Msg::Resize { rows, cols }) if rows > 0 && cols > 0 => {
                if let (Some(stream), Some(key)) = (&mut self.stream, &self.attachment) {
                    if let Some(Err(e)) = self.lease.with_shell(key, |shell| {
                        let result = stream.resize(shell, rows, cols);
                        let closed = result.is_err().then_some(Closed::Io);
                        (result, closed)
                    }) {
                        eprintln!("{}: {e}", t("terminal.io"));
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
                self.detach(Reason::Left.as_str());
                if !self.check_revocation() {
                    self.finish(None);
                }
            }
            Ok(Msg::Terminate {}) if active => match self.attachment.as_ref().and_then(|key| self.lease.terminate(key)) {
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

    pub(super) fn pump(&mut self, port: &mut impl Port) -> Result<bool, PumpError> {
        self.check_revocation();
        if let Some(reply) = &self.reply {
            let bytes = serde_json::to_vec(reply).map_err(|e| PumpError::Terminal(e.into()))?;
            if !port.write(false, &bytes).map_err(PumpError::Transport)? {
                return Ok(false);
            }
            self.reply = None;
            self.check_revocation();
            if self.stream.is_none() {
                return Ok(true);
            }
        }
        let buffered = port.buffered();
        let (Some(stream), Some(key)) = (&mut self.stream, &self.attachment) else { return Ok(false) };
        let Some(result) = self.lease.with_shell(key, |shell| {
            let result = stream.pump(shell, buffered, |binary, data| {
                if binary && !self.lease.accessible() { return Ok(false); }
                Ok(port.write(binary, data).map_err(PortWrite)?)
            }).map_err(|e| match e.downcast::<PortWrite>() {
                Ok(PortWrite(e)) => PumpError::Transport(e),
                Err(e) => PumpError::Terminal(e),
            });
            let closed = match &result {
                Ok((_, exit)) => exit.map(Closed::Exit),
                Err(PumpError::Terminal(_)) => Some(Closed::Io),
                Err(PumpError::Transport(_)) => None,
            };
            (result, closed)
        }) else { return Ok(false) };
        let (progress, exit) = result?;
        if let Some(code) = exit {
            self.finish(Some(Msg::Exit { code }));
        }
        Ok(progress || exit.is_some())
    }

    pub(super) fn pump_failed(&mut self, failure: PumpError) {
        let error = match failure {
            PumpError::Terminal(error) | PumpError::Transport(error) => error,
        };
        eprintln!("{}: {error:#}", t("terminal.io"));
        self.error(TerminalError::Io);
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn finished(&self) -> bool {
        self.done && self.reply.is_none()
    }

    pub(super) fn discard_reply(&mut self) {
        self.reply = None;
    }

    pub(super) fn transport_closed(&mut self) {
        self.detach(Reason::Failed.as_str());
        self.attachment = None;
        self.stream = None;
        self.reply = None;
        self.done = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_selections_require_the_peer_capability_only_for_explicit_fields() {
        let returning = |take| Some(Selection::Id { id: 7, take });
        for (id, new, take, terminals, takeover, expected) in [
            (None, None, None, false, false, Some(Selection::LatestOrNew)),
            (Some(7), None, None, false, false, None),
            (Some(7), None, None, true, false, returning(false)),
            (None, Some(true), None, true, false, Some(Selection::New)),
            (None, Some(false), None, true, false, Some(Selection::LatestOrNew)),
            (Some(7), Some(true), None, true, false, None),
            (Some(7), Some(false), None, true, false, None),
            (Some(7), None, Some(true), true, false, None),
            (Some(7), None, Some(true), true, true, returning(true)),
            (Some(7), None, Some(false), true, true, returning(false)),
            (None, None, Some(true), true, true, None),
            (Some(7), Some(true), Some(true), true, true, None),
        ] {
            assert_eq!(selection(id, new, take, terminals, takeover), expected);
        }
    }

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
    fn output_uses_buffer_hysteresis_and_emulator_credit() {
        let mut flow = OutputCredit::default();
        for (buffered, expected) in [(0, FRAME), (HIGH - 1, 1), (HIGH, 0), (LOW, 0), (LOW - 1, FRAME)] {
            assert_eq!(flow.capacity(buffered), expected);
        }
        assert!(flow.window.take(OUTPUT_WINDOW, OUTPUT_WINDOW));
        assert_eq!(flow.capacity(0), 0);
        for invalid in [0, OUTPUT_WINDOW + 1] {
            assert!(!flow.acknowledge(invalid));
            assert_eq!(flow.window.outstanding(), OUTPUT_WINDOW);
        }
        assert!(flow.acknowledge(7));
        assert_eq!(flow.capacity(0), 7);
    }
}
