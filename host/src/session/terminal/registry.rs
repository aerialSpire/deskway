//! Owns the computer's one PTY. Only the desk seat's current session can attach to it.

use std::io;
use std::sync::{Arc, Condvar, Mutex, mpsc::Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wire::terminal::FRAME;
use wire::types::KeptTerminal;

use crate::i18n::{t, tf};
use crate::platform::Pty;
#[cfg(test)]
use crate::platform::WorkspaceState;
use crate::workspace::BrowserState;
use crate::session::SessionUpdate;
use super::{replay::History, retry, Replay, POLL};

const IDLE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Registry {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Registry {
    #[cfg(test)]
    pub fn new(open: impl Fn(u16, u16) -> io::Result<Box<dyn Pty>> + Send + Sync + 'static, updates: Sender<SessionUpdate>) -> Self {
        Self::guarded(Arc::new(move |_, rows, cols| open(rows, cols)), Arc::new(|| WorkspaceState::READY), updates)
    }

    pub fn guarded(open: Arc<Open>, access: Arc<BrowserState>, updates: Sender<SessionUpdate>) -> Self {
        let shared = Arc::new(Shared { open, access, updates, state: Mutex::default(), wake: Condvar::new() });
        let runner = shared.clone();
        let worker = thread::Builder::new().name("terminal".into()).spawn(move || runner.drain())
            .expect("spawn terminal thread");
        Self { shared, worker: Some(worker) }
    }

    #[cfg(test)]
    pub fn admit(&self, id: String, name: String, cred: String) -> Lease {
        self.admit_user(id, name, cred, WorkspaceState::READY.user.unwrap())
    }

    pub fn admit_user(&self, id: String, name: String, cred: String, user: u64) -> Lease {
        self.shared.state.lock().unwrap().seat = Some(id.clone());
        Lease { shared: self.shared.clone(), id, name, cred, user }
    }

    pub fn holds(&self, id: &str) -> bool {
        self.shared.state.lock().unwrap().seat.as_deref() == Some(id)
    }

    pub fn user_changed(&self) {
        self.shared.state.lock().unwrap().close(Closed::UserChanged);
        self.shared.changed();
    }

    pub fn end(&self, id: &str, why: &str) {
        self.shared.detach(id, why, true);
    }

    pub fn status(&self) -> Option<KeptTerminal> {
        self.shared.state.lock().unwrap().left().map(|left| left.report.clone())
    }

    pub fn stop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let mut state = self.shared.state.lock().unwrap();
            state.stop();
            self.shared.wake.notify_one();
            drop(state);
            worker.join().expect("join terminal thread");
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop();
    }
}

type Open = dyn Fn(u64, u16, u16) -> io::Result<Box<dyn Pty>> + Send + Sync;

struct Shared {
    open: Arc<Open>,
    access: Arc<BrowserState>,
    updates: Sender<SessionUpdate>,
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn changed(&self) {
        let _ = self.updates.send(SessionUpdate::TerminalChanged);
        self.wake.notify_one();
    }

    fn detach(&self, id: &str, why: &str, end: bool) {
        let mut state = self.state.lock().unwrap();
        if state.detach(id, why, Instant::now(), SystemTime::now(), end) {
            self.changed();
        }
    }

    // Independent of signaling and of every WebRTC connection. There is nothing to poll
    // when the PTY is attached (its session drives the credited reads) or absent.
    fn drain(&self) {
        let mut state = self.state.lock().unwrap();
        let mut buf = [0; FRAME];
        while !state.stopping {
            if state.left().is_none() {
                state = self.wake.wait(state).unwrap();
                continue;
            }
            let progress = state.tick(Instant::now(), &mut buf);
            if state.left().is_none() {
                self.changed();
            }
            if progress {
                drop(state);
                thread::yield_now();
                state = self.state.lock().unwrap();
            } else {
                state = self.wake.wait_timeout(state, POLL).unwrap().0;
            }
        }
    }
}

#[derive(Clone)]
pub struct Lease {
    shared: Arc<Shared>,
    id: String,
    name: String,
    cred: String,
    user: u64,
}

impl Lease {
    pub(in crate::session) fn id(&self) -> &str {
        &self.id
    }

    pub(in crate::session) fn name(&self) -> &str {
        &self.name
    }

    pub(in crate::session) fn holds_seat(&self) -> bool {
        self.while_seated(|| ()).is_some()
    }

    pub(in crate::session) fn while_seated<T>(&self, run: impl FnOnce() -> T) -> Option<T> {
        let state = self.shared.state.lock().unwrap();
        (!state.stopping && state.seat.as_deref() == Some(self.id.as_str()) && self.accessible()).then(run)
    }

    fn accessible(&self) -> bool {
        let state = (self.shared.access)();
        state.user == Some(self.user) && state.refusal(false).is_none()
    }

    pub(super) fn attach(&self, rows: u16, cols: u16) -> io::Result<Option<(Option<Replay>, bool)>> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.accessible() {
            return Ok(None);
        }
        let was_left = state.left().is_some();
        let info = state.attach(&self.id, &self.name, &self.cred, Instant::now(), || (self.shared.open)(self.user, rows, cols))
            .map(|attached| attached.map(|(shell, returned)| (returned.then(|| Replay::new(&shell.replay)), shell.replay.dropped)));
        if was_left && state.left().is_none() {
            self.shared.changed();
        }
        info
    }

    pub(super) fn with_shell<T>(&self, f: impl FnOnce(&mut Shell) -> T) -> Option<T> {
        let mut state = self.shared.state.lock().unwrap();
        self.accessible().then(|| state.attached(&self.id).map(f)).flatten()
    }

    pub fn detach(&self, why: &str) {
        self.shared.detach(&self.id, why, false);
    }

    pub(super) fn end(&self, why: &str) {
        self.shared.detach(&self.id, why, true);
    }

    pub fn close(&self, why: Closed) -> bool {
        self.shared.state.lock().unwrap().close_attached(&self.id, why)
    }

    pub(super) fn terminate(&self) -> Option<io::Result<bool>> {
        let mut state = self.shared.state.lock().unwrap();
        self.accessible().then(|| state.terminate(&self.id)).flatten()
    }
}

#[derive(Default)]
struct State {
    seat: Option<String>,
    terminal: Option<Kept>,
    stopping: bool,
}

struct Kept {
    shell: Shell,
    place: Place,
}

enum Place {
    Attached { name: String, cred: String },
    Left(Left),
}

struct Left {
    at: Instant,
    report: KeptTerminal,
}

#[derive(Debug)]
pub enum Closed {
    Exit(i32),
    Person,
    Idle,
    Stop,
    Io,
    UserChanged,
}

impl State {
    fn left(&self) -> Option<&Left> {
        match &self.terminal.as_ref()?.place {
            Place::Left(left) => Some(left),
            Place::Attached { .. } => None,
        }
    }

    fn attach(&mut self, id: &str, name: &str, cred: &str, now: Instant,
        open: impl FnOnce() -> io::Result<Box<dyn Pty>>,
    ) -> io::Result<Option<(&mut Shell, bool)>> {
        if self.stopping || self.seat.as_deref() != Some(id) || self.attached(id).is_some() {
            return Ok(None);
        }
        self.reap_left(now);
        let returned = self.terminal.is_some();
        let place = Place::Attached { name: name.into(), cred: cred.into() };
        let event = if let Some(kept) = &mut self.terminal {
            kept.place = place;
            "terminal.returned"
        } else {
            self.terminal = Some(Kept { shell: Shell::new(open()?), place });
            "terminal.opened"
        };
        eprintln!("{}", tf(event, &[("name", name.into())]));
        Ok(self.attached(id).map(|shell| (shell, returned)))
    }

    fn attached(&mut self, id: &str) -> Option<&mut Shell> {
        let kept = self.terminal.as_mut()?;
        (self.seat.as_deref() == Some(id) && matches!(kept.place, Place::Attached { .. })).then_some(&mut kept.shell)
    }

    fn detach(&mut self, id: &str, why: &str, now: Instant, wall: SystemTime, end: bool) -> bool {
        if self.seat.as_deref() != Some(id) {
            return false;
        }
        if end {
            self.seat = None;
        }
        let Some(kept) = &mut self.terminal else { return false };
        let Place::Attached { name, cred } = &kept.place else { return false };
        eprintln!("{}", tf("terminal.left", &[("name", name.clone()), ("reason", why.into())]));
        kept.place = Place::Left(Left { at: now, report: KeptTerminal {
            since: wall.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(), name: name.clone(), cred: Some(cred.clone()).filter(|cred| !cred.is_empty()),
        } });
        self.reap_left(now);
        true
    }

    fn reap_left(&mut self, now: Instant) {
        let Some(left) = self.left() else { return };
        if now >= left.at + IDLE {
            self.close(Closed::Idle);
        } else {
            match self.terminal.as_mut().unwrap().shell.pty.exit() {
                Ok(Some(code)) => self.close(Closed::Exit(code)),
                Err(_) => self.close(Closed::Io),
                Ok(None) => {}
            }
        }
    }

    fn tick(&mut self, now: Instant, buf: &mut [u8]) -> bool {
        self.reap_left(now);
        let Some(kept) = &mut self.terminal else { return false };
        match kept.shell.read(buf) {
            Ok(n) => n > 0,
            Err(_) => { self.close(Closed::Io); false }
        }
    }

    fn close_attached(&mut self, id: &str, why: Closed) -> bool {
        if self.attached(id).is_none() {
            return false;
        }
        self.close(why);
        true
    }

    fn terminate(&mut self, id: &str) -> Option<io::Result<bool>> {
        let result = self.attached(id)?.pty.terminate();
        if matches!(result, Ok(true)) {
            self.close(Closed::Person);
        }
        Some(result)
    }

    fn stop(&mut self) {
        self.stopping = true;
        self.close(Closed::Stop);
    }

    fn close(&mut self, why: Closed) {
        if self.terminal.take().is_none() {
            return;
        }
        let reason = match why {
            Closed::Exit(code) => tf("terminal.exit", &[("code", code.to_string())]),
            Closed::Person => t("terminal.person"),
            Closed::Idle => t("terminal.idle"),
            Closed::Stop => t("terminal.stop"),
            Closed::Io => t("terminal.io"),
            Closed::UserChanged => t("terminal.user_changed"),
        };
        eprintln!("{}", tf("terminal.closed", &[("reason", reason)]));
    }
}

pub(super) struct Shell {
    pub(super) pty: Box<dyn Pty>,
    pub(super) replay: History,
    pub(super) eof: bool,
}

impl Shell {
    fn new(pty: Box<dyn Pty>) -> Self {
        Self { pty, replay: History::default(), eof: false }
    }

    pub(super) fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.eof {
            return Ok(0);
        }
        match self.pty.read(buf) {
            Ok(n) => {
                self.eof = n == 0;
                self.replay.append(&buf[..n]);
                Ok(n)
            }
            Err(e) if retry(&e) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::Stream;
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use wire::terminal::OUTPUT_WINDOW;

    #[derive(Default)]
    struct Process {
        output: VecDeque<u8>,
        input: Vec<u8>,
        redraw: Vec<(u16, u16)>,
        redraw_error: bool,
        terminate_error: bool,
        exit: Option<i32>,
        drops: usize,
    }

    struct FakePty(Arc<Mutex<Process>>);

    impl Read for FakePty {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut process = self.0.lock().unwrap();
            let len = buf.len().min(process.output.len());
            for (dst, src) in buf.iter_mut().zip(process.output.drain(..len)) {
                *dst = src;
            }
            if len == 0 && process.exit.is_none() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Ok(len)
        }
    }

    impl Write for FakePty {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().input.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    impl Pty for FakePty {
        fn resize(&mut self, _: u16, _: u16) -> io::Result<()> { Ok(()) }
        fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> {
            let mut process = self.0.lock().unwrap();
            process.redraw.push((rows, cols));
            if process.redraw_error {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        }
        fn exit(&mut self) -> io::Result<Option<i32>> { Ok(self.0.lock().unwrap().exit) }
        fn terminate(&mut self) -> io::Result<bool> {
            let process = self.0.lock().unwrap();
            if process.terminate_error {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(process.exit.is_none())
        }
    }

    impl Drop for FakePty {
        fn drop(&mut self) { self.0.lock().unwrap().drops += 1; }
    }

    fn opened(now: Instant) -> (State, Arc<Mutex<Process>>) {
        let process = Arc::new(Mutex::new(Process::default()));
        let mut state = State { seat: Some("one".into()), ..State::default() };
        let (_, returned) = state.attach("one", "Phone", "cred1", now, || Ok(Box::new(FakePty(process.clone()))))
            .unwrap().unwrap();
        assert!(!returned);
        (state, process)
    }

    #[test]
    fn publishing_holds_the_same_guard_as_revocation() {
        let (updates, _) = std::sync::mpsc::channel();
        let registry = Registry::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("first".into(), "browser".into(), "device".into());
        assert_eq!(lease.while_seated(|| registry.shared.state.try_lock().is_err()), Some(true));
        registry.end("first", "taken");
        registry.admit("next".into(), "browser".into(), "device".into());
        assert_eq!(lease.while_seated(|| panic!("lost the seat")), None::<()>);
    }

    #[test]
    fn locking_revokes_the_attachment_and_user_change_closes_the_kept_shell() {
        let (updates, _) = std::sync::mpsc::channel();
        for next in [WorkspaceState::NO_USER, WorkspaceState { user: Some(2), locked: false }] {
            let state = Arc::new(Mutex::new(WorkspaceState::READY));
            let process = Arc::new(Mutex::new(Process::default()));
            let open = process.clone();
            let native = state.clone();
            let registry = Registry::guarded(
                Arc::new(move |_, _, _| Ok(Box::new(FakePty(open.clone())))),
                Arc::new(move || *native.lock().unwrap()), updates.clone(),
            );
            let lease = registry.admit("one".into(), "Phone".into(), "cred".into());
            assert!(lease.attach(24, 80).unwrap().is_some());
            state.lock().unwrap().locked = true;
            assert!(lease.with_shell(|_| ()).is_none());
            registry.end("one", "locked");
            assert_eq!(registry.status().unwrap().name, "Phone");
            assert_eq!(process.lock().unwrap().drops, 0);
            *state.lock().unwrap() = next;
            registry.user_changed();
            assert!(registry.status().is_none());
            assert_eq!(process.lock().unwrap().drops, 1);
        }
    }

    #[test]
    fn lifetime_ends_only_on_exit_explicit_close_idle_or_stop() {
        for (why, detached) in [
            (Closed::Exit(17), false), (Closed::Exit(17), true), (Closed::Person, false),
            (Closed::Idle, true), (Closed::Stop, false), (Closed::Stop, true),
        ] {
            let now = Instant::now();
            let wall = UNIX_EPOCH + Duration::from_secs(123);
            let (mut state, process) = opened(now);
            assert!(state.left().is_none());
            if detached {
                assert!(state.detach("one", "left", now, wall, true));
                assert_eq!(state.left().unwrap().report, KeptTerminal { since: 123, name: "Phone".into(), cred: Some("cred1".into()) });
                assert!(!state.detach("one", "failed", now + Duration::from_secs(1), wall, true));
                assert!(state.terminate("one").is_none());
            }
            match why {
                Closed::Exit(code) if detached => {
                    process.lock().unwrap().exit = Some(code);
                    state.tick(now, &mut [0; FRAME]);
                }
                Closed::Exit(code) => {
                    process.lock().unwrap().exit = Some(code);
                    let mut stream = Stream::new(24, 80, None);
                    let (_, exit) = stream.pump(state.attached("one").unwrap(), 0, |_, _| Ok(true)).unwrap();
                    assert_eq!(exit, Some(code));
                    assert!(state.close_attached("one", Closed::Exit(code)));
                }
                Closed::Person => assert!(state.terminate("one").unwrap().unwrap()),
                Closed::Idle => {
                    state.tick(now + IDLE - Duration::from_secs(1), &mut [0; FRAME]);
                    assert!(state.left().is_some());
                    assert_eq!(process.lock().unwrap().drops, 0);
                    state.tick(now + IDLE, &mut [0; FRAME]);
                }
                Closed::Stop => state.stop(),
                Closed::Io | Closed::UserChanged => unreachable!(),
            }
            assert!(state.terminal.is_none(), "{why:?}, detached={detached}");
            assert_eq!(process.lock().unwrap().drops, 1, "{why:?}, detached={detached}");
        }
    }

    #[test]
    fn close_failure_and_prior_exit_keep_the_attachment() {
        for exited in [false, true] {
            let (mut state, process) = opened(Instant::now());
            {
                let mut process = process.lock().unwrap();
                process.exit = exited.then_some(17);
                process.terminate_error = !exited;
            }
            let result = state.terminate("one").unwrap();
            if exited {
                assert!(!result.unwrap());
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            }
            assert!(state.attached("one").is_some());
            assert_eq!(process.lock().unwrap().drops, 0);
            if !exited {
                process.lock().unwrap().terminate_error = false;
                assert!(state.terminate("one").unwrap().unwrap());
                assert_eq!(process.lock().unwrap().drops, 1);
            }
        }
    }

    #[test]
    fn a_new_attachment_has_fresh_credit_and_replays_before_live_io() {
        let now = Instant::now();
        let (mut state, process) = opened(now);
        let first = &mut Stream::new(24, 80, None);
        assert!(first.credit.window.take(OUTPUT_WINDOW, OUTPUT_WINDOW));
        assert!(first.input.receive(b"old input".to_vec()));
        assert!(state.detach("one", "taken", now, UNIX_EPOCH, true));
        assert!(state.attached("one").is_none());
        let detached = b'x';
        let live = b'y';
        let mode = b"\x1b[?2004h";
        {
            let mut process = process.lock().unwrap();
            process.output.extend(mode);
            process.output.extend(std::iter::repeat_n(detached, 2 * super::super::replay::LIMIT));
            process.redraw_error = true;
        }
        while state.tick(now, &mut [0; FRAME]) {}
        assert!(process.lock().unwrap().output.is_empty());

        state.seat = Some("two".into());
        let (shell, returned) = state.attach("two", "Laptop", "cred2", now, || panic!("second shell"))
            .unwrap().unwrap();
        assert!(returned);
        assert!(shell.replay.dropped);
        let mut stream = Stream::new(25, 90, Some(Replay::new(&shell.replay)));
        let mut sent = Vec::new();
        let mut pump = |stream: &mut Stream, shell: &mut Shell| {
            stream.pump(shell, 0, |binary, bytes| {
                if binary { sent.extend_from_slice(bytes); }
                Ok(true)
            }).unwrap()
        };
        process.lock().unwrap().output.push_back(live);
        assert!(stream.input.receive(b"new input".to_vec()));
        while pump(&mut stream, shell).0 {}
        assert_eq!(stream.credit.window.outstanding(), OUTPUT_WINDOW);
        assert!(process.lock().unwrap().redraw.is_empty());
        assert!(process.lock().unwrap().input.is_empty());
        assert_eq!(process.lock().unwrap().output.front(), Some(&live));

        while stream.replay.is_some() {
            let outstanding = stream.credit.window.outstanding();
            assert!(stream.credit.acknowledge(outstanding));
            while pump(&mut stream, shell).0 {}
        }
        assert!(sent.starts_with(mode));
        assert_eq!(sent.len(), mode.len() + shell.replay.bytes.len() + 1);
        assert_eq!(sent.last(), Some(&live));
        assert_eq!(process.lock().unwrap().redraw, [(25, 90)]);
        assert_eq!(process.lock().unwrap().input, b"new input");
        assert!(state.left().is_none());
        assert!(state.terminate("one").is_none());
    }
}
