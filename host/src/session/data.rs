//! The desk seat's data-only browser role. Opening a channel starts no host program.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc::Sender, Arc, Mutex};
use std::time::{Duration, Instant};

use peer::state::Signal;
use str0m::change::SdpOffer;
use str0m::channel::{ChannelData, ChannelId, Reliability};
use str0m::Event;
use wire::browse::{self, Browse, BrowseError};
use wire::control::Control;
use wire::types::Reason;

use super::terminal::{registry::Lease, Terminal};
use super::{file_transfer::{Page, Transfers}, Cx, Role, SessionEvent};
use crate::i18n::t;
use crate::workspace::{BrowseFiles, BrowserUser};

/// A single enabled application media section, covered by the same proof as a picture offer.
pub fn data_only(sdp: &str) -> bool {
    SdpOffer::from_sdp_string(sdp).is_ok_and(|offer| {
        matches!(&offer.media_lines[..], [media] if media.typ.is_channel() && !media.disabled)
    })
}

pub struct Data {
    lease: Lease,
    terminal: Terminal,
    terminal_id: Option<ChannelId>,
    files: Option<Files>,
    transfers: Option<Transfers>,
    _browser: Option<BrowserUser>,
}

impl Data {
    pub(crate) fn new(lease: Lease, browser: BrowserUser, page: Arc<Mutex<Page>>, own: Sender<SessionEvent>) -> Self {
        Self {
            lease: lease.clone(),
            terminal: Terminal::new(lease.clone()),
            terminal_id: None,
            files: Some(Files {
                call: browser.browse.clone(), lease: lease.clone(), own: own.clone(), channel: None, work: Work::Idle,
            }),
            transfers: Some(Transfers::new(browser.transfers.clone(), page, lease, own)),
            _browser: Some(browser),
        }
    }
}

impl Role for Data {
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
        if reliable {
            match label {
                "control" if cx.control().is_none() => return true,
                wire::terminal::CHANNEL if self.terminal_id.is_none() => {
                    self.terminal_id = Some(id);
                    return false;
                }
                wire::file_transfer::CHANNEL => {
                    if let Some(transfer) = &mut self.transfers {
                        if let Some(old) = transfer.attach(id) {
                            cx.rtc.direct_api().close_data_channel(old);
                        }
                        return false;
                    }
                }
                browse::CHANNEL => {
                    if let Some(files) = self.files.as_mut().filter(|files| files.channel.is_none()) {
                        files.channel = Some(id);
                        return false;
                    }
                }
                _ => {}
            }
        }
        cx.rtc.direct_api().close_data_channel(id);
        false
    }

    fn channel_data(&mut self, cx: &mut Cx, d: ChannelData) -> bool {
        if cx.control() == Some(d.id) && !d.binary {
            if let Ok(Control::End { reason, .. }) = serde_json::from_slice(&d.data) {
                return cx.signal(Signal::PeerEnded(reason));
            }
        } else if !self.lease.holds_seat() {
            return false;
        } else if self.terminal_id == Some(d.id) {
            self.terminal.message(cx.active(), d);
        } else if let Some(transfer) = self.transfers.as_mut().filter(|t| t.channel() == Some(d.id)) {
            if !transfer.receive(d.data, d.binary) {
                cx.rtc.direct_api().close_data_channel(d.id);
                transfer.protocol_error();
            }
        } else if let Some(files) = self.files.as_mut().filter(|files| files.channel == Some(d.id)) {
            if !files.request(d.binary, &d.data) {
                cx.rtc.direct_api().close_data_channel(d.id);
                self.files = None;
            }
        }
        false
    }

    fn rtc_event(&mut self, _: &mut Cx, e: Event) {
        if let Event::ChannelClose(id) = e {
            if self.terminal_id == Some(id) {
                self.terminal.channel_closed();
            } else if self.files.as_ref().is_some_and(|files| files.channel == Some(id)) {
                self.files = None;
            } else if let Some(transfer) = self.transfers.as_mut().filter(|t| t.channel() == Some(id)) {
                transfer.detach();
            }
        }
    }

    fn file_reply(&mut self, text: Vec<u8>) {
        if let Some(files) = &mut self.files {
            files.work = Work::Reply(text);
        }
    }

    fn ended(&mut self, reason: Reason) {
        self.terminal.ended(reason);
        if let Some(mut transfer) = self.transfers.take() {
            transfer.ended(reason);
        }
    }

    fn before_wait(&mut self, cx: &mut Cx, _: usize) -> Option<Instant> {
        if !cx.active() || !self.lease.holds_seat() {
            return None;
        }
        let terminal = self.terminal_id.and_then(|id| self.terminal.poll(cx, id));
        let files = self.files.as_mut().and_then(|files| files.poll(cx));
        let transfer = self.transfers.as_mut().and_then(|t| t.poll(cx));
        terminal.into_iter().chain(files).chain(transfer).min()
    }
}

struct Files {
    call: BrowseFiles,
    lease: Lease,
    own: Sender<SessionEvent>,
    channel: Option<ChannelId>,
    work: Work,
}

const MAX_FILE_WORKERS: usize = 2;
static FILE_WORKERS: AtomicUsize = AtomicUsize::new(0);

pub(super) struct WorkerSlot(&'static AtomicUsize);

impl WorkerSlot {
    pub fn for_files() -> Option<Self> {
        Self::take(&FILE_WORKERS)
    }

    fn take(workers: &'static AtomicUsize) -> Option<Self> {
        workers.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
            (count < MAX_FILE_WORKERS).then_some(count + 1)
        }).ok().map(|_| Self(workers))
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

enum Work {
    Idle,
    Request(Browse, &'static str),
    Running,
    Reply(Vec<u8>),
}

impl Files {
    fn request(&mut self, binary: bool, bytes: &[u8]) -> bool {
        if !matches!(self.work, Work::Idle) || binary || bytes.len() > browse::MAX_BYTES {
            return false;
        }
        let Ok(op) = serde_json::from_slice(bytes) else { return false };
        let operation = match op {
            Browse::List { .. } => "list",
            Browse::Mkdir { .. } => "mkdir",
            Browse::Rename { .. } => "rename",
            Browse::Delete { .. } => "delete",
            _ => return false,
        };
        self.work = Work::Request(op, operation);
        true
    }

    fn start(&mut self, workers: &'static AtomicUsize) {
        let Work::Request(op, operation) = std::mem::replace(&mut self.work, Work::Running) else { unreachable!() };
        let Some(slot) = WorkerSlot::take(workers) else {
            self.work = Work::Reply(error(BrowseError::Busy));
            return;
        };
        let (call, own, lease) = (self.call.clone(), self.own.clone(), self.lease.clone());
        let worker = std::thread::Builder::new().name("files".into()).spawn(move || {
            if !lease.holds_seat() {
                return;
            }
            let (path, reply) = call(op, &|| lease.holds_seat());
            let mut text = encode(&reply);
            let outcome = if text.len() > browse::MAX_BYTES {
                text = error(BrowseError::Io);
                BrowseError::Io.as_str()
            } else if let Browse::Error { why } = reply {
                why.as_str()
            } else {
                "ok"
            };
            eprintln!("{} device={:?} operation={operation} path={path:?} outcome={outcome}", t("files.operation"), lease.name());
            drop(slot);
            let _ = own.send(SessionEvent::Browse(text));
        });
        if worker.is_err() {
            self.work = Work::Reply(error(BrowseError::Io));
        }
    }

    fn poll(&mut self, cx: &mut Cx) -> Option<Instant> {
        if matches!(self.work, Work::Request(..)) {
            self.start(&FILE_WORKERS);
        }
        let Work::Reply(reply) = &self.work else { return None };
        let id = self.channel?;
        match cx.rtc.channel(id)?.write(false, reply) {
            Ok(false) => return Some(Instant::now() + Duration::from_millis(10)),
            Ok(true) => {}
            Err(_) => cx.rtc.direct_api().close_data_channel(id),
        }
        self.work = Work::Idle;
        Some(Instant::now())
    }
}

fn encode(reply: &Browse) -> Vec<u8> {
    serde_json::to_vec(reply).expect("file reply serializes")
}

fn error(why: BrowseError) -> Vec<u8> {
    encode(&Browse::Error { why })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::terminal::registry::Registry;
    use std::path::PathBuf;
    use std::sync::{mpsc, Mutex};

    fn admitted(registry: &Registry, id: &str, call: impl Fn(Browse, &dyn Fn() -> bool) -> (PathBuf, Browse) + Send + Sync + 'static) -> (Data, mpsc::Receiver<SessionEvent>) {
        let (own, replies) = mpsc::channel();
        let lease = registry.admit(id.into(), "browser".into(), "device".into());
        let data = Data {
            lease: lease.clone(),
            terminal: Terminal::new(lease.clone()),
            terminal_id: None,
            files: Some(Files { call: Arc::new(call), lease, own, channel: None, work: Work::Idle }),
            transfers: None,
            _browser: None,
        };
        (data, replies)
    }

    fn listing(_: Browse, _: &dyn Fn() -> bool) -> (PathBuf, Browse) {
        (PathBuf::from("/"), Browse::Listing { path: vec![], entries: vec![], roots: None, cursor: None })
    }

    fn reply(replies: &mpsc::Receiver<SessionEvent>) -> Browse {
        let SessionEvent::Browse(text) = replies.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected file reply");
        };
        serde_json::from_slice(&text).unwrap()
    }

    #[test]
    fn browsing_on_an_admitted_connection_does_not_open_a_terminal() {
        static SHELLS: AtomicUsize = AtomicUsize::new(0);
        static WORKERS: AtomicUsize = AtomicUsize::new(0);
        let open = |_, _| -> std::io::Result<Box<dyn crate::platform::Pty>> {
            SHELLS.fetch_add(1, Ordering::Relaxed);
            Err(std::io::ErrorKind::Unsupported.into())
        };
        let (updates, _) = mpsc::channel();
        let registry = Registry::new(open, updates);
        let (mut data, replies) = admitted(&registry, "session", listing);
        let files = data.files.as_mut().unwrap();
        assert!(files.request(false, br#"{"t":"list"}"#));
        assert!(!files.request(false, br#"{"t":"list"}"#));
        files.start(&WORKERS);
        assert!(matches!(reply(&replies), Browse::Listing { .. }));
        data.ended(Reason::Left);
        assert_eq!(SHELLS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_queued_change_cannot_start_after_the_seat_ends() {
        static WORKERS: AtomicUsize = AtomicUsize::new(0);
        for replaces in [false, true] {
            let (updates, _) = mpsc::channel();
            let registry = Registry::new(|_, _| unreachable!(), updates);
            let (mut data, replies) = admitted(&registry, "old", listing);
            let mut files = data.files.take().unwrap();
            assert!(files.request(false, br#"{"t":"delete","path":[],"entry":"eA"}"#));
            registry.end("old", if replaces { "taken" } else { "ended" });
            if replaces {
                registry.admit("new".into(), "browser".into(), "device".into());
            }
            files.start(&WORKERS);
            drop(files);
            assert_eq!(replies.recv_timeout(Duration::from_secs(2)).err(), Some(mpsc::RecvTimeoutError::Disconnected));
        }
    }

    #[test]
    fn blocking_calls_keep_worker_slots_even_after_their_sessions_end() {
        static WORKERS: AtomicUsize = AtomicUsize::new(0);
        static ENTERED: Mutex<Option<Sender<()>>> = Mutex::new(None);
        static BLOCK: Mutex<()> = Mutex::new(());
        let block = BLOCK.lock().unwrap();
        let (entered, started) = mpsc::channel();
        *ENTERED.lock().unwrap() = Some(entered);
        let (updates, _) = mpsc::channel();
        let registry = Registry::new(|_, _| unreachable!(), updates);
        let mut running = Vec::new();
        for i in 0..MAX_FILE_WORKERS {
            let id = i.to_string();
            let (mut data, replies) = admitted(&registry, &id, |op, seated| {
                ENTERED.lock().unwrap().as_ref().unwrap().send(()).unwrap();
                let _blocked = BLOCK.lock().unwrap();
                listing(op, seated)
            });
            let files = data.files.as_mut().unwrap();
            assert!(files.request(false, br#"{"t":"list"}"#));
            files.start(&WORKERS);
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            data.ended(Reason::Taken);
            running.push(replies);
        }
        let (mut data, replies) = admitted(&registry, "next", listing);
        let files = data.files.as_mut().unwrap();
        assert!(files.request(false, br#"{"t":"list"}"#));
        files.start(&WORKERS);
        let Work::Reply(text) = std::mem::replace(&mut files.work, Work::Idle) else { panic!("expected busy reply") };
        assert_eq!(serde_json::from_slice::<Browse>(&text).unwrap(), Browse::Error { why: BrowseError::Busy });
        drop(block);
        for replies in running {
            assert!(matches!(reply(&replies), Browse::Listing { .. }));
        }
        assert!(files.request(false, br#"{"t":"list"}"#));
        files.start(&WORKERS);
        assert!(matches!(reply(&replies), Browse::Listing { .. }));
    }
}
