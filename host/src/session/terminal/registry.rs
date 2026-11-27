//! Owns the computer's one PTY. Only the desk seat's current session can attach to it.

use std::io;
use std::sync::{mpsc::{self, Receiver, Sender, SyncSender, TryRecvError}, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wake::Wake;

use wire::types::KeptTerminal;

use crate::i18n::{t, tf};
use crate::platform::Pty;
use crate::platform::pty::{PtyEvent, PtyEvents, OUTPUT_FRAMES};
#[cfg(test)]
use crate::platform::WorkspaceState;
use crate::workspace::BrowserState;
use crate::session::{SessionEvent, SessionUpdate};
use super::{replay::History, Replay};

const KEEP_FOR: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Registry {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Registry {
    #[cfg(test)]
    pub fn new(open: impl Fn(u16, u16) -> io::Result<Box<dyn Pty>> + Send + Sync + 'static, updates: Wake<SessionUpdate>) -> Self {
        Self::guarded(Arc::new(move |_, rows, cols| open(rows, cols)), Arc::new(|| WorkspaceState::READY), updates)
    }

    pub fn guarded(open: Arc<Open>, access: Arc<BrowserState>, updates: Wake<SessionUpdate>) -> Self {
        let (wake, ready) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared { open, access, updates, state: Mutex::default(), wake });
        let runner = shared.clone();
        let worker = thread::Builder::new().name("terminal".into()).spawn(move || runner.drain(ready))
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
            self.shared.notify();
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
    updates: Wake<SessionUpdate>,
    state: Mutex<State>,
    wake: SyncSender<()>,
}

impl Shared {
    fn changed(&self) {
        let _ = self.updates.send(SessionUpdate::TerminalChanged);
        self.notify();
    }

    fn detach(&self, id: &str, why: &str, end: bool) {
        let mut state = self.state.lock().unwrap();
        if state.detach(id, why, Instant::now(), SystemTime::now(), end) {
            self.changed();
        }
    }

    fn notify(&self) {
        let _ = self.wake.try_send(());
    }

    // Detached output drains independently of signaling. An attached terminal's session
    // applies browser credit before consuming output.
    fn drain(&self, ready: Receiver<()>) {
        loop {
            let deadline = {
                let mut state = self.state.lock().unwrap();
                if state.stopping { break; }
                if state.left().is_some() {
                    let more = state.drain_left(Instant::now());
                    if state.left().is_none() { self.changed(); }
                    if more {
                        drop(state);
                        thread::yield_now();
                        continue;
                    }
                }
                if let Some(Kept { place: Place::Attached { notify, .. }, .. }) = &state.terminal {
                    let _ = notify.send(SessionEvent::TerminalReady);
                }
                state.left().map(|left| left.at + KEEP_FOR)
            };
            if let Some(deadline) = deadline {
                let _ = ready.recv_timeout(deadline.saturating_duration_since(Instant::now()));
            } else if ready.recv().is_err() {
                break;
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

    pub(super) fn accessible(&self) -> bool {
        let state = (self.shared.access)();
        state.user == Some(self.user) && state.refusal(false).is_none()
    }

    pub(super) fn attach(&self, rows: u16, cols: u16, notify: Sender<SessionEvent>) -> io::Result<Option<(Option<Replay>, bool)>> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.accessible() {
            return Ok(None);
        }
        let was_left = state.left().is_some();
        let info = state.attach(&self.id, &self.name, &self.cred, Instant::now(), notify, || {
            Shell::new((self.shared.open)(self.user, rows, cols)?, self.shared.wake.clone())
        }).map(|attached| attached.map(|(shell, returned)| {
            (returned.then(|| Replay::new(&shell.replay)), shell.replay.dropped)
        }));
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
    Attached { name: String, cred: String, notify: Sender<SessionEvent> },
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

    fn attach(&mut self, id: &str, name: &str, cred: &str, now: Instant, notify: Sender<SessionEvent>,
        open: impl FnOnce() -> io::Result<Shell>,
    ) -> io::Result<Option<(&mut Shell, bool)>> {
        if self.stopping || self.seat.as_deref() != Some(id) || self.attached(id).is_some() {
            return Ok(None);
        }
        for _ in 0..=OUTPUT_FRAMES {
            if !self.drain_left(now) { break; }
        }
        let returned = self.terminal.is_some();
        let place = Place::Attached { name: name.into(), cred: cred.into(), notify };
        let event = if let Some(kept) = &mut self.terminal {
            kept.place = place;
            "terminal.returned"
        } else {
            self.terminal = Some(Kept { shell: open()?, place });
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
        let Place::Attached { name, cred, .. } = &kept.place else { return false };
        eprintln!("{}", tf("terminal.left", &[("name", name.clone()), ("reason", why.into())]));
        kept.place = Place::Left(Left { at: now, report: KeptTerminal {
            since: wall.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(), name: name.clone(), cred: Some(cred.clone()).filter(|cred| !cred.is_empty()),
        } });
        true
    }

    fn drain_left(&mut self, now: Instant) -> bool {
        let Some(left) = self.left() else { return false; };
        if now >= left.at + KEEP_FOR {
            self.close(Closed::Idle);
            return false;
        }
        match self.terminal.as_mut().unwrap().shell.output(usize::MAX, |_| Ok(true)) {
            Ok((bytes, None)) => bytes > 0,
            Ok((_, Some(code))) => { self.close(Closed::Exit(code)); false }
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
    events: Receiver<PtyEvent>,
    pending: Option<PtyEvent>,
    offset: usize,
    pub(super) pty: Box<dyn Pty>,
    pub(super) replay: History,
}

impl Shell {
    fn new(mut pty: Box<dyn Pty>, wake: SyncSender<()>) -> io::Result<Self> {
        let (sender, events) = mpsc::sync_channel(OUTPUT_FRAMES);
        let writable = wake.clone();
        pty.listen(PtyEvents {
            output: Box::new(move |event| {
                if sender.send(event).is_err() { return false; }
                let _ = wake.try_send(());
                true
            }),
            writable: Arc::new(move || { let _ = writable.try_send(()); }),
        })?;
        Ok(Self { events, pending: None, offset: 0, pty, replay: History::default() })
    }

    pub(super) fn output(&mut self, capacity: usize, send: impl FnOnce(&[u8]) -> anyhow::Result<bool>) -> anyhow::Result<(usize, Option<i32>)> {
        if self.pending.is_none() {
            self.pending = match self.events.try_recv() {
                Ok(event) => Some(event),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return Err(io::Error::from(io::ErrorKind::BrokenPipe).into()),
            };
        }
        match self.pending.as_ref() {
            Some(PtyEvent::Bytes(data)) if capacity > 0 => {
                let end = self.offset + capacity.min(data.len() - self.offset);
                let bytes = &data[self.offset..end];
                if !send(bytes)? { return Ok((0, None)); }
                self.replay.append(bytes);
                let sent = bytes.len();
                self.offset = end;
                if end == data.len() {
                    self.pending = None;
                    self.offset = 0;
                    self.pty.consumed();
                }
                Ok((sent, None))
            }
            Some(PtyEvent::Exit(_)) => {
                let Some(PtyEvent::Exit(code)) = self.pending.take() else { unreachable!() };
                Ok((0, Some(code)))
            }
            Some(PtyEvent::Error(_)) => {
                let Some(PtyEvent::Error(error)) = self.pending.take() else { unreachable!() };
                Err(error.into())
            }
            _ => Ok((0, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{Stream, Terminal};
    use crate::wake::Waker;
    use std::io::Write;
    use wire::terminal::{FRAME, OUTPUT_WINDOW};

    #[derive(Default)]
    struct Process {
        events: Option<PtyEvents>,
        input: Vec<u8>,
        redraw: Vec<(u16, u16)>,
        redraw_error: bool,
        terminate_error: bool,
        exit: Option<i32>,
        drops: usize,
    }

    struct FakePty(Arc<Mutex<Process>>);

    impl Process {
        fn emit(&mut self, event: PtyEvent) {
            assert!((self.events.as_mut().unwrap().output)(event));
        }

        fn exit(&mut self, code: i32) {
            self.exit = Some(code);
            self.emit(PtyEvent::Exit(code));
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
        fn listen(&mut self, events: PtyEvents) -> io::Result<()> {
            self.0.lock().unwrap().events = Some(events);
            Ok(())
        }
        fn resize(&mut self, _: u16, _: u16) -> io::Result<()> { Ok(()) }
        fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> {
            let mut process = self.0.lock().unwrap();
            process.redraw.push((rows, cols));
            if process.redraw_error {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            Ok(())
        }
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
        let (_, returned) = state.attach("one", "Phone", "cred1", now, mpsc::channel().0, || Shell::new(Box::new(FakePty(process.clone())), mpsc::sync_channel(1).0))
            .unwrap().unwrap();
        assert!(!returned);
        (state, process)
    }

    #[test]
    fn publishing_holds_the_same_guard_as_revocation() {
        let (updates, _) = Waker::default().channel();
        let registry = Registry::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("first".into(), "browser".into(), "device".into());
        assert_eq!(lease.while_seated(|| registry.shared.state.try_lock().is_err()), Some(true));
        registry.end("first", "taken");
        registry.admit("next".into(), "browser".into(), "device".into());
        assert_eq!(lease.while_seated(|| panic!("lost the seat")), None::<()>);
    }

    #[test]
    fn locking_revokes_the_attachment_and_user_change_closes_the_kept_shell() {
        let (updates, _) = Waker::default().channel();
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
            assert!(lease.attach(24, 80, mpsc::channel().0).unwrap().is_some());
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
    fn revocation_before_queueing_live_output_or_replay_keeps_the_bytes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (updates, _) = Waker::default().channel();
        let process = Arc::new(Mutex::new(Process::default()));
        let revoke = Arc::new(AtomicBool::new(false));
        let locked = Arc::new(AtomicBool::new(false));
        let access = {
            let (revoke, locked) = (revoke.clone(), locked.clone());
            Arc::new(move || {
                let state = WorkspaceState { locked: locked.load(Ordering::Relaxed), ..WorkspaceState::READY };
                if revoke.swap(false, Ordering::Relaxed) {
                    locked.store(true, Ordering::Relaxed);
                }
                state
            })
        };
        let open = process.clone();
        let registry = Registry::guarded(Arc::new(move |_, _, _| Ok(Box::new(FakePty(open.clone())))), access, updates);
        for (id, withhold) in [("live", true), ("replay", true), ("return", false)] {
            locked.store(false, Ordering::Relaxed);
            let mut terminal = Terminal::new(registry.admit(id.into(), "browser".into(), "device".into()), mpsc::channel().0);
            terminal.open(24, 80);
            if id == "live" {
                process.lock().unwrap().emit(PtyEvent::Bytes(b"kept output".to_vec()));
            }
            if withhold {
                // The attachment check succeeds, then access is revoked before the frame is queued.
                revoke.store(true, Ordering::Relaxed);
                terminal.pump_stream(0, |_, _| panic!("queued output after revocation")).unwrap();
                assert!(locked.load(Ordering::Relaxed));
            } else {
                let mut sent = Vec::new();
                terminal.pump_stream(0, |binary, data| {
                    assert!(binary);
                    sent.extend_from_slice(data);
                    Ok(true)
                }).unwrap();
                assert_eq!(sent, b"kept output");
            }
            registry.end(id, "locked");
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
                    process.lock().unwrap().exit(code);
                    state.drain_left(now);
                }
                Closed::Exit(code) => {
                    process.lock().unwrap().exit(code);
                    let mut stream = Stream::new(24, 80, None);
                    let (_, exit) = stream.pump(state.attached("one").unwrap(), 0, |_, _| Ok(true)).unwrap();
                    assert_eq!(exit, Some(code));
                    assert!(state.close_attached("one", Closed::Exit(code)));
                }
                Closed::Person => assert!(state.terminate("one").unwrap().unwrap()),
                Closed::Idle => {
                    state.drain_left(now + KEEP_FOR - Duration::from_secs(1));
                    assert!(state.left().is_some());
                    assert_eq!(process.lock().unwrap().drops, 0);
                    state.drain_left(now + KEEP_FOR);
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
    fn a_slow_consumer_bounds_output_and_receives_one_exit_after_the_bytes() {
        let (mut state, process) = opened(Instant::now());
        let mut events = process.lock().unwrap().events.take().unwrap();
        let (attempt, attempts) = mpsc::channel();
        let (queued, done) = mpsc::channel();
        let reader = thread::spawn(move || {
            for sequence in 0..=OUTPUT_FRAMES {
                attempt.send(sequence).unwrap();
                assert!((events.output)(PtyEvent::Bytes(vec![sequence as u8; FRAME])));
                queued.send(sequence).unwrap();
            }
            assert!((events.output)(PtyEvent::Exit(17)));
        });
        let timeout = Duration::from_secs(2);
        for sequence in 0..OUTPUT_FRAMES {
            assert_eq!(attempts.recv_timeout(timeout).unwrap(), sequence);
            assert_eq!(done.recv_timeout(timeout).unwrap(), sequence);
        }
        assert_eq!(attempts.recv_timeout(timeout).unwrap(), OUTPUT_FRAMES);
        assert_eq!(done.recv_timeout(Duration::from_millis(30)), Err(mpsc::RecvTimeoutError::Timeout));

        let mut stream = Stream::new(24, 80, None);
        let mut sent = Vec::new();
        let deadline = Instant::now() + timeout;
        loop {
            let (progress, exit) = stream.pump(state.attached("one").unwrap(), 0, |binary, data| {
                assert!(binary);
                sent.extend_from_slice(data);
                Ok(true)
            }).unwrap();
            if let Some(code) = exit {
                assert_eq!(code, 17);
                assert!(state.close_attached("one", Closed::Exit(code)));
                assert!(!state.close_attached("one", Closed::Exit(code)));
                break;
            }
            if progress {
                let outstanding = stream.credit.window.outstanding();
                assert!(stream.credit.acknowledge(outstanding));
            } else {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
        }
        reader.join().unwrap();
        assert_eq!(sent, (0..=OUTPUT_FRAMES).flat_map(|sequence| vec![sequence as u8; FRAME]).collect::<Vec<_>>());
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
        process.lock().unwrap().emit(PtyEvent::Bytes(mode.to_vec()));
        assert!(state.drain_left(now));
        for _ in 0..2 * super::super::replay::LIMIT / FRAME {
            process.lock().unwrap().emit(PtyEvent::Bytes(vec![detached; FRAME]));
            assert!(state.drain_left(now));
        }
        process.lock().unwrap().redraw_error = true;

        state.seat = Some("two".into());
        let (shell, returned) = state.attach("two", "Laptop", "cred2", now, mpsc::channel().0, || panic!("second shell"))
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
        process.lock().unwrap().emit(PtyEvent::Bytes(vec![live]));
        assert!(stream.input.receive(b"new input".to_vec()));
        while pump(&mut stream, shell).0 {}
        assert_eq!(stream.credit.window.outstanding(), OUTPUT_WINDOW);
        assert!(process.lock().unwrap().redraw.is_empty());
        assert!(process.lock().unwrap().input.is_empty());

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
