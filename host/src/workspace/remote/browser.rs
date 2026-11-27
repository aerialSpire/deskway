
//! Browser workspace requests use the user worker's existing pipe. Each step carries at
//! most one channel frame and awaits its reply before sending more.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc::RecvTimeoutError, Arc, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use wire::browse::{Browse, BrowseError};
use wire::file_transfer::{Error, CHUNK};
use wire::terminal::FRAME;

use super::{Link, Remote, RequestOp};
use crate::platform::{Pty, WorkspaceState};
use crate::platform::workspace::{WorkspaceActivity, WorkspaceUse};
use crate::workspace::{Answer, Browser, BrowserUser, Code, Conflict, Download, FileTransfers, Upload, WorkError};

const PUBLISH_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(tag = "browser", rename_all = "snake_case")]
pub(super) enum Operation {
    Browse { request: Browse },
    Path { folder: Vec<String>, name: String, upload: bool },
    OpenUpload { path: Vec<u16>, size: u64 },
    OpenDownload { path: Vec<u16> },
    OpenPty { rows: u16, cols: u16 },
    OpenLease,
    Step { resource: u64, step: Step },
    Close { resource: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Step {
    Conflict,
    AcceptConflict,
    Prepare,
    Write(String),
    Verify(String),
    Publish,
    Read(usize),
    Rewind,
    Finish,
    PtyRead { len: usize, detached: bool },
    PtyWrite(String),
    Resize(u16, u16),
    Redraw(u16, u16),
    Exit,
    Terminate,
}

#[derive(Serialize, Deserialize)]
enum PtyError {
    Wait,
    Denied,
    Io,
}

impl From<io::Error> for PtyError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Self::Wait,
            io::ErrorKind::PermissionDenied => Self::Denied,
            _ => Self::Io,
        }
    }
}

impl From<PtyError> for io::Error {
    fn from(error: PtyError) -> Self {
        match error {
            PtyError::Wait => io::ErrorKind::WouldBlock,
            PtyError::Denied => io::ErrorKind::PermissionDenied,
            PtyError::Io => io::ErrorKind::Other,
        }.into()
    }
}

fn value(value: impl Serialize) -> Answer {
    Ok(serde_json::to_value(value).expect("browser workspace reply serializes"))
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}

fn path(wide: &[u16]) -> PathBuf {
    OsString::from_wide(wide).into()
}

impl Remote {
    pub fn browser(self: &Arc<Self>) -> Browser {
        let state = self.clone();
        let bound = self.clone();
        Browser {
            state: Arc::new(move || state.worker.lock().unwrap().as_ref()
                .map_or(WorkspaceState::NO_USER, |link| *link.state.lock().unwrap())),
            bind: Arc::new(move |user| {
                let link = bound.worker.lock().unwrap().clone()?;
                let current = *link.state.lock().unwrap();
                if current.user != Some(user) || current.refusal(false).is_some() { return None; }
                link.browser_user()
            }),
        }
    }
}

impl Link {
    fn browser_user(self: Arc<Self>) -> Option<BrowserUser> {
        let id = self.browser_request::<Result<u64, Error>>(Operation::OpenLease, None).ok()?.ok()?;
        let lease = Arc::new(ResourceLink { link: self.clone(), id });
        let (browse, resolve, upload, download) = (self.clone(), self.clone(), self.clone(), self.clone());
        Some(BrowserUser {
            open: Arc::new(move |rows, cols| {
                let id = self.browser_request::<Result<u64, PtyError>>(Operation::OpenPty { rows, cols }, None)
                    .map_err(|_| io::ErrorKind::BrokenPipe)??;
                Ok(Box::new(ResourceLink { link: self.clone(), id }))
            }),
            browse: Arc::new(move |request, seated| {
                if seated() {
                    if let Ok((name, reply)) = browse.browser_request::<(Vec<u16>, Browse)>(Operation::Browse { request }, None) {
                        if seated() { return (path(&name), reply); }
                    }
                }
                (PathBuf::new(), Browse::Error { why: BrowseError::Denied })
            }),
            transfers: FileTransfers {
                path: Arc::new(move |folder, name, upload, seated| {
                    resolve.files(Operation::Path { folder: folder.to_vec(), name: name.into(), upload }, seated)
                        .map(|name: Vec<u16>| path(&name))
                }),
                upload: Arc::new(move |path, size, seated| {
                    let id = upload.files(Operation::OpenUpload { path: wide(path), size }, seated)?;
                    Ok(Box::new(ResourceLink { link: upload.clone(), id }))
                }),
                download: Arc::new(move |path, seated| {
                    let (id, size) = download.files(Operation::OpenDownload { path: wide(path) }, seated)?;
                    Ok(Box::new(Source { resource: ResourceLink { link: download.clone(), id }, size }))
                }),
            },
            _lease: Some(lease),
        })
    }

    fn browser_request<T: DeserializeOwned>(&self, operation: Operation, timeout: Option<Duration>) -> Result<T, Error> {
        let answer = self.request(RequestOp::Browser(operation), timeout).map_err(|err| match err {
            RecvTimeoutError::Timeout => Error::Unconfirmed,
            RecvTimeoutError::Disconnected => Error::Cancelled,
        })?;
        serde_json::from_value(answer.map_err(|_| Error::Cancelled)?).map_err(|_| Error::Cancelled)
    }

    fn files<T: DeserializeOwned>(&self, operation: Operation, seated: &dyn Fn() -> bool) -> Result<T, Error> {
        if !seated() { return Err(Error::Cancelled); }
        self.browser_request(operation, None)?
    }
}

struct ResourceLink {
    link: Arc<Link>,
    id: u64,
}

impl ResourceLink {
    fn files<T: DeserializeOwned>(&self, step: Step, seated: &dyn Fn() -> bool) -> Result<T, Error> {
        self.link.files(Operation::Step { resource: self.id, step }, seated)
    }

    fn read_output(&self, buf: &mut [u8], detached: bool) -> io::Result<usize> {
        let data: String = self.pty(Step::PtyRead { len: buf.len(), detached })?;
        STANDARD.decode_slice(&data, buf).map_err(io::Error::other)
    }

    fn pty<T: DeserializeOwned>(&self, step: Step) -> io::Result<T> {
        let result: Result<T, PtyError> = self.link.browser_request(Operation::Step { resource: self.id, step }, None)
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        result.map_err(io::Error::from)
    }
}

impl Drop for ResourceLink {
    fn drop(&mut self) {
        let _ = self.link.send(RequestOp::Browser(Operation::Close { resource: self.id }), None);
    }
}

impl Upload for ResourceLink {
    fn conflict(&mut self, seated: &dyn Fn() -> bool) -> Result<Option<Conflict>, Error> {
        self.files(Step::Conflict, seated)
    }

    fn accept_conflict(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.files(Step::AcceptConflict, seated)
    }

    fn prepare(&mut self, seated: &dyn Fn() -> bool) -> Result<u64, Error> {
        self.files(Step::Prepare, seated)
    }

    fn write(&mut self, bytes: &[u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.files(Step::Write(STANDARD.encode(bytes)), seated)
    }

    fn verify(&mut self, digest: &str, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.files(Step::Verify(digest.into()), seated)
    }

    fn publish(&mut self) -> Result<bool, Error> {
        self.link.browser_request(Operation::Step { resource: self.id, step: Step::Publish }, Some(PUBLISH_REPLY_TIMEOUT))?
    }
}

struct Source {
    resource: ResourceLink,
    size: u64,
}

impl Download for Source {
    fn size(&self) -> u64 { self.size }

    fn rewind(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.resource.files(Step::Rewind, seated)
    }

    fn read(&mut self, bytes: &mut [u8], seated: &dyn Fn() -> bool) -> Result<(), Error> {
        let data: String = self.resource.files(Step::Read(bytes.len()), seated)?;
        let count = STANDARD.decode_slice(&data, bytes).map_err(|_| Error::Io)?;
        if count == bytes.len() { Ok(()) } else { Err(Error::Io) }
    }

    fn finish(&mut self, seated: &dyn Fn() -> bool) -> Result<(), Error> {
        self.resource.files(Step::Finish, seated)
    }
}

impl Read for ResourceLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_output(buf, false)
    }
}

impl Write for ResourceLink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pty(Step::PtyWrite(STANDARD.encode(&bytes[..bytes.len().min(FRAME)])))
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl Pty for ResourceLink {
    fn drain(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.read_output(buf, true) }
    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> { self.pty(Step::Resize(rows, cols)) }
    fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> { self.pty(Step::Redraw(rows, cols)) }
    fn exit(&mut self) -> io::Result<Option<i32>> { self.pty(Step::Exit) }
    fn terminate(&mut self) -> io::Result<bool> { self.pty(Step::Terminate) }
}

enum Resource {
    Lease,
    Upload(Box<dyn Upload>),
    Download(Box<dyn Download>),
    Pty(Box<dyn Pty>),
}

struct OpenResource {
    resource: Arc<Mutex<Resource>>,
    _used: WorkspaceUse,
}

pub(super) struct Worker {
    browser: Browser,
    next: AtomicU64,
    alive: AtomicBool,
    resources: Mutex<HashMap<u64, OpenResource>>,
    activity: Arc<WorkspaceActivity>,
}

impl Worker {
    pub fn new(browser: Browser, activity: Arc<WorkspaceActivity>) -> Self {
        Self {
            browser, next: AtomicU64::new(0),
            alive: AtomicBool::new(true), resources: Mutex::default(), activity,
        }
    }

    pub fn disconnect(&self) {
        self.alive.store(false, Ordering::Release);
        self.resources.lock().unwrap().clear();
    }

    fn insert(&self, resource: Resource) -> Result<u64, Error> {
        let mut resources = self.resources.lock().unwrap();
        if !self.alive.load(Ordering::Acquire) { return Err(Error::Cancelled); }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        resources.insert(id, OpenResource { resource: Arc::new(Mutex::new(resource)), _used: self.activity.begin() });
        Ok(id)
    }

    pub fn call(&self, op: Operation) -> Answer {
        let user = (self.browser.state)().user;
        let allowed = || {
            let current = (self.browser.state)();
            self.alive.load(Ordering::Acquire) && user == current.user && current.refusal(false).is_none()
        };
        if let Operation::Step { resource, step } = op {
            let resource = self.resources.lock().unwrap().get(&resource)
                .map(|open| open.resource.clone()).ok_or_else(invalid)?;
            return resource.lock().unwrap().step(step, &allowed);
        }
        if let Operation::Close { resource } = op {
            let old = self.resources.lock().unwrap().remove(&resource);
            drop(old);
            return value(());
        }
        let Some(browser) = user.and_then(|user| (self.browser.bind)(user)) else { return Err(invalid()); };
        match op {
            Operation::OpenLease => value(if allowed() { self.insert(Resource::Lease) } else { Err(Error::Cancelled) }),
            Operation::OpenPty { rows, cols } => value(if allowed() {
                (browser.open)(rows, cols).map_err(PtyError::from).and_then(|pty| self.insert(Resource::Pty(pty)).map_err(|_| PtyError::Io))
            } else { Err(PtyError::Denied) }),
            Operation::Browse { request } => {
                let (name, reply) = (browser.browse)(request, &allowed);
                value((wide(&name), reply))
            }
            Operation::Path { folder, name, upload } => {
                let result = (browser.transfers.path)(&folder, &name, upload, &allowed);
                value(result.as_deref().map(wide))
            }
            Operation::OpenUpload { path: name, size } => value((browser.transfers.upload)(&path(&name), size, &allowed)
                .and_then(|file| self.insert(Resource::Upload(file)))),
            Operation::OpenDownload { path: name } => value((browser.transfers.download)(&path(&name), &allowed)
                .and_then(|file| {
                    let size = file.size();
                    self.insert(Resource::Download(file)).map(|id| (id, size))
                })),
            Operation::Step { .. } | Operation::Close { .. } => unreachable!(),
        }
    }
}

fn invalid() -> WorkError {
    WorkError::new(Code::Denied, "browser workspace unavailable", "")
}

impl Resource {
    fn step(&mut self, step: Step, allowed: &dyn Fn() -> bool) -> Answer {
        match (self, step) {
            (Self::Pty(pty), Step::Exit) => value(pty.exit().map_err(PtyError::from)),
            (Self::Pty(pty), Step::PtyRead { len, detached }) if len <= FRAME => {
                let mut bytes = vec![0; len];
                let read = if detached { pty.drain(&mut bytes) }
                    else if allowed() { pty.read(&mut bytes) }
                    else { Err(io::ErrorKind::WouldBlock.into()) };
                value(read.map(|n| STANDARD.encode(&bytes[..n])).map_err(PtyError::from))
            }
            (Self::Pty(pty), step) => {
                if !allowed() {
                    return match step {
                        Step::Resize(..) | Step::Redraw(..) => value(Ok::<(), PtyError>(())),
                        Step::Terminate => value(Ok::<bool, PtyError>(false)),
                        Step::PtyWrite(_) => value(Err::<usize, _>(PtyError::Wait)),
                        _ => Err(invalid()),
                    };
                }
                match step {
                    Step::Resize(rows, cols) => value(pty.resize(rows, cols).map_err(PtyError::from)),
                    Step::Redraw(rows, cols) => value(pty.redraw(rows, cols).map_err(PtyError::from)),
                    Step::Terminate => value(pty.terminate().map_err(PtyError::from)),
                    Step::PtyWrite(text) => {
                        let bytes = STANDARD.decode(text).map_err(|_| invalid())?;
                        if bytes.len() > FRAME { return Err(invalid()); }
                        value(pty.write(&bytes).map_err(PtyError::from))
                    }
                    _ => Err(invalid()),
                }
            }
            (Self::Upload(file), Step::Conflict) => value(file.conflict(allowed)),
            (Self::Upload(file), Step::AcceptConflict) => value(file.accept_conflict(allowed)),
            (Self::Upload(file), Step::Prepare) => value(file.prepare(allowed)),
            (Self::Upload(file), Step::Write(text)) => {
                let bytes = STANDARD.decode(text).map_err(|_| invalid())?;
                if bytes.len() > CHUNK { return Err(invalid()); }
                value(file.write(&bytes, allowed))
            }
            (Self::Upload(file), Step::Verify(digest)) => value(file.verify(&digest, allowed)),
            (Self::Upload(file), Step::Publish) => value(if allowed() { file.publish() } else { Err(Error::Cancelled) }),
            (Self::Download(file), Step::Rewind) => value(file.rewind(allowed)),
            (Self::Download(file), Step::Read(len)) if len <= CHUNK => {
                let mut bytes = vec![0; len];
                value(file.read(&mut bytes, allowed).map(|()| STANDARD.encode(bytes)))
            }
            (Self::Download(file), Step::Finish) => value(file.finish(allowed)),
            _ => Err(invalid()),
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ipc::{self, Pipe};
    use std::fs;
    use std::sync::mpsc::channel;

    #[test]
    fn browser_requests_keep_resources_on_the_worker_and_enforce_its_lock() {
        let paths = format!(r"\\.\pipe\deskway-test-browser-ws-{}", std::process::id());
        let server = Arc::new(Pipe::create(&paths, &format!("D:P(A;;GA;;;{})", ipc::own_sid().unwrap())).unwrap());
        let client = {
            let opened = std::thread::spawn(move || Pipe::connect(&paths).unwrap());
            server.accept().unwrap();
            Arc::new(opened.join().unwrap())
        };
        let state = Arc::new(Mutex::new(WorkspaceState::READY));
        let current = state.clone();
        let browser = crate::workspace::local_browser(
            Arc::new(|_, _| Ok(Box::new(EchoPty(Vec::new())))),
            Arc::new(move || *current.lock().unwrap()),
        );
        client.send_json(&super::super::Reply::Workspace(WorkspaceState::READY)).unwrap();
        let activity = Arc::new(WorkspaceActivity::default());
        let (used, usage) = channel();
        assert!(!activity.watch(move |active| { let _ = used.send(active); }));
        let worker = Arc::new(Worker::new(browser, activity));
        let served = {
            let pipe = client.clone();
            let worker = worker.clone();
            std::thread::spawn(move || super::super::serve_requests(
                pipe, Arc::new(crate::workspace::Local::default()),
                // Observe resource activity on its own, without counting the requests.
                Some(worker), None,
            ))
        };
        let (updates, notices) = channel();
        let remote = Arc::new(Remote::user(updates));
        let attached = {
            let remote = remote.clone();
            let pipe = server.clone();
            std::thread::spawn(move || remote.attach(pipe))
        };
        notices.recv_timeout(Duration::from_secs(5)).unwrap();
        let browser = remote.browser();
        assert!((browser.bind)(1).is_none());
        let user = (browser.bind)(0).unwrap();
        assert!(usage.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(matches!(*worker.resources.lock().unwrap()[&0].resource.lock().unwrap(), Resource::Lease));
        drop(user);
        assert!(!usage.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(worker.resources.lock().unwrap().is_empty());
        let user = (browser.bind)(0).unwrap();
        let mut pty = (user.open)(24, 80).unwrap();
        assert_eq!(pty.write(b"pty bytes").unwrap(), 9);
        let mut bytes = [0; 9];
        pty.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"pty bytes");

        let dir = std::env::temp_dir().join(format!("deskway-test-ipc-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        fs::create_dir(&dir).unwrap();
        let path = dir.canonicalize().unwrap().join("example.txt");
        let chunks = [vec![1; CHUNK], vec![2; CHUNK], vec![3; 3]];
        let length = chunks.iter().map(Vec::len).sum::<usize>();
        let mut output = (user.transfers.upload)(&path, length as u64, &|| true).unwrap();
        assert_eq!(output.prepare(&|| true), Ok(0));
        output.write(&chunks[0], &|| true).unwrap();
        let tokens = crate::platform::file_metadata::path_parts(path.parent().unwrap()).iter()
            .map(|part| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(part.to_str().unwrap()))
            .collect::<Vec<_>>();
        let (_, Browse::Listing { entries, .. }) = (user.browse)(
            Browse::List { path: Some(tokens.clone()), cursor: None }, &|| true,
        ) else { panic!("the user worker should list its partial file"); };
        assert_eq!(entries.len(), 1);
        let codec = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let alias = String::from_utf8(codec.decode(&entries[0].name).unwrap()).unwrap().to_ascii_uppercase();
        let (_, reply) = (user.browse)(Browse::Delete { path: tokens, entry: codec.encode(alias) }, &|| true);
        assert_eq!(reply, Browse::Error { why: BrowseError::InUse });
        pty.write_all(b"background").unwrap();
        state.lock().unwrap().locked = true;
        assert_eq!((browser.state)(), WorkspaceState::READY, "the network side has not received the lock");
        let mut bytes = [0; 10];
        assert_eq!(pty.read(&mut bytes).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(pty.drain(&mut bytes).unwrap(), bytes.len());
        assert_eq!(&bytes, b"background");
        assert_eq!(output.write(&chunks[1], &|| true), Err(Error::Cancelled));
        assert_eq!(pty.write(b"unavailable").unwrap_err().kind(), io::ErrorKind::WouldBlock);
        state.lock().unwrap().locked = false;
        assert_eq!(output.prepare(&|| true), Ok(CHUNK as u64));
        for bytes in &chunks[1..] {
            output.write(bytes, &|| true).unwrap();
        }
        output.verify(&wire::crypto::hex(&wire::crypto::sha256(&chunks.concat())), &|| true).unwrap();
        assert_eq!(output.publish(), Ok(false));
        drop(output);
        let mut input = (user.transfers.download)(&path, &|| true).unwrap();
        assert_eq!(input.size(), length as u64);
        for expected in chunks {
            let mut bytes = vec![0; expected.len()];
            input.read(&mut bytes, &|| true).unwrap();
            assert_eq!(bytes, expected);
        }
        input.finish(&|| true).unwrap();
        drop(input);
        let bytes = b"replacement";
        let mut replacement = (user.transfers.upload)(&path, bytes.len() as u64, &|| true).unwrap();
        assert_eq!(replacement.conflict(&|| true).unwrap().unwrap().size, length as u64);
        replacement.accept_conflict(&|| true).unwrap();
        assert_eq!(replacement.prepare(&|| true), Ok(0));
        replacement.write(bytes, &|| true).unwrap();
        replacement.verify(&wire::crypto::hex(&wire::crypto::sha256(bytes)), &|| true).unwrap();
        assert_eq!(replacement.publish(), Ok(true));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        drop(replacement);
        server.disconnect();
        assert!(attached.join().unwrap().is_err());
        assert!(served.join().unwrap().is_err());
        assert!((browser.bind)(0).is_none());
        assert_eq!(pty.write(b"ended").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        fs::remove_dir_all(dir).unwrap();
    }

    struct EchoPty(Vec<u8>);

    impl Read for EchoPty {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = bytes.len().min(self.0.len());
            bytes[..count].copy_from_slice(&self.0[..count]);
            self.0.drain(..count);
            Ok(count)
        }
    }

    impl Write for EchoPty {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.0.write(bytes) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    impl Pty for EchoPty {
        fn resize(&mut self, _: u16, _: u16) -> io::Result<()> { Ok(()) }
        fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> { self.resize(rows, cols) }
        fn exit(&mut self) -> io::Result<Option<i32>> { Ok(None) }
        fn terminate(&mut self) -> io::Result<bool> { Ok(true) }
    }
}
