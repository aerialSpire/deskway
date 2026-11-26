//! One file transfer per data session. The worker owns the open files; the session only moves
//! bounded messages and services terminal and browse before sending a transfer frame.

use std::path::{Path, PathBuf};
use std::sync::{
    mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use str0m::channel::ChannelId;
use wire::file_transfer::{self as frames, Error, Message as Msg, CHUNK, FRAME, WINDOW};

use super::{credit::Credit, data::WorkerSlot, terminal::registry::Lease, Cx, SessionEvent};
use crate::{i18n::t, workspace::FileTransfers};

const SLOTS: usize = WINDOW / CHUNK + 2;

pub(super) struct Transfers {
    files: FileTransfers,
    lease: Lease,
    wake: Sender<SessionEvent>,
    channel: Option<ChannelId>,
    last_id: u32,
    job: Option<Job>,
    pending: Option<Packet>,
}

struct Job {
    input: Option<SyncSender<Packet>>,
    output: Receiver<Packet>,
    stopped: Arc<Mutex<Option<Error>>>,
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

impl Job {
    fn stop(&mut self, why: Error) {
        if let Some(input) = self.input.take() {
            *self.stopped.lock().unwrap() = Some(why);
            drop(input);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop(Error::Cancelled);
    }
}

impl Transfers {
    pub fn new(files: FileTransfers, lease: Lease, wake: Sender<SessionEvent>) -> Self {
        Self {
            files,
            lease,
            wake,
            channel: None,
            last_id: 0,
            job: None,
            pending: None,
        }
    }

    pub fn channel(&self) -> Option<ChannelId> {
        self.channel
    }

    pub fn attach(&mut self, id: ChannelId) -> bool {
        if self.channel.is_some() {
            return false;
        }
        self.channel = Some(id);
        true
    }

    pub fn detach(&mut self) {
        self.channel = None;
        self.job = None;
        self.pending = None;
    }

    pub fn cancel(&mut self) {
        if let Some(job) = &mut self.job {
            job.stop(Error::Cancelled);
        }
    }

    pub fn protocol_error(&mut self) {
        if let Some(job) = &mut self.job {
            job.stop(Error::Protocol);
        }
        self.detach();
    }

    pub fn receive(&mut self, data: Vec<u8>, binary: bool) -> bool {
        if data.len() > FRAME {
            return false;
        }
        if binary {
            let Some((id, offset, payload)) = frames::frame(&data) else {
                return false;
            };
            if id < self.last_id || (id == self.last_id && self.job.is_none()) {
                return true;
            }
            let Some(job) = &mut self.job else {
                return false;
            };
            let Some(input) = &job.input else { return true };
            if id != self.last_id
                || !job.receiving
                || offset != job.received
                || !job.credit.take(payload.len(), WINDOW)
            {
                return false;
            }
            job.received += payload.len() as u64;
            return enqueue(input, Packet::Data(data));
        }
        let Ok(message) = serde_json::from_slice::<Msg>(&data) else {
            return false;
        };
        let id = message.id();
        match message {
            Msg::Upload {
                path, name, size, ..
            } => self.start(id, path, name, Direction::Upload(size)),
            Msg::Download {
                path, entry, limit, ..
            } => self.start(id, path, entry, Direction::Download(limit)),
            _ if id < self.last_id || (id == self.last_id && self.job.is_none()) => true,
            _ if id != self.last_id => false,
            Msg::Cancel { .. } => {
                self.cancel();
                true
            }
            Msg::Error {
                why: why @ (Error::Io | Error::Protocol),
                ..
            } => {
                if let Some(job) = &mut self.job {
                    job.stop(why);
                }
                true
            }
            Msg::Ack { .. } | Msg::Start { .. } | Msg::Finish { .. } | Msg::Done { .. } => self
                .job
                .as_ref()
                .and_then(|job| job.input.as_ref())
                .is_none_or(|input| enqueue(input, Packet::Text(message))),
            _ => false,
        }
    }

    fn start(&mut self, id: u32, path: Vec<String>, name: String, direction: Direction) -> bool {
        if id <= self.last_id || self.job.is_some() || self.pending.is_some() {
            return false;
        }
        self.last_id = id;
        let upload = matches!(direction, Direction::Upload(_));
        let worker = Request {
            id,
            direction,
            path: (self.files.path)(&path, &name, upload),
        };
        let Some(slot) = WorkerSlot::for_files() else {
            worker.log(&self.lease, worker.size(), Err(Error::Busy));
            self.pending = Some(Packet::End(Msg::Error {
                id,
                why: Error::Busy,
            }));
            return true;
        };
        let (input, incoming) = mpsc::sync_channel(SLOTS);
        let (outgoing, output) = mpsc::sync_channel(SLOTS);
        let stopped = Arc::new(Mutex::new(None));
        let files = self.files;
        let lease = self.lease.clone();
        let wake = self.wake.clone();
        let stop = stopped.clone();
        let fallback = worker
            .path
            .clone()
            .unwrap_or_else(|_| PathBuf::from("<invalid path>"));
        let size = worker.size();
        let thread = std::thread::Builder::new()
            .name("file-transfer".into())
            .spawn(move || {
                let io = Pipe {
                    incoming,
                    outgoing,
                    stopped: stop,
                    lease,
                    wake,
                };
                let (size, result) = worker.run(files, &io);
                let result = if result == Err(Error::Cancelled) {
                    Err(io.stopped.lock().unwrap().unwrap_or(Error::Cancelled))
                } else {
                    result
                };
                worker.log(&io.lease, size, result);
                let reply = match result {
                    Ok(offset) => Msg::Done { id, offset },
                    Err(why) => Msg::Error { id, why },
                };
                drop(slot);
                let _ = io.send(Packet::End(reply));
            });
        if thread.is_ok() {
            self.job = Some(Job {
                input: Some(input),
                output,
                stopped,
                upload,
                receiving: false,
                received: 0,
                credit: Credit::default(),
            });
        } else {
            log(&self.lease, direction, &fallback, size, Err(Error::Io));
            self.pending = Some(Packet::End(Msg::Error { id, why: Error::Io }));
        }
        true
    }

    pub fn poll(&mut self, cx: &mut Cx) -> Option<Instant> {
        let id = self.channel?;
        let mut ch = cx.rtc.channel(id)?;
        if self.pending.is_none() {
            self.pending = self.job.as_ref().and_then(|job| job.output.try_recv().ok());
        }
        let packet = self.pending.as_ref()?;
        if ch.buffered_amount() >= FRAME {
            return Some(Instant::now() + Duration::from_millis(10));
        }
        let sent = match packet {
            Packet::Data(data) => ch.write(true, data),
            Packet::Text(msg) | Packet::End(msg) => {
                ch.write(false, &serde_json::to_vec(msg).expect("transfer message"))
            }
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
            Packet::Text(Msg::Ready { .. }) => {
                let job = self.job.as_mut().unwrap();
                job.receiving = job.upload;
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

fn enqueue(input: &SyncSender<Packet>, packet: Packet) -> bool {
    !matches!(input.try_send(packet), Err(TrySendError::Full(_)))
}

#[derive(Clone, Copy)]
enum Direction {
    Upload(u64),
    Download(Option<u64>),
}

struct Request {
    id: u32,
    direction: Direction,
    path: Result<PathBuf, Error>,
}

impl Request {
    fn size(&self) -> Option<u64> {
        match self.direction {
            Direction::Upload(size) => Some(size),
            Direction::Download(_) => None,
        }
    }

    fn log(&self, lease: &Lease, size: Option<u64>, result: Result<u64, Error>) {
        let path = self.path.as_deref().unwrap_or(Path::new("<invalid path>"));
        log(lease, self.direction, path, size, result);
    }

    fn run(&self, files: FileTransfers, io: &Pipe) -> (Option<u64>, Result<u64, Error>) {
        let path = match &self.path {
            Ok(path) => path,
            Err(why) => return (self.size(), Err(*why)),
        };
        match self.direction {
            Direction::Upload(size) => (Some(size), self.upload(path, size, files, io)),
            Direction::Download(limit) => {
                let file = match (files.download)(path, &|| io.seated()) {
                    Ok(file) => file,
                    Err(why) => return (None, Err(why)),
                };
                let size = file.size();
                (Some(size), self.download(file, limit, io))
            }
        }
    }

    fn upload(
        &self,
        path: &Path,
        size: u64,
        files: FileTransfers,
        io: &Pipe,
    ) -> Result<u64, Error> {
        let id = self.id;
        let seated = || io.seated();
        let mut file = (files.upload)(path, size, &seated)?;
        io.text(Msg::Ready { id, size })?;
        let mut offset = 0;
        while offset < size {
            let Packet::Data(frame) = io.receive()? else {
                return Err(Error::Size);
            };
            let (_, at, bytes) = frames::frame(&frame).ok_or(Error::Protocol)?;
            if at != offset || bytes.len() != chunk(size - offset) {
                return Err(Error::Size);
            }
            file.write(bytes, &seated)?;
            offset += bytes.len() as u64;
            io.text(Msg::Ack { id, offset })?;
        }
        match io.receive()? {
            Packet::Text(Msg::Finish { offset, .. }) if offset == size => file.commit(&seated)?,
            _ => return Err(Error::Size),
        }
        Ok(size)
    }

    fn download(
        &self,
        mut file: Box<dyn crate::workspace::Download>,
        limit: Option<u64>,
        io: &Pipe,
    ) -> Result<u64, Error> {
        let id = self.id;
        let size = file.size();
        if limit.is_some_and(|limit| size > limit) {
            return Err(Error::Limit);
        }
        io.text(Msg::Ready { id, size })?;
        if !matches!(io.receive()?, Packet::Text(Msg::Start { offset: 0, .. })) {
            return Err(Error::Protocol);
        }
        let mut credit = Credit::default();
        let mut sent = 0;
        while sent < size || credit.outstanding() != 0 {
            let bytes = chunk(size - sent);
            if credit.take(bytes, WINDOW) {
                let mut frame = frames::header(id, sent);
                frame.resize(frames::HEADER + bytes, 0);
                file.read(&mut frame[frames::HEADER..], &|| io.seated())?;
                io.send(Packet::Data(frame))?;
                sent += bytes as u64;
            } else {
                let Packet::Text(Msg::Ack { offset, .. }) = io.receive()? else {
                    return Err(Error::Protocol);
                };
                let from = sent - credit.outstanding() as u64;
                let bytes = offset
                    .checked_sub(from)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or(Error::Protocol)?;
                if !credit.acknowledge(bytes) {
                    return Err(Error::Protocol);
                }
            }
        }
        file.verify(&|| io.seated())?;
        io.text(Msg::Finish { id, offset: size })?;
        match io.receive()? {
            Packet::Text(Msg::Done { offset, .. }) if offset == size => Ok(size),
            _ => Err(Error::Protocol),
        }
    }
}

fn chunk(left: u64) -> usize {
    left.min(CHUNK as u64) as usize
}

struct Pipe {
    incoming: Receiver<Packet>,
    outgoing: SyncSender<Packet>,
    stopped: Arc<Mutex<Option<Error>>>,
    lease: Lease,
    wake: Sender<SessionEvent>,
}

impl Pipe {
    fn seated(&self) -> bool {
        self.stopped.lock().unwrap().is_none() && self.lease.holds_seat()
    }

    fn receive(&self) -> Result<Packet, Error> {
        if !self.seated() {
            return Err(Error::Cancelled);
        }
        self.incoming.recv().map_err(|_| Error::Cancelled)
    }

    fn text(&self, msg: Msg) -> Result<(), Error> {
        self.send(Packet::Text(msg))
    }

    fn send(&self, packet: Packet) -> Result<(), Error> {
        self.outgoing.send(packet).map_err(|_| Error::Cancelled)?;
        let _ = self.wake.send(SessionEvent::TransferReady);
        Ok(())
    }
}

fn log(
    lease: &Lease,
    direction: Direction,
    path: &Path,
    size: Option<u64>,
    result: Result<u64, Error>,
) {
    let direction = match direction {
        Direction::Upload(_) => "upload",
        Direction::Download(_) => "download",
    };
    let outcome = match result {
        Ok(_) => "done",
        Err(Error::Cancelled) => "cancelled",
        Err(_) => "failed",
    };
    let size = size.map_or_else(|| "unknown".into(), |size| size.to_string());
    let why = result
        .err()
        .filter(|why| *why != Error::Cancelled)
        .map_or("", |why| why.as_str());
    eprintln!("{} device={:?} direction={direction} path={path:?} size={size} outcome={outcome} why={why}", t("files.transfer"), lease.name());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::terminal::registry::Registry;
    use crate::workspace::Download;

    struct Source(u64);

    impl Download for Source {
        fn size(&self) -> u64 {
            self.0
        }

        fn read(&mut self, _: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
            if seated() {
                Ok(())
            } else {
                Err(Error::Cancelled)
            }
        }

        fn verify(&self, _: &dyn Fn() -> bool) -> Result<(), Error> {
            Ok(())
        }
    }

    #[test]
    fn download_waits_for_credit_and_can_be_ended_while_waiting() {
        let (updates, _) = mpsc::channel();
        let registry = Registry::new(|_, _| unreachable!(), updates);
        let lease = registry.admit("desk".into(), "browser".into(), "device".into());
        for cancel in [false, true] {
            let (input, incoming) = mpsc::sync_channel(SLOTS);
            let (outgoing, output) = mpsc::sync_channel(SLOTS);
            let (wake, _events) = mpsc::channel();
            let stopped = Arc::new(Mutex::new(None));
            let pipe = Pipe {
                incoming,
                outgoing,
                stopped,
                lease: lease.clone(),
                wake,
            };
            let size = (WINDOW + CHUNK) as u64;
            let worker = std::thread::spawn(move || {
                let request = Request {
                    id: 1,
                    direction: Direction::Download(None),
                    path: Ok(PathBuf::from("/file")),
                };
                request.download(Box::new(Source(size)), None, &pipe)
            });
            let read = || output.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(
                matches!(read(), Packet::Text(Msg::Ready { size: value, .. }) if value == size)
            );
            input
                .send(Packet::Text(Msg::Start { id: 1, offset: 0 }))
                .ok()
                .unwrap();
            for at in (0..WINDOW).step_by(CHUNK) {
                let Packet::Data(bytes) = read() else {
                    panic!("expected a frame")
                };
                let (id, offset, payload) = frames::frame(&bytes).unwrap();
                assert_eq!((id, offset, payload.len()), (1, at as u64, CHUNK));
            }
            assert!(matches!(
                output.recv_timeout(Duration::from_millis(20)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            if cancel {
                drop(input);
                assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
            } else {
                input
                    .send(Packet::Text(Msg::Ack {
                        id: 1,
                        offset: WINDOW as u64,
                    }))
                    .ok()
                    .unwrap();
                let Packet::Data(bytes) = read() else {
                    panic!("expected another frame")
                };
                assert_eq!(frames::frame(&bytes).unwrap().1, WINDOW as u64);
                input
                    .send(Packet::Text(Msg::Ack {
                        id: 1,
                        offset: size,
                    }))
                    .ok()
                    .unwrap();
                assert!(
                    matches!(read(), Packet::Text(Msg::Finish { offset, .. }) if offset == size)
                );
                input
                    .send(Packet::Text(Msg::Done {
                        id: 1,
                        offset: size,
                    }))
                    .ok()
                    .unwrap();
                assert_eq!(worker.join().unwrap(), Ok(size));
            }
        }
    }
}
