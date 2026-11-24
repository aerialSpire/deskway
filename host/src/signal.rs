//! Signaling connection to the server, with automatic reconnect, the host's status, and the
//! two seats (`seats`, each a `registry`): a person's browser on the desk seat, an AI client on
//! the AI seat, side by side (docs/architecture.md, "Two seats side by side"). The host decides who holds each: on the
//! desk seat one session, replaced only by the same device's next attempt while it is still
//! connecting, or by a signed `force` (docs/session-lifecycle.md, "Admission"); on the AI seat
//! one AI client, while a browser of the account allows AI here, replaced by the same device's
//! next attempt in any state and by nothing else. Losing the server ends no session: after
//! reconnecting, the status names the holders again so the server can route them.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message as WsMessage, WebSocket};

use crate::audit::Audit;
use crate::auth::{AiState, Trust};
use crate::config::Config;
use crate::display::DisplayInfo;
use crate::mcp::{Facts, SharedFacts};
use crate::platform::{Desk, Platform, PlatformEvent, Replies};
use wire::control::Control as Message;
use wire::signal::Signal as Wire;
use wire::types::{self as types, Reason};
use peer::state::End;
use crate::session::{self, SessionEvent, SessionUpdate};
use crate::settings::{PanelRequest, Settings};

mod alive;
mod registry;
mod seats;

use registry::{Admit, Entry, Nonces};
use seats::{Seat, Seats};

/// What the panel shows, wherever the host runs.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Status {
    pub online: bool,
    /// Signed in, but an approved browser has not let this computer in yet.
    pub waiting: bool,
    /// While waiting: the six digits the approval shows too (`challenge::computer_code`).
    pub approval_code: Option<String>,
    pub controller: Option<String>,
    /// The AI client using this computer (its device name), beside the controller.
    pub ai_client: Option<String>,
    /// What AI clients may do here now: "off", "on" or "stopped".
    pub ai_state: String,
    /// The display being shown to the controller, for the on-screen border.
    pub display: Option<DisplayInfo>,
    pub name: String,
    pub server: String,
    /// The account this computer belongs to, as the server last said.
    pub account: Option<crate::config::Account>,
    pub auto_lock: bool,
    /// How many devices may control this computer.
    pub paired: usize,
    pub pairing: bool,
    /// The link behind the panel's QR code, while a pairing window is open.
    pub pair_url: Option<String>,
    /// Devices asking the person at this computer to let them in.
    pub asks: Vec<AskView>,
    /// The devices that can control this computer.
    pub devices: Vec<DeviceView>,
    /// Why the server cannot be reached, when it cannot.
    pub problem: Option<String>,
    /// A line about files arriving, beside the indicator's name: text, and their folder.
    pub note: Option<(String, Option<String>)>,
    /// The host's language setting; None follows the system.
    pub lang: Option<String>,
    /// The keep-awake setting (`power`).
    pub prevent_sleep: bool,
}

/// A device that can control this computer, as the panel lists it.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    /// When it last controlled this computer (Unix seconds); 0: not yet.
    pub used: u64,
    /// It controls this computer now.
    pub controlling: bool,
    /// An AI client, not a browser.
    #[serde(default)]
    pub ai: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AskView {
    pub session: String,
    pub name: String,
    pub from: String,
    pub code: String,
}

impl Status {
    /// Before the first connection attempt.
    pub fn offline(cfg: &Config) -> Status {
        Status { name: cfg.name.clone(), server: cfg.server.clone(), account: cfg.account.clone(), auto_lock: cfg.auto_lock, lang: cfg.lang.clone(), prevent_sleep: cfg.prevent_sleep, ..Status::default() }
    }
}

pub type Notifier = Box<dyn Fn(Status) + Send>;

fn describe(e: &anyhow::Error) -> String {
    let raw = format!("{e:#}");
    let lower = raw.to_lowercase();
    let what = if lower.contains("tls") || lower.contains("certificate") {
        "problem.tls"
    } else if lower.contains("dns") || lower.contains("resolve") || lower.contains("no such host") {
        "problem.dns"
    } else if lower.contains("refused") || lower.contains("timed out") || lower.contains("unreachable") || lower.contains("10060") || lower.contains("10061") {
        "problem.network"
    } else if lower.contains("401") {
        "problem.revoked"
    } else {
        "status.no_server"
    };
    crate::i18n::tf("problem.detail", &[("what", crate::i18n::t(what)), ("detail", raw.chars().take(160).collect())])
}

/// What the host is asked to do, by its settings (`settings::Settings`) and by the app.
#[derive(Debug, PartialEq)]
pub enum Control {
    Disconnect,
    SetAutoLock(bool),
    /// The shared clipboard on or off (already saved).
    SetClipShare(bool),
    /// Shows a QR code that adds a phone or computer.
    StartPairing,
    StopPairing,
    /// The person's answer to a device asking to be let in.
    AnswerAsk(String, bool),
    /// Removes devices at this computer: these ids, or all of them.
    RemoveDevices(Option<Vec<String>>),
    /// Stop AI control here, until a browser of the account allows it again.
    StopAi,
    /// Start with the computer or not.
    SetAutoStart(bool),
    /// A setting changed: the panel shows it.
    Report,
    Quit,
}

/// Sends each platform event where it belongs: to the desk seat's session, to the host, or to
/// both. (The AI seat's session hears nothing from the platform yet: the AI side reads the
/// host's `Facts`.)
struct Router {
    desk: Mutex<Option<Sender<SessionEvent>>>,
    host: Sender<PlatformEvent>,
    /// Answers under an id go to whoever asked (`Replies`), before anything else.
    replies: Arc<Replies>,
}

impl Router {
    /// (for the desk session, for the host). Every event is placed here, so a new one cannot be
    /// forgotten.
    fn targets(e: &PlatformEvent) -> (bool, bool) {
        match e {
            PlatformEvent::Frame(_)
            | PlatformEvent::SharpTile { .. }
            | PlatformEvent::CaptureAvailable(_)
            | PlatformEvent::EncodedSize { .. }
            | PlatformEvent::CursorShape(_)
            | PlatformEvent::CursorPosition { .. }
            | PlatformEvent::LayoutChanged { .. }
            | PlatformEvent::FileMessage(_)
            | PlatformEvent::FileData(_)
            | PlatformEvent::ClipboardChanged { .. }
            | PlatformEvent::ClipboardAnnounced { .. }
            | PlatformEvent::ClipboardRender { .. }
            | PlatformEvent::ClipboardData { .. }
            | PlatformEvent::ClipboardFailed { .. } => (true, false),
            PlatformEvent::Capturing { .. } | PlatformEvent::Desktop(_) => (true, true),
            PlatformEvent::Thumbnail { .. } | PlatformEvent::DisplaysChanged(_) | PlatformEvent::TransferNote(_) => (false, true),
            // Answers: handed over in `route`, before this is asked.
            PlatformEvent::Snapshot { .. } | PlatformEvent::SnapshotFailed { .. } | PlatformEvent::InputDone { .. } => (false, false),
        }
    }

    fn route(&self, e: PlatformEvent) {
        let Some(e) = self.replies.answer(e) else { return };
        let to_session = |e: PlatformEvent| {
            if let Some(s) = self.desk.lock().unwrap().as_ref() {
                let _ = s.send(SessionEvent::Platform(e));
            }
        };
        // Only the rare events for both are copied; video frames never are.
        match Self::targets(&e) {
            (true, true) => {
                to_session(e.clone());
                let _ = self.host.send(e);
            }
            (true, false) => to_session(e),
            (false, true) => {
                let _ = self.host.send(e);
            }
            (false, false) => {}
        }
    }

    /// Routes until the platform's last sender is gone.
    fn spawn(events: Receiver<PlatformEvent>) -> (Arc<Router>, Receiver<PlatformEvent>) {
        let (host, host_rx) = channel();
        let router = Arc::new(Router { desk: Mutex::new(None), host, replies: Arc::default() });
        let r = router.clone();
        std::thread::Builder::new()
            .name("platform-events".into())
            .spawn(move || {
                for e in events {
                    r.route(e);
                }
            })
            .expect("spawn event router");
        (router, host_rx)
    }
}


/// What a host needs to run.
pub struct Params {
    pub cfg: Config,
    pub view_only: bool,
    pub platform: Arc<dyn Platform>,
    /// Everything `platform` reports.
    pub events: Receiver<PlatformEvent>,
    /// Whether this computer can take a session now (macOS: the needed permissions).
    pub can_host: Box<dyn Fn() -> bool + Send>,
    pub notify: Notifier,
    /// Where the controlling browser's clipboard switch is saved.
    pub settings: Arc<Settings>,
    /// The person's workspace, when this process can reach one (`workspace`).
    pub workspace: Option<Arc<dyn crate::workspace::Workspace>>,
    /// The person's workspace as an administrator, where it is apart (the Windows admin worker).
    pub admin: Option<Arc<dyn crate::workspace::Workspace>>,
}

struct Host {
    cfg: Config,
    view_only: bool,
    auto_lock: bool,
    clip_share: bool,
    /// The session's browser asks to switch the shared clipboard.
    share_tx: Sender<bool>,
    online: bool,
    waiting: bool,
    /// Why the last attempt to reach the server failed, in words for the panel.
    problem: Option<String>,
    quit: bool,
    platform: Arc<dyn Platform>,
    router: Arc<Router>,
    can_host: Box<dyn Fn() -> bool + Send>,
    out_tx: Sender<String>,
    updates_tx: Sender<SessionUpdate>,
    /// The sessions on the two seats (their handles feed the session threads).
    sessions: Seats<Sender<SessionEvent>>,
    /// The connect nonces issued, per session: one set for every session, whoever controls.
    nonces: Nonces,
    /// Sessions ended for new ones (replaced or taken over) whose threads have not finished
    /// yet. The status waits for them: their bye or end reaches the server before the status
    /// that no longer names them, so their ends hear why.
    replaced: Vec<String>,
    /// What the AI side knows of this computer, kept here and read by AI sessions' tools.
    facts: SharedFacts,
    /// The person's workspace, whose programs outlive sessions (`workspace`).
    workspace: Option<Arc<dyn crate::workspace::Workspace>>,
    admin: Option<Arc<dyn crate::workspace::Workspace>>,
    /// What AI clients did here.
    audit: Arc<Audit>,
    /// The input desktop, as the platform last reported it.
    desk: Option<Desk>,
    display: Option<DisplayInfo>,
    /// Every display attached, as the platform last reported.
    displays: Vec<DisplayInfo>,
    note: Option<(String, Option<String>)>,
    notify: Notifier,
    /// None if the trust store could not be opened: then every offer is refused.
    trust: Option<Trust>,
    settings: Arc<Settings>,
}

impl Host {
    fn report(&self) {
        let (asks, pair_url, devices) = match self.trust.as_ref() {
            Some(t) => {
                let controlling: Vec<&str> = Seat::BOTH.iter().filter_map(|s| self.sessions.of(*s).controlling_device()).collect();
                let devices = t.controllers().iter().map(|c| DeviceView { id: c.id.clone(), name: c.name.clone(), used: c.used, controlling: controlling.contains(&c.id.as_str()), ai: c.role == wire::types::Role::Ai }).collect::<Vec<_>>();
                (t.asks(), t.pairing_url(), devices)
            }
            None => (Vec::new(), None, Vec::new()),
        };
        // Settings an administrator or the panel changed meanwhile show at once.
        let cfg = self.settings.load().unwrap_or_else(|_| self.cfg.clone());
        self.update_facts(&cfg.name);
        (self.notify)(Status {
            online: self.online,
            waiting: self.waiting,
            approval_code: self.waiting.then(|| wire::challenge::computer_code(&self.cfg.id)),
            controller: self.sessions.of(Seat::Desk).controller(),
            ai_client: self.sessions.of(Seat::Ai).controller(),
            ai_state: self.ai_state().as_str().to_string(),
            display: self.display.clone(),
            name: cfg.name,
            server: cfg.server,
            account: cfg.account,
            auto_lock: cfg.auto_lock,
            paired: devices.len(),
            pairing: pair_url.is_some(),
            pair_url,
            asks: asks.into_iter().map(|a| AskView { session: a.session, name: a.name, from: a.from, code: a.code }).collect(),
            devices,
            problem: if self.online { None } else { self.problem.clone() },
            note: self.note.clone(),
            lang: cfg.lang,
            prevent_sleep: cfg.prevent_sleep,
        });
    }

    /// What AI clients may do here now (off with no trust store: every offer is refused then).
    fn ai_state(&self) -> types::AiState {
        match self.trust.as_ref().map(Trust::ai_state) {
            Some(AiState::On { .. }) => types::AiState::On,
            Some(AiState::Stopped) => types::AiState::Stopped,
            _ => types::AiState::Off,
        }
    }

    /// The facts AI sessions read, brought up to date (`name`: the computer's, as configured).
    fn update_facts(&self, name: &str) {
        let facts = Facts {
            name: if cfg!(target_os = "macos") { crate::shown_name(name) } else { name.to_string() },
            os: std::env::consts::OS.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            displays: self.displays.clone(),
            desk: self.desk,
            person: self.sessions.of(Seat::Desk).controller(),
            admin: matches!(self.trust.as_ref().map(Trust::ai_state), Some(AiState::On { admin: true })),
            view_only: self.view_only,
            // Asked of the workspace when read (`tools::Ctx::facts`).
            user_session: false,
        };
        let mut f = self.facts.write().unwrap_or_else(|e| e.into_inner());
        if *f != facts {
            *f = facts;
        }
    }

    /// The sessions holding this computer's seats (connecting or established), the displays and
    /// what AI clients may do, so the server knows which sessions live and routes them, also
    /// after it restarted. Held back while a replaced session is still saying goodbye
    /// (`replaced`).
    fn send_status(&self) {
        crate::power::ai_session(self.sessions.of(Seat::Ai).current().is_some());
        if self.replaced.is_empty() {
            let (holder, ai) = (self.sessions.of(Seat::Desk).holder(), self.sessions.of(Seat::Ai).holder());
            let ai_refused = self.trust.as_ref().and_then(Trust::ai_refused);
            self.send(&Wire::Status { displays: self.displays.clone(), holder, ai, ai_state: Some(self.ai_state()), ai_refused });
        }
        self.report();
    }

    /// Ends the session on `seat`; the session tells its other end why. A controller taken over
    /// or ended at this computer does not come back by itself: its resume tokens go (only its own).
    fn end_seat(&mut self, seat: Seat, end: End) {
        let Some(cur) = self.sessions.of(seat).current() else { return };
        if matches!(end.reason, Reason::Taken | Reason::Ended) {
            let controller = cur.controller.clone();
            if let Some(t) = self.trust.as_mut() {
                t.revoke_resume(&controller);
            }
        }
        let _ = cur.handle.send(SessionEvent::End(end));
    }

    fn send(&self, m: &Wire) {
        let _ = self.out_tx.send(serde_json::to_string(m).expect("signal serializes"));
    }

    fn bye(&self, session: &str, reason: Reason) {
        self.send(&Wire::bye(session, reason));
    }

    /// A browser asks for a connect nonce for a new session.
    fn on_challenge(&mut self, m: &Value) {
        let session = m["session"].as_str().unwrap_or_default();
        if session.is_empty() || session.len() > 64 {
            return;
        }
        let nonce = self.nonces.issue(session, Instant::now());
        self.send(&Wire::Nonce { session: session.to_string(), nonce });
    }

    fn on_offer(&mut self, m: &Value) {
        let session = m["session"].as_str().unwrap_or_default().to_string();
        let sdp = m["sdp"].as_str().unwrap_or_default().to_string();
        // The offer of a session holding a seat, again (its answer may not have arrived): the
        // session answers that epoch again. Only the end that started it can send on its id
        // (the server routes by session), and nothing new is admitted.
        if let Some((_, cur)) = self.sessions.find(&session) {
            let epoch = m["epoch"].as_u64().unwrap_or_default() as u32;
            let _ = cur.handle.send(SessionEvent::Negotiate { sdp, epoch });
            return;
        }
        let force = m["force"].as_bool().unwrap_or(false);
        // The nonce issued for this session is used up by this offer, whatever its fate.
        let nonce = m["auth"]["nonce"].as_str().unwrap_or_default().to_string();
        let nonce_ok = self.nonces.take(&session, &nonce, Instant::now());
        if !(self.can_host)() {
            eprintln!("session refused: this computer cannot host one now");
            return self.bye(&session, Reason::Permissions);
        }
        let Some(trust) = self.trust.as_mut() else { return self.bye(&session, Reason::Auth) };
        trust.reload();
        if trust.controllers().is_empty() {
            return self.bye(&session, Reason::Unpaired);
        }
        let verified = match trust.verify_offer(&session, if nonce_ok { &nonce } else { "" }, &sdp, force, &m["auth"]) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("refused offer: {e:#}");
                let known = trust.controllers().iter().any(|c| Some(c.id.as_str()) == m["auth"]["cred"].as_str()) || m["auth"]["k"] == "rs";
                return self.bye(&session, if known { Reason::Auth } else { Reason::Unpaired });
            }
        };
        let seat = Seat::of(verified.role);
        if seat == Seat::Ai {
            // An AI client only while a browser of the account allows AI here, and not stopped;
            // never taking over, and never with media.
            if !matches!(trust.ai_state(), AiState::On { .. }) {
                return self.bye(&session, Reason::AiOff);
            }
            if force || session::ai::has_media(&sdp) {
                return self.bye(&session, Reason::Forbidden);
            }
        }
        // The name shown at this computer is the paired device's own, which this host verified.
        let name = verified.name.clone();
        match self.sessions.of(seat).admit(&verified.controller, force) {
            Admit::Busy { holder } => {
                self.send(&Wire::Bye { session, reason: Reason::Busy, by: None, same: None, holder: Some(holder) });
                return;
            }
            Admit::TakeOver | Admit::Replace => {
                let old = self.sessions.of(seat).current().map(|c| (c.id.clone(), c.controller.clone()));
                if let Some((id, cred)) = old {
                    let same = !cred.is_empty() && cred == verified.controller;
                    self.end_seat(seat, End { reason: Reason::Taken, by: Some(name.clone()), same });
                    self.replaced.push(id);
                }
            }
            Admit::Free => {}
        }
        let Some(trust) = self.trust.as_mut() else { return };
        // Issued after a takeover revoked the previous controller's tokens: the same device
        // taking over from itself keeps this one. An AI client signs without asking anyone, so
        // it gets no resume token.
        let greeting = match seat {
            Seat::Desk => vec![Message::Hello { version: wire::PROTOCOL_VERSION, caps: session::desk::caps(self.view_only) }, trust.issue_resume(&verified)],
            Seat::Ai => vec![session::ai::hello()],
        };
        let sign = trust.answer_signer(&session);
        let stun: Vec<String> = m["ice"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|s| s["urls"].as_array().into_iter().flatten())
            .filter_map(|u| u.as_str())
            .filter(|u| u.starts_with("stun:"))
            .map(String::from)
            .collect();
        let params = session::Params { session: session.clone(), sdp: sdp.clone(), stun, turn: peer::turn::servers(&m["ice"]), out: self.out_tx.clone(), updates: self.updates_tx.clone(), sign, greeting };
        let tx = match seat {
            Seat::Desk => {
                let desk = session::desk::Desk::new(session::desk::Params {
                    session: session.clone(),
                    offer: sdp,
                    view: serde_json::from_value(m["view"].clone()).ok(),
                    platform: self.platform.clone(),
                    view_only: self.view_only,
                    clip_share: self.clip_share,
                    share: self.share_tx.clone(),
                });
                let tx = session::spawn(params, |_| desk);
                *self.router.desk.lock().unwrap() = Some(tx.clone());
                self.note = None;
                tx
            }
            Seat::Ai => {
                let gate = crate::mcp::gate::Gate { audit: self.audit.clone(), client: name.clone(), device: verified.controller.clone(), session: session.clone() };
                let tunnels = crate::tools::tunnel::Allowed::default();
                let ctx = crate::tools::Ctx { facts: self.facts.clone(), platform: self.platform.clone(), session: session.clone(), replies: self.router.replies.clone(), shot: Default::default(), workspace: self.workspace.clone(), tunnels: tunnels.clone(), admin: self.admin.clone(), audit: self.audit.clone() };
                // Its input is the AI client's hand from now on (`platform::local`).
                self.platform.ai_session(Some(session.clone()));
                let (platform, id) = (self.platform.clone(), session.clone());
                session::spawn(params, move |own| session::ai::Ai::new(id, platform, crate::mcp::spawn(gate, ctx, own.clone()), session::ai::IDLE, tunnels, own))
            }
        };
        eprintln!("session started by {name} ({seat:?} seat)");
        let user = m["user"].as_str().unwrap_or_default().to_string();
        // Holding the seat, still connecting: the status names it so from now on.
        self.sessions.of_mut(seat).start(Entry { id: session, name, user, controller: verified.controller.clone(), since: crate::auth::unix_now(), handle: tx });
        self.send_status();
    }

    fn on_message(&mut self, text: &str) {
        let Ok(m) = serde_json::from_str::<Value>(text) else { return };
        let session = m["session"].as_str().unwrap_or_default();
        match m["type"].as_str().unwrap_or_default() {
            // The server's version and caps: nothing here depends on them yet.
            "hello" => {}
            "challenge" => self.on_challenge(&m),
            "offer" => self.on_offer(&m),
            "restart" => {
                if let (Some((_, cur)), Some(sdp)) = (self.sessions.find(session), m["sdp"].as_str()) {
                    let epoch = m["epoch"].as_u64().unwrap_or_default() as u32;
                    let _ = cur.handle.send(SessionEvent::Negotiate { sdp: sdp.to_string(), epoch });
                }
            }
            "pair" => self.on_pair(&m),
            "pairask" => self.on_ask(&m),
            // The account's signed device log: who else may control this computer.
            "devlog" => {
                if let Some(t) = self.trust.as_mut() {
                    t.set_log(&m["auth"], &m["ai"]);
                }
                self.hold_to_trust();
                self.send_status();
            }
            "candidate" => {
                if let Some((_, cur)) = self.sessions.find(session) {
                    if let Some(c) = m["candidate"]["candidate"].as_str().filter(|c| !c.is_empty()) {
                        let _ = cur.handle.send(SessionEvent::RemoteCandidate(c.to_string()));
                    }
                }
            }
            // The browser ends its session through the server (its channel never opened, or
            // cannot carry it now).
            "bye" => {
                if let Some((_, cur)) = self.sessions.find(session) {
                    let reason = serde_json::from_value(m["reason"].clone()).unwrap_or(Reason::Left);
                    let _ = cur.handle.send(SessionEvent::PeerBye(reason));
                }
            }
            "thumbs" => self.platform.thumbnails(m["on"].as_bool().unwrap_or(false)),
            // Who this computer belongs to, for the panel.
            "account" => {
                if let Ok(Wire::Account { user, display }) = serde_json::from_value::<Wire>(m.clone()) {
                    let _ = self.settings.set_account(crate::config::Account { user, display });
                    self.send_status();
                }
            }
            "locate" => self.on_locate(&m),
            // A browser of the account reads what AI clients did here, a page at a time.
            "auditreq" => {
                if let Ok(Wire::AuditRequest { probe, query }) = serde_json::from_value::<Wire>(m.clone()) {
                    let mut q: crate::audit::Query = serde_json::from_value(query).unwrap_or_default();
                    q.limit = q.limit.clamp(1, 200);
                    let page = serde_json::to_value(self.audit.calls(&q)).expect("a page serializes");
                    self.send(&Wire::Audit { probe, page });
                }
            }
            _ => {}
        }
    }

    /// A browser or AI client of the account says where it runs: answered `here` when that is
    /// this computer (`peer::here`). Looking takes up to a second, so not on this thread.
    fn on_locate(&self, m: &Value) {
        let Ok(Wire::Locate { probe, place }) = serde_json::from_value::<Wire>(m.clone()) else { return };
        let out = self.out_tx.clone();
        let _ = std::thread::Builder::new().name("locate".into()).spawn(move || {
            if peer::here::any_here(&place) {
                let _ = out.send(serde_json::to_string(&Wire::Here { probe }).expect("signal serializes"));
            }
        });
    }

    /// A device asks the person at this computer to let it in: the panel shows who and a code.
    fn on_ask(&mut self, m: &Value) {
        let session = m["session"].as_str().unwrap_or_default();
        let from = m["from"].as_str().unwrap_or_default();
        let r = match self.trust.as_mut() {
            Some(t) => t.ask(session, &m["auth"], from),
            None => Err(anyhow::anyhow!("trust store unavailable")),
        };
        if let Err(e) = r {
            eprintln!("request to be let in refused: {e:#}");
            self.bye(session, refusal(&e));
        }
        self.report();
    }

    /// A browser proving it knows the pairing code shown on this computer.
    fn on_pair(&mut self, m: &Value) {
        let session = m["session"].as_str().unwrap_or_default();
        match self.trust.as_mut().map(|t| t.pair(&m["auth"])) {
            Some(Ok(auth)) => self.send(&Wire::Paired { session: session.to_string(), auth }),
            Some(Err(e)) => {
                eprintln!("pairing refused: {e:#}");
                self.bye(session, refusal(&e));
            }
            None => self.bye(session, Reason::Pair),
        }
        self.report();
    }

    /// The seats as trust now allows: a device no longer trusted loses its seat (`unpaired`),
    /// and the AI seat goes when AI is no longer allowed here (`ai_off`).
    fn hold_to_trust(&mut self) {
        let Some(t) = self.trust.as_ref() else { return };
        let ai_on = matches!(t.ai_state(), AiState::On { .. });
        let mut ends = Vec::new();
        for seat in Seat::BOTH {
            if let Some(c) = self.sessions.of(seat).current() {
                if !c.controller.is_empty() && !t.controllers().iter().any(|d| d.id == c.controller) {
                    ends.push((seat, Reason::Unpaired));
                } else if seat == Seat::Ai && !ai_on {
                    ends.push((seat, Reason::AiOff));
                }
            }
        }
        for (seat, reason) in ends {
            self.end_seat(seat, End::plain(reason));
        }
    }

    fn on_update(&mut self, u: SessionUpdate) {
        match u {
            SessionUpdate::Established(id) => {
                let Some((seat, _)) = self.sessions.find(&id) else { return };
                if self.sessions.of_mut(seat).established(&id) {
                    if let (Some(t), Some(device)) = (self.trust.as_mut(), self.sessions.of(seat).controlling_device().map(str::to_string)) {
                        if let Err(e) = t.mark_used(&device) {
                            eprintln!("cannot note when {device} was used: {e:#}");
                        }
                    }
                    self.send_status();
                }
            }
            SessionUpdate::Ended(id) => self.on_ended(&id),
        }
    }

    fn on_ended(&mut self, session: &str) {
        // The replaced session has said goodbye: the status may name its successor now.
        if self.replaced.iter().any(|r| r == session) {
            self.replaced.retain(|r| r != session);
            self.send_status();
        }
        let Some((seat, _)) = self.sessions.find(session) else { return };
        // An attempt that never got established controlled nothing: nothing to lock.
        let was_controller = self.sessions.of(seat).controller().is_some();
        if self.sessions.of_mut(seat).ended(session) {
            eprintln!("session ended ({seat:?} seat)");
            match seat {
                Seat::Desk => {
                    *self.router.desk.lock().unwrap() = None;
                    self.note = None;
                }
                Seat::Ai => self.platform.ai_session(None),
            }
            self.send_status();
            // Locking follows the person's session only: an AI client leaving (idle, stopped)
            // must not lock out the person sitting at the computer.
            if seat == Seat::Desk && was_controller && self.auto_lock && !self.quit {
                self.platform.lock();
            }
        }
    }

    /// Events from the platform for the host itself. Thumbnails go out on `ws` when connected.
    fn on_platform(&mut self, e: PlatformEvent, ws: Option<&mut WebSocket<MaybeTlsStream<TcpStream>>>) -> Result<()> {
        match e {
            PlatformEvent::Thumbnail { display, jpeg } => {
                if let Some(ws) = ws {
                    use base64::Engine;
                    let data = base64::engine::general_purpose::STANDARD.encode(&jpeg.0);
                    ws.send(WsMessage::text(serde_json::to_string(&Wire::Thumb { screen: display, data })?))?;
                }
            }
            // A display plugged in or out: the controller's detail page shows the new layout.
            PlatformEvent::DisplaysChanged(list) => {
                if list != self.displays {
                    self.displays = list;
                    self.send_status();
                }
            }
            PlatformEvent::Desktop(d) => {
                if self.desk != Some(d) {
                    self.desk = Some(d);
                    self.report();
                }
            }
            PlatformEvent::Capturing { current, .. } => {
                if self.display.as_ref() != Some(&current) {
                    self.display = Some(current);
                    self.report();
                }
            }
            PlatformEvent::TransferNote(note) => {
                self.note = note.map(|n| (n.text, n.folder));
                self.report();
            }
            _ => {}
        }
        Ok(())
    }

    fn on_control(&mut self, c: Control) {
        match c {
            // The panel's Disconnect is the person's session.
            Control::Disconnect => self.end_seat(Seat::Desk, End::plain(Reason::Ended)),
            Control::SetAutoLock(on) => {
                self.auto_lock = on;
                self.platform.auto_lock(on);
            }
            Control::SetClipShare(on) => self.set_clip_share(on),
            Control::SetAutoStart(on) => self.platform.autostart(on),
            Control::StartPairing => {
                if let Ok(d) = crate::config::dir() {
                    if let Err(e) = crate::auth::Trust::start_pairing(&d) {
                        eprintln!("cannot open pairing: {e:#}");
                    }
                }
                self.report();
            }
            Control::StopPairing => {
                if let Some(t) = &self.trust {
                    t.cancel_pairing();
                }
                self.report();
            }
            Control::StopAi => {
                if let Some(t) = self.trust.as_mut() {
                    t.stop_ai();
                }
                self.hold_to_trust();
                self.send_status();
            }
            Control::RemoveDevices(ids) => {
                if let Some(t) = self.trust.as_mut() {
                    let r = match ids {
                        Some(ids) => t.remove_here(&ids),
                        None => t.remove_all(),
                    };
                    if let Err(e) = r {
                        eprintln!("cannot remove devices: {e:#}");
                    }
                }
                // A removed device holding a seat right now loses it too.
                self.hold_to_trust();
                self.report();
            }
            Control::AnswerAsk(session, allow) => {
                match self.trust.as_mut().map(|t| t.answer(&session, allow)) {
                    Some(Ok(Some(reply))) => self.send(&reply),
                    Some(Err(e)) => eprintln!("cannot add the device: {e:#}"),
                    _ => {}
                }
                self.report();
            }
            Control::Report => self.report(),
            Control::Quit => {
                self.quit = true;
                for seat in Seat::BOTH {
                    self.end_seat(seat, End::plain(Reason::Ended));
                }
            }
        }
    }

    /// The shared clipboard switched: the session in progress follows at once, the panel shows it.
    fn set_clip_share(&mut self, on: bool) {
        self.clip_share = on;
        if let Some(c) = self.sessions.of(Seat::Desk).current() {
            let _ = c.handle.send(SessionEvent::ClipShare(on));
        }
        self.report();
    }

    /// The controlling browser switched it: it is the computer's setting, saved like the panel's.
    /// A change comes back from the settings (`SetClipShare`); a switch to what it already is
    /// is answered at once, so the browser shows it.
    fn browser_set_clip_share(&mut self, on: bool) {
        if let Err(e) = self.settings.handle(PanelRequest::ClipShare(on)) {
            eprintln!("{e:#}");
        }
        if self.clip_share == on {
            self.set_clip_share(on);
        }
    }
}

fn connect(cfg: &Config) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
    let base = cfg.server.replacen("http", "ws", 1);
    let mut req = format!("{base}/ws/host?id={}", cfg.id).into_client_request()?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.token).parse().context("token header")?);
    let (ws, _) = tungstenite::connect(req)?;
    let tcp = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::NativeTls(s) => s.get_ref(),
        _ => return Ok(ws),
    };
    // A short read timeout lets one thread both read and drain the outgoing queue. A write
    // that cannot go out for long means the network is gone, not busy.
    tcp.set_read_timeout(Some(Duration::from_millis(20)))?;
    tcp.set_write_timeout(Some(alive::WRITE_TIMEOUT))?;
    tcp.set_nodelay(true)?;
    Ok(ws)
}

struct Queues {
    out: Receiver<String>,
    updates: Receiver<SessionUpdate>,
    platform: Receiver<PlatformEvent>,
    control: Receiver<Control>,
    share: Receiver<bool>,
}

impl Queues {
    /// What can be handled without the server.
    fn drain_local(&self, host: &mut Host) {
        while let Ok(c) = self.control.try_recv() {
            host.on_control(c);
        }
        while let Ok(u) = self.updates.try_recv() {
            host.on_update(u);
        }
        while let Ok(on) = self.share.try_recv() {
            host.browser_set_clip_share(on);
        }
    }
}

fn serve(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, host: &mut Host, q: &Queues) -> Result<()> {
    // The AI tools this computer carries out: an AI client tells its own from them before it
    // calls (`unsupported`, for a computer older than it).
    let tools = crate::tools::implemented().into_iter().map(String::from).collect();
    let hello = Wire::Hello { version: wire::PROTOCOL_VERSION, caps: session::desk::caps(host.view_only), tools: Some(tools) };
    ws.send(WsMessage::text(serde_json::to_string(&hello)?))?;
    host.send_status();
    let mut alive = alive::Alive::new(Instant::now());
    loop {
        match ws.read() {
            Ok(m) => {
                alive.heard(Instant::now());
                if let WsMessage::Text(t) = m {
                    host.on_message(t.as_str());
                }
            }
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        match alive.due(Instant::now()) {
            alive::Due::Nothing => {}
            alive::Due::Ping => ws.send(WsMessage::Ping(Default::default()))?,
            alive::Due::Gone => anyhow::bail!("the server said nothing for {}s", alive::SILENT.as_secs()),
        }
        q.drain_local(host);
        while let Ok(e) = q.platform.try_recv() {
            host.on_platform(e, Some(ws))?;
        }
        while let Ok(m) = q.out.try_recv() {
            ws.send(WsMessage::text(m))?;
        }
        ws.flush().or_else(|e| match e {
            tungstenite::Error::Io(ref io) if io.kind() == ErrorKind::WouldBlock => Ok(()),
            e => Err(e),
        })?;
        if host.quit && Seat::BOTH.iter().all(|s| host.sessions.of(*s).current().is_none()) {
            return Ok(());
        }
    }
}

impl Host {
    /// The host as it starts: offline, nobody in control, with the trust store of `cfg`'s
    /// folder (None refuses every offer).
    fn new(p: Params, trust: Option<Trust>, control: Receiver<Control>) -> (Host, Queues) {
        let Params { cfg, view_only, platform, events, can_host, notify, settings, workspace, admin } = p;
        let (out_tx, out) = channel();
        let (updates_tx, updates) = channel();
        let (share_tx, share) = channel();
        let (router, platform_events) = Router::spawn(events);
        platform.auto_lock(cfg.auto_lock);
        let host = Host {
            view_only,
            auto_lock: cfg.auto_lock,
            clip_share: cfg.clip_share,
            cfg,
            share_tx,
            online: false,
            waiting: false,
            problem: None,
            quit: false,
            platform,
            router,
            can_host,
            out_tx,
            updates_tx,
            sessions: Seats::default(),
            nonces: Nonces::default(),
            replaced: Vec::new(),
            facts: Arc::new(RwLock::new(Facts::default())),
            workspace,
            admin,
            audit: Arc::new(Audit::new(crate::config::dir().ok())),
            desk: None,
            display: None,
            displays: Vec::new(),
            note: None,
            notify,
            trust,
            settings,
        };
        (host, Queues { out, updates, platform: platform_events, control, share })
    }
}

/// Runs the host until it is told to quit or `control` closes.
pub fn run(p: Params, control: Receiver<Control>) {
    let trust = crate::config::dir().and_then(|d| Trust::open(d, &p.cfg.id, &p.cfg.server)).map_err(|e| eprintln!("trust store unavailable, refusing all sessions: {e:#}")).ok();
    let (mut host, q) = Host::new(p, trust, control);
    let cfg = host.cfg.clone();
    let view_only = host.view_only;
    let mut delay = Duration::from_secs(1);
    eprintln!("Deskway host: {} -> {}{}", cfg.name, cfg.server, if view_only { " (view only)" } else { "" });
    while !host.quit {
        match connect(&cfg) {
            Ok(mut ws) => {
                eprintln!("connected");
                host.problem = None;
                host.online = true;
                host.waiting = false;
                delay = Duration::from_secs(1);
                if let Err(e) = serve(&mut ws, &mut host, &q) {
                    eprintln!("connection lost: {e}");
                }
                host.online = false;
                host.report();
                // Thumbnails resume only if the server asks again after reconnecting.
                host.platform.thumbnails(false);
            }
            Err(e) => {
                host.waiting = e.downcast_ref::<tungstenite::Error>().is_some_and(|e| matches!(e, tungstenite::Error::Http(r) if r.status() == 403));
                host.problem = if host.waiting { None } else { Some(describe(&e)) };
                if host.waiting {
                    // Approval should take effect quickly, not after a long backoff.
                    delay = Duration::from_secs(2);
                } else {
                    eprintln!("cannot reach server: {e}");
                }
                host.report();
            }
        }
        // Sleeps on the control queue so the local panel stays responsive while offline.
        let wake = Instant::now() + delay;
        while !host.quit {
            let left = wake.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match q.control.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(c) => host.on_control(c),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => host.quit = true,
            }
            q.drain_local(&mut host);
            // Displays and notes still count offline; thumbnails have nowhere to go.
            while let Ok(e) = q.platform.try_recv() {
                let _ = host.on_platform(e, None);
            }
        }
        if !host.waiting {
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    }
}


/// What a browser is told when adding its device is refused: full, or that it did not work.
fn refusal(e: &anyhow::Error) -> Reason {
    if e.is::<crate::auth::DevicesFull>() { Reason::Full } else { Reason::Pair }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{Bytes, Call, Desk, Forward};
    use serde_json::json;

    #[derive(Default)]
    struct Calls(Mutex<Vec<Call>>);
    impl Forward for Calls {
        fn forward(&self, call: Call) {
            self.0.lock().unwrap().push(call);
        }
    }

    /// A host as `run` makes it, offline, in a folder of its own; `trust` decides who may connect.
    struct Test {
        host: Host,
        q: Queues,
        shown: Arc<Mutex<Vec<Status>>>,
        dir: std::path::PathBuf,
        calls: Arc<Calls>,
    }

    impl Test {
        fn new(name: &str, can_host: bool, trust: bool) -> Test {
            let dir = std::env::temp_dir().join(format!("deskway-signal-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("host.json"), r#"{"server":"https://s.example","id":"H1","token":"t","name":"Desk"}"#).unwrap();
            let cfg = Config::load_from(&dir).unwrap();
            let calls = Arc::new(Calls::default());
            let platform: Arc<dyn Platform> = calls.clone();
            let (control, control_rx) = channel();
            let (settings, _) = Settings::start(dir.clone(), &cfg, platform.clone(), control);
            let (_events_tx, events) = channel();
            let shown = Arc::new(Mutex::new(Vec::new()));
            let to = shown.clone();
            let trust = trust.then(|| Trust::open(dir.clone(), &cfg.id, &cfg.server).unwrap());
            let p = Params { cfg, view_only: false, platform, events, can_host: Box::new(move || can_host), notify: Box::new(move |s| to.lock().unwrap().push(s)), settings: Arc::new(settings), workspace: None, admin: None };
            let (host, q) = Host::new(p, trust, control_rx);
            Test { host, q, shown, dir, calls }
        }

        /// What went to the server since the last call.
        fn sent(&self) -> Vec<Value> {
            self.q.out.try_iter().map(|m| serde_json::from_str(&m).unwrap()).collect()
        }

        fn offer(&mut self, auth: Value) -> Value {
            self.host.on_message(&json!({ "type": "offer", "session": "s1", "epoch": 0, "sdp": "v=0", "ice": [], "view": { "w": 1, "h": 1, "maxW": 1, "maxH": 1, "fps": 30 }, "user": "u", "auth": auth }).to_string());
            self.sent().pop().expect("an answer to the offer")
        }

        /// A session holding the computer, established, from the device `cred`, and what it is
        /// told.
        fn holding(&mut self, cred: &str) -> Receiver<SessionEvent> {
            let (tx, rx) = channel();
            self.host.sessions.of_mut(Seat::Desk).start(Entry { id: "s1".into(), name: "Ann".into(), user: "u".into(), controller: cred.into(), since: 0, handle: tx });
            self.host.sessions.of_mut(Seat::Desk).established("s1");
            rx
        }

        /// A device paired here ("Phone", credential `PHONE`), signing offers the way a browser does.
        fn pair_phone(&self) -> p256::ecdsa::SigningKey {
            use base64::Engine;
            use p256::pkcs8::EncodePublicKey;
            let key = p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
            let spki = base64::engine::general_purpose::STANDARD.encode(key.verifying_key().to_public_key_der().unwrap().as_bytes());
            std::fs::write(self.dir.join("controllers.json"), json!([{ "id": PHONE, "alg": -7, "key": spki, "name": "Phone", "added": 1 }]).to_string()).unwrap();
            key
        }

        /// A signed offer for `session` from the phone: challenge, nonce, offer. Returns what
        /// went to the server after the nonce.
        fn signed_offer(&mut self, key: &p256::ecdsa::SigningKey, session: &str, force: bool) -> Vec<Value> {
            self.offer_from(key, PHONE, session, force, "v=0")
        }

        /// The same from any device here: `cred` signing with `key`, offering `sdp`.
        fn offer_from(&mut self, key: &p256::ecdsa::SigningKey, cred: &str, session: &str, force: bool, sdp: &str) -> Vec<Value> {
            use base64::Engine;
            use p256::ecdsa::signature::Signer;
            self.host.on_message(&json!({ "type": "challenge", "session": session }).to_string());
            let nonce = self.sent().pop().expect("a nonce")["nonce"].as_str().unwrap().to_string();
            let challenge = wire::crypto::sha256(wire::challenge::connect_v3("H1", session, &nonce, force, sdp).as_bytes());
            let sig: p256::ecdsa::Signature = key.sign(&challenge);
            let auth = json!({ "k": "dk", "nonce": nonce, "cred": cred, "sig": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_bytes()) });
            self.host.on_message(&json!({ "type": "offer", "session": session, "epoch": 0, "sdp": sdp, "ice": [], "view": { "w": 1, "h": 1, "maxW": 1, "maxH": 1, "fps": 30 }, "user": "u2", "force": force, "auth": auth }).to_string());
            self.sent()
        }

        /// The phone and two AI clients paired here (in the file, as a browser's endorsement
        /// would leave them): their keys, phone first.
        fn pair_all(&self) -> [p256::ecdsa::SigningKey; 3] {
            use base64::Engine;
            use p256::pkcs8::EncodePublicKey;
            let keys = [7u8, 8, 9].map(|b| p256::ecdsa::SigningKey::from_slice(&[b; 32]).unwrap());
            let spki = |k: &p256::ecdsa::SigningKey| base64::engine::general_purpose::STANDARD.encode(k.verifying_key().to_public_key_der().unwrap().as_bytes());
            let list = json!([
                { "id": PHONE, "alg": -7, "key": spki(&keys[0]), "name": "Phone", "added": 1 },
                { "id": CLAUDE, "alg": -7, "key": spki(&keys[1]), "name": "Claude", "added": 1, "role": "ai" },
                { "id": CURSOR, "alg": -7, "key": spki(&keys[2]), "name": "Cursor", "added": 1, "role": "ai" },
            ]);
            std::fs::write(self.dir.join("controllers.json"), list.to_string()).unwrap();
            keys
        }

        /// The phone signs this computer's AI switch, and the server delivers it.
        fn switch(&mut self, phone: &p256::ecdsa::SigningKey, ai: bool, ts: i64) {
            use base64::Engine;
            use p256::ecdsa::signature::Signer;
            let sig: p256::ecdsa::Signature = phone.sign(&wire::crypto::sha256(wire::challenge::ai_v1("H1", ai, false, ts).as_bytes()));
            let sw = json!({ "host": "H1", "ai": ai, "admin": false, "ts": ts, "by": PHONE, "sig": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_bytes()) });
            self.host.trust.as_mut().unwrap().reload();
            self.host.on_message(&json!({ "type": "devlog", "auth": [], "ai": sw }).to_string());
        }

        /// A session holding `seat`, established, from the device `cred`, and what it is told.
        fn seated(&mut self, seat: Seat, id: &str, cred: &str) -> Receiver<SessionEvent> {
            let (tx, rx) = channel();
            self.host.sessions.of_mut(seat).start(Entry { id: id.into(), name: cred.into(), user: "u".into(), controller: cred.into(), since: 0, handle: tx });
            self.host.sessions.of_mut(seat).established(id);
            rx
        }
    }

    const CLAUDE: &str = "Q0xBVURF";
    const CURSOR: &str = "Q1VSU09S";

    const PHONE: &str = "UEhPTkU";

    fn ended(rx: &Receiver<SessionEvent>) -> Option<End> {
        match rx.try_recv() {
            Ok(SessionEvent::End(end)) => Some(end),
            _ => None,
        }
    }

    fn reason(rx: &Receiver<SessionEvent>) -> Option<Reason> {
        ended(rx).map(|e| e.reason)
    }

    fn statuses(sent: &[Value]) -> Vec<&Value> {
        sent.iter().filter(|m| m["type"] == "status").collect()
    }

    #[test]
    fn offers_are_refused_with_the_reason_the_browser_shows() {
        let mut t = Test::new("refuse", true, false);
        assert_eq!(t.offer(json!({})), json!({ "type": "bye", "session": "s1", "reason": "auth" }), "no trust store: nobody is let in");
        let mut t = Test::new("unpaired", true, true);
        assert_eq!(t.offer(json!({}))["reason"], "unpaired", "no device was ever paired");
        let mut t = Test::new("permissions", false, true);
        assert_eq!(t.offer(json!({}))["reason"], "permissions");
    }

    #[test]
    fn an_offer_from_a_device_paired_here_with_a_wrong_proof_is_auth_else_unpaired() {
        let mut t = Test::new("proof", true, true);
        t.pair_phone();
        assert_eq!(t.offer(json!({ "k": "dk", "nonce": "n", "cred": PHONE, "sig": "AAAA" }))["reason"], "auth", "a known device whose proof fails");
        assert_eq!(t.offer(json!({ "k": "dk", "nonce": "n", "cred": "other", "sig": "AAAA" }))["reason"], "unpaired");
        assert_eq!(t.offer(json!({ "k": "rs", "nonce": "n", "id": "gone", "mac": "AAAA" }))["reason"], "auth", "a resume token that expired");
    }

    #[test]
    fn a_busy_computer_names_its_holder() {
        let mut t = Test::new("busy", true, true);
        let key = t.pair_phone();
        let _rx = t.holding("c1");
        let sent = t.signed_offer(&key, "s2", false);
        let bye = sent.iter().find(|m| m["type"] == "bye").expect("refused");
        assert_eq!(bye["reason"], "busy");
        assert_eq!(bye["holder"], json!({ "session": "s1", "cred": "c1", "name": "Ann", "user": "u", "established": true, "since": 0 }));
        assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "s1", "the holder keeps the computer");
    }

    #[test]
    fn a_takeover_ends_the_holder_and_the_status_waits_for_its_goodbye() {
        let mut t = Test::new("takeover", true, true);
        let key = t.pair_phone();
        let rx = t.holding("c1");
        let sent = t.signed_offer(&key, "s2", true);
        assert_eq!(ended(&rx), Some(End { reason: Reason::Taken, by: Some("Phone".into()), same: false }), "another device took it");
        assert!(statuses(&sent).is_empty(), "no status while the replaced session is still saying why it ended");
        assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "s2");
        t.host.on_update(SessionUpdate::Ended("s1".into()));
        let sent = t.sent();
        let status = statuses(&sent);
        assert_eq!(status.len(), 1, "{sent:?}");
        assert_eq!(status[0]["holder"]["session"], "s2");
        assert_eq!(status[0]["holder"]["established"], false, "the new holder is named while connecting");
        assert_eq!(status[0]["holder"]["cred"], PHONE);

        // The same device taking over from itself is told so.
        let mut t = Test::new("takeover-same", true, true);
        let key = t.pair_phone();
        let rx = t.holding(PHONE);
        t.signed_offer(&key, "s2", true);
        assert_eq!(ended(&rx), Some(End { reason: Reason::Taken, by: Some("Phone".into()), same: true }));
    }

    #[test]
    fn a_repeated_offer_goes_to_the_session_that_holds_the_computer() {
        let mut t = Test::new("repeat", true, true);
        let rx = t.holding("c1");
        t.host.on_message(&json!({ "type": "offer", "session": "s1", "epoch": 0, "sdp": "v=0", "ice": [], "view": { "w": 1, "h": 1, "maxW": 1, "maxH": 1, "fps": 30 }, "user": "u", "auth": {} }).to_string());
        assert!(matches!(rx.try_recv(), Ok(SessionEvent::Negotiate { epoch: 0, .. })), "answered again by the session, not admitted anew");
        assert!(t.sent().is_empty(), "no refusal, no new nonce used");
        t.host.on_message(&json!({ "type": "restart", "session": "s1", "epoch": 3, "sdp": "v=0" }).to_string());
        assert!(matches!(rx.try_recv(), Ok(SessionEvent::Negotiate { epoch: 3, .. })));
        t.host.on_message(&json!({ "type": "bye", "session": "s1", "reason": "failed" }).to_string());
        assert!(matches!(rx.try_recv(), Ok(SessionEvent::PeerBye(Reason::Failed))), "the browser's bye with its reason");
    }

    #[test]
    fn the_status_names_the_holder_until_it_ends() {
        let mut t = Test::new("status", true, true);
        let _rx = t.holding("c1");
        t.host.send_status();
        let sent = t.sent();
        assert_eq!(statuses(&sent)[0]["holder"], json!({ "session": "s1", "cred": "c1", "name": "Ann", "user": "u", "established": true, "since": 0 }));
        t.host.on_update(SessionUpdate::Ended("s1".into()));
        let sent = t.sent();
        assert_eq!(statuses(&sent).len(), 1);
        assert!(statuses(&sent)[0].get("holder").is_none(), "nobody holds it: no holder");
    }

    #[test]
    fn the_person_here_ends_the_session_with_its_reason() {
        let mut t = Test::new("end", true, true);
        let rx = t.holding("c1");
        t.host.on_control(Control::Disconnect);
        assert_eq!(reason(&rx), Some(Reason::Ended));
        t.host.on_control(Control::Quit);
        assert_eq!(reason(&rx), Some(Reason::Ended));
        assert!(t.host.quit);
        // Removing every device takes it from the one in control too.
        let rx = t.holding("c1");
        t.host.on_control(Control::RemoveDevices(None));
        assert_eq!(reason(&rx), Some(Reason::Unpaired));
    }

    #[test]
    fn the_panel_hears_who_controls_and_what_changed() {
        let mut t = Test::new("report", true, true);
        let _rx = t.holding("c1");
        t.host.on_control(Control::Report);
        let s = t.shown.lock().unwrap().pop().expect("a status");
        assert_eq!((s.controller.as_deref(), s.name.as_str(), s.online), (Some("Ann"), "Desk", false));
        // Settings saved meanwhile show as saved.
        Config::update_in(&t.dir, |c| c.prevent_sleep = true).unwrap();
        t.host.on_control(Control::Report);
        assert!(t.shown.lock().unwrap().pop().unwrap().prevent_sleep);
        // The browser's clipboard switch is saved like the panel's, and comes back to the host.
        t.host.browser_set_clip_share(true);
        assert!(Config::load_from(&t.dir).unwrap().clip_share);
        assert!(t.q.control.try_iter().any(|c| c == Control::SetClipShare(true)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ai_facts_use_the_name_shown_when_the_mac_name_is_left_blank() {
        let mut t = Test::new("mac-name", true, true);
        t.host.on_control(Control::Report);
        assert_eq!(t.host.facts.read().unwrap().name, "Desk");
        Config::update_in(&t.dir, |c| c.name.clear()).unwrap();
        t.host.on_control(Control::Report);
        let name = t.host.facts.read().unwrap().name.clone();
        assert_eq!(name, crate::shown_name(""));
        assert!(!name.is_empty());
    }

    #[test]
    fn platform_events_go_to_the_session_the_host_or_both() {
        let (events_tx, events) = channel();
        let (router, host) = Router::spawn(events);
        // No session yet: its events are dropped, the host's still arrive.
        events_tx.send(PlatformEvent::Frame(Bytes(vec![1]))).unwrap();
        events_tx.send(PlatformEvent::DisplaysChanged(Vec::new())).unwrap();
        assert_eq!(host.recv_timeout(Duration::from_secs(5)).unwrap(), PlatformEvent::DisplaysChanged(Vec::new()));

        let (session_tx, session) = channel();
        *router.desk.lock().unwrap() = Some(session_tx);
        let d = DisplayInfo { id: "A".into(), x: 0, y: 0, w: 10, h: 10, primary: true };
        events_tx.send(PlatformEvent::Desktop(Desk::Winlogon)).unwrap();
        events_tx.send(PlatformEvent::Capturing { list: vec![d.clone()], current: d.clone() }).unwrap();
        events_tx.send(PlatformEvent::Thumbnail { display: "A".into(), jpeg: Bytes(vec![0xff, 0xd8]) }).unwrap();
        let to_session: Vec<PlatformEvent> = (0..2)
            .map(|_| match session.recv_timeout(Duration::from_secs(5)).unwrap() {
                SessionEvent::Platform(e) => e,
                _ => panic!("not a platform event"),
            })
            .collect();
        assert_eq!(to_session, vec![PlatformEvent::Desktop(Desk::Winlogon), PlatformEvent::Capturing { list: vec![d.clone()], current: d.clone() }]);
        // The input desktop is the host's too: the AI side reads it from the host's facts.
        assert_eq!(host.recv_timeout(Duration::from_secs(5)).unwrap(), PlatformEvent::Desktop(Desk::Winlogon));
        assert_eq!(host.recv_timeout(Duration::from_secs(5)).unwrap(), PlatformEvent::Capturing { list: vec![d.clone()], current: d });
        assert!(matches!(host.recv_timeout(Duration::from_secs(5)).unwrap(), PlatformEvent::Thumbnail { .. }));
        assert!(session.try_recv().is_err(), "thumbnails are the host's alone");
    }

    /// (docs/session-lifecycle.md, E cases) What an AI client's offer meets, in order: AI off,
    /// media or force, the seat held by another AI client, the person on the desk seat.
    #[test]
    fn an_ai_client_takes_the_ai_seat_only_while_ai_is_on_and_beside_the_person() {
        let mut t = Test::new("ai-seat", true, true);
        let [phone, claude, cursor] = t.pair_all();
        // The bye for one session. An admitted session runs on its own thread and may end at any
        // time (these offers carry no usable SDP), so its bye can arrive with a later offer's answer.
        let bye = |sent: &[Value], session: &str| sent.iter().find(|m| m["type"] == "bye" && m["session"] == session).cloned();
        let reason = |sent: &[Value], session: &str| bye(sent, session).map(|m| m["reason"].as_str().unwrap().to_string());
        // E1: never allowed.
        assert_eq!(reason(&t.offer_from(&claude, CLAUDE, "a1", false, "v=0"), "a1").as_deref(), Some("ai_off"));
        t.switch(&phone, true, 10);
        // Never with media, never taking over.
        assert_eq!(reason(&t.offer_from(&claude, CLAUDE, "a2", false, "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\n"), "a2").as_deref(), Some("forbidden"));
        assert_eq!(reason(&t.offer_from(&claude, CLAUDE, "a3", true, "v=0"), "a3").as_deref(), Some("forbidden"));
        // E5: the person holds the desk seat; the AI client takes the AI seat beside them.
        let _person = t.seated(Seat::Desk, "d1", PHONE);
        let sent = t.offer_from(&claude, CLAUDE, "a4", false, "v=0");
        let status = statuses(&sent).pop().cloned().expect("a status");
        assert_eq!((status["holder"]["session"].as_str(), status["ai_session"]["session"].as_str(), status["ai_state"].as_str()), (Some("d1"), Some("a4"), Some("on")));
        // E2: another AI client is busy, told who holds it.
        let sent = t.offer_from(&cursor, CURSOR, "a5", false, "v=0");
        let busy = bye(&sent, "a5").expect("a bye for a5");
        assert_eq!((busy["reason"].as_str(), busy["holder"]["session"].as_str()), (Some("busy"), Some("a4")));
        // The same AI client again (its deskway started again) takes its own place, though its
        // old session is established.
        t.host.sessions.of_mut(Seat::Ai).established("a4");
        let sent = t.offer_from(&claude, CLAUDE, "a6", false, "v=0");
        assert!(reason(&sent, "a6").is_none(), "admitted: {sent:?}");
        assert_eq!((t.host.sessions.of(Seat::Ai).current().unwrap().id.as_str(), t.host.replaced.as_slice()), ("a6", ["a4".to_string()].as_slice()));
        assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "d1", "the person's session is untouched");
    }

    /// Where a browser or AI client runs: this computer answers `here` when that is itself, and
    /// says nothing otherwise (what counts as itself is `peer::here`'s).
    #[test]
    fn a_locate_naming_this_computer_is_answered_here_and_no_other() {
        let mut t = Test::new("locate", true, true);
        let me = peer::here::machine().expect("this machine's locator");
        for (place, want) in [(vec!["x.example".to_string(), me], Some(json!({ "type": "here", "probe": "p1" }))), (vec!["machine:00".into()], None)] {
            t.host.on_message(&json!({ "type": "locate", "probe": "p1", "where": place }).to_string());
            let got = t.q.out.recv_timeout(Duration::from_millis(1500)).ok().map(|m| serde_json::from_str::<Value>(&m).unwrap());
            assert_eq!(got, want, "{place:?}");
        }
    }

    /// Stop at the computer, the switch turned off, the AI client removed: each ends the AI seat
    /// with its reason and leaves the person's session alone; locking follows the person only.
    #[test]
    fn stop_switch_off_and_removal_end_the_ai_seat_and_only_it() {
        // (what happens, the AI session's end)
        let cases: [(&str, fn(&mut Test, &p256::ecdsa::SigningKey), Reason); 3] = [
            ("stop at the computer", |t, _| t.host.on_control(Control::StopAi), Reason::AiOff),
            ("the switch turned off", |t, phone| t.switch(phone, false, 11), Reason::AiOff),
            ("the AI client removed here", |t, _| t.host.on_control(Control::RemoveDevices(Some(vec![CLAUDE.into()]))), Reason::Unpaired),
        ];
        for (i, (what, act, why)) in cases.into_iter().enumerate() {
            let mut t = Test::new(&format!("ai-end-{i}"), true, true);
            let [phone, ..] = t.pair_all();
            t.switch(&phone, true, 10);
            let person = t.seated(Seat::Desk, "d1", PHONE);
            let ai = t.seated(Seat::Ai, "a1", CLAUDE);
            act(&mut t, &phone);
            assert_eq!(reason(&ai), Some(why), "{what}");
            assert_eq!(reason(&person), None, "{what}: the person's session goes on");
        }
        let mut t = Test::new("ai-stopped", true, true);
        let [phone, ..] = t.pair_all();
        t.switch(&phone, true, 10);
        t.host.on_control(Control::StopAi);
        assert_eq!(statuses(&t.sent()).pop().unwrap()["ai_state"], "stopped");
        assert_eq!(t.shown.lock().unwrap().last().unwrap().ai_state, "stopped", "the panel shows it");

        let mut t = Test::new("ai-lock", true, true);
        t.host.auto_lock = true;
        let _ai = t.seated(Seat::Ai, "a1", CLAUDE);
        let _person = t.seated(Seat::Desk, "d1", PHONE);
        t.host.on_update(SessionUpdate::Ended("a1".into()));
        assert!(!t.calls.0.lock().unwrap().contains(&Call::Lock {}), "an AI client leaving never locks the person out");
        t.host.on_update(SessionUpdate::Ended("d1".into()));
        assert!(t.calls.0.lock().unwrap().contains(&Call::Lock {}), "the person's session ending locks, as set");
    }
}
