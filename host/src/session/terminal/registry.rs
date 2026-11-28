//! Owns the computer's terminal PTYs and their desk and local attachments.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::{self, Receiver, SyncSender, TryRecvError}, Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::wake::Wake;

use wire::types::{Reason, TerminalStatus};

use crate::i18n::{t, tf};
use crate::platform::Pty;
use crate::platform::pty::{PtyEvent, PtyEvents, OUTPUT_FRAMES};
use crate::platform::WorkspaceState;
use crate::workspace::BrowserState;
use crate::session::SessionUpdate;
use super::{replay::History, Notify, Replay};

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
        Self::guarded(Arc::new(move |_, rows, cols| open(rows, cols)), Arc::new(|| WorkspaceState::READY), WorkspaceState::READY, false, updates)
    }

    pub fn guarded(open: Arc<Open>, access: Arc<BrowserState>, initial: WorkspaceState, view_only: bool, updates: Wake<SessionUpdate>) -> Self {
        let (wake, ready) = mpsc::sync_channel(1);
        let state = Mutex::new(State { user: initial.user, ..State::default() });
        let shared = Arc::new(Shared { open, access, view_only, updates, state, wake });
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
            if state.end_desk(&old, Reason::Taken.as_str(), Instant::now(), SystemTime::now()) {
                self.shared.changed();
            }
        }
        state.seat = Some(id.clone());
        let identity = Attacher::Device { name, cred: Some(cred).filter(|cred| !cred.is_empty()) };
        Lease { shared: self.shared.clone(), holder: Holder::Desk(id), identity, user }
    }

    pub fn holds(&self, id: &str) -> bool {
        self.shared.state.lock().unwrap().seat.as_deref() == Some(id)
    }

    pub fn workspace_changed(&self, next: WorkspaceState) {
        let mut state = self.shared.state.lock().unwrap();
        let reason = if state.user.is_some() && state.user != next.user {
            state.close_all(Closed::UserChanged);
            None
        } else {
            next.refusal(self.shared.view_only)
        };
        state.user = next.user;
        if let Some(reason) = reason {
            if let Some(id) = state.held_by(&Holder::Here) {
                state.detach(id, reason.as_str(), Instant::now(), SystemTime::now(), Some(reason));
            }
        }
        self.shared.changed();
    }

    pub fn end(&self, id: &str, why: &str) {
        self.shared.end_desk(id, why);
    }

    #[cfg(any(windows, target_os = "macos", test))]
    pub fn local_handle(&self) -> LocalTerminals {
        LocalTerminals(self.shared.clone())
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
    view_only: bool,
    updates: Wake<SessionUpdate>,
    state: Mutex<State>,
    wake: SyncSender<()>,
}

impl Shared {
    fn changed(&self) {
        let _ = self.updates.send(SessionUpdate::TerminalChanged);
        self.notify();
    }

    fn end_desk(&self, id: &str, why: &str) {
        let mut state = self.state.lock().unwrap();
        if state.end_desk(id, why, Instant::now(), SystemTime::now()) {
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
                for kept in state.terminals.values() {
                    if let Place::Attached { notify, .. } = &kept.place {
                        if kept.shell.ready.swap(false, Ordering::Relaxed) {
                            notify();
                        }
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

#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone)]
pub struct LocalTerminals(Arc<Shared>);

#[cfg(any(windows, target_os = "macos", test))]
impl LocalTerminals {
    #[cfg(any(target_os = "macos", test))]
    pub fn user(&self) -> Option<u64> {
        let state = self.0.state.lock().unwrap();
        if state.stopping { return None; }
        state.user
    }

    pub fn lease(&self, user: u64) -> Option<Lease> {
        self.accessible(user).then(|| Lease {
            shared: self.0.clone(), holder: Holder::Here, identity: Attacher::Here, user,
        })
    }

    pub fn accessible(&self, user: u64) -> bool {
        self.0.refusal(&self.0.state.lock().unwrap(), user).is_none()
    }

    pub fn status(&self, user: u64) -> Result<Vec<TerminalStatus>, Reason> {
        let state = self.0.state.lock().unwrap();
        if let Some(reason) = self.0.refusal(&state, user) { return Err(reason); }
        Ok(state.status().into_iter().map(|mut terminal| {
            terminal.cred = None;
            terminal
        }).collect())
    }
}

#[derive(Clone)]
enum Attacher {
    Device { name: String, cred: Option<String> },
    Here,
}

impl Attacher {
    fn name(&self) -> Cow<'_, str> {
        match self {
            Self::Device { name, .. } => name.into(),
            // Also the legacy row name that servers and browsers without `here` show.
            Self::Here => t("device.this_computer").into(),
        }
    }

    fn cred(&self) -> Option<String> {
        match self {
            Self::Device { cred, .. } => cred.clone(),
            Self::Here => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Holder {
    Desk(String),
    Here,
}

impl Shared {
    fn live_refusal(&self, user: u64) -> Option<Reason> {
        let workspace = (self.access)();
        workspace.refusal(self.view_only).or_else(|| (workspace.user != Some(user)).then_some(Reason::UserChanged))
    }

    fn refusal(&self, state: &State, user: u64) -> Option<Reason> {
        if state.stopping { return Some(Reason::Failed); }
        if state.user != Some(user) { return Some(Reason::UserChanged); }
        self.live_refusal(user)
    }
}

#[derive(Clone)]
pub struct Lease {
    shared: Arc<Shared>,
    holder: Holder,
    identity: Attacher,
    user: u64,
}

impl Lease {
    pub(in crate::session) fn id(&self) -> &str {
        let Holder::Desk(id) = &self.holder else { unreachable!("files require the desk seat") };
        id
    }

    pub(in crate::session) fn name(&self) -> Cow<'_, str> {
        self.identity.name()
    }

    pub(in crate::session) fn holds_seat(&self) -> bool {
        self.while_seated(|| ()).is_some()
    }

    pub(in crate::session) fn while_seated<T>(&self, run: impl FnOnce() -> T) -> Option<T> {
        let state = self.shared.state.lock().unwrap();
        (state.seat.as_deref() == Some(self.id()) && self.admitted(&state)).then(run)
    }

    fn admitted(&self, state: &State) -> bool {
        self.shared.refusal(state, self.user).is_none()
    }

    pub(super) fn accessible(&self) -> bool {
        self.shared.live_refusal(self.user).is_none()
    }

    pub(super) fn attach(&self, target: Selection, rows: u16, cols: u16, notify: Notify) -> io::Result<Option<Attachment>> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.admitted(&state) {
            return Ok(None);
        }
        let before = state.terminals.len();
        let info = state.attach(&self.holder, &self.identity, target, (Instant::now(), SystemTime::now()), notify, || {
            Shell::new((self.shared.open)(self.user, rows, cols)?, self.shared.wake.clone())
        }).map(|attached| attached.map(|(key, shell, returned)| Attachment {
            key, replay: returned.then(|| Replay::new(&shell.replay)), dropped: shell.replay.dropped,
        }));
        if matches!(info, Ok(Some(_))) || before != state.terminals.len() {
            self.shared.changed();
        }
        info
    }

    pub(super) fn with_shell<T>(&self, key: &AttachmentId, f: impl FnOnce(&mut Shell) -> (T, Option<Closed>)) -> Option<T> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.admitted(&state) { return None; }
        let (result, closed) = f(state.attached(key)?);
        if let Some(why) = closed {
            state.close(key.terminal, why);
            self.shared.changed();
        }
        Some(result)
    }

    pub(super) fn detach(&self, key: &AttachmentId, why: &str) {
        let mut state = self.shared.state.lock().unwrap();
        if state.attached(key).is_some() {
            state.detach(key.terminal, why, Instant::now(), SystemTime::now(), None);
            self.shared.changed();
        }
    }

    pub(in crate::session) fn end(&self, why: &str) {
        self.shared.end_desk(self.id(), why);
    }

    pub(super) fn terminate(&self, key: &AttachmentId) -> Option<io::Result<bool>> {
        let mut state = self.shared.state.lock().unwrap();
        let result = self.admitted(&state).then(|| state.terminate(key)).flatten();
        if matches!(result, Some(Ok(true))) { self.shared.changed(); }
        result
    }
}

#[derive(Default)]
struct State {
    user: Option<u64>,
    seat: Option<String>,
    terminals: BTreeMap<u64, Kept>,
    last_id: u64,
    stopping: bool,
}

#[derive(Clone)]
pub(super) struct AttachmentId {
    pub(super) terminal: u64,
    // Each attachment gets its own token, even when returning to the same shell.
    revoked: Arc<OnceLock<Reason>>,
}

impl AttachmentId {
    pub(super) fn revoked(&self) -> Option<Reason> {
        self.revoked.get().copied()
    }
}

pub(super) struct Attachment {
    pub(super) key: AttachmentId,
    pub(super) replay: Option<Replay>,
    pub(super) dropped: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Selection {
    LatestOrNew,
    Id { id: u64, take: bool },
    New,
}

struct Kept {
    shell: Shell,
    place: Place,
    since: u64,
    last: Attacher,
}

enum Place {
    Attached { holder: Holder, key: AttachmentId, notify: Notify },
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
            name: kept.last.name().into(), cred: kept.last.cred(), left: kept.left().map(|left| left.wall),
            here: matches!(kept.place, Place::Attached { holder: Holder::Here, .. }).then_some(true),
        }).collect()
    }

    fn attach(
        &mut self, holder: &Holder, identity: &Attacher, selection: Selection,
        (now, wall): (Instant, SystemTime), notify: Notify,
        open: impl FnOnce() -> io::Result<Shell>,
    ) -> io::Result<Option<(AttachmentId, &mut Shell, bool)>> {
        if self.stopping || matches!(holder, Holder::Desk(session) if self.seat.as_ref() != Some(session)) {
            return Ok(None);
        }
        let target = match selection {
            Selection::Id { id, take } => {
                let Some(kept) = self.terminals.get(&id) else { return Ok(None) };
                if let Place::Attached { holder: current, .. } = &kept.place {
                    if !take || current == holder { return Ok(None); }
                    self.detach(id, Reason::Taken.as_str(), now, wall, Some(Reason::Taken));
                } else if !self.prepare_return(id, now) {
                    return Ok(None);
                }
                Some(id)
            }
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
        let id = match target {
            Some(id) => id,
            None => {
                let shell = open()?;
                self.last_id += 1;
                self.terminals.insert(self.last_id, Kept {
                    shell, place: Place::Left(Left { at: now, wall: unix_seconds(wall) }),
                    since: unix_seconds(wall), last: identity.clone(),
                });
                self.last_id
            }
        };
        if let Some(old) = self.held_by(holder) {
            self.detach(old, Reason::Left.as_str(), now, wall, Some(Reason::Left));
        }
        let key = AttachmentId { terminal: id, revoked: Arc::new(OnceLock::new()) };
        let kept = self.terminals.get_mut(&id).unwrap();
        kept.place = Place::Attached { holder: holder.clone(), key: key.clone(), notify };
        kept.last = identity.clone();
        kept.shell.pty.set_attached(true);
        let event = if target.is_some() { "terminal.returned" } else { "terminal.opened" };
        eprintln!("{}", tf(event, &[("name", identity.name().into())]));
        Ok(Some((key, &mut kept.shell, target.is_some())))
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

    fn held_by(&self, holder: &Holder) -> Option<u64> {
        self.terminals.iter().find_map(|(&id, kept)| {
            matches!(&kept.place, Place::Attached { holder: current, .. } if current == holder).then_some(id)
        })
    }

    fn attached(&mut self, key: &AttachmentId) -> Option<&mut Shell> {
        let kept = self.terminals.get_mut(&key.terminal)?;
        matches!(&kept.place, Place::Attached { key: current, .. } if Arc::ptr_eq(&current.revoked, &key.revoked))
            .then_some(&mut kept.shell)
    }

    fn end_desk(&mut self, session: &str, why: &str, now: Instant, wall: SystemTime) -> bool {
        if self.seat.as_deref() != Some(session) { return false; }
        self.seat = None;
        let Some(id) = self.held_by(&Holder::Desk(session.into())) else { return false };
        self.detach(id, why, now, wall, None);
        true
    }

    fn detach(&mut self, id: u64, why: &str, now: Instant, wall: SystemTime, revoked: Option<Reason>) {
        let kept = self.terminals.get_mut(&id).unwrap();
        if let Some(reason) = revoked { kept.revoke(reason); }
        kept.shell.pty.set_attached(false);
        eprintln!("{}", tf("terminal.left", &[("name", kept.last.name().into()), ("reason", why.into())]));
        kept.place = Place::Left(Left { at: now, wall: unix_seconds(wall) });
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

    fn close_attached(&mut self, key: &AttachmentId, why: Closed) -> bool {
        if self.attached(key).is_none() { return false; }
        self.close(key.terminal, why);
        true
    }

    fn terminate(&mut self, key: &AttachmentId) -> Option<io::Result<bool>> {
        let result = self.attached(key)?.pty.terminate();
        if matches!(result, Ok(true)) {
            self.close_attached(key, Closed::Person);
        }
        Some(result)
    }

    fn stop(&mut self) {
        self.stopping = true;
        self.close_all(Closed::Stop);
    }

    fn close(&mut self, id: u64, why: Closed) {
        self.terminals.remove(&id);
        why.log();
    }

    fn close_all(&mut self, why: Closed) {
        let reason = if matches!(why, Closed::UserChanged) { Reason::UserChanged } else { Reason::Failed };
        for (_, kept) in std::mem::take(&mut self.terminals) {
            kept.revoke(reason);
            why.log();
        }
    }
}

impl Kept {
    fn revoke(&self, reason: Reason) {
        if let Place::Attached { key, notify, .. } = &self.place {
            let _ = key.revoked.set(reason);
            notify();
        }
    }

    fn left(&self) -> Option<&Left> {
        match &self.place {
            Place::Left(left) => Some(left),
            Place::Attached { .. } => None,
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

    fn device(name: &str, cred: &str) -> Attacher {
        Attacher::Device { name: name.into(), cred: Some(cred.into()) }
    }

    fn desk(id: &str) -> Holder {
        Holder::Desk(id.into())
    }

    fn key(state: &State, session: &str) -> AttachmentId {
        let id = state.held_by(&desk(session)).unwrap();
        let Place::Attached { key, .. } = &state.terminals[&id].place else { unreachable!() };
        key.clone()
    }

    fn shell<'a>(state: &'a mut State, session: &str) -> &'a mut Shell {
        state.attached(&key(state, session)).unwrap()
    }

    fn opened(now: Instant) -> (State, Arc<Mutex<Process>>) {
        let mut state = State { seat: Some("one".into()), ..State::default() };
        let (_, process) = open_fake(&mut state, now);
        (state, process)
    }

    fn open_fake(state: &mut State, now: Instant) -> (u64, Arc<Mutex<Process>>) {
        let process = Arc::new(Mutex::new(Process::default()));
        let (key, _, returned) = state.attach(&desk("one"), &device("Phone", "cred1"), Selection::New, (now, UNIX_EPOCH + Duration::from_secs(100)), Arc::new(|| {}),
            || Shell::new(Box::new(FakePty(process.clone())), mpsc::sync_channel(1).0))
            .unwrap().unwrap();
        assert!(!returned);
        (key.terminal, process)
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
    fn two_holders_can_take_an_attachment_without_taking_the_desk_seat() {
        for (from_here, take, exiting) in [
            (false, false, false), (false, true, false), (false, true, true),
            (true, false, false), (true, true, false), (true, true, true),
        ] {
            let (updates, _) = Waker::default().channel();
            let (opened, processes) = mpsc::channel();
            let registry = Registry::new(move |_, _| {
                let process = Arc::new(Mutex::new(Process::default()));
                opened.send(process.clone()).unwrap();
                Ok(Box::new(FakePty(process)))
            }, updates);
            let desk = registry.admit("one".into(), "Phone".into(), "cred".into());
            let local = registry.local_handle();
            let user = local.user().unwrap();
            let here = local.lease(user).unwrap();
            let (from, to) = if from_here { (&here, &desk) } else { (&desk, &here) };
            let (wake, ready) = mpsc::channel();
            let first = from.attach(Selection::New, 24, 80, Arc::new(move || { let _ = wake.send(()); })).unwrap().unwrap();
            let second = to.attach(Selection::New, 24, 80, Arc::new(|| {})).unwrap().unwrap();
            let id = first.key.terminal;
            assert_eq!(registry.status().iter().filter(|t| t.attached).count(), 2);
            assert_eq!(registry.status().iter().find(|t| t.id == id).unwrap().here, from_here.then_some(true));
            assert!(local.status(user).unwrap().iter().all(|t| t.cred.is_none()));

            if exiting {
                let process = processes.try_recv().unwrap();
                let mut process = process.lock().unwrap();
                process.emit(PtyEvent::Bytes(b"last output".to_vec()));
                process.exit(17);
            }
            let result = to.attach(Selection::Id { id, take }, 24, 80, Arc::new(|| {})).unwrap();
            assert_eq!(result.is_some(), take);
            if let Some(result) = result {
                assert!(result.replay.is_some());
                assert_eq!(ready.recv_timeout(Duration::from_secs(2)), Ok(()));
                assert_eq!(first.key.revoked(), Some(Reason::Taken));
                assert_eq!(second.key.revoked(), Some(Reason::Left));
                assert!(from.with_shell(&first.key, |_| ((), None)).is_none());
                assert!(from.terminate(&first.key).is_none());
                assert!(from.with_shell(&first.key, |_| ((), Some(Closed::Person))).is_none());
                from.detach(&first.key, "left");
                assert_eq!(to.with_shell(&result.key, |_| ((), None)), Some(()));
                if exiting {
                    assert_eq!(to.with_shell(&result.key, |shell| {
                        let output = shell.output(FRAME, |bytes| {
                            assert_eq!(bytes, b"last output");
                            Ok(true)
                        }).unwrap();
                        assert_eq!(output, (b"last output".len(), None));
                        let exit = shell.output(FRAME, |_| panic!("unexpected output")).unwrap();
                        assert_eq!(exit, (0, Some(17)));
                        ((), Some(Closed::Exit(17)))
                    }), Some(()));
                }
            } else {
                assert_eq!(first.key.revoked(), None);
                assert_eq!(second.key.revoked(), None);
                assert_eq!(from.with_shell(&first.key, |_| ((), None)), Some(()));
                assert_eq!(to.with_shell(&second.key, |_| ((), None)), Some(()));
            }
            assert!(desk.holds_seat());
        }
    }

    #[test]
    fn seat_changes_keep_here_attached_and_workspace_changes_revoke_it() {
        for (next, remaining) in [
            (WorkspaceState { user: Some(0), locked: true }, 2),
            (WorkspaceState::NO_USER, 0),
            (WorkspaceState { user: Some(1), locked: false }, 0),
        ] {
            let (updates, _) = Waker::default().channel();
            let registry = Registry::new(|_, _| Ok(fake_pty()), updates);
            let desk = registry.admit("one".into(), "Phone".into(), "cred".into());
            let local = registry.local_handle();
            let here = local.lease(local.user().unwrap()).unwrap();
            let first = here.attach(Selection::New, 24, 80, Arc::new(|| {})).unwrap().unwrap();
            let second = desk.attach(Selection::LatestOrNew, 24, 80, Arc::new(|| {})).unwrap().unwrap();
            assert_ne!(first.key.terminal, second.key.terminal);
            registry.admit("new".into(), "Laptop".into(), "next".into());
            assert!(desk.with_shell(&second.key, |_| ((), None)).is_none());
            assert_eq!(here.with_shell(&first.key, |_| ((), None)), Some(()));
            registry.workspace_changed(next);
            assert!(here.with_shell(&first.key, |_| ((), None)).is_none());
            assert!(first.key.revoked().is_some());
            assert_eq!(registry.status().len(), remaining);
        }
    }

    #[test]
    fn a_displaced_stream_reports_loss_with_the_negotiated_message() {
        for (caps, expected) in [
            (vec![wire::caps::TERMINALS.into()], serde_json::json!({"t":"error","why":"io"})),
            (vec![wire::caps::TERMINALS.into(), wire::caps::TERMINAL_TAKEOVER.into()],
                serde_json::json!({"t":"detached","reason":"taken"})),
        ] {
            let (updates, _) = Waker::default().channel();
            let registry = Registry::new(|_, _| Ok(fake_pty()), updates);
            let desk = registry.admit("one".into(), "Phone".into(), "cred".into());
            let mut terminal = Terminal::new(desk, || {});
            terminal.negotiate(&caps, true);
            terminal.open(24, 80, Selection::New);
            let local = registry.local_handle();
            let here = local.lease(local.user().unwrap()).unwrap();
            assert!(here.attach(Selection::Id { id: 1, take: true }, 24, 80, Arc::new(|| {})).unwrap().is_some());

            terminal.message(true, false, br#"{"t":"close"}"#.to_vec());
            let mut port = FakePort::default();
            while terminal.pump(&mut port).unwrap() {}
            let messages: Vec<serde_json::Value> = port.writes.iter().map(|(binary, bytes)| {
                assert!(!binary);
                serde_json::from_slice(bytes).unwrap()
            }).collect();
            assert_eq!(messages, [serde_json::json!({"t":"opened","id":1}), expected]);
        }
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
                Arc::new(move || *native.lock().unwrap()), WorkspaceState::READY, false, updates.clone(),
            );
            let lease = registry.admit("one".into(), "Phone".into(), "cred".into());
            for id in [1, 2] {
                assert_eq!(lease.attach(Selection::New, 24, 80, Arc::new(|| {})).unwrap().unwrap().key.terminal, id);
            }
            assert!(registry.status().iter().any(|terminal| terminal.id == 2 && terminal.attached));
            let new = registry.admit("two".into(), "Laptop".into(), "other".into());
            assert!(!lease.holds_seat());
            let status = registry.status();
            assert_eq!(status.len(), 2);
            assert!(status.iter().all(|terminal| !terminal.attached && terminal.left.is_some() && terminal.name == "Phone"));
            let attachment = new.attach(Selection::LatestOrNew, 24, 80, Arc::new(|| {})).unwrap().unwrap();
            assert_eq!(attachment.key.terminal, 2);
            state.lock().unwrap().locked = true;
            assert!(new.with_shell(&attachment.key, |_| ((), None)).is_none());
            registry.end("two", "locked");
            assert_eq!(registry.status()[0].name, "Laptop");
            assert!(processes.iter().all(|process| process.lock().unwrap().drops == 0));
            *state.lock().unwrap() = next;
            registry.workspace_changed(next);
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
        let registry = Registry::guarded(Arc::new(move |_, _, _| Ok(Box::new(FakePty(open.clone())))), access, WorkspaceState::READY, false, updates);
        for (id, withhold) in [("live", true), ("replay", true), ("return", false)] {
            locked.store(false, Ordering::Relaxed);
            let mut terminal = Terminal::new(registry.admit(id.into(), "browser".into(), "device".into()), || {});
            let target = if id == "live" { Selection::New } else { Selection::Id { id: 1, take: false } };
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
            let mut terminal = Terminal::new(lease, || {});
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
            (2, false, Selection::Id { id: 2, take: false }, None),
            (3, false, Selection::LatestOrNew, Some(1)),
            (5, true, Selection::LatestOrNew, Some(1)),
        ] {
            let at = now + Duration::from_secs(step);
            if leave_first {
                let id = state.held_by(&desk("one")).unwrap();
                state.detach(id, "left", at - Duration::from_secs(1), UNIX_EPOCH, None);
            }
            let attachment = state.attach(&desk("one"), &device("Phone", "cred1"), target, (at, UNIX_EPOCH), Arc::new(|| {}), || panic!("unexpected shell"))
                .unwrap().map(|(key, _, _)| key.terminal);
            assert_eq!(attachment, expected);
            assert_eq!(state.held_by(&desk("one")), expected.or(Some(2)));
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
                let old = key(&state, "one");
                state.detach(1, "left", now, wall, None);
                assert!(state.status().contains(&TerminalStatus {
                    id: 1, since: 100, name: "Phone".into(), cred: Some("cred1".into()), attached: false, left: Some(123), here: None,
                }));
                assert!(state.terminate(&old).is_none());
                let (_, other) = open_fake(&mut state, now + Duration::from_secs(1));
                let later = Duration::from_secs(2);
                assert!(state.end_desk("one", "left", now + later, wall + later));
                assert!(!state.end_desk("one", "failed", now + later, wall + later));
                (1, first, other)
            } else {
                let (id, process) = open_fake(&mut state, now + Duration::from_secs(1));
                (id, process, first)
            };
            let attached = if detached { None } else { Some(key(&state, "one")) };
            match why {
                Closed::Exit(code) if detached => {
                    process.lock().unwrap().exit(code);
                    state.drain_left(now);
                }
                Closed::Exit(code) => {
                    process.lock().unwrap().exit(code);
                    let mut stream = Stream::new(24, 80, None);
                    let (_, exit) = stream.pump(shell(&mut state, "one"), 0, |_, _| Ok(true)).unwrap();
                    assert_eq!(exit, Some(code));
                    assert!(state.close_attached(attached.as_ref().unwrap(), Closed::Exit(code)));
                }
                Closed::Person => assert!(state.terminate(attached.as_ref().unwrap()).unwrap().unwrap()),
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
            let attached = key(&state, "one");
            let result = state.terminate(&attached).unwrap();
            if exited {
                assert!(!result.unwrap());
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            }
            assert!(state.held_by(&desk("one")).is_some());
            assert_eq!(process.lock().unwrap().drops, 0);
            if !exited {
                process.lock().unwrap().terminate_error = false;
                assert!(state.terminate(&attached).unwrap().unwrap());
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
        let attached = key(&state, "one");
        let deadline = Instant::now() + timeout;
        loop {
            let (progress, exit) = stream.pump(shell(&mut state, "one"), 0, |binary, data| {
                assert!(binary);
                sent.extend_from_slice(data);
                Ok(true)
            }).unwrap();
            if let Some(code) = exit {
                assert_eq!(code, 17);
                assert!(state.close_attached(&attached, Closed::Exit(code)));
                assert!(!state.close_attached(&attached, Closed::Exit(code)));
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
        let old = key(&state, "one");
        let (_, other) = open_fake(&mut state, now);
        assert!(state.end_desk("one", "taken", now, UNIX_EPOCH));
        assert!(state.held_by(&desk("one")).is_none());
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
        assert!(state.attach(&desk("two"), &device("Laptop", "cred2"), Selection::Id { id: 99, take: false }, (now, UNIX_EPOCH), Arc::new(|| {}), || panic!("unexpected shell"))
            .unwrap().is_none());
        let (key, shell, returned) = state.attach(&desk("two"), &device("Laptop", "cred2"), Selection::Id { id: 1, take: false }, (now, UNIX_EPOCH), Arc::new(|| {}), || panic!("unexpected shell"))
            .unwrap().unwrap();
        assert_eq!(key.terminal, 1);
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
        assert!(state.terminate(&old).is_none());
    }
}
