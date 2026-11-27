//! The panel's backend on macOS: the host runs in this process, with this computer's platform.
//! Signing in and out happens here too, as do the Mac's own settings: its permissions and the
//! login item.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tao::event_loop::EventLoopProxy;

use super::{Backend, PageMessage, UiEvent};
use crate::config::Config;
use crate::i18n::t;
use crate::local_server::{Handle, Preferences, Reply, Request, State};
use crate::platform;
use crate::settings::{PanelRequest, Settings};
use crate::signal::{self, Control, Status};
use crate::wake::Wake;

pub enum BackendEvent {
    /// Signing in finished: the new config, or why it failed.
    Enrolled(Result<Config, String>, bool),
    /// Signed out.
    Left(Result<Option<String>, String>, bool),
    RootChanged(Option<String>),
    /// Ask macOS for a permission (Screen Recording: true), once the panel says it is asked.
    Permission(bool),
    /// Quit gracefully once a launcher is waiting to reopen this bundle.
    Reopen,
    Local(State),
    LocalReply(Request, Result<Reply, String>),
}

struct RunningHost {
    control: Wake<Control>,
    thread: std::thread::JoinHandle<()>,
}

pub struct InProcess {
    proxy: EventLoopProxy<UiEvent>,
    /// This Mac's config; None while signed out.
    cfg: Option<Config>,
    /// The running host and its queue, shared with the local service's sign-out callback.
    signal: Arc<Mutex<Option<RunningHost>>>,
    settings: Option<Arc<Settings>>,
    workspace_watch: Option<platform::WorkspaceWatch>,
    /// A sign-in, sign-out or browser trust prompt is in progress.
    busy: bool,
    /// Why signing in failed.
    error: String,
    root_problem: Option<String>,
    /// Why the last Mac setting could not be changed.
    settings_error: String,
    permission_request: Option<&'static str>,
    local: super::local::Panel,
    local_handle: Option<Handle>,
    after_leave: Option<Preferences>,
}

impl InProcess {
    pub fn new(proxy: EventLoopProxy<UiEvent>, _at_login: bool, _dev: bool) -> InProcess {
        InProcess { proxy, cfg: Config::load().ok(), signal: Arc::default(), settings: None, workspace_watch: None, busy: false, error: String::new(), root_problem: None, settings_error: String::new(), permission_request: None, local: super::local::Panel::default(), local_handle: None, after_leave: None }
    }

    fn local_request(&mut self, action: Request) {
        let own = self.local.state.as_ref().is_some_and(|s| s.own);
        if action == Request::Start && self.cfg.is_some() && !own {
            match crate::config::dir().and_then(|dir| Preferences::read(&dir, platform::ui::login_status().registered())) {
                Ok(previous) => {
                    self.after_leave = Some(previous);
                    self.leave();
                }
                Err(e) => self.local.replied(action, Err(format!("{e:#}"))),
            }
        } else {
            self.send_local(action, None);
        }
    }

    fn send_local(&mut self, action: Request, previous: Option<Preferences>) {
        if let Some(handle) = &self.local_handle {
            let proxy = self.proxy.clone();
            handle.request(action, previous, move |result| {
                let result = match result {
                    Ok(Reply::Browser { url, address }) => open_local(&url, &address)
                        .map(|()| Reply::Done).map_err(|e| format!("{e:#}")),
                    result => result,
                };
                let _ = proxy.send_event(UiEvent::Backend(BackendEvent::LocalReply(action, result)));
            });
        } else {
            self.local.replied(action, Err(t("local.unavailable")));
        }
    }

    fn set_login_item(&mut self, on: bool) -> Result<()> {
        platform::ui::set_login(on).with_context(|| t(if on { "err.mac_login_on" } else { "err.mac_login_off" }))?;
        if let Some(cfg) = self.cfg.as_mut().filter(|cfg| cfg.login_item_setup_pending) {
            *cfg = Config::update(|c| c.login_item_setup_pending = false).with_context(|| t("err.save_settings"))?;
        }
        Ok(())
    }

    /// Runs the host in this process, with this computer's platform. Keep-awake is applied by
    /// the settings as they start; a newly enrolled Mac registers its login item.
    fn start_host(&mut self) {
        let Some(cfg) = self.cfg.clone() else { return };
        let signal = self.signal.clone();
        let mut running = signal.lock().unwrap();
        // A local stop can clear the sign-in after enrollment posted its event.
        if Config::load().is_err() {
            return;
        }
        let (events, platform_events) = std::sync::mpsc::channel();
        let local: Arc<dyn crate::platform::Platform> = Arc::new(platform::Local::new(events));
        let (control, requests) = signal::control_channel();
        let dir = match crate::config::dir() {
            Ok(d) => d,
            Err(e) => {
                self.settings_error = format!("{e:#}");
                return;
            }
        };
        self.workspace_watch = None;
        let (browser, watch) = platform::workspace_browser(control.clone());
        self.workspace_watch = Some(watch);
        let (settings, awake) = Settings::start(dir, &cfg, Some(local.clone()), control.clone());
        let settings = Arc::new(settings);
        let login = if cfg.login_item_setup_pending { self.set_login_item(true) } else { Ok(()) };
        self.settings_error = [awake, login]
            .into_iter()
            .filter_map(|r| r.err())
            .map(|e| {
                eprintln!("{e:#}");
                format!("{e:#}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let _ = self.proxy.send_event(UiEvent::Status(Some(Status::offline(&cfg))));
        let proxy = self.proxy.clone();
        let params = crate::signal::Params {
            // Locking when a session ends is a Windows setting: an old config's value is ignored.
            cfg: Config { auto_lock: false, ..cfg },
            view_only: false,
            desktop: Some(crate::signal::DesktopParams {
                features: platform::DESKTOP,
                platform: local,
                events: platform_events,
                can_host: Box::new(|| {
                    let p = platform::permissions();
                    if !p.ready() {
                        eprintln!("Screen Recording={}, Accessibility={}", p.screen, p.accessibility);
                    }
                    p.ready()
                }),
            }),
            browser: Some(browser),
            notify: Box::new(move |s| {
                let _ = proxy.send_event(UiEvent::Status(Some(s)));
            }),
            settings: settings.clone(),
            // This app is the person signed in.
            workspace: Some(std::sync::Arc::new(crate::workspace::Local::default())),
            // A Mac's own workspace asks the person there for administrator rights.
            admin: None,
        };
        let thread = std::thread::Builder::new().name("signal".into()).spawn(move || {
            signal::run(params, requests);
        }).expect("spawn signal thread");
        *running = Some(RunningHost { control, thread });
        self.settings = Some(settings);
    }

    fn enroll(&mut self, server: String, user: String, password: String) {
        self.busy = true;
        self.error.clear();
        self.root_problem = None;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let res = crate::enroll_with(&server, &user, &password, "", Config::kept().as_ref()).and_then(Config::settle).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Enrolled(res, false)));
        });
    }

    /// Signs out here only: the server keeps this computer, offline, for the same account to
    /// take back (`Config::sign_out`).
    fn leave(&mut self) {
        if self.cfg.is_none() && !self.root_cleanup_pending() {
            return;
        }
        self.busy = true;
        self.error.clear();
        let proxy = self.proxy.clone();
        let signal = self.signal.clone();
        std::thread::spawn(move || {
            let result = crate::config::dir().and_then(|dir| sign_out(&dir, &signal)).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Left(result, false)));
        });
    }

    fn root_cleanup_pending(&self) -> bool {
        crate::config::dir().is_ok_and(|dir| dir.join(platform::INSTALLED_ROOT).exists())
    }
}

impl Backend for InProcess {
    fn start(&mut self) -> bool {
        self.start_host();
        let (state, enrolled) = (self.proxy.clone(), self.proxy.clone());
        let (left, signal) = (self.proxy.clone(), self.signal.clone());
        self.local_handle = crate::config::dir().and_then(|dir| Handle::start(
            dir,
            move |s| { let _ = state.send_event(UiEvent::Backend(BackendEvent::Local(s))); },
            platform::ui::set_login,
            || platform::ui::login_status().registered(),
            move |cfg| {
                let address = cfg.server.clone();
                let _ = enrolled.send_event(UiEvent::Backend(BackendEvent::Enrolled(Ok(cfg), true)));
                let error = trust_hub(&address).err().map(|e| format!("{e:#}"));
                let _ = enrolled.send_event(UiEvent::Backend(BackendEvent::RootChanged(error)));
            },
            move |dir| {
                let running = signal.lock().unwrap().take();
                stop_host(running);
                let result = clear_sign_in(dir);
                let _ = left.send_event(UiEvent::Backend(BackendEvent::Left(Ok(None), true)));
                result
            },
            |_| {},
        )).map_err(|e| eprintln!("{e:#}")).ok();
        self.cfg.is_none() || !platform::permissions().ready()
    }

    fn extras(&self) -> Value {
        let permissions = platform::permissions();
        let login = platform::ui::login_status();
        json!({
            "enrolled": self.cfg.is_some(), "busy": self.busy, "error": self.error,
            "root_problem": super::root_problem(self.root_problem.as_deref()),
            "retry_sign_out": self.cfg.is_none() && self.root_cleanup_pending(),
            "autostart": login.registered(),
            "local": self.local.view(),
            "mac": {
                "screen": permissions.screen,
                "accessibility": permissions.accessibility,
                "full_disk_access": platform::full_disk_access(),
                "permission_request": self.permission_request,
                "login": login,
                "error": self.settings_error,
            },
        })
    }

    fn local(&self) -> &super::local::Panel {
        &self.local
    }

    fn request(&mut self, r: PanelRequest) -> bool {
        let awake_changed = matches!(r, PanelRequest::PreventSleep(_));
        let result = match r {
            PanelRequest::AutoStart(false) if self.local.state.as_ref().is_some_and(|s| s.enabled) => Ok(()),
            PanelRequest::AutoStart(on) => self.set_login_item(on),
            r => match &self.settings {
                Some(settings) => settings.handle(r),
                None if r == PanelRequest::PreventSleep(true) => Err(anyhow::anyhow!(t("err.sign_in_first"))),
                // Signed out: the language shows for now; nothing else has a host to go to.
                None => Ok(()),
            },
        };
        if awake_changed && result.is_ok() {
            if let Some(handle) = &self.local_handle {
                handle.changed();
            }
        }
        self.settings_error = result.err().map(|e| format!("{e:#}")).unwrap_or_default();
        true
    }

    fn page(&mut self, m: PageMessage) -> bool {
        let result = match m {
            PageMessage::Login { server, user, password } if !self.busy && !self.local.state.as_ref().is_some_and(|s| s.enabled) => {
                self.enroll(server, user, password);
                return true;
            }
            PageMessage::Leave if !self.busy && !self.local.state.as_ref().is_some_and(|s| s.enabled) => {
                self.leave();
                return true;
            }
            PageMessage::LocalService { action } => {
                self.local_request(action);
                return true;
            }
            PageMessage::ScreenRequest | PageMessage::AccessibilityRequest => {
                let screen = m == PageMessage::ScreenRequest;
                self.permission_request = Some(if screen { "screen" } else { "accessibility" });
                // Asked after the panel shows it is asking: macOS's own prompt may hold the app.
                let _ = self.proxy.send_event(UiEvent::Backend(BackendEvent::Permission(screen)));
                Ok(())
            }
            PageMessage::ScreenSettings => platform::ui::open_permission_settings(true),
            PageMessage::AccessibilitySettings => platform::ui::open_permission_settings(false),
            PageMessage::FullDiskAccessSettings => platform::ui::open_full_disk_access_settings(),
            PageMessage::ReopenForPermissions => platform::ui::reopen_after_exit().map(|()| {
                let _ = self.proxy.send_event(UiEvent::Backend(BackendEvent::Reopen));
            }),
            PageMessage::LoginSettings => {
                platform::ui::open_login_settings();
                Ok(())
            }
            _ => return false,
        };
        self.settings_error = result.err().map(|e| format!("{e:#}")).unwrap_or_default();
        true
    }

    fn status(&mut self, _status: Option<&Status>) {}

    fn event(&mut self, e: BackendEvent) -> bool {
        match e {
            BackendEvent::Enrolled(res, trust_pending) => {
                self.busy = trust_pending;
                match res {
                    Ok(cfg) => {
                        if let Some(handle) = &self.local_handle {
                            handle.changed();
                        }
                        self.cfg = Some(cfg);
                        self.error.clear();
                        self.start_host();
                    }
                    Err(e) => self.error = e,
                }
                true
            }
            BackendEvent::RootChanged(error) => {
                self.busy = false;
                self.root_problem = error;
                true
            }
            BackendEvent::Left(result, local_cleanup) => {
                self.busy = false;
                let after_leave = self.after_leave.take();
                self.root_problem = match result {
                    Ok(error) => error,
                    Err(e) => {
                        if after_leave.is_some() {
                            self.local.replied(Request::Start, Err(e));
                        } else {
                            self.error = e;
                        }
                        return true;
                    }
                };
                self.cfg = None;
                self.settings = None;
                self.workspace_watch = None;
                if let Some(handle) = &self.local_handle {
                    handle.changed();
                }
                if !local_cleanup && after_leave.is_none() {
                    let _ = crate::power::prevent_sleep(false);
                    self.settings_error = platform::ui::set_login(false).with_context(|| t("err.mac_login_off")).err().map(|e| format!("{e:#}")).unwrap_or_default();
                }
                // No host runs now: nothing is shown as controlled.
                let _ = self.proxy.send_event(UiEvent::Status(None));
                if let Some(previous) = after_leave {
                    if let Some(error) = &self.root_problem {
                        self.local.replied(Request::Start, Err(error.clone()));
                    } else {
                        self.send_local(Request::Start, Some(previous));
                    }
                }
                true
            }
            BackendEvent::Permission(screen) => {
                platform::ui::request_permission(screen);
                true
            }
            BackendEvent::Reopen => {
                self.quit();
                std::thread::sleep(std::time::Duration::from_millis(300));
                false
            }
            BackendEvent::Local(state) => {
                if self.local.state.as_ref().map(|s| s.enabled) != Some(state.enabled) {
                    if let Some(settings) = &self.settings {
                        if let Err(e) = settings.reload() {
                            self.settings_error = format!("{e:#}");
                        }
                    }
                }
                self.local.state = Some(state);
                true
            }
            BackendEvent::LocalReply(action, result) => {
                self.local.replied(action, result);
                true
            }
        }
    }

    fn quit(&mut self) {
        self.workspace_watch = None;
        if let Some(host) = self.signal.lock().unwrap().as_ref() {
            let _ = host.control.send(Control::Quit);
        }
        self.local_handle.take();
    }
}

fn sign_out(dir: &std::path::Path, signal: &Mutex<Option<RunningHost>>) -> Result<Option<String>> {
    let host = {
        let mut running = signal.lock().unwrap();
        Config::sign_out()?;
        running.take()
    };
    stop_host(host);
    Ok(clear_sign_in(dir).err().map(|e| format!("{e:#}")))
}

fn stop_host(host: Option<RunningHost>) {
    if let Some(host) = host {
        let _ = host.control.send(Control::Quit);
        let _ = host.thread.join();
    }
}

fn clear_sign_in(dir: &std::path::Path) -> Result<()> {
    crate::config::remove_sign_in_root(dir)
        .and(platform::remove_installed_server_root(dir))
}

fn open_local(url: &str, address: &str) -> Result<()> {
    trust_hub(address)?;
    platform::ui::open_browser(url)
}

fn trust_hub(address: &str) -> Result<()> {
    let address = deskway_server_connection::Address::parse(address)?;
    let dir = crate::config::dir()?;
    let root = crate::local_server::own_root_path(&dir);
    address.verified_root(&std::fs::read_to_string(&root)?)?;
    platform::install_hub_root(&dir, &root, address.fingerprint().unwrap())
}
