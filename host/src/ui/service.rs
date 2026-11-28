//! The panel's backend on Windows: a remote for the network service, which runs the host.
//! Requests go over the service's UI pipe; what needs an administrator (starting a stopped
//! service, signing the computer in or out) runs elevated, with the UAC prompt over the panel.
//!
//! Signing in: the person's own sign-in is made here, in their folder, then an elevated copy
//! (`sign-in --from`) puts it where the service waits for it; the password never reaches the
//! elevated command line. The service names the computer signed out there, so the same account
//! takes it back; its token never leaves the service's folder. Declined, a computer the server
//! just added is taken away again.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tao::event_loop::EventLoopProxy;

use super::{Backend, PageMessage, UiEvent};
use crate::config::Config;
use crate::i18n::t;
use crate::local_server::{Reply, Request, State};
use crate::service::install;
use crate::service::net::{ToPanel, UiClient};
use crate::settings::PanelRequest;
use crate::signal::Status;

pub enum BackendEvent {
    /// The installer asked every panel to quit (through the service).
    Exit,
    /// An elevated command finished: what it was, and whether it worked.
    Admin(&'static str, bool),
    /// The service says this computer is not signed in, and which one is kept there.
    SignedOut(Option<crate::config::Kept>),
    /// Signing in finished: why not, if it did not.
    SignedIn(Result<(), String>),
    RootProblem(Option<String>),
    Local(State),
    LocalAccess(bool),
    LocalOwner(Result<String, String>),
    LocalReply(Request, Result<Reply, String>),
    DevLeft(Result<(), String>),
    Disconnected,
}

pub struct Service {
    proxy: EventLoopProxy<UiEvent>,
    client: Arc<Mutex<Option<UiClient>>>,
    /// The service answered last time it was asked.
    running: bool,
    /// The first attempt to reach the service has finished, one way or the other.
    checked: bool,
    /// An administrator was asked to start the stopped service.
    starting: bool,
    /// Opened at sign-in, not by hand: never asks to start a stopped service.
    at_login: bool,
    /// The service says this computer is not signed in.
    signed_out: bool,
    /// The computer the service keeps signed out, for the same account to take back.
    kept: Option<crate::config::Kept>,
    /// Signing in or out is under way; and why the last one did not happen.
    busy: bool,
    error: String,
    root_problem: Option<String>,
    dev: bool,
    local: super::local::Panel,
    after_leave: Option<Request>,
    leaving: bool,
    start_switch: Option<bool>,
}

impl Service {
    pub fn new(proxy: EventLoopProxy<UiEvent>, at_login: bool, dev: bool) -> Service {
        let mut local = super::local::Panel::default();
        local.link_allowed = Some(false);
        Service { proxy, client: Arc::new(Mutex::new(None)), running: false, checked: false, starting: false, at_login, signed_out: false, kept: None, busy: false, error: String::new(), root_problem: None, dev, local, after_leave: None, leaving: false, start_switch: None }
    }

    fn send(&self, r: PanelRequest) -> bool {
        self.client.lock().unwrap().as_ref().is_some_and(|c| c.send(r).is_ok())
    }

    fn local_request(&mut self, action: Request) {
        let own = self.local.state.as_ref().is_some_and(|s| s.own);
        let switching = action == Request::Start && !self.signed_out && !own;
        if !self.dev && action == Request::Start {
            if self.send(PanelRequest::LocalOwner) {
                self.start_switch = Some(switching);
            } else {
                self.local.replied(action, Err(t("status.service_down")));
            }
        } else if !self.dev && action == Request::StopAndDelete {
            self.admin_local(action, "stop-local-and-delete".into());
        } else if switching {
            self.after_leave = Some(action);
            self.leaving = true;
            if !self.send(PanelRequest::DevLeave) {
                self.finish_leave(Err(t("status.service_down")));
            }
        } else if !self.send(PanelRequest::Local(action)) {
            self.local.replied(action, Err(t("status.service_down")));
        }
    }

    fn finish_leave(&mut self, result: Result<(), String>) {
        self.leaving = false;
        if let Err(error) = result {
            if let Some(action) = self.after_leave.take() {
                self.local.replied(action, Err(error));
            } else {
                self.busy = false;
                self.error = error;
            }
        }
        self.continue_local();
    }

    fn continue_local(&mut self) {
        if !self.leaving && self.signed_out {
            if let Some(action) = self.after_leave {
                if self.send(PanelRequest::Local(action)) {
                    self.after_leave = None;
                }
            }
        }
    }

    fn open_local(&self, action: Request, url: String, address: String) {
        let (proxy, client, dev) = (self.proxy.clone(), self.client.clone(), self.dev);
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Reply> {
                let address = deskway_server_connection::Address::parse(&address)?;
                let dev_dir = dev.then(crate::config::dir).transpose()?;
                let pem = match &dev_dir {
                    Some(dir) => std::fs::read_to_string(crate::local_server::own_root_path(dir))?,
                    None => address.fetch_root()?.ok_or_else(|| anyhow::anyhow!(t("local.failed")))?,
                };
                let der = address.verified_root(&pem)?;
                if let Some(dir) = dev_dir {
                    crate::platform::sync_dev_browser_root(&dir, Some(&der))?;
                } else if !crate::platform::browser_trusts(&der)? {
                    anyhow::ensure!(install::elevate(&format!("authorize-local-root {}", address.fingerprint().unwrap()))? == 0, t("err.not_done"));
                    if let Some(c) = client.lock().unwrap().as_ref() {
                        c.send(PanelRequest::SyncLocalRoot)?;
                    }
                    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                    while !crate::platform::browser_trusts(&der)? {
                        anyhow::ensure!(std::time::Instant::now() < until, t("local.trust_failed"));
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                }
                crate::platform::ui::open_browser(&url)?;
                Ok(Reply::Done)
            })().map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::LocalReply(action, result)));
        });
    }

    /// Runs this program elevated in the background; the UAC prompt appears over the panel.
    fn admin(&self, what: &'static str, args: &str) {
        let (proxy, args) = (self.proxy.clone(), args.to_string());
        std::thread::spawn(move || {
            let ok = install::elevate(&args).map(|code| code == 0).unwrap_or(false);
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Admin(what, ok)));
        });
    }

    fn admin_local(&self, action: Request, args: String) {
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let id = wire::crypto::hex(&wire::crypto::random::<16>());
            let file = std::env::temp_dir().join(format!("deskway-local-{id}.txt"));
            let result = install::elevate(&format!("{args} --result \"{}\"", file.display()));
            let reason = std::fs::read_to_string(&file).unwrap_or_default();
            let _ = std::fs::remove_file(file);
            let result = match result {
                Ok(0) => Ok(Reply::Done),
                Ok(_) if !reason.trim().is_empty() => Err(reason),
                _ => Err(t("err.not_done")),
            };
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::LocalReply(action, result)));
        });
    }

    fn start_service(&mut self) {
        self.starting = true;
        self.admin("start", "start");
    }

    fn sign_in(&mut self, server: String, user: String, password: String) {
        self.busy = true;
        self.error.clear();
        let (proxy, kept, dev) = (self.proxy.clone(), self.kept.clone(), self.dev);
        std::thread::spawn(move || {
            let done = (|| -> Result<(), String> {
                let cfg = crate::enroll_with(&server, &user, &password, "", kept.as_ref()).map_err(|e| e.to_string())?;
                let mine = crate::config::dir().map_err(|e| e.to_string())?;
                if dev {
                    return Config::settle(cfg).map(drop).map_err(|e| format!("{e:#}"));
                }
                cfg.save().map_err(|e| e.to_string())?;
                let ok = install::elevate(&format!("sign-in --from \"{}\"", mine.display())).map(|code| code == 0).unwrap_or(false);
                // A computer given back has no new token: there is nothing to withdraw.
                if !ok && !cfg.token.is_empty() {
                    let _ = crate::withdraw(&cfg);
                }
                // The service holds it now, or nobody does: no copy is left in the person's folder.
                let _ = Config::forget_in(&mine);
                if ok { Ok(()) } else { Err(t("err.not_done")) }
            })();
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::SignedIn(done)));
        });
    }
}

impl Backend for Service {
    fn terminal(&self) -> anyhow::Result<crate::session::terminal::local::Connection> {
        UiClient::terminal(self.dev)
    }

    /// Keeps a connection to the network service and reports its status.
    fn start(&mut self) -> bool {
        let (proxy, client, dev) = (self.proxy.clone(), self.client.clone(), self.dev);
        std::thread::spawn(move || loop {
            match UiClient::connect(dev) {
                Ok((c, rx)) => {
                    *client.lock().unwrap() = Some(c);
                    for m in rx {
                        let (event, exit) = match m {
                            ToPanel::Status(s) => (UiEvent::Status(Some(s)), false),
                            ToPanel::SignedOut(kept) => (UiEvent::Backend(BackendEvent::SignedOut(kept)), false),
                            ToPanel::Exit => (UiEvent::Backend(BackendEvent::Exit), true),
                            ToPanel::RootProblem(error) => (UiEvent::Backend(BackendEvent::RootProblem(error)), false),
                            ToPanel::Local(state) => (UiEvent::Backend(BackendEvent::Local(state)), false),
                            ToPanel::LocalOwner(owner) => (UiEvent::Backend(BackendEvent::LocalOwner(owner)), false),
                            ToPanel::LocalAccess(allowed) => (UiEvent::Backend(BackendEvent::LocalAccess(allowed)), false),
                            ToPanel::LocalReply(action, result) => (UiEvent::Backend(BackendEvent::LocalReply(action, result)), false),
                            ToPanel::DevLeft(result) => (UiEvent::Backend(BackendEvent::DevLeft(result)), false),
                        };
                        if proxy.send_event(event).is_err() || exit {
                            return;
                        }
                    }
                    *client.lock().unwrap() = None;
                    if proxy.send_event(UiEvent::Backend(BackendEvent::Disconnected)).is_err() {
                        return;
                    }
                }
                Err(e) => eprintln!("service: {e:#}"),
            }
            if proxy.send_event(UiEvent::Status(None)).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        });
        true
    }

    fn extras(&self) -> Value {
        json!({
            "enrolled": !self.signed_out, "busy": self.busy, "error": self.error,
            "root_problem": super::root_problem(self.root_problem.as_deref()),
            "service": true, "service_running": self.running, "service_checked": self.checked,
            "starting": self.starting, "autostart": install::autostart(),
            "local": self.local.view(),
        })
    }

    fn local(&self) -> &super::local::Panel {
        &self.local
    }

    fn request(&mut self, r: PanelRequest) -> bool {
        match r {
            PanelRequest::AutoStart(on) => {
                self.send(PanelRequest::AutoStart(on));
                // The agent changes it a moment later; show what it is then.
                let p = self.proxy.clone();
                std::thread::spawn(move || {
                    for _ in 0..3 {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let _ = p.send_event(UiEvent::Refresh);
                    }
                });
            }
            r => { self.send(r); }
        }
        false
    }

    fn page(&mut self, m: PageMessage) -> bool {
        match m {
            PageMessage::StartService if !self.starting => self.start_service(),
            PageMessage::Login { server, user, password } if !self.busy && !self.local.state.as_ref().is_some_and(|s| s.enabled) => self.sign_in(server, user, password),
            PageMessage::Leave if !self.busy && !self.local.state.as_ref().is_some_and(|s| s.enabled) => {
                self.busy = true;
                self.error.clear();
                if self.dev {
                    if !self.send(PanelRequest::DevLeave) {
                        self.finish_leave(Err(t("status.service_down")));
                    }
                } else {
                    self.admin("sign-out", "sign-out");
                }
            }
            PageMessage::LocalService { action } => self.local_request(action),
            _ => return false,
        }
        true
    }

    fn status(&mut self, status: Option<&Status>) {
        let first = !self.checked;
        // A status is the host's: it runs signed in.
        if status.is_some() {
            self.signed_out = false;
        }
        self.running = status.is_some();
        self.checked = true;
        if first && status.is_none() && !self.at_login && !self.starting && !self.dev {
            self.start_service();
        }
        if status.is_some() {
            self.starting = false;
        }
    }

    fn event(&mut self, e: BackendEvent) -> bool {
        match e {
            // Deskway is being installed again or removed: this panel quits by itself.
            BackendEvent::Exit => false,
            BackendEvent::Admin(what, ok) => {
                // Declined or failed: the panel offers the button again.
                if (what, ok) == ("start", false) {
                    self.starting = false;
                }
                if what == "sign-out" {
                    self.busy = false;
                    if !ok {
                        self.error = t("err.not_done");
                    }
                }
                true
            }
            BackendEvent::SignedOut(kept) => {
                self.signed_out = true;
                self.kept = kept;
                self.running = true;
                self.checked = true;
                self.starting = false;
                self.continue_local();
                true
            }
            BackendEvent::SignedIn(done) => {
                self.busy = false;
                self.error = done.err().unwrap_or_default();
                true
            }
            BackendEvent::RootProblem(error) => {
                self.root_problem = error;
                true
            }
            BackendEvent::Local(state) => {
                self.local.state = Some(state);
                self.continue_local();
                true
            }
            BackendEvent::LocalOwner(owner) => {
                if let Some(switching) = self.start_switch.take() {
                    match owner {
                        Ok(owner) => {
                            let switch = if switching { " --switch" } else { "" };
                            self.admin_local(Request::Start, format!("start-local --owner-sid {owner}{switch}"));
                        }
                        Err(e) => self.local.replied(Request::Start, Err(e)),
                    }
                }
                true
            }
            BackendEvent::LocalAccess(allowed) => {
                self.local.link_allowed = Some(allowed);
                true
            }
            BackendEvent::LocalReply(action, Ok(Reply::Browser { url, address })) => {
                self.open_local(action, url, address);
                true
            }
            BackendEvent::LocalReply(action, result) => {
                self.local.replied(action, result);
                true
            }
            BackendEvent::DevLeft(result) => {
                self.busy = false;
                if result.is_ok() {
                    self.signed_out = true;
                }
                self.finish_leave(result);
                true
            }
            BackendEvent::Disconnected => {
                if self.start_switch.take().is_some() {
                    self.local.replied(Request::Start, Err(t("status.service_down")));
                }
                self.local.state = None;
                self.local.link_allowed = Some(false);
                true
            }
        }
    }

    /// Quitting the panel leaves the service running.
    fn quit(&mut self) {}
}
