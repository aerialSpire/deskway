//! File transfers belong to an admitted page. Each channel attempt borrows that page's
//! record while a worker performs filesystem operations.

use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use str0m::channel::ChannelId;
use wire::file_transfer::{self as frames, Error, Message as Msg, CHUNK, FRAME, WINDOW};
use wire::types::Reason;

use super::{credit::Credit, data::WorkerSlot, terminal::registry::Lease, Cx, SessionEvent};
use crate::{i18n::t, workspace::{Download, FileTransfers, Upload}};

const SLOTS: usize = WINDOW / CHUNK + 2;
const WAIT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Registry(Option<Arc<Mutex<Page>>>);

pub(crate) struct Page {
    device: String,
    owner: String,
    session: String,
    expires: Instant,
    attached: bool,
    record: Option<Arc<Record>>,
}

impl Registry {
    pub(crate) fn is_returning(&self, device: &str, owner: &str, session: &str) -> bool {
        self.0.as_ref().is_some_and(|page| {
            let page = page.lock().unwrap();
            page.owns(device, owner) && page.session == session && page.record.is_some()
        })
    }

    pub(crate) fn admit(&mut self, device: &str, owner: &str, session: &str, expires: Instant) -> Arc<Mutex<Page>> {
        self.expire(Instant::now());
        if let Some(page) = &self.0 {
            let mut current = page.lock().unwrap();
            if current.owns(device, owner) {
                current.session = session.into();
                current.attached = true;
                return page.clone();
            }
            current.record = None;
        }
        let page = Arc::new(Mutex::new(Page {
            device: device.into(), owner: owner.into(), session: session.into(), expires,
            attached: true, record: None,
        }));
        self.0 = Some(page.clone());
        page
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        if let Some(page) = &self.0 {
            let mut page = page.lock().unwrap();
            if !page.attached && now >= page.expires {
                page.record = None;
            }
        }
    }

    pub(crate) fn forget_untrusted(&self, trusted: impl FnOnce(&str) -> bool) {
        if let Some(page) = &self.0 {
            let mut page = page.lock().unwrap();
            if !page.device.is_empty() && !trusted(&page.device) {
                page.record = None;
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        if let Some(page) = self.0.take() {
            page.lock().unwrap().record = None;
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.clear();
    }
}

impl Page {
    fn owns(&self, device: &str, owner: &str) -> bool {
        self.device == device && self.owner == owner
    }
}

pub(super) struct Transfers {
    files: FileTransfers,
    page: Arc<Mutex<Page>>,
    lease: Lease,
    wake: Sender<SessionEvent>,
    channel: Option<ChannelId>,
    waiting: Option<(Arc<Record>, bool, Option<Error>)>,
    job: Option<Job>,
    pending: Option<Packet>,
}

struct Job {
    input: Option<SyncSender<Packet>>,
    output: Receiver<Packet>,
    stopped: Arc<Mutex<Option<Stop>>>,
    id: u32,
    upload: bool,
    receiving: bool,
    received: u64,
    credit: Credit,
}

enum Packet {
    Text(Msg),
    Data(Vec<u8>),
    End(Msg),
}

#[derive(Clone, Copy)]
enum Stop {
    Interrupted,
    Failed(Error),
}

impl Job {
    fn stop(&mut self, why: Stop) {
        if self.input.is_some() {
            *self.stopped.lock().unwrap() = Some(why);
            self.input = None;
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop(Stop::Interrupted);
    }
}

impl Transfers {
    pub fn new(files: FileTransfers, page: Arc<Mutex<Page>>, lease: Lease, wake: Sender<SessionEvent>) -> Self {
        Self { files, page, lease, wake, channel: None, waiting: None, job: None, pending: None }
    }

    pub fn channel(&self) -> Option<ChannelId> {
        self.channel
    }

    pub fn attach(&mut self, id: ChannelId) -> Option<ChannelId> {
        let old = self.channel;
        self.detach();
        self.channel = Some(id);
        old
    }

    pub fn detach(&mut self) {
        self.channel = None;
        self.waiting = None;
        self.job = None;
        self.pending = None;
    }

    pub fn ended(&mut self, reason: Reason) {
        self.detach();
        let mut page = self.page.lock().unwrap();
        if page.session == self.lease.id() {
            page.attached = false;
            if !matches!(reason, Reason::Failed | Reason::Locked) {
                page.record = None;
            }
        }
    }

    fn stop(&mut self, why: Error) {
        if let Some(job) = &mut self.job {
            job.stop(Stop::Failed(why));
        } else if let Some((_, _, stopped)) = &mut self.waiting {
            *stopped = Some(why);
        }
    }

    pub fn protocol_error(&mut self) {
        self.stop(Error::Protocol);
        self.detach();
    }

    pub fn receive(&mut self, data: Vec<u8>, binary: bool) -> bool {
        if data.len() > FRAME || !self.lease.holds_seat() {
            return false;
        }
        if binary {
            let Some((id, offset, payload)) = frames::frame(&data) else {
                return false;
            };
            let Some(job) = &mut self.job else { return true };
            let Some(input) = &job.input else { return true };
            if id != job.id {
                return id < job.id;
            }
            if !job.receiving || offset != job.received || !job.credit.take(payload.len(), WINDOW) {
                return false;
            }
            job.received += payload.len() as u64;
            return enqueue(input, Packet::Data(data));
        }
        let Ok(message) = serde_json::from_slice::<Msg>(&data) else { return false };
        let id = message.id();
        match message {
            Msg::Upload { path, name, size, .. } => self.offer(id, path, name, Direction::Upload(size)),
            Msg::Download { path, entry, limit, .. } => self.offer(id, path, entry, Direction::Download(limit)),
            Msg::Resume { .. } => self.resume(id),
            _ => {
                let current = self.job.as_ref().map(|job| job.id)
                    .or_else(|| self.waiting.as_ref().map(|(record, ..)| record.id));
                let Some(current) = current else { return true };
                if id != current {
                    return id < current;
                }
                match message {
                    Msg::Cancel { .. } => self.stop(Error::Cancelled),
                    Msg::Error { why: why @ (Error::Io | Error::Protocol | Error::Integrity), .. } => self.stop(why),
                    Msg::Ack { .. } | Msg::Start { .. } | Msg::Finish { .. } | Msg::Done { .. } | Msg::Replace { .. } => {
                        return self.job.as_ref().and_then(|job| job.input.as_ref())
                            .is_some_and(|input| enqueue(input, Packet::Text(message)));
                    }
                    _ => return false,
                }
                true
            }
        }
    }

    fn idle(&self) -> bool {
        self.job.is_none() && self.pending.is_none() && self.waiting.is_none()
    }

    fn offer(&mut self, id: u32, folder: Vec<String>, name: String, direction: Direction) -> bool {
        if id == 0 || !self.idle() {
            return false;
        }
        let mut page = self.page.lock().unwrap();
        if page.session != self.lease.id() {
            return false;
        }
        if let Some(record) = &page.record {
            if id < record.id {
                return false;
            }
            if id == record.id {
                self.waiting = Some((record.clone(), true, None));
                return true;
            }
        }
        // A fresh offer on a new channel can follow a session view that gave up reconnecting.
        let record = Arc::new(Record {
            id, direction, name: self.lease.name().into(),
            running: AtomicBool::new(false),
            state: Mutex::new(State {
                initial: Some((folder, name)), size: direction.size(), ..State::default()
            }),
        });
        page.record = Some(record.clone());
        self.waiting = Some((record, false, None));
        true
    }

    fn resume(&mut self, id: u32) -> bool {
        if id == 0 || !self.idle() {
            return false;
        }
        let page = self.page.lock().unwrap();
        if page.session != self.lease.id() {
            return false;
        }
        if let Some(record) = page.record.as_ref().filter(|record| record.id == id) {
            self.waiting = Some((record.clone(), true, None));
        } else {
            self.pending = Some(Packet::End(Msg::Error { id, why: Error::Expired }));
        }
        true
    }

    fn start(&mut self) -> bool {
        let (record, resumed, stopped) = self.waiting.as_ref().unwrap();
        if record.running.load(Ordering::Acquire) {
            return false;
        }
        let slot = WorkerSlot::for_files();
        if slot.is_none() && *resumed && stopped.is_none() && !record.finished() {
            return false;
        }
        if record.running.swap(true, Ordering::AcqRel) {
            return false;
        }
        let (record, _, stopped) = self.waiting.take().unwrap();
        let id = record.id;
        let upload = matches!(record.direction, Direction::Upload(_));
        if let Some(reply) = record.state.lock().unwrap().result.clone() {
            record.running.store(false, Ordering::Release);
            self.pending = Some(Packet::End(reply));
            return true;
        }
        let Some(slot) = slot.filter(|_| stopped.is_none()) else {
            let reply = record.complete(&mut record.state.lock().unwrap(), Err(stopped.unwrap_or(Error::Busy)));
            record.running.store(false, Ordering::Release);
            self.pending = Some(Packet::End(reply));
            return true;
        };
        let (input, incoming) = mpsc::sync_channel(SLOTS);
        let (outgoing, output) = mpsc::sync_channel(SLOTS);
        let stopped = Arc::new(Mutex::new(None));
        let io = Pipe { incoming, outgoing, stopped: stopped.clone(), lease: self.lease.clone(), wake: self.wake.clone(), wait: WAIT };
        let files = self.files.clone();
        let task = record.clone();
        let thread = std::thread::Builder::new().name("file-transfer".into()).spawn(move || {
            let mut state = task.state.lock().unwrap();
            let result = state.run(id, task.direction, files, &io);
            let result = match result {
                Err(Error::Cancelled) => match *io.stopped.lock().unwrap() {
                    Some(Stop::Failed(why)) => Some(Err(why)),
                    _ => None,
                },
                result => Some(result),
            };
            let reply = result.map(|result| task.complete(&mut state, result));
            drop(state);
            drop(slot);
            task.running.store(false, Ordering::Release);
            if let Some(reply) = reply {
                let _ = io.send(Packet::End(reply));
            }
            drop(io.outgoing);
            let _ = io.wake.send(SessionEvent::TransferReady);
        });
        if thread.is_ok() {
            self.job = Some(Job {
                input: Some(input), output, stopped, id, upload, receiving: false, received: 0, credit: Credit::default(),
            });
        } else {
            let reply = record.complete(&mut record.state.lock().unwrap(), Err(Error::Io));
            record.running.store(false, Ordering::Release);
            self.pending = Some(Packet::End(reply));
        }
        true
    }

    pub fn poll(&mut self, cx: &mut Cx) -> Option<Instant> {
        let id = self.channel?;
        if self.waiting.is_some() && !self.start() {
            return Some(Instant::now() + Duration::from_millis(10));
        }
        if self.pending.is_none() {
            if let Some(job) = &self.job {
                match job.output.try_recv() {
                    Ok(packet) => self.pending = Some(packet),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        cx.rtc.direct_api().close_data_channel(id);
                        self.detach();
                        return None;
                    }
                }
            }
        }
        let packet = self.pending.as_ref()?;
        let mut ch = cx.rtc.channel(id)?;
        if ch.buffered_amount() >= FRAME {
            return Some(Instant::now() + Duration::from_millis(10));
        }
        let sent = match packet {
            Packet::Data(data) => ch.write(true, data),
            Packet::Text(msg) | Packet::End(msg) => ch.write(false, &serde_json::to_vec(msg).expect("transfer message")),
        };
        match sent {
            Ok(false) => return Some(Instant::now() + Duration::from_millis(10)),
            Err(_) => {
                cx.rtc.direct_api().close_data_channel(id);
                self.detach();
                return None;
            }
            Ok(true) => {}
        }
        match self.pending.take().unwrap() {
            Packet::End(_) => self.job = None,
            Packet::Text(Msg::Ready { offset, .. }) => {
                let job = self.job.as_mut().unwrap();
                job.receiving = job.upload;
                job.received = offset;
            }
            Packet::Text(Msg::Ack { offset, .. }) => {
                let job = self.job.as_mut().unwrap();
                let from = job.received - job.credit.outstanding() as u64;
                job.credit.acknowledge((offset - from) as usize);
            }
            _ => {}
        }
        Some(Instant::now())
    }
}

impl Drop for Transfers {
    fn drop(&mut self) {
        self.ended(Reason::Failed);
    }
}

fn enqueue(input: &SyncSender<Packet>, packet: Packet) -> bool {
    !matches!(input.try_send(packet), Err(TrySendError::Full(_)))
}

#[derive(Clone, Copy)]
enum Direction {
    Upload(u64),
    Download(Option<u64>),
}

impl Direction {
    fn size(self) -> Option<u64> {
        match self {
            Self::Upload(size) => Some(size),
            Self::Download(_) => None,
        }
    }
}

struct Record {
    id: u32,
    name: String,
    direction: Direction,
    running: AtomicBool,
    state: Mutex<State>,
}

impl Record {
    fn finished(&self) -> bool {
        self.state.try_lock().is_ok_and(|state| state.result.is_some())
    }

    fn complete(&self, state: &mut State, result: Result<(), Error>) -> Msg {
        self.log(state, result);
        let reply = match result {
            Ok(()) => Msg::Done { id: self.id, offset: state.size.unwrap() },
            Err(why) => Msg::Error { id: self.id, why },
        };
        state.file = None;
        state.result = Some(reply.clone());
        reply
    }

    fn log(&self, state: &State, result: Result<(), Error>) {
        let direction = if matches!(self.direction, Direction::Upload(_)) { "upload" } else { "download" };
        let (outcome, why) = match result {
            Ok(()) => ("done", ""),
            Err(Error::Cancelled) => ("cancelled", ""),
            Err(why) => ("failed", why.as_str()),
        };
        let path = state.path.as_ref().map_or_else(
            || state.initial.as_ref().map(|(_, name)| name.as_str()).unwrap_or("<invalid path>").into(),
            |path| path.to_string_lossy(),
        );
        let size = state.size.map_or_else(|| "unknown".into(), |size| size.to_string());
        let replace = state.replace;
        eprintln!("{} device={:?} direction={direction} path={path:?} size={size} outcome={outcome} why={why} replace={replace}", t("files.transfer"), self.name);
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap();
        if state.result.is_none() {
            let state = std::mem::take(state);
            self.log(&state, Err(Error::Cancelled));
        }
    }
}

enum OpenFile {
    Upload(Box<dyn Upload>),
    Download(Box<dyn Download>, Option<String>),
}

#[derive(Default)]
struct State {
    initial: Option<(Vec<String>, String)>,
    path: Option<PathBuf>,
    file: Option<OpenFile>,
    size: Option<u64>,
    replace: bool,
    staged: bool,
    result: Option<Msg>,
}

impl State {
    fn run(&mut self, id: u32, direction: Direction, files: FileTransfers, io: &Pipe) -> Result<(), Error> {
        let seated = || io.seated();
        if let Some((folder, name)) = &self.initial {
            self.path = Some((files.path)(folder, name, matches!(direction, Direction::Upload(_)), &seated)?);
            let path = self.path.as_deref().unwrap();
            self.file = Some(match direction {
                Direction::Upload(size) => OpenFile::Upload((files.upload)(path, size, &seated)?),
                Direction::Download(limit) => {
                    let file = (files.download)(path, &seated)?;
                    self.size = Some(file.size());
                    if limit.is_some_and(|limit| file.size() > limit) {
                        return Err(Error::Limit);
                    }
                    OpenFile::Download(file, None)
                }
            });
            self.initial = None;
        }
        let size = self.size.unwrap();
        match self.file.as_mut().unwrap() {
            OpenFile::Upload(file) => upload(id, size, file.as_mut(), &mut self.replace, &mut self.staged, io)?,
            OpenFile::Download(file, digest) => download(id, size, file.as_mut(), digest, io)?,
        }
        self.file = None;
        Ok(())
    }
}

fn approve(id: u32, file: &mut dyn Upload, replace: &mut bool, io: &Pipe) -> Result<(), Error> {
    let observed = file.conflict(&|| io.seated())?;
    if let Some(conflict) = &observed {
        io.text(Msg::Conflict { id, size: conflict.size, modified: conflict.modified,
            changed: replace.then_some(true) })?;
        if !matches!(io.receive()?, Packet::Text(Msg::Replace { .. })) {
            return Err(Error::Protocol);
        }
        file.accept_conflict(&|| io.seated())?;
        *replace = true;
    }
    Ok(())
}

fn upload(id: u32, size: u64, file: &mut dyn Upload, replace: &mut bool, staged: &mut bool, io: &Pipe) -> Result<(), Error> {
    let seated = || io.seated();
    if !*replace && !*staged {
        approve(id, file, replace, io)?;
    }
    let mut offset = file.prepare(&seated)?;
    *staged = true;
    io.text(Msg::Ready { id, size, offset })?;
    while offset < size {
        let Packet::Data(frame) = io.receive()? else { return Err(Error::Size) };
        let (_, at, bytes) = frames::frame(&frame).ok_or(Error::Protocol)?;
        if at != offset || bytes.len() != chunk(size - offset) {
            return Err(Error::Size);
        }
        file.write(bytes, &seated)?;
        offset += bytes.len() as u64;
        io.text(Msg::Ack { id, offset })?;
    }
    let Packet::Text(Msg::Finish { offset, sha256, .. }) = io.receive()? else { return Err(Error::Size) };
    if offset != size {
        return Err(Error::Size);
    }
    loop {
        file.verify(&sha256, &seated)?;
        if io.stopped.lock().unwrap().is_some() {
            return Err(Error::Cancelled);
        }
        match io.lease.while_seated(|| file.publish()).ok_or(Error::Cancelled)? {
            Err(Error::Exists) => approve(id, file, replace, io)?,
            Err(why) => return Err(why),
            Ok(replaced) => {
                *replace = replaced;
                return Ok(());
            }
        }
    }
}

fn download(id: u32, size: u64, file: &mut dyn Download, digest: &mut Option<String>, io: &Pipe) -> Result<(), Error> {
    io.text(Msg::Ready { id, size, offset: 0 })?;
    let offset = match io.receive()? {
        Packet::Text(Msg::Done { offset, .. }) if offset == size && digest.is_some() => return Ok(()),
        Packet::Text(Msg::Start { offset, .. }) => offset,
        _ => return Err(Error::Protocol),
    };
    if offset > size {
        return Err(Error::Protocol);
    }
    if offset < size || digest.is_none() {
        *digest = Some(read_download(id, size, offset, file, io)?);
    }
    io.text(Msg::Finish { id, offset: size, sha256: digest.as_ref().unwrap().clone() })?;
    match io.receive()? {
        Packet::Text(Msg::Done { offset, .. }) if offset == size => Ok(()),
        _ => Err(Error::Protocol),
    }
}

fn read_download(id: u32, size: u64, offset: u64, file: &mut dyn Download, io: &Pipe) -> Result<String, Error> {
    let seated = || io.seated();
    file.rewind(&seated)?;
    let mut hash = Sha256::new();
    let mut prefix = 0;
    let mut bytes = vec![0; chunk(offset)];
    while prefix < offset {
        bytes.resize(chunk(offset - prefix), 0);
        file.read(&mut bytes, &seated)?;
        hash.update(&bytes);
        prefix += bytes.len() as u64;
    }
    let mut sent = offset;
    let mut credit = Credit::default();
    while sent < size || credit.outstanding() != 0 {
        let bytes = chunk(size - sent);
        if credit.take(bytes, WINDOW) {
            let mut frame = frames::header(id, sent);
            frame.resize(frames::HEADER + bytes, 0);
            file.read(&mut frame[frames::HEADER..], &seated)?;
            hash.update(&frame[frames::HEADER..]);
            io.send(Packet::Data(frame))?;
            sent += bytes as u64;
        } else {
            let Packet::Text(Msg::Ack { offset, .. }) = io.receive()? else { return Err(Error::Protocol) };
            let from = sent - credit.outstanding() as u64;
            let bytes = offset.checked_sub(from).and_then(|n| usize::try_from(n).ok()).ok_or(Error::Protocol)?;
            if !credit.acknowledge(bytes) {
                return Err(Error::Protocol);
            }
        }
    }
    file.finish(&seated)?;
    Ok(wire::crypto::hex(&hash.finalize()))
}

fn chunk(left: u64) -> usize {
    left.min(CHUNK as u64) as usize
}

struct Pipe {
    incoming: Receiver<Packet>,
    outgoing: SyncSender<Packet>,
    stopped: Arc<Mutex<Option<Stop>>>,
    lease: Lease,
    wake: Sender<SessionEvent>,
    wait: Duration,
}

impl Pipe {
    fn seated(&self) -> bool {
        self.stopped.lock().unwrap().is_none() && self.lease.holds_seat()
    }

    fn receive(&self) -> Result<Packet, Error> {
        if !self.seated() {
            return Err(Error::Cancelled);
        }
        let packet = self.incoming.recv_timeout(self.wait).map_err(|_| Error::Cancelled)?;
        if !self.seated() {
            return Err(Error::Cancelled);
        }
        Ok(packet)
    }

    fn text(&self, msg: Msg) -> Result<(), Error> {
        self.send(Packet::Text(msg))
    }

    fn send(&self, packet: Packet) -> Result<(), Error> {
        self.outgoing.try_send(packet).map_err(|_| Error::Cancelled)?;
        let _ = self.wake.send(SessionEvent::TransferReady);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::terminal::registry::Registry as Terminals;
    use crate::workspace::Download;

    struct Source(u64, bool);

    impl Download for Source {
        fn size(&self) -> u64 {
            self.0
        }

        fn rewind(&mut self, _: &dyn Fn() -> bool) -> Result<(), Error> {
            assert!(self.1, "tried to rewind a finished download");
            Ok(())
        }

        fn read(&mut self, _: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
            if seated() {
                Ok(())
            } else {
                Err(Error::Cancelled)
            }
        }

        fn finish(&mut self, _: &dyn Fn() -> bool) -> Result<(), Error> {
            Ok(())
        }
    }

    fn pipe(lease: Lease, wait: Duration) -> (Pipe, SyncSender<Packet>, Receiver<Packet>) {
        let (input, incoming) = mpsc::sync_channel(SLOTS);
        let (outgoing, output) = mpsc::sync_channel(SLOTS);
        let (wake, _) = mpsc::channel();
        let pipe = Pipe { incoming, outgoing, stopped: Arc::new(Mutex::new(None)), lease, wake, wait };
        (pipe, input, output)
    }

    #[cfg(browser_workspace)]
    #[test]
    fn resuming_after_the_second_conflict_asks_again() {
        use crate::platform::WorkspaceState;
        use std::fs;

        for lock in [false, true] {
            let dir = std::env::temp_dir().join(format!("deskway-conflict-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
            fs::create_dir(&dir).unwrap();
            let dest = dir.join("file");
            fs::write(&dest, b"first").unwrap();
            let state = Arc::new(Mutex::new(WorkspaceState::READY));
            let access = {
                let state = state.clone();
                Arc::new(move || *state.lock().unwrap())
            };
            let browser = crate::workspace::local_browser(Arc::new(|_, _| unreachable!()), access.clone());
            let files = (browser.bind)(0).unwrap().transfers;
            let mut file = (files.upload)(&dest, 0, &|| true).unwrap();
            let (updates, _) = mpsc::channel();
            let registry = Terminals::guarded(Arc::new(|_, _, _| unreachable!()), access, updates);
            let mut replace = false;
            let mut staged = false;
            let wait = Duration::from_secs(5);
            let finish = || Packet::Text(Msg::Finish {
                id: 1, offset: 0, sha256: wire::crypto::hex(&wire::crypto::sha256(b"")),
            });
            let answer = || Packet::Text(Msg::Replace { id: 1 });
            let receive = |output: &Receiver<Packet>| output.recv_timeout(wait).unwrap();

            let lease = registry.admit("first".into(), "browser".into(), "device".into());
            let (io, input, output) = pipe(lease, wait);
            std::thread::scope(|scope| {
                let (file, replace, staged) = (file.as_mut(), &mut replace, &mut staged);
                let worker = scope.spawn(move || upload(1, 0, file, replace, staged, &io));
                assert!(matches!(receive(&output), Packet::Text(Msg::Conflict { size: 5, changed: None, .. })));
                input.send(answer()).ok().unwrap();
                assert!(matches!(receive(&output), Packet::Text(Msg::Ready { .. })));
                fs::write(&dest, b"second").unwrap();
                input.send(finish()).ok().unwrap();
                assert!(matches!(receive(&output), Packet::Text(Msg::Conflict { size: 6, changed: Some(true), .. })));
                if lock {
                    state.lock().unwrap().locked = true;
                    registry.end("first", "locked");
                }
                drop(input);
                assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
            });

            state.lock().unwrap().locked = false;
            let lease = registry.admit("second".into(), "browser".into(), "device".into());
            let (io, input, output) = pipe(lease, wait);
            std::thread::scope(|scope| {
                let (file, replace, staged) = (file.as_mut(), &mut replace, &mut staged);
                let worker = scope.spawn(move || upload(1, 0, file, replace, staged, &io));
                assert!(matches!(receive(&output), Packet::Text(Msg::Ready { .. })));
                input.send(finish()).ok().unwrap();
                assert!(matches!(receive(&output), Packet::Text(Msg::Conflict { size: 6, changed: Some(true), .. })));
                assert_eq!(fs::read(&dest).unwrap(), b"second");
                input.send(answer()).ok().unwrap();
                assert_eq!(worker.join().unwrap(), Ok(()));
            });
            drop(file);
            assert!(fs::read(dest).unwrap().is_empty());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn download_waits_for_credit_and_can_be_ended_while_waiting() {
        let (updates, _) = mpsc::channel();
        let registry = Terminals::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("desk".into(), "browser".into(), "device".into());
        for (cancel, offset) in [(false, 0), (true, 0), (false, CHUNK as u64), (false, 3 * CHUNK as u64)] {
            let (pipe, input, output) = pipe(lease.clone(), Duration::from_secs(2));
            let size = (WINDOW + CHUNK) as u64;
            let worker = std::thread::spawn(move || {
                download(1, size, &mut Source(size, true), &mut None, &pipe)
            });
            let read = || output.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(
                matches!(read(), Packet::Text(Msg::Ready { size: value, .. }) if value == size)
            );
            input
                .send(Packet::Text(Msg::Start { id: 1, offset }))
                .ok()
                .unwrap();
            let first_end = (offset + WINDOW as u64).min(size);
            for at in (offset..first_end).step_by(CHUNK) {
                let Packet::Data(bytes) = read() else { panic!("expected a frame") };
                let (id, offset, payload) = frames::frame(&bytes).unwrap();
                assert_eq!((id, offset, payload.len()), (1, at, CHUNK));
            }
            assert!(matches!(
                output.recv_timeout(Duration::from_millis(20)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            if cancel {
                drop(input);
                assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
                continue;
            }
            input.send(Packet::Text(Msg::Ack { id: 1, offset: first_end })).ok().unwrap();
            if first_end < size {
                let Packet::Data(bytes) = read() else { panic!("expected another frame") };
                assert_eq!(frames::frame(&bytes).unwrap().1, first_end);
                input.send(Packet::Text(Msg::Ack { id: 1, offset: size })).ok().unwrap();
            }
            let Packet::Text(Msg::Finish { offset, sha256, .. }) = read() else { panic!("expected a hash") };
            assert_eq!(offset, size);
            assert_eq!(sha256, wire::crypto::hex(&wire::crypto::sha256(&vec![0; size as usize])));
            input.send(Packet::Text(Msg::Done { id: 1, offset: size })).ok().unwrap();
            assert_eq!(worker.join().unwrap(), Ok(()));
        }
    }

    #[test]
    fn a_download_waiting_for_credit_interrupts_at_the_wait_limit() {
        let (updates, _) = mpsc::channel();
        let registry = Terminals::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("desk".into(), "browser".into(), "device".into());
        let (pipe, input, output) = pipe(lease, Duration::ZERO);
        input.try_send(Packet::Text(Msg::Start { id: 1, offset: 0 })).ok().unwrap();
        let size = (WINDOW + CHUNK) as u64;
        let mut digest = None;
        assert_eq!(download(1, size, &mut Source(size, true), &mut digest, &pipe), Err(Error::Cancelled));
        assert!(digest.is_none());
        assert_eq!(output.try_iter().filter(|packet| matches!(packet, Packet::Data(_))).count(), WINDOW / CHUNK);
    }

    #[test]
    fn closing_the_channel_interrupts_the_worker_even_with_pending_input() {
        let (updates, _) = mpsc::channel();
        let registry = Terminals::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("desk".into(), "browser".into(), "device".into());
        let (pipe, input, output) = pipe(lease.clone(), Duration::ZERO);
        input.try_send(Packet::Text(Msg::Start { id: 1, offset: 0 })).ok().unwrap();
        drop(Job {
            input: Some(input), output, stopped: pipe.stopped.clone(),
            id: 1, upload: false, receiving: false, received: 0, credit: Credit::default(),
        });
        assert!(lease.holds_seat());
        assert!(!pipe.seated());
        assert!(matches!(pipe.receive(), Err(Error::Cancelled)));
    }

    #[test]
    fn a_download_resumed_at_the_end_uses_its_cached_finish() {
        let (updates, _) = mpsc::channel();
        let registry = Terminals::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("desk".into(), "browser".into(), "device".into());
        let size = 1;
        for verified in [false, true] {
            let (pipe, input, output) = pipe(lease.clone(), Duration::ZERO);
            if !verified {
                input.try_send(Packet::Text(Msg::Start { id: 1, offset: size })).ok().unwrap();
            }
            input.try_send(Packet::Text(Msg::Done { id: 1, offset: size })).ok().unwrap();
            let mut digest = Some("original digest".into());
            assert_eq!(download(1, size, &mut Source(size, false), &mut digest, &pipe), Ok(()));
            assert!(matches!(output.try_recv(), Ok(Packet::Text(Msg::Ready { .. }))));
            if !verified {
                assert!(matches!(output.try_recv(), Ok(Packet::Text(Msg::Finish { offset, sha256, .. }))
                    if offset == size && sha256 == "original digest"));
            }
            assert!(output.try_recv().is_err());
        }
    }

    #[test]
    fn a_record_belongs_to_the_admitted_device_and_page_until_expiry() {
        let (updates, _) = mpsc::channel();
        let terminals = Terminals::new(|_, _| unreachable!(), updates);
        let (wake, _) = mpsc::channel();
        let files = FileTransfers {
            path: Arc::new(|_, _, _, _| unreachable!()),
            upload: Arc::new(|_, _, _| unreachable!()),
            download: Arc::new(|_, _| unreachable!()),
        };
        let until = Instant::now() + Duration::from_secs(60);
        for (device, owner, restart, expected) in [
            ("one", "page", false, true),
            ("one", "different-page", false, false),
            ("two", "page", false, false),
            ("one", "page", true, false),
        ] {
            let mut registry = Registry::default();
            let page = registry.admit("one", "page", "first", until);
            let lease = terminals.admit("first".into(), "browser".into(), "one".into());
            let mut transfer = Transfers::new(files.clone(), page.clone(), lease, wake.clone());
            assert!(transfer.offer(1, vec![], "file".into(), Direction::Upload(2)));
            registry.expire(until);
            assert!(page.lock().unwrap().record.is_some(), "an attached owner may still return");
            transfer.ended(Reason::Failed);
            registry.expire(until - Duration::from_secs(1));
            assert!(page.lock().unwrap().record.is_some());
            let next = if restart {
                Registry::default().admit(device, owner, "second", until)
            } else {
                registry.admit(device, owner, "second", until)
            };
            assert_eq!(next.lock().unwrap().record.is_some(), expected);
            if expected {
                next.lock().unwrap().attached = false;
                registry.expire(until);
                assert!(next.lock().unwrap().record.is_none());
            }
        }
    }

    #[test]
    fn leaving_or_expiring_drops_the_record_and_cancel_completes_it() {
        let (updates, _) = mpsc::channel();
        let terminals = Terminals::new(|_, _| unreachable!(), updates);
        let (wake, _) = mpsc::channel();
        let files = FileTransfers {
            path: Arc::new(|_, _, _, _| unreachable!()),
            upload: Arc::new(|_, _, _| unreachable!()),
            download: Arc::new(|_, _| unreachable!()),
        };
        for reason in [Some(Reason::Left), Some(Reason::Taken), Some(Reason::Failed), Some(Reason::Locked), None] {
            let mut registry = Registry::default();
            let deadline = Instant::now() + Duration::from_secs(60);
            let page = registry.admit("one", "page", "first", deadline);
            let lease = terminals.admit("first".into(), "browser".into(), "one".into());
            let mut transfers = Transfers::new(files.clone(), page.clone(), lease, wake.clone());
            assert!(transfers.offer(1, vec![], "file".into(), Direction::Upload(2)));
            let record = Arc::downgrade(page.lock().unwrap().record.as_ref().unwrap());
            if let Some(reason) = reason {
                transfers.ended(reason);
                if matches!(reason, Reason::Failed | Reason::Locked) {
                    assert!(record.upgrade().is_some());
                    registry.expire(deadline);
                }
                assert!(record.upgrade().is_none());
            } else {
                transfers.stop(Error::Cancelled);
                assert!(transfers.start());
                assert!(matches!(transfers.pending, Some(Packet::End(Msg::Error { why: Error::Cancelled, .. }))));
                assert!(record.upgrade().unwrap().finished());
            }
        }
    }
}
