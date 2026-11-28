//! Owns the computer's browser PTYs. The desk seat's session can attach to one at a time.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::{self, Receiver, Sender, SyncSender, TryRecvError}, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wake::Wake;

use wire::types::{Reason, TerminalStatus};

use crate::i18n::{t, tf};
use crate::platform::Pty;
use crate::platform::pty::{PtyEvent, PtyEvents, OUTPUT_FRAMES};
#[cfg(test)]
use crate::platform::WorkspaceState;
use crate::workspace::BrowserState;
use crate::session::{SessionEvent, SessionUpdate};
use super::{replay::History, Replay};

#[cfg(test)]
pub(in crate::session) use tests::fake_pty;

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
        let mut state = self.shared.state.lock().unwrap();
        if let Some(old) = state.seat.clone().filter(|old| old != &id) {
            if state.detach(&old, wire::types::Reason::Taken.as_str(), Instant::now(), SystemTime::now(), true) {
                self.shared.changed();
            }
        }
        state.seat = Some(id.clone());
        Lease { shared: self.shared.clone(), id, name, cred, user }
    }

    pub fn holds(&self, id: &str) -> bool {
        self.shared.state.lock().unwrap().seat.as_deref() == Some(id)
    }

    pub fn user_changed(&self) {
        self.shared.state.lock().unwrap().close_all(Closed::UserChanged);
        self.shared.changed();
    }

    pub fn end(&self, id: &str, why: &str) {
        self.shared.detach(id, why, true);
    }

    pub fn status(&self) -> Vec<TerminalStatus> {
        self.shared.state.lock().unwrap().status()
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
                let count = state.terminals.len();
                let (more, deadline) = state.drain_left(Instant::now());
                if count != state.terminals.len() { self.changed(); }
                if let Some(Kept { shell, place: Place::Attached(notify), .. }) = state.attached.and_then(|id| state.terminals.get(&id)) {
                    if shell.ready.swap(false, Ordering::Relaxed) {
                        let _ = notify.send(SessionEvent::TerminalReady);
                    }
                }
                if more {
                    drop(state);
                    thread::yield_now();
                    continue;
                }
                deadline
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

    pub(super) fn attach(&self, target: Selection, rows: u16, cols: u16, notify: Sender<SessionEvent>) -> io::Result<Option<Attachment>> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.accessible() {
            return Ok(None);
        }
        let before = (state.attached, state.terminals.len());
        let info = state.attach((&self.id, &self.name, &self.cred), target, (Instant::now(), SystemTime::now()), notify, || {
            Shell::new((self.shared.open)(self.user, rows, cols)?, self.shared.wake.clone())
        }).map(|attached| attached.map(|(id, shell, returned)| Attachment {
            id, replay: returned.then(|| Replay::new(&shell.replay)), dropped: shell.replay.dropped,
        }));
        if matches!(info, Ok(Some(_))) || before != (state.attached, state.terminals.len()) {
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

    pub(in crate::session) fn end(&self, why: &str) {
        self.shared.detach(&self.id, why, true);
    }

    pub fn close(&self, why: Closed) -> bool {
        let closed = self.shared.state.lock().unwrap().close_attached(&self.id, why);
        if closed { self.shared.changed(); }
        closed
    }

    pub(super) fn terminate(&self) -> Option<io::Result<bool>> {
        let mut state = self.shared.state.lock().unwrap();
        let result = self.accessible().then(|| state.terminate(&self.id)).flatten();
        if matches!(result, Some(Ok(true))) { self.shared.changed(); }
        result
    }
}

#[derive(Default)]
struct State {
    seat: Option<String>,
    terminals: BTreeMap<u64, Kept>,
    attached: Option<u64>,
    last_id: u64,
    stopping: bool,
}

pub(super) struct Attachment {
    pub(super) id: u64,
    pub(super) replay: Option<Replay>,
    pub(super) dropped: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Selection {
    LatestOrNew,
    Id(u64),
    New,
}

struct Kept {
    shell: Shell,
    place: Place,
    since: u64,
    name: String,
    cred: Option<String>,
}

enum Place {
    Attached(Sender<SessionEvent>),
    Left(Left),
}

struct Left {
    at: Instant,
    wall: u64,
}

#[derive(Clone, Copy, Debug)]
pub enum Closed {
    Exit(i32),
    Person,
    Idle,
    Stop,
    Io,
    UserChanged,
}

impl State {
    // The newest departure comes first so signaling can also report the legacy terminal.
    fn status(&self) -> Vec<TerminalStatus> {
        let mut terminals: Vec<_> = self.terminals.iter().collect();
        terminals.sort_by_key(|(&id, kept)| Reverse(kept.left().map(|left| (left.at, id))));
        terminals.into_iter().map(|(&id, kept)| TerminalStatus {
            id, since: kept.since, attached: kept.left().is_none(),
            name: kept.name.clone(), cred: kept.cred.clone(), left: kept.left().map(|left| left.wall),
        }).collect()
    }

    fn attach(
        &mut self, device: (&str, &str, &str), selection: Selection, (now, wall): (Instant, SystemTime),
        notify: Sender<SessionEvent>, open: impl FnOnce() -> io::Result<Shell>,
    ) -> io::Result<Option<(u64, &mut Shell, bool)>> {
        let (session, name, cred) = device;
        if self.stopping || self.seat.as_deref() != Some(session) {
            return Ok(None);
        }
        let target = match selection {
            Selection::Id(id) if self.attached == Some(id) || !self.prepare_return(id, now) => return Ok(None),
            Selection::Id(id) => Some(id),
            Selection::New => None,
            Selection::LatestOrNew => loop {
                let latest = self.terminals.iter()
                    .filter_map(|(&id, kept)| Some((kept.left()?.at, id)))
                    .max().map(|(_, id)| id);
                match latest {
                    Some(id) if !self.prepare_return(id, now) => {}
                    _ => break latest,
                }
            },
        };
        self.detach(session, Reason::Left.as_str(), now, wall, false);
        let place = Place::Attached(notify);
        let cred = (!cred.is_empty()).then(|| cred.to_owned());
        let (id, kept, event) = if let Some(id) = target {
            let kept = self.terminals.get_mut(&id).unwrap();
            kept.place = place;
            kept.name = name.into();
            kept.cred = cred;
            (id, kept, "terminal.returned")
        } else {
            let shell = open()?;
            self.last_id += 1;
            let id = self.last_id;
            let kept = self.terminals.entry(id).or_insert(Kept {
                shell, place, since: unix_seconds(wall), name: name.into(), cred,
            });
            (id, kept, "terminal.opened")
        };
        self.attached = Some(id);
        kept.shell.pty.set_attached(true);
        eprintln!("{}", tf(event, &[("name", name.into())]));
        Ok(Some((id, &mut kept.shell, target.is_some())))
    }

    fn prepare_return(&mut self, id: u64, now: Instant) -> bool {
        let Some(kept) = self.terminals.get_mut(&id) else { return false };
        for _ in 0..=OUTPUT_FRAMES {
            match kept.drain(now) {
                Ok(true) => {}
                Ok(false) => return true,
                Err(why) => {
                    self.close(id, why);
                    return false;
                }
            }
        }
        true
    }

    fn attached(&mut self, session: &str) -> Option<&mut Shell> {
        self.terminals.get_mut(&self.attached_id(session)?).map(|kept| &mut kept.shell)
    }

    fn attached_id(&self, session: &str) -> Option<u64> {
        self.attached.filter(|_| self.seat.as_deref() == Some(session))
    }

    fn detach(&mut self, session: &str, why: &str, now: Instant, wall: SystemTime, end: bool) -> bool {
        if self.seat.as_deref() != Some(session) {
            return false;
        }
        if end {
            self.seat = None;
        }
        let Some(id) = self.attached.take() else { return false };
        let kept = self.terminals.get_mut(&id).unwrap();
        kept.shell.pty.set_attached(false);
        eprintln!("{}", tf("terminal.left", &[("name", kept.name.clone()), ("reason", why.into())]));
        kept.place = Place::Left(Left { at: now, wall: unix_seconds(wall) });
        true
    }

    fn drain_left(&mut self, now: Instant) -> (bool, Option<Instant>) {
        let mut more = false;
        let mut deadline = None;
        self.terminals.retain(|_, kept| {
            let Some(left) = kept.left() else { return true };
            let expiry = left.at + KEEP_FOR;
            match kept.drain(now) {
                Ok(progress) => {
                    more |= progress;
                    deadline = Some(deadline.map_or(expiry, |earliest: Instant| earliest.min(expiry)));
                    true
                }
                Err(why) => {
                    why.log();
                    false
                }
            }
        });
        (more, deadline)
    }

    fn close_attached(&mut self, session: &str, why: Closed) -> bool {
        let Some(id) = self.attached_id(session) else { return false };
        self.close(id, why);
        true
    }

    fn terminate(&mut self, session: &str) -> Option<io::Result<bool>> {
        let result = self.attached(session)?.pty.terminate();
        if matches!(result, Ok(true)) {
            self.close_attached(session, Closed::Person);
        }
        Some(result)
    }

    fn stop(&mut self) {
        self.stopping = true;
        self.close_all(Closed::Stop);
    }

    fn close(&mut self, id: u64, why: Closed) {
        if self.attached == Some(id) {
            self.attached = None;
        }
        self.terminals.remove(&id);
        why.log();
    }

    fn close_all(&mut self, why: Closed) {
        self.attached = None;
        for _ in std::mem::take(&mut self.terminals) {
            why.log();
        }
    }
}

impl Kept {
    fn left(&self) -> Option<&Left> {
        match &self.place {
            Place::Left(left) => Some(left),
            Place::Attached(_) => None,
        }
    }

    fn drain(&mut self, now: Instant) -> Result<bool, Closed> {
        if self.left().is_some_and(|left| now >= left.at + KEEP_FOR) {
            return Err(Closed::Idle);
        }
        match self.shell.output(usize::MAX, |_| Ok(true)) {
            Ok((bytes, None)) => Ok(bytes > 0),
            Ok((_, Some(code))) => Err(Closed::Exit(code)),
            Err(_) => Err(Closed::Io),
        }
    }
}

impl Closed {
    fn log(self) {
        let reason = match self {
            Self::Exit(code) => tf("terminal.exit", &[("code", code.to_string())]),
            Self::Person => t("terminal.person"),
            Self::Idle => t("terminal.idle"),
            Self::Stop => t("terminal.stop"),
            Self::Io => t("terminal.io"),
            Self::UserChanged => t("terminal.user_changed"),
        };
        eprintln!("{}", tf("terminal.closed", &[("reason", reason)]));
    }
}

fn unix_seconds(wall: SystemTime) -> u64 {
    wall.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub(super) struct Shell {
    // Drop the receiver before the PTY so a reader blocked on the bounded queue can exit.
    events: Receiver<PtyEvent>,
    ready: Arc<AtomicBool>,
    pending: Option<PtyEvent>,
    offset: usize,
    pub(super) pty: Box<dyn Pty>,
    pub(super) replay: History,
}

impl Shell {
    fn new(mut pty: Box<dyn Pty>, wake: SyncSender<()>) -> io::Result<Self> {
        let (sender, events) = mpsc::sync_channel(OUTPUT_FRAMES);
        let writable = wake.clone();
        let ready = Arc::new(AtomicBool::new(false));
        let (output_ready, write_ready) = (ready.clone(), ready.clone());
        pty.listen(PtyEvents {
            output: Box::new(move |event| {
                if sender.send(event).is_err() { return false; }
                output_ready.store(true, Ordering::Relaxed);
                let _ = wake.try_send(());
                true
            }),
            writable: Arc::new(move || {
                write_ready.store(true, Ordering::Relaxed);
                let _ = writable.try_send(());
            }),
        })?;
        Ok(Self { events, ready, pending: None, offset: 0, pty, replay: History::default() })
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
    use super::super::{Port, PumpError, Stream, Terminal, HIGH};
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

    pub(in crate::session) fn fake_pty() -> Box<dyn Pty> {
        Box::new(FakePty(Arc::new(Mutex::new(Process::default()))))
    }

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

    struct FakePort {
        buffered: usize,
        result: Result<bool, io::ErrorKind>,
        writes: Vec<(bool, Vec<u8>)>,
    }

    impl Default for FakePort {
        fn default() -> Self {
            Self { buffered: 0, result: Ok(true), writes: Vec::new() }
        }
    }

    impl Port for FakePort {
        fn write(&mut self, binary: bool, data: &[u8]) -> anyhow::Result<bool> {
            self.writes.push((binary, data.to_vec()));
            Ok(self.result.map_err(io::Error::from)?)
        }

        fn buffered(&mut self) -> usize {
            self.buffered
        }
    }

    fn opened(now: Instant) -> (State, Arc<Mutex<Process>>) {
        let mut state = State { seat: Some("one".into()), ..State::default() };
        let (_, process) = open_fake(&mut state, now);
        (state, process)
    }

    fn open_fake(state: &mut State, now: Instant) -> (u64, Arc<Mutex<Process>>) {
        let process = Arc::new(Mutex::new(Process::default()));
        let (id, _, returned) = state.attach(("one", "Phone", "cred1"), Selection::New, (now, UNIX_EPOCH + Duration::from_secs(100)), mpsc::channel().0,
            || Shell::new(Box::new(FakePty(process.clone())), mpsc::sync_channel(1).0))
            .unwrap().unwrap();
        assert!(!returned);
        (id, process)
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
    fn taking_the_seat_detaches_the_shell_and_user_change_closes_all_shells() {
        let (updates, _) = Waker::default().channel();
        for next in [WorkspaceState::NO_USER, WorkspaceState { user: Some(2), locked: false }] {
            let state = Arc::new(Mutex::new(WorkspaceState::READY));
            let processes = [Arc::new(Mutex::new(Process::default())), Arc::new(Mutex::new(Process::default()))];
            let open = Mutex::new(processes.clone().into_iter());
            let native = state.clone();
            let registry = Registry::guarded(
                Arc::new(move |_, _, _| Ok(Box::new(FakePty(open.lock().unwrap().next().unwrap())))),
                Arc::new(move || *native.lock().unwrap()), updates.clone(),
            );
            let lease = registry.admit("one".into(), "Phone".into(), "cred".into());
            for id in [1, 2] {
                assert_eq!(lease.attach(Selection::New, 24, 80, mpsc::channel().0).unwrap().unwrap().id, id);
            }
            assert!(registry.status().iter().any(|terminal| terminal.id == 2 && terminal.attached));
            let new = registry.admit("two".into(), "Laptop".into(), "other".into());
            assert!(!lease.holds_seat());
            let status = registry.status();
            assert_eq!(status.len(), 2);
            assert!(status.iter().all(|terminal| !terminal.attached && terminal.left.is_some() && terminal.name == "Phone"));
            assert_eq!(new.attach(Selection::LatestOrNew, 24, 80, mpsc::channel().0).unwrap().unwrap().id, 2);
            state.lock().unwrap().locked = true;
            assert!(new.with_shell(|_| ()).is_none());
            registry.end("two", "locked");
            assert_eq!(registry.status()[0].name, "Laptop");
            assert!(processes.iter().all(|process| process.lock().unwrap().drops == 0));
            *state.lock().unwrap() = next;
            registry.user_changed();
            assert!(registry.status().is_empty());
            assert!(processes.iter().all(|process| process.lock().unwrap().drops == 1));
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
            let target = if id == "live" { Selection::New } else { Selection::Id(1) };
            terminal.open(24, 80, target);
            if id == "live" {
                process.lock().unwrap().emit(PtyEvent::Bytes(b"kept output".to_vec()));
            }
            // The attachment check succeeds, then access is revoked before the frame is queued.
            revoke.store(withhold, Ordering::Relaxed);
            let mut port = FakePort::default();
            assert_eq!(terminal.pump(&mut port).unwrap(), !withhold);
            if withhold {
                assert!(locked.load(Ordering::Relaxed));
                assert_eq!(port.writes.len(), 1); // Opened only.
            } else {
                assert_eq!(port.writes[1..], [(true, b"kept output".to_vec())]);
            }
            assert!(!port.writes[0].0);
            registry.end(id, "locked");
        }
    }

    #[test]
    fn pump_classifies_pty_and_port_failures() {
        #[derive(Clone, Copy, Debug)]
        enum Outcome { Progress, Waiting, PtyFailed, TransportFailed }
        use Outcome::*;

        let broken = io::ErrorKind::BrokenPipe;
        for (buffered, write, event, expected) in [
            (0, Ok(true), PtyEvent::Bytes(b"output".to_vec()), Progress),
            (0, Ok(false), PtyEvent::Bytes(b"output".to_vec()), Waiting),
            (HIGH, Ok(true), PtyEvent::Bytes(b"output".to_vec()), Waiting),
            (0, Err(broken), PtyEvent::Bytes(b"output".to_vec()), TransportFailed),
            (0, Ok(true), PtyEvent::Error(broken.into()), PtyFailed),
        ] {
            let (updates, _) = Waker::default().channel();
            let process = Arc::new(Mutex::new(Process::default()));
            let open = process.clone();
            let registry = Registry::new(move |_, _| Ok(Box::new(FakePty(open.clone()))), updates);
            let lease = registry.admit("session".into(), "browser".into(), "device".into());
            let mut terminal = Terminal::new(lease, mpsc::channel().0);
            terminal.open(24, 80, Selection::New);
            let mut port = FakePort::default();
            assert!(!terminal.pump(&mut port).unwrap());
            assert_eq!(port.writes, [(false, br#"{"t":"opened"}"#.to_vec())]);
            port.writes.clear();

            process.lock().unwrap().emit(event);
            port.buffered = buffered;
            port.result = write;
            match (terminal.pump(&mut port), expected) {
                (Ok(true), Progress) => assert_eq!(port.writes, [(true, b"output".to_vec())]),
                (Ok(false), Waiting) => {
                    assert_eq!(port.writes.len(), usize::from(buffered == 0));
                    port = FakePort::default();
                    assert!(terminal.pump(&mut port).unwrap());
                    assert_eq!(port.writes, [(true, b"output".to_vec())]);
                }
                (Err(error @ PumpError::Terminal(_)), PtyFailed)
                | (Err(error @ PumpError::Transport(_)), TransportFailed) => {
                    terminal.pump_failed(error);
                    let status = registry.status();
                    assert_eq!(status.len(), usize::from(matches!(expected, TransportFailed)));
                    assert!(status.iter().all(|terminal| !terminal.attached));
                    port = FakePort::default();
                    assert!(terminal.pump(&mut port).unwrap());
                    assert_eq!(port.writes, [(false, br#"{"t":"error","why":"io"}"#.to_vec())]);
                }
                (actual, expected) => panic!("expected {expected:?}, got {actual:?}"),
            }
        }
    }

    #[test]
    fn terminal_selection_uses_departure_order_and_refuses_an_attached_id() {
        let now = Instant::now();
        let (mut state, _) = opened(now);
        open_fake(&mut state, now + Duration::from_secs(1));
        for (step, leave_first, target, expected) in [
            (2, false, Selection::Id(2), None),
            (3, false, Selection::LatestOrNew, Some(1)),
            (5, true, Selection::LatestOrNew, Some(1)),
        ] {
            let at = now + Duration::from_secs(step);
            if leave_first {
                assert!(state.detach("one", "left", at - Duration::from_secs(1), UNIX_EPOCH, false));
            }
            let attachment = state.attach(("one", "Phone", "cred1"), target, (at, UNIX_EPOCH), mpsc::channel().0, || panic!("unexpected shell"))
                .unwrap().map(|(id, _, _)| id);
            assert_eq!(attachment, expected);
            assert_eq!(state.attached, expected.or(Some(2)));
        }
    }

    #[test]
    fn shells_exit_close_and_expire_independently_until_stop() {
        for (why, detached) in [
            (Closed::Exit(17), false), (Closed::Exit(17), true), (Closed::Person, false),
            (Closed::Idle, true), (Closed::Io, true), (Closed::Stop, false), (Closed::Stop, true),
        ] {
            let now = Instant::now();
            let wall = UNIX_EPOCH + Duration::from_secs(123);
            let (mut state, first) = opened(now);
            let (id, process, other) = if detached {
                assert!(state.detach("one", "left", now, wall, false));
                assert!(state.status().contains(&TerminalStatus {
                    id: 1, since: 100, name: "Phone".into(), cred: Some("cred1".into()), attached: false, left: Some(123),
                }));
                assert!(state.terminate("one").is_none());
                let (_, other) = open_fake(&mut state, now + Duration::from_secs(1));
                let later = Duration::from_secs(2);
                assert!(state.detach("one", "left", now + later, wall + later, true));
                assert!(!state.detach("one", "failed", now + later, wall + later, true));
                (1, first, other)
            } else {
                let (id, process) = open_fake(&mut state, now + Duration::from_secs(1));
                (id, process, first)
            };
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
                    assert_eq!(state.drain_left(now + KEEP_FOR - Duration::from_secs(1)).1, Some(now + KEEP_FOR));
                    assert_eq!(process.lock().unwrap().drops, 0);
                    assert_eq!(state.drain_left(now + KEEP_FOR).1, Some(now + KEEP_FOR + Duration::from_secs(2)));
                }
                Closed::Io => {
                    process.lock().unwrap().emit(PtyEvent::Error(io::ErrorKind::BrokenPipe.into()));
                    state.drain_left(now);
                }
                Closed::Stop => state.stop(),
                Closed::UserChanged => unreachable!(),
            }
            assert!(!state.terminals.contains_key(&id), "{why:?}, detached={detached}");
            assert_eq!(other.lock().unwrap().drops, usize::from(matches!(why, Closed::Stop)));
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
        let (_, other) = open_fake(&mut state, now);
        assert!(state.detach("one", "taken", now, UNIX_EPOCH, true));
        assert!(state.attached("one").is_none());
        let detached = b'x';
        let live = b'y';
        let mode = b"\x1b[?2004h";
        process.lock().unwrap().emit(PtyEvent::Bytes(mode.to_vec()));
        assert!(state.drain_left(now).0);
        for _ in 0..2 * super::super::replay::LIMIT / FRAME {
            process.lock().unwrap().emit(PtyEvent::Bytes(vec![detached; FRAME]));
            assert!(state.drain_left(now).0);
        }
        process.lock().unwrap().redraw_error = true;

        state.seat = Some("two".into());
        assert!(state.attach(("two", "Laptop", "cred2"), Selection::Id(99), (now, UNIX_EPOCH), mpsc::channel().0, || panic!("unexpected shell"))
            .unwrap().is_none());
        let (id, shell, returned) = state.attach(("two", "Laptop", "cred2"), Selection::Id(1), (now, UNIX_EPOCH), mpsc::channel().0, || panic!("unexpected shell"))
            .unwrap().unwrap();
        assert_eq!(id, 1);
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
        assert!(state.terminals[&1].left().is_none());
        assert!(state.terminals[&2].left().is_some());
        assert_eq!(other.lock().unwrap().drops, 0);
        assert!(state.terminate("one").is_none());
    }
}
