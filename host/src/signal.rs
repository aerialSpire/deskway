//! Signaling connection to the server, with automatic reconnect, the host's status, and the
//! two seats (`seats`, each a `registry`): a person's browser on the desk seat, an AI client on
//! the AI seat, side by side (docs/architecture.md, "Two seats side by side"). The host decides who holds each: on the
//! desk seat one session, replaced only by the same device's next attempt while it is still
//! connecting, or by a signed `force` (docs/session-lifecycle.md, "Admission"); on the AI seat
//! one AI client, while a browser of the account allows AI here, replaced by the same device's
//! next attempt in any state and by nothing else. Losing the server ends no session: after
//! reconnecting, the status names the holders again so the server can route them.

use std::io::ErrorKind;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use deskway_server_connection::Address;
use polling::{Event, Events, Poller};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message as WsMessage, WebSocket};

use crate::audit::Audit;
use crate::auth::{AiState, Trust};
use crate::config::Config;
use crate::discovery::{Browse, Change};
use crate::display::DisplayInfo;
use crate::mcp::{Facts, SharedFacts};
use crate::platform::{BrowserSession, Desk, DeskParams, DesktopFeatures, Features, Platform, PlatformEvent, Replies, WorkspaceState};
use wire::control::Control as Message;
use wire::signal::Signal as Wire;
use wire::types::{self as types, Reason};
use peer::state::End;
use crate::session::{self, SessionEvent, SessionUpdate};
use crate::session::terminal::registry::Registry as Terminals;
use crate::settings::{PanelRequest, Settings};
use crate::vitals::{self, Sample};
use crate::wake::{Wake, Waker, WeakWake};

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
    WorkspaceChanged(WorkspaceState),
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
    /// The pinned local server announced itself while signaling was offline.
    ServerAnnounced,
    Quit,
}

/// Sends each platform event where it belongs: to the desk seat's session, to the host, or to
/// both. (The AI seat's session hears nothing from the platform yet: the AI side reads the
/// host's `Facts`.)
struct Router {
    desk: Mutex<Option<Sender<SessionEvent>>>,
    host: Wake<PlatformEvent>,
    /// Answers under an id go to whoever asked (`Replies`), before anything else.
    replies: Arc<Replies>,
}

impl Router {
    /// (for the desk session, for the host). Every event is placed here, so a new one cannot be
    /// forgotten.
    fn targets(e: &PlatformEvent) -> (bool, bool) {
        match e {
            PlatformEvent::Frame(_)
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
            PlatformEvent::Snapshot { .. } | PlatformEvent::SnapshotFailed { .. } | PlatformEvent::InputDone { .. } | PlatformEvent::ServerRootSynced { .. } => (false, false),
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
    fn spawn(events: Receiver<PlatformEvent>, wake: &Waker) -> (Arc<Router>, Receiver<PlatformEvent>) {
        let (host, host_rx) = wake.channel();
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


pub struct DesktopParams {
    pub features: DesktopFeatures,
    pub platform: Arc<dyn Platform>,
    /// Everything the platform reports.
    pub events: Receiver<PlatformEvent>,
    /// Whether this computer can take a session now (macOS: the needed permissions).
    pub can_host: Box<dyn Fn() -> bool + Send>,
}

struct Desktop {
    features: DesktopFeatures,
    platform: Arc<dyn Platform>,
    router: Arc<Router>,
    can_host: Box<dyn Fn() -> bool + Send>,
}

/// What a host needs to run.
pub struct Params {
    pub cfg: Config,
    pub view_only: bool,
    pub desktop: Option<DesktopParams>,
    pub browser: Option<crate::workspace::Browser>,
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
    share_tx: Wake<bool>,
    online: bool,
    waiting: bool,
    /// Why the last attempt to reach the server failed, in words for the panel.
    problem: Option<String>,
    quit: bool,
    desktop: Option<Desktop>,
    terminal: Option<Terminals>,
    browser: Option<crate::workspace::Browser>,
    workspace_state: WorkspaceState,
    transfers: session::file_transfer::Registry,
    machine: Option<types::Machine>,
    sampling: Option<Receiver<Option<Sample>>>,
    out_tx: Wake<String>,
    updates_tx: Wake<SessionUpdate>,
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
    fn workspace_access(&self) -> Option<Reason> {
        self.browser.as_ref()?;
        self.workspace_state.refusal(self.view_only)
    }

    fn workspace_changed(&mut self, next: WorkspaceState) {
        if self.browser.is_none() { return; }
        let previous = std::mem::replace(&mut self.workspace_state, next);
        if previous == next {
            return;
        }
        let changed_user = previous.user.is_some() && previous.user != next.user;
        let reason = if changed_user && next.user.is_some() { Some(Reason::UserChanged) }
            else { next.refusal(self.view_only) };
        if let (Some(terminal), Some(reason)) = (&self.terminal, reason) {
            if let Some(id) = self.sessions.of(Seat::Desk).current().filter(|cur| terminal.holds(&cur.id)).map(|cur| cur.id.clone()) {
                self.end_seat(Seat::Desk, End::plain(reason));
                self.sessions.of_mut(Seat::Desk).ended(&id);
                self.replaced.push(id);
            }
        }
        if changed_user {
            self.transfers.clear();
            if let Some(terminal) = &self.terminal {
                terminal.user_changed();
            }
        }
        self.send_status();
    }

    fn features(&self) -> Features {
        Features {
            desktop: self.desktop.as_ref().map(|d| d.features), terminal: self.browser.is_some(),
            work: self.workspace.is_some(), browse: self.browser.is_some(),
            file_transfers: self.browser.is_some(), view_only: self.view_only,
        }
    }

    fn hello(&self) -> Wire {
        let features = self.features();
        let mut caps = features.signaling_caps();
        if self.machine.is_some() {
            caps.push(wire::caps::VITALS.into());
        }
        let tools = features.tools().map(String::from).collect();
        Wire::Hello {
            version: wire::PROTOCOL_VERSION, caps, tools: Some(tools),
            app: Some(env!("CARGO_PKG_VERSION").into()), machine: self.machine.clone(),
        }
    }

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
            name: crate::platform::facts_name(name),
            os: std::env::consts::OS.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            displays: self.displays.clone(),
            desk: self.desk,
            person: self.sessions.of(Seat::Desk).controller(),
            admin: self.desktop.as_ref().map(|_| matches!(self.trust.as_ref().map(Trust::ai_state), Some(AiState::On { admin: true }))),
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
        if let Some(desk) = &self.desktop {
            (desk.features.ai_awake)(self.sessions.of(Seat::Ai).current().is_some());
        }
        if self.replaced.is_empty() {
            let (holder, ai) = (self.sessions.of(Seat::Desk).holder(), self.sessions.of(Seat::Ai).holder());
            let ai_refused = self.trust.as_ref().and_then(Trust::ai_refused);
            let kept_terminal = self.terminal.as_ref().and_then(Terminals::status);
            self.send(&Wire::Status { displays: self.displays.clone(), holder, ai, ai_state: Some(self.ai_state()), ai_refused, kept_terminal, workspace_access: self.workspace_access() });
        }
        self.report();
    }

    /// Ends the session on `seat`; the session tells its other end why. A controller taken over
    /// or ended at this computer does not come back by itself: its resume tokens go (only its own).
    fn end_seat(&mut self, seat: Seat, end: End) {
        let Some(cur) = self.sessions.of(seat).current() else { return };
        if seat == Seat::Desk {
            if let Some(terminal) = &self.terminal {
                terminal.end(&cur.id, end.reason.as_str());
            }
        }
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
        let data_only = session::data::data_only(&sdp);
        let lacks_permissions = || self.desktop.as_ref().is_some_and(|d| !(d.can_host)());
        if !data_only && lacks_permissions() {
            eprintln!("session refused: this computer cannot host one now");
            return self.bye(&session, Reason::Permissions);
        }
        let features = self.features();
        let workspace_access = self.workspace_access();
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
        let browser = if seat == Seat::Desk {
            match features.browser_session(data_only) {
                Ok(BrowserSession::Data) => {
                    if let Some(reason) = workspace_access {
                        return self.bye(&session, reason);
                    }
                    Some(BrowserSession::Data)
                }
                Ok(mode) => Some(mode),
                Err(reason) => return self.bye(&session, reason),
            }
        } else {
            None
        };
        let data_user = if matches!(browser, Some(BrowserSession::Data)) {
            let Some(user) = self.workspace_state.user else { return self.bye(&session, Reason::NoUserSession); };
            let Some(files) = self.browser.as_ref().and_then(|b| (b.bind)(user)) else { return self.bye(&session, Reason::NoUserSession); };
            Some((user, files))
        } else {
            None
        };
        if seat == Seat::Ai {
            if data_only && lacks_permissions() {
                return self.bye(&session, Reason::Permissions);
            }
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
        let returning = data_only && seat == Seat::Desk && self.sessions.of(seat).current()
            .is_some_and(|c| self.transfers.is_returning(&verified.controller, &verified.page, &c.id));
        match self.sessions.of(seat).admit(&verified.controller, force || returning) {
            Admit::Busy { holder } => {
                self.send(&Wire::Bye { session, reason: Reason::Busy, by: None, same: None, holder: Some(holder) });
                return;
            }
            Admit::TakeOver | Admit::Replace => {
                let old = self.sessions.of(seat).current().map(|c| (c.id.clone(), c.controller.clone()));
                if let Some((id, cred)) = old {
                    let same = !cred.is_empty() && cred == verified.controller;
                    let end = if returning { End::plain(Reason::Failed) }
                        else { End { reason: Reason::Taken, by: Some(name.clone()), same } };
                    self.end_seat(seat, end);
                    self.replaced.push(id);
                }
            }
            Admit::Free => {}
        }
        let Some(trust) = self.trust.as_mut() else { return };
        // Issued after a takeover revoked the previous controller's tokens: the same device
        // taking over from itself keeps this one. An AI client signs without asking anyone, so
        // it gets no resume token.
        let greeting = match browser {
            Some(mode) => {
                let caps = match mode {
                    BrowserSession::Desktop(_) => features.desk_caps(),
                    BrowserSession::Data => features.data_caps(),
                };
                vec![Message::Hello { version: wire::PROTOCOL_VERSION, caps }, trust.issue_resume(&verified)]
            }
            None => vec![session::ai::hello()],
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
        let tx = match browser {
            Some(BrowserSession::Desktop(features)) => {
                let desktop = self.desktop.as_ref().expect("a desk seat was admitted");
                let tx = (features.spawn)(params, DeskParams {
                    session: session.clone(),
                    offer: sdp,
                    view: serde_json::from_value(m["view"].clone()).ok(),
                    display: m["display"].as_str().map(str::to_owned),
                    platform: desktop.platform.clone(),
                    view_only: self.view_only,
                    clip_share: self.clip_share,
                    share: self.share_tx.clone(),
                });
                *desktop.router.desk.lock().unwrap() = Some(tx.clone());
                self.note = None;
                tx
            }
            Some(BrowserSession::Data) => {
                let (user, browser) = data_user.expect("data session has a workspace");
                let lease = self.terminal.as_ref().expect("a terminal was admitted")
                    .admit_user(session.clone(), name.clone(), verified.controller.clone(), user);
                let page = self.transfers.admit(&verified.controller, &verified.page, &session, verified.resume_until());
                session::spawn(params, |own| session::data::Data::new(lease, browser, page, own))
            }
            None => {
                let gate = crate::mcp::gate::Gate { audit: self.audit.clone(), client: name.clone(), device: verified.controller.clone(), session: session.clone() };
                let tunnels = crate::tools::tunnel::Allowed::default();
                let platform = self.desktop.as_ref().map(|d| d.platform.clone());
                if let Some(platform) = &platform {
                    // Its input is the AI client's hand from now on (`platform::local`).
                    platform.ai_session(Some(session.clone()));
                }
                let ctx = crate::tools::Ctx {
                    facts: self.facts.clone(), session: session.clone(),
                    platform: platform.clone(),
                    replies: self.desktop.as_ref().map(|d| d.router.replies.clone()).unwrap_or_default(),
                    shot: Default::default(), workspace: self.workspace.clone(), tunnels: tunnels.clone(),
                    admin: self.admin.clone(), audit: self.audit.clone(),
                };
                let id = session.clone();
                session::spawn(params, move |own| session::ai::Ai::new(id, platform, crate::mcp::spawn(features, gate, ctx, own.clone()), session::ai::IDLE, tunnels, own))
            }
        };
        if !matches!(browser, Some(BrowserSession::Data)) {
            eprintln!("session started by {name} ({seat:?} seat)");
        }
        let user = m["user"].as_str().unwrap_or_default().to_string();
        // Holding the seat, still connecting: the status names it so from now on.
        self.sessions.of_mut(seat).start(Entry { id: session, name, user, controller: verified.controller.clone(), since: crate::auth::unix_now(), handle: tx });
        self.send_status();
    }

    /// Returns whether the message requests a vitals sample.
    fn on_message(&mut self, text: &str) -> bool {
        let Ok(m) = serde_json::from_str::<Value>(text) else { return false };
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
            "pair" if self.desktop.is_some() => self.on_pair(&m),
            "pairask" if self.desktop.is_some() => self.on_ask(&m),
            "pair" | "pairask" => self.bye(session, Reason::NoDesktop),
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
            "thumbs" => {
                if let Ok(Wire::Thumbs { on, vitals }) = serde_json::from_value(m) {
                    if let Some(desk) = &self.desktop {
                        desk.platform.thumbnails(on);
                    }
                    return on && vitals == Some(true) && self.machine.is_some();
                }
            }
            // Who this computer belongs to, for the panel.
            "account" => {
                if let Ok(Wire::Account { user, display }) = serde_json::from_value::<Wire>(m.clone()) {
                    let _ = self.settings.set_account(crate::config::Account { user, display });
                    self.send_status();
                }
            }
            "locate" => self.on_locate(&m),
            "relay-list" => self.on_relay_list(m),
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
        false
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

    fn on_relay_list(&self, m: Value) {
        let Ok(Wire::RelayList { probe, relays }) = serde_json::from_value::<Wire>(m) else { return };
        let out = self.out_tx.clone();
        let _ = std::thread::Builder::new().name("relay-rtt".into()).spawn(move || {
            let addresses: Vec<_> = relays.into_iter().filter_map(|(id, addr)| Some((id, addr.parse().ok()?))).collect();
            let times = peer::stun::measure(addresses.iter().map(|(_, addr)| *addr));
            let rtts = addresses.into_iter().filter_map(|(id, addr)| Some((id, times.get(&addr)?.as_millis() as u32))).collect();
            let _ = out.send(serde_json::to_string(&Wire::RelayRtts { probe, rtts }).expect("signal serializes"));
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
        self.transfers.forget_untrusted(|id| t.controllers().iter().any(|d| d.id == id));
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
            SessionUpdate::TerminalChanged => self.send_status(),
        }
    }

    fn on_ended(&mut self, session: &str) {
        if let Some(terminal) = &self.terminal {
            terminal.end(session, Reason::Failed.as_str());
        }
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
            if let Some(desk) = &self.desktop {
                match seat {
                    Seat::Desk => {
                        *desk.router.desk.lock().unwrap() = None;
                        self.note = None;
                    }
                    Seat::Ai => desk.platform.ai_session(None),
                }
            }
            self.send_status();
            // Locking follows the person's session only.
            if seat == Seat::Desk && was_controller && self.auto_lock && !self.quit {
                if let Some(desk) = &self.desktop {
                    desk.platform.lock();
                }
            }
        }
    }

    /// Events from the platform for the host itself. Thumbnails go out on `ws` when connected.
    fn on_platform(&mut self, e: PlatformEvent, ws: Option<&mut Socket>) -> Result<()> {
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
            Control::WorkspaceChanged(state) => self.workspace_changed(state),
            // The panel's Disconnect is the person's session.
            Control::Disconnect => self.end_seat(Seat::Desk, End::plain(Reason::Ended)),
            Control::SetAutoLock(on) => {
                self.auto_lock = on;
                if let Some(desk) = &self.desktop {
                    desk.platform.auto_lock(on);
                }
            }
            Control::SetClipShare(on) => self.set_clip_share(on),
            Control::SetAutoStart(on) => {
                if let Some(desk) = &self.desktop {
                    desk.platform.autostart(on);
                }
            }
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
            Control::ServerAnnounced => {}
            Control::Quit => {
                self.quit = true;
                if let Some(terminal) = &mut self.terminal {
                    terminal.stop();
                }
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

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn tcp(ws: &Socket) -> &TcpStream {
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::NativeTls(s) => s.get_ref(),
        _ => unreachable!("signaling uses plain TCP or native TLS"),
    }
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Default)]
struct Connecting {
    cancelled: bool,
    socket: Option<TcpStream>,
}

fn time_left(deadline: Instant) -> std::io::Result<Duration> {
    deadline.checked_duration_since(Instant::now()).filter(|left| !left.is_zero())
        .ok_or_else(|| std::io::Error::new(ErrorKind::TimedOut, "signaling connection timed out"))
}

fn connect(cfg: &Config, deadline: Instant, attempt: &Mutex<Connecting>) -> Result<Socket> {
    let ws = cfg.connection()?.websocket_with_tcp(&format!("/ws/host?id={}", cfg.id), |req| {
        req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.token).parse().context("token header")?);
        Ok(())
    }, |host, port| {
        let mut error = None;
        for addr in (host, port).to_socket_addrs().context("resolve signaling hostname")? {
            if attempt.lock().unwrap().cancelled {
                anyhow::bail!("signaling connection cancelled");
            }
            match TcpStream::connect_timeout(&addr, time_left(deadline)?) {
                Ok(stream) => {
                    let left = time_left(deadline)?;
                    stream.set_read_timeout(Some(left))?;
                    stream.set_write_timeout(Some(left))?;
                    let mut attempt = attempt.lock().unwrap();
                    anyhow::ensure!(!attempt.cancelled, "signaling connection cancelled");
                    attempt.socket = Some(stream.try_clone()?);
                    return Ok(stream);
                }
                Err(e) => error = Some(e),
            }
        }
        Err(error.context("signaling hostname resolved to no addresses")?.into())
    })?;
    tcp(&ws).set_write_timeout(Some(alive::WRITE_TIMEOUT))?;
    // A blocking TLS write can also need to read, so bound that wait too.
    tcp(&ws).set_read_timeout(Some(alive::WRITE_TIMEOUT))?;
    Ok(ws)
}

/// Wait on the control queue and the result together. Shutting down the registered socket
/// interrupts both TLS and HTTP upgrade, even if the peer keeps sending partial data.
fn await_connection(host: &mut Host, q: &Queues, timeout: Duration) -> Result<Option<Socket>> {
    let deadline = Instant::now() + timeout;
    let poller = Arc::new(Poller::new()?);
    q.wake.attach(&poller);
    let (done, result) = q.wake.channel();
    let attempt = Arc::new(Mutex::new(Connecting::default()));
    let worker = attempt.clone();
    let cfg = host.cfg.clone();
    std::thread::Builder::new().name("signal-connect".into()).spawn(move || {
        let _ = done.send(connect(&cfg, deadline, &worker));
    })?;

    let outcome = (|| {
        let mut events = Events::new();
        loop {
            let first = match q.control.try_recv() {
                Ok(control) => Some(control),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Control::Quit),
            };
            q.drain_local(host, first);
            if host.quit {
                return Ok(None);
            }
            let left = time_left(deadline)?;
            match result.try_recv() {
                Ok(result) => return result.map(Some),
                Err(TryRecvError::Disconnected) => anyhow::bail!("signaling connection worker stopped"),
                Err(TryRecvError::Empty) => {}
            }
            poller.wait(&mut events, Some(left))?;
        }
    })();
    let mut attempt = attempt.lock().unwrap();
    attempt.cancelled = true;
    if let Some(socket) = attempt.socket.take().filter(|_| !matches!(&outcome, Ok(Some(_)))) {
        let _ = socket.shutdown(Shutdown::Both);
    }
    outcome
}

struct Connection<'a> {
    ws: &'a mut Socket,
    poller: Arc<Poller>,
}

const READABLE: Event = Event::readable(0);

impl<'a> Connection<'a> {
    fn register(ws: &'a mut Socket, wake: &Waker) -> Result<Self> {
        let poller = Arc::new(Poller::new()?);
        // SAFETY: the connection borrows the socket until Drop deregisters it.
        unsafe { poller.add(tcp(ws), READABLE)? };
        let connection = Self { ws, poller };
        wake.attach(&connection.poller);
        Ok(connection)
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.poller.delete(tcp(self.ws)) {
            eprintln!("cannot deregister signaling socket: {e}");
        }
    }
}

/// The control receiver and the shared wake handle for this host's queues.
pub struct Controls {
    rx: Receiver<Control>,
    wake: Waker,
    sender: WeakWake<Control>,
}

pub fn control_channel() -> (Wake<Control>, Controls) {
    let wake = Waker::default();
    let (tx, rx) = wake.channel();
    let sender = tx.downgrade();
    (tx, Controls { rx, wake, sender })
}

struct Queues {
    wake: Waker,
    out: Receiver<String>,
    updates: Receiver<SessionUpdate>,
    platform: Option<Receiver<PlatformEvent>>,
    control: Receiver<Control>,
    share: Receiver<bool>,
}

impl Queues {
    fn drain_control(&self, host: &mut Host, first: Option<Control>) -> bool {
        let mut announced = false;
        for c in first.into_iter().chain(self.control.try_iter()) {
            announced |= c == Control::ServerAnnounced;
            host.on_control(c);
        }
        announced
    }

    fn on_message(&self, host: &mut Host, text: &str) -> bool {
        self.drain_control(host, None);
        !host.quit && host.on_message(text)
    }

    /// What can be handled without the server.
    fn drain_local(&self, host: &mut Host, first: Option<Control>) -> bool {
        host.transfers.expire(Instant::now());
        let announced = self.drain_control(host, first);
        while let Ok(u) = self.updates.try_recv() {
            host.on_update(u);
        }
        while let Ok(on) = self.share.try_recv() {
            host.browser_set_clip_share(on);
        }
        announced
    }

    fn wait_for_retry(&self, host: &mut Host, deadline: Instant) {
        while !host.quit {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let first = match self.control.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => Some(Control::Quit),
            };
            let announced = self.drain_local(host, first);
            // Displays and notes still count offline; thumbnails have nowhere to go.
            for e in self.platform.iter().flat_map(Receiver::try_iter) {
                let _ = host.on_platform(e, None);
            }
            if announced {
                break;
            }
        }
    }
}

fn serve(ws: &mut Socket, host: &mut Host, q: &Queues) -> Result<()> {
    let connection = Connection::register(ws, &q.wake)?;
    let ws = &mut *connection.ws;
    let poller = &connection.poller;
    let mut events = Events::new();
    ws.send(WsMessage::text(serde_json::to_string(&host.hello())?))?;
    host.send_status();
    let mut alive = alive::Alive::new(Instant::now());
    let mut sampling_here = false;
    let mut sample_requested = false;
    loop {
        tcp(ws).set_nonblocking(true)?;
        let read = match ws.read() {
            Ok(m) => {
                let now = Instant::now();
                alive.heard(now);
                match m {
                    WsMessage::Text(t) => {
                        sample_requested |= q.on_message(host, t.as_str()) && !sampling_here;
                    }
                    WsMessage::Pong(payload) => alive.pong(&payload, now),
                    _ => {}
                }
                true
            }
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => false,
            Err(e) => return Err(e.into()),
        };
        // Blocking sends and flushes use WRITE_TIMEOUT, including tungstenite's automatic
        // replies. This keeps the write deadline without juggling partial WebSocket or TLS writes.
        tcp(ws).set_nonblocking(false)?;
        let now = Instant::now();
        match alive.due(now) {
            alive::Due::Nothing => {}
            alive::Due::Ping => ws.send(WsMessage::Ping(alive.ping_payload(now).to_vec().into()))?,
            alive::Due::Gone => anyhow::bail!("the server said nothing for {}s", alive::SILENT.as_secs()),
        }
        q.drain_local(host, None);
        if let Some(samples) = &host.sampling {
            let result = samples.try_recv();
            if !matches!(result, Err(TryRecvError::Empty)) {
                host.sampling = None;
                // A sample from an old connection must never be sent on its replacement.
                if std::mem::take(&mut sampling_here) {
                    if let Ok(Some(sample)) = result {
                        ws.send(WsMessage::text(serde_json::to_string(&sample.message(alive.rtt()))?))?;
                    }
                }
            }
        }
        // If a worker survived a reconnect, wait for it before sampling for this socket.
        if sample_requested && host.sampling.is_none() {
            sample_requested = false;
            let (tx, rx) = q.wake.channel();
            if Sample::request(tx).is_ok() {
                host.sampling = Some(rx);
                sampling_here = true;
            }
        }
        for e in q.platform.iter().flat_map(Receiver::try_iter) {
            host.on_platform(e, Some(ws))?;
        }
        while let Ok(m) = q.out.try_recv() {
            ws.send(WsMessage::text(m))?;
        }
        ws.flush()?;
        if host.quit && !host.has_sessions() {
            return Ok(());
        }

        // Keep the same processing order for each message. Read again before sleeping because
        // tungstenite and native TLS can hold data after the underlying socket stops being readable.
        if !read {
            if !events.is_empty() {
                poller.modify(tcp(ws), READABLE)?;
                events.clear();
            }
            let deadline = alive.next_deadline();
            let deadline = host.transfers.next_deadline().map_or(deadline, |t| t.min(deadline));
            poller.wait(&mut events, Some(deadline.saturating_duration_since(Instant::now())))?;
        }
    }
}

impl Host {
    fn has_sessions(&self) -> bool {
        !self.replaced.is_empty() || Seat::BOTH.iter().any(|s| self.sessions.of(*s).current().is_some())
    }

    /// The host as it starts: offline, nobody in control, with the trust store of `cfg`'s
    /// folder (None refuses every offer).
    fn new(p: Params, trust: Option<Trust>, controls: Controls) -> (Host, Queues) {
        let Params { cfg, view_only, desktop, browser, notify, settings, workspace, admin } = p;
        let Controls { rx: control, wake, .. } = controls;
        let (out_tx, out) = wake.channel();
        let (updates_tx, updates) = wake.channel();
        let (share_tx, share) = wake.channel();
        let (desktop, platform_events) = desktop.map(|d| {
            let (router, events) = Router::spawn(d.events, &wake);
            d.platform.auto_lock(cfg.auto_lock);
            (Desktop { features: d.features, platform: d.platform, router, can_host: d.can_host }, events)
        }).unzip();
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
            desktop,
            terminal: browser.as_ref().map(|b| {
                let bind = b.bind.clone();
                Terminals::guarded(Arc::new(move |user, rows, cols| {
                    let browser = bind(user).ok_or(ErrorKind::PermissionDenied)?;
                    (browser.open)(rows, cols)
                }), b.state.clone(), updates_tx.clone())
            }),
            workspace_state: browser.as_ref().map_or(WorkspaceState::READY, |b| (b.state)()),
            browser,
            transfers: Default::default(),
            machine: vitals::machine(),
            sampling: None,
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
        (host, Queues { wake, out, updates, platform: platform_events, control, share })
    }
}

const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

struct AnnouncementWake {
    pin: String,
    sender: WeakWake<Control>,
    last_wake: Mutex<Option<Instant>>,
}

impl AnnouncementWake {
    fn on_change(&self, change: Change, now: Instant) {
        let Change::Update { server: Some(server), .. } = change else { return };
        if server.address.fingerprint() != Some(self.pin.as_str()) {
            return;
        }
        let mut last_wake = self.last_wake.lock().unwrap();
        if last_wake.is_some_and(|last| now.duration_since(last) < MAX_RETRY_DELAY) {
            return;
        }
        if self.sender.upgrade().is_some_and(|tx| tx.send(Control::ServerAnnounced).is_ok()) {
            *last_wake = Some(now);
        }
    }
}

/// Runs the host until it is told to quit or `control` closes.
pub fn run(p: Params, control: Controls) {
    let trust = crate::config::dir().and_then(|d| Trust::open(d, &p.cfg.id, &p.cfg.server)).map_err(|e| eprintln!("trust store unavailable, refusing all sessions: {e:#}")).ok();
    let address = Address::parse(&p.cfg.server).ok();
    let announcement = address.as_ref().and_then(Address::fingerprint).map(|pin| Arc::new(AnnouncementWake {
        pin: pin.into(), sender: control.sender.clone(), last_wake: Mutex::new(None),
    }));
    let (mut host, q) = Host::new(p, trust, control);
    let cfg = host.cfg.clone();
    let view_only = host.view_only;
    let mut delay = Duration::from_secs(1);
    let mut browse = None;
    eprintln!("Deskway host: {} -> {}{}", cfg.name, cfg.server, if view_only { " (view only)" } else { "" });
    while !host.quit {
        match await_connection(&mut host, &q, CONNECT_TIMEOUT) {
            Ok(Some(mut ws)) => {
                browse = None;
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
                if let Some(desk) = &host.desktop {
                    desk.platform.thumbnails(false);
                }
            }
            Ok(None) => break,
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
        // A fresh announcement can shorten a retry wait. Keep its cooldown across reconnects.
        if !host.quit && browse.is_none() {
            if let Some(announcement) = announcement.clone() {
                browse = Browse::start(move |change| announcement.on_change(change, Instant::now()))
                    .map_err(|e| eprintln!("cannot browse for the local server: {e:#}")).ok();
            }
        }
        q.wait_for_retry(&mut host, Instant::now() + delay);
        if !host.waiting {
            delay = (delay * 2).min(MAX_RETRY_DELAY);
        }
    }
    drop(browse);
    // Without signaling, still wait for session-owned resources to be released before exit.
    while host.has_sessions() {
        let Ok(update) = q.updates.recv() else { break };
        host.on_update(update);
    }
}


/// What a browser is told when adding its device is refused: full, or that it did not work.
fn refusal(e: &anyhow::Error) -> Reason {
    if e.is::<crate::auth::DevicesFull>() { Reason::Full } else { Reason::Pair }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::channel;
    use std::thread;

    use tungstenite::protocol::frame::{coding::{Data, OpCode}, Frame};
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
    #[test]
    fn stalled_tls_and_websocket_handshakes_time_out_or_quit() {
        for (scheme, quit) in [("https", false), ("http", false), ("https", true), ("http", true)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let mut test = Test::new(&format!("connect-{scheme}-{quit}"), false, false);
            test.host.cfg.server = format!("{scheme}://{}", listener.local_addr().unwrap());
            let control = test.control.clone();
            let (done, result) = channel();
            let timeout = Duration::from_millis(if quit { 10_000 } else { 350 });
            let worker = thread::spawn(move || {
                done.send(await_connection(&mut test.host, &test.q, timeout)).unwrap();
            });
            let (mut socket, _) = listener.accept().unwrap();
            if quit {
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                assert!(socket.read(&mut [0; 4096]).unwrap() > 0);
                control.send(Control::Quit).unwrap();
                assert!(result.recv_timeout(Duration::from_secs(2)).unwrap().unwrap().is_none());
            } else {
                assert!(result.recv_timeout(Duration::from_secs(2)).unwrap().is_err());
            }
            drop(socket);
            worker.join().unwrap();
        }
    }

    struct Test {
        host: Host,
        q: Queues,
        control: Wake<Control>,
        shown: Arc<Mutex<Vec<Status>>>,
        dir: std::path::PathBuf,
        calls: Arc<Calls>,
    }

    impl Test {
        fn new(name: &str, can_host: bool, trust: bool) -> Test {
            Self::with_features(name, can_host, trust, Features { desktop: Some(crate::platform::TEST_DESKTOP), ..Features::default() })
        }

        fn with_features(name: &str, can_host: bool, trust: bool, features: Features) -> Test {
            let dir = std::env::temp_dir().join(format!("deskway-signal-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("host.json"), r#"{"server":"https://s.example","id":"H1","token":"t","name":"Desk"}"#).unwrap();
            let cfg = Config::load_from(&dir).unwrap();
            let calls = Arc::new(Calls::default());
            let platform: Arc<dyn Platform> = calls.clone();
            let (control, control_rx) = control_channel();
            let (settings, _) = Settings::start(dir.clone(), &cfg, features.desktop.map(|_| platform.clone()), control.clone());
            let (_events_tx, events) = channel();
            let shown = Arc::new(Mutex::new(Vec::new()));
            let to = shown.clone();
            let trust = trust.then(|| Trust::open(dir.clone(), &cfg.id, &cfg.server).unwrap());
            let desktop = features.desktop.map(|features| DesktopParams { features, platform, events, can_host: Box::new(move || can_host) });
            let workspace = features.work.then(|| Arc::new(crate::workspace::Local::default()) as Arc<dyn crate::workspace::Workspace>);
            let p = Params { cfg, view_only: features.view_only, desktop, browser: features.terminal.then(|| crate::workspace::Browser {
                state: Arc::new(|| WorkspaceState::READY),
                bind: Arc::new(|_| Some(crate::workspace::BrowserUser {
                    open: Arc::new(|_, _| panic!("admission alone must not start a shell")),
                    browse: Arc::new(|_, _| unreachable!()),
                    transfers: crate::workspace::FileTransfers {
                        path: Arc::new(|_, _, _, _| unreachable!()), upload: Arc::new(|_, _, _| unreachable!()),
                        download: Arc::new(|_, _| unreachable!()),
                    },
                    #[cfg(windows)]
                    _lease: None,
                })),
            }), notify: Box::new(move |s| to.lock().unwrap().push(s)), settings: Arc::new(settings), workspace, admin: None };
            let (host, q) = Host::new(p, trust, control_rx);
            Test { host, q, control, shown, dir, calls }
        }

        /// Start signaling against a plain WebSocket server on loopback.
        fn serve(mut self) -> (WebSocket<TcpStream>, Wake<Control>, Receiver<Result<()>>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            self.host.cfg.server = format!("http://{}", listener.local_addr().unwrap());
            let control = self.control.clone();
            let (done, host) = channel();
            thread::spawn(move || {
                let result = await_connection(&mut self.host, &self.q, CONNECT_TIMEOUT)
                    .and_then(|ws| ws.map_or(Ok(()), |mut ws| serve(&mut ws, &mut self.host, &self.q)));
                let _ = done.send(result);
            });
            let (socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut server = tungstenite::accept(socket).unwrap();
            assert_eq!(read_signal(&mut server)["type"], "hello");
            assert!(matches!(server.read().unwrap(), WsMessage::Ping(_)));
            server.flush().unwrap(); // tungstenite queued the pong when it read the ping
            assert_eq!(read_signal(&mut server)["type"], "status");
            (server, control, host)
        }

        /// What went to the server since the last call.
        fn sent(&self) -> Vec<Value> {
            self.q.out.try_iter().map(|m| serde_json::from_str(&m).unwrap()).collect()
        }

        fn offer(&mut self, auth: Value) -> Value {
            self.q.on_message(&mut self.host, &json!({ "type": "offer", "session": "s1", "epoch": 0, "sdp": "v=0", "ice": [], "view": { "w": 1, "h": 1, "maxW": 1, "maxH": 1, "fps": 30 }, "user": "u", "auth": auth }).to_string());
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
            self.q.on_message(&mut self.host, &json!({ "type": "offer", "session": session, "epoch": 0, "sdp": sdp, "ice": [], "view": { "w": 1, "h": 1, "maxW": 1, "maxH": 1, "fps": 30 }, "user": "u2", "force": force, "auth": auth }).to_string());
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

    #[test]
    fn matching_local_announcement_interrupts_retry_wait() {
        let mut t = Test::new("local-retry", false, false);
        let pin = "ab".repeat(32);
        let announce = |name: &str, fingerprint: &str| Change::Update {
            instance: format!("{name}._deskway._tcp.local."),
            server: Some(crate::discovery::Server {
                name: name.into(),
                address: Address::parse(&format!("https://hub.local:18499#ca={fingerprint}")).unwrap(),
            }),
        };
        let wake = AnnouncementWake { pin: pin.clone(), sender: t.control.downgrade(), last_wake: Mutex::new(None) };
        let now = Instant::now();
        wake.on_change(Change::Update { instance: "hub._deskway._tcp.local.".into(), server: None }, now);
        wake.on_change(announce("hub", &"cd".repeat(32)), now);
        assert!(matches!(t.q.control.try_recv(), Err(TryRecvError::Empty)));

        // A notification already queued behind other work must still end the wait.
        t.control.send(Control::Report).unwrap();
        wake.on_change(announce("hub", &pin), now);
        let deadline = Instant::now() + Duration::from_secs(2);
        t.q.wait_for_retry(&mut t.host, deadline);
        assert!(Instant::now() < deadline);
        assert!(matches!(t.q.control.try_recv(), Err(TryRecvError::Empty)));

        wake.on_change(Change::Reset, now);
        wake.on_change(announce("renamed-hub", &pin), now + MAX_RETRY_DELAY - Duration::from_millis(1));
        assert!(matches!(t.q.control.try_recv(), Err(TryRecvError::Empty)));
        wake.on_change(announce("renamed-hub", &pin), now + MAX_RETRY_DELAY);
        assert_eq!(t.q.control.try_recv().unwrap(), Control::ServerAnnounced);
    }

    #[test]
    fn a_preview_request_samples_vitals_only_when_supported_and_requested() {
        for (cap, flag, want) in [(true, Some(true), true), (true, None, false), (true, Some(false), false), (false, Some(true), false)] {
            let mut test = Test::with_features("vitals", false, false, Features::default());
            if !cap {
                test.host.machine = None;
            }
            let Wire::Hello { caps, app, machine, .. } = test.host.hello() else { panic!("expected hello") };
            assert_eq!(caps.iter().any(|cap| cap == wire::caps::VITALS), cap);
            assert_eq!(app.as_deref(), Some(env!("CARGO_PKG_VERSION")));
            assert_eq!(machine.is_some(), cap);

            let (mut server, control, done) = test.serve();
            let mut request = json!({ "type": "thumbs", "on": true });
            if let Some(flag) = flag {
                request["vitals"] = json!(flag);
            }
            server.send(WsMessage::text(request.to_string())).unwrap();
            if want {
                let report = read_signal(&mut server);
                assert_eq!(report["type"], "vitals");
                assert!(report["cpu"].is_number() && report["memory"].is_object() && report["disks"].is_array());
                assert!(report["rtt"].is_u64());
            } else {
                server.get_mut().set_read_timeout(Some(Duration::from_millis(500))).unwrap();
                assert!(matches!(server.read(), Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)));
            }
            control.send(Control::Quit).unwrap();
            done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        }
    }

    #[test]
    fn a_request_after_reconnecting_waits_for_the_hosts_previous_sample() {
        let mut test = Test::with_features("vitals-reconnect", false, false, Features::default());
        let (finish_old_sample, old_sample) = test.q.wake.channel();
        test.host.sampling = Some(old_sample);
        let (mut server, control, done) = test.serve();
        server.send(WsMessage::text(r#"{"type":"thumbs","on":true,"vitals":true}"#)).unwrap();
        server.get_mut().set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        assert!(matches!(server.read(), Err(tungstenite::Error::Io(e))
            if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)));

        finish_old_sample.send(None).unwrap();
        server.get_mut().set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(read_signal(&mut server)["type"], "vitals");
        control.send(Control::Quit).unwrap();
        done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    }

    fn read_signal(server: &mut WebSocket<TcpStream>) -> Value {
        serde_json::from_str(server.read().unwrap().into_text().unwrap().as_str()).unwrap()
    }

    #[test]
    fn queued_messages_wake_an_idle_connection_and_are_written_in_full() {
        for (name, size, max_ms) in [("wake", 32, Some(75)), ("large-write", 200 * 1024, None)] {
            let test = Test::new(name, true, false);
            let out = test.host.out_tx.clone();
            let (mut server, control, host) = test.serve();
            let text = "x".repeat(size);
            let queued = text.clone();
            let sender = thread::spawn(move || {
                // Let signaling settle into its wait while the server stays silent.
                thread::sleep(Duration::from_millis(50));
                let start = Instant::now();
                out.send(queued).unwrap();
                start
            });
            let received = server.read().unwrap();
            let finished = Instant::now();
            assert_eq!(received, WsMessage::text(text));
            let elapsed = finished.duration_since(sender.join().unwrap());
            if let Some(ms) = max_ms {
                assert!(elapsed < Duration::from_millis(ms), "{size} bytes took {elapsed:?}");
            }
            control.send(Control::Quit).unwrap();
            host.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        }
    }

    #[test]
    fn all_websocket_frames_in_one_tcp_write_are_handled() {
        let mut test = Test::new("buffered", true, false);
        let session = test.holding(PHONE);
        let updates = test.host.updates_tx.clone();
        let (mut server, control, host) = test.serve();
        let mut batch = Vec::new();
        // Candidates go straight to the session. They cannot wake signaling as a queued reply would.
        let candidates = ["one", "two", "three", "four"];
        for candidate in candidates {
            let text = serde_json::to_vec(&Wire::Candidate {
                session: "s1".into(),
                candidate: types::IceCandidate {
                    candidate: candidate.into(),
                    sdp_mid: None,
                    sdp_m_line_index: None,
                    username_fragment: None,
                },
            }).unwrap();
            Frame::message(text, OpCode::Data(Data::Text), true).format(&mut batch).unwrap();
        }
        assert_eq!(server.get_mut().write(&batch).unwrap(), batch.len());
        for candidate in candidates {
            let event = session.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(matches!(event, SessionEvent::RemoteCandidate(got) if got == candidate));
        }
        control.send(Control::Quit).unwrap();
        updates.send(SessionUpdate::Ended("s1".into())).unwrap();
        host.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
    }

    const CLAUDE: &str = "Q0xBVURF";
    const CURSOR: &str = "Q1VSU09S";

    const PHONE: &str = "UEhPTkU";

    const TERMINAL_SDP: &str = concat!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\n",
        "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=mid:0\r\na=sctp-port:5000\r\n",
    );

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
    fn hello_and_desk_admission_follow_the_feature_set() {
        let desktop = crate::platform::TEST_DESKTOP;
        for (case, features, refusal) in [
            ("desktop", Features { desktop: Some(desktop), work: true, ..Features::default() }, None),
            ("view-only", Features { desktop: Some(desktop), work: true, view_only: true, ..Features::default() }, None),
            ("base", Features::default(), Some("no_desktop")),
            ("headless", Features { work: true, ..Features::default() }, Some("no_desktop")),
        ] {
            let mut t = Test::with_features(case, true, true, features);
            let Wire::Hello { mut caps, tools, .. } = t.host.hello() else { panic!("expected hello") };
            caps.retain(|cap| cap != wire::caps::VITALS);
            assert_eq!(caps.iter().any(|c| c == wire::caps::DESK), refusal.is_none());
            assert_eq!(caps.last().map(String::as_str), Some(wire::caps::RELAY_RTT));
            assert_eq!(tools.unwrap(), features.tools().collect::<Vec<_>>());
            if refusal.is_some() {
                assert_eq!(caps, [wire::caps::RELAY_RTT]);
            } else {
                let mut expected = (desktop.caps)(features.view_only);
                expected.push(wire::caps::RELAY_RTT.into());
                assert_eq!(caps, expected);
            }
            let phone = t.pair_phone();
            let sent = t.signed_offer(&phone, "desk", false);
            if let Some(reason) = refusal {
                assert_eq!(sent, [json!({ "type": "bye", "session": "desk", "reason": reason })]);
                assert!(t.host.sessions.of(Seat::Desk).current().is_none());
            } else {
                assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "desk");
            }
        }
    }

    #[test]
    fn workspace_status_admission_and_end_follow_the_console_user() {
        for (index, native, view_only, wanted) in [
            (0, WorkspaceState::READY, false, None),
            (1, WorkspaceState { user: Some(1), locked: true }, false, Some(Reason::Locked)),
            (2, WorkspaceState::NO_USER, false, Some(Reason::NoUserSession)),
            (3, WorkspaceState::READY, true, Some(Reason::ViewOnly)),
        ] {
            let mut t = Test::with_features(&format!("workspace-{index}"), true, true, Features {
                desktop: Some(crate::platform::TEST_DESKTOP), terminal: true, work: true, view_only,
                ..Features::default()
            });
            let state = Arc::new(Mutex::new(native));
            let watch = state.clone();
            t.host.browser.as_mut().unwrap().state = Arc::new(move || *watch.lock().unwrap());
            t.host.workspace_changed(native);
            t.host.send_status();
            let output = t.sent();
            let status = output.iter().rfind(|m| m["type"] == "status").unwrap();
            assert_eq!(status["workspace_access"].as_str(), wanted.map(|why| why.as_str()));
            let phone = t.pair_phone();
            let offer = t.offer_from(&phone, PHONE, "workspace", false, TERMINAL_SDP);
            if let Some(wanted) = wanted {
                assert_eq!(offer[0]["reason"], wanted.as_str());
                assert!(t.host.sessions.of(Seat::Desk).current().is_none());
                continue;
            }
            assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "workspace");
            t.host.on_ended("workspace");

            // A data seat ends, while desktop sessions are unaffected by this signal.
            t.host.auto_lock = true;
            for (why, next) in [
                (Reason::Locked, WorkspaceState { user: Some(0), locked: true }),
                (Reason::NoUserSession, WorkspaceState::NO_USER),
                (Reason::UserChanged, WorkspaceState { user: Some(2), locked: false }),
            ] {
                *state.lock().unwrap() = WorkspaceState::READY;
                t.host.workspace_changed(WorkspaceState::READY);
                let rx = t.seated(Seat::Desk, "desk", PHONE);
                *state.lock().unwrap() = next;
                t.host.workspace_changed(next);
                assert!(rx.try_recv().is_err(), "a desktop session must remain seated");
                *state.lock().unwrap() = WorkspaceState::READY;
                t.host.workspace_changed(WorkspaceState::READY);
                let rx = t.seated(Seat::Desk, "desk", PHONE);
                t.host.terminal.as_ref().unwrap().admit("desk".into(), "browser".into(), PHONE.into());
                *state.lock().unwrap() = next;
                t.host.workspace_changed(next);
                assert_eq!(reason(&rx), Some(why));
                t.host.on_ended("desk");
                assert!(!t.calls.0.lock().unwrap().contains(&Call::Lock {}));
            }
        }
    }

    #[test]
    fn an_offer_after_unlock_applies_queued_revocations_to_the_old_data_seat() {
        for revoked in [
            WorkspaceState { user: Some(0), locked: true },
            WorkspaceState::NO_USER,
            WorkspaceState { user: Some(2), locked: false },
        ] {
            let mut t = Test::with_features("queued-unlock", false, true, Features {
                desktop: Some(crate::platform::TEST_DESKTOP), terminal: true, ..Features::default()
            });
            let phone = t.pair_phone();
            t.host.auto_lock = true;
            let rx = t.seated(Seat::Desk, "old", PHONE);
            t.host.terminal.as_ref().unwrap().admit("old".into(), "browser".into(), PHONE.into());
            t.control.send(Control::WorkspaceChanged(revoked)).unwrap();
            t.control.send(Control::WorkspaceChanged(WorkspaceState::READY)).unwrap();
            t.offer_from(&phone, PHONE, "next", false, TERMINAL_SDP);
            assert!(reason(&rx).is_some());
            assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "next");
            t.host.on_ended("old");
            assert!(!t.calls.0.lock().unwrap().contains(&Call::Lock {}));
            assert!(t.host.terminal.as_ref().unwrap().holds("next"));
        }
    }

    #[test]
    fn terminal_offers_use_the_desk_seat_and_the_signed_device_policy() {
        use base64::Engine;
        use p256::ecdsa::signature::Signer;
        assert!(session::data::data_only(TERMINAL_SDP));
        for (case, features, cap) in [
            ("terminal-desktop", Features { desktop: Some(crate::platform::TEST_DESKTOP), ..Features::default() }, false),
            ("terminal-headless", Features { work: true, ..Features::default() }, false),
            ("terminal-linux", Features { terminal: true, work: true, ..Features::default() }, true),
        ] {
            let mut t = Test::with_features(case, true, true, features);
            let Wire::Hello { caps, .. } = t.host.hello() else { panic!("expected hello") };
            assert_eq!(caps.iter().any(|c| c == wire::caps::TERMINAL), cap);
            let phone = t.pair_phone();
            let off: p256::ecdsa::Signature = phone.sign(&wire::crypto::sha256(wire::challenge::policy_v1(false, 5).as_bytes()));
            let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(off.to_bytes());
            for (id, sdp) in [("unsigned-picture", "v=0"), ("unsigned-terminal", TERMINAL_SDP)] {
                t.host.on_message(&json!({ "type": "offer", "session": id, "sdp": sdp }).to_string());
                assert_eq!(t.sent()[0]["reason"], "unpaired");
            }
            t.host.on_message(&json!({
                "type": "devlog", "auth": [{ "op": "policy", "keys": false, "ts": 5, "by": PHONE, "sig": sig }],
            }).to_string());
            t.sent();
            if cap {
                assert_eq!(t.signed_offer(&phone, "picture", false)[0]["reason"], "no_desktop");
                let rx = t.seated(Seat::Desk, "desk", PHONE);
                let offer = t.offer_from(&phone, PHONE, "terminal", false, TERMINAL_SDP);
                assert_eq!(offer[0]["reason"], "busy");
                t.offer_from(&phone, PHONE, "terminal", true, TERMINAL_SDP);
                assert_eq!(reason(&rx), Some(Reason::Taken));
                assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "terminal");
                t.host.on_ended("terminal");
            } else {
                let offer = t.offer_from(&phone, PHONE, "terminal", false, TERMINAL_SDP);
                assert_eq!(offer[0]["reason"], "no_terminal");
            }
            t.host.on_message(&json!({ "type": "offer", "session": "password", "sdp": TERMINAL_SDP }).to_string());
            if cap {
                assert_eq!(t.host.sessions.of(Seat::Desk).current().unwrap().id, "password");
            } else {
                assert_eq!(t.sent().last().unwrap()["reason"], "no_terminal");
            }
        }
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

    #[test]
    fn ai_facts_use_the_platform_name_when_the_config_name_is_left_blank() {
        let mut t = Test::new("mac-name", true, true);
        t.host.on_control(Control::Report);
        assert_eq!(t.host.facts.read().unwrap().name, "Desk");
        Config::update_in(&t.dir, |c| c.name.clear()).unwrap();
        t.host.on_control(Control::Report);
        let name = t.host.facts.read().unwrap().name.clone();
        assert_eq!(name, crate::platform::facts_name(""));
    }

    #[test]
    fn platform_events_go_to_the_session_the_host_or_both() {
        let (events_tx, events) = channel();
        let (router, host) = Router::spawn(events, &Waker::default());
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

    #[test]
    fn a_host_without_a_desktop_still_requires_ai_authorization() {
        let mut t = Test::with_features("headless-ai", false, true, Features::default());
        let [phone, claude, _] = t.pair_all();
        let sent = t.offer_from(&claude, CLAUDE, "off", false, "v=0");
        assert_eq!(sent[0]["reason"], "ai_off");
        t.switch(&phone, true, 10);
        let _ = t.sent();
        let sent = t.offer_from(&claude, CLAUDE, "ai", false, "v=0");
        assert_eq!(statuses(&sent)[0]["ai_session"]["session"], "ai");
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

    #[test]
    fn a_relay_list_is_answered_with_measured_round_trips() {
        let mut t = Test::new("relay-rtt", true, true);
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let addr = socket.local_addr().unwrap();
        let responder = std::thread::spawn(move || {
            let mut request = [0; 20];
            let (_, source) = socket.recv_from(&mut request).unwrap();
            // A Binding success with a fixed XOR-MAPPED-ADDRESS. Echo the transaction ID.
            let mut answer = [
                0x01, 0x01, 0, 12, 0x21, 0x12, 0xa4, 0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0x20, 0, 8, 0, 1, 0x32, 0x9a, 0x5e, 0x12, 0xa4, 0x43,
            ];
            answer[8..20].copy_from_slice(&request[8..20]);
            socket.send_to(&answer, source).unwrap();
        });
        t.host.on_message(&json!({ "type": "relay-list", "probe": "p1", "relays": { "node": addr.to_string() } }).to_string());
        let report = t.q.out.recv_timeout(Duration::from_secs(5)).unwrap();
        let Wire::RelayRtts { probe, rtts } = serde_json::from_str(&report).unwrap() else { panic!("expected a measurement report") };
        assert_eq!(probe, "p1");
        assert_eq!(rtts.len(), 1);
        assert!(rtts.contains_key("node"));
        responder.join().unwrap();
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
