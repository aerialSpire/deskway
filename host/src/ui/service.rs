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
}

impl Service {
    pub fn new(proxy: EventLoopProxy<UiEvent>, at_login: bool) -> Service {
        Service { proxy, client: Arc::new(Mutex::new(None)), running: false, checked: false, starting: false, at_login, signed_out: false, kept: None, busy: false, error: String::new(), root_problem: None }
    }

    fn send(&self, r: PanelRequest) {
        if let Some(c) = self.client.lock().unwrap().as_ref() {
            let _ = c.send(r);
        }
    }

    /// Runs this program elevated in the background; the UAC prompt appears over the panel.
    fn admin(&self, what: &'static str, args: &str) {
        let (proxy, args) = (self.proxy.clone(), args.to_string());
        std::thread::spawn(move || {
            let ok = install::elevate(&args).map(|code| code == 0).unwrap_or(false);
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Admin(what, ok)));
        });
    }

    fn start_service(&mut self) {
        self.starting = true;
        self.admin("start", "start");
    }

    fn sign_in(&mut self, server: String, user: String, password: String) {
        self.busy = true;
        self.error.clear();
        let (proxy, kept) = (self.proxy.clone(), self.kept.clone());
        std::thread::spawn(move || {
            let done = (|| -> Result<(), String> {
                let cfg = crate::enroll_with(&server, &user, &password, "", kept.as_ref()).map_err(|e| e.to_string())?;
                let mine = crate::config::dir().map_err(|e| e.to_string())?;
                cfg.save().map_err(|e| e.to_string())?;
                let address = deskway_server_connection::Address::parse(&cfg.server).map_err(|e| e.to_string())?;
                let pin = address.fingerprint().map(|pin| format!(" --ca-pin {pin}")).unwrap_or_default();
                let ok = install::elevate(&format!("sign-in --from \"{}\"{pin}", mine.display())).map(|code| code == 0).unwrap_or(false);
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
    /// Keeps a connection to the network service and reports its status.
    fn start(&mut self) -> bool {
        let (proxy, client) = (self.proxy.clone(), self.client.clone());
        std::thread::spawn(move || loop {
            match UiClient::connect() {
                Ok((c, rx)) => {
                    *client.lock().unwrap() = Some(c);
                    for m in rx {
                        let (event, exit) = match m {
                            ToPanel::Status(s) => (UiEvent::Status(Some(s)), false),
                            ToPanel::SignedOut(kept) => (UiEvent::Backend(BackendEvent::SignedOut(kept)), false),
                            ToPanel::Exit => (UiEvent::Backend(BackendEvent::Exit), true),
                            ToPanel::RootProblem(error) => (UiEvent::Backend(BackendEvent::RootProblem(error)), false),
                        };
                        if proxy.send_event(event).is_err() || exit {
                            return;
                        }
                    }
                    *client.lock().unwrap() = None;
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
        })
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
            r => self.send(r),
        }
        false
    }

    fn page(&mut self, m: PageMessage) -> bool {
        match m {
            PageMessage::StartService if !self.starting => self.start_service(),
            PageMessage::Login { server, user, password } if !self.busy => self.sign_in(server, user, password),
            PageMessage::Leave if !self.busy => {
                self.busy = true;
                self.error.clear();
                self.admin("sign-out", "sign-out");
            }
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
        if first && status.is_none() && !self.at_login && !self.starting {
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
        }
    }

    /// Quitting the panel leaves the service running.
    fn quit(&mut self) {}
}
