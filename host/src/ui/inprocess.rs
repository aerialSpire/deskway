//! The panel's backend on macOS: the host runs in this process, with this computer's platform.
//! Signing in and out happens here too, as do the Mac's own settings: its permissions and the
//! login item.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tao::event_loop::EventLoopProxy;

use super::{Backend, PageMessage, UiEvent};
use crate::config::Config;
use crate::i18n::t;
use crate::local_server::{Handle, Reply, Request, State};
use crate::platform;
use crate::settings::{PanelRequest, Settings};
use crate::signal::{self, Control, Status};
use crate::wake::Wake;

pub enum BackendEvent {
    /// Signing in finished: the new config, or why it failed.
    Enrolled(std::result::Result<Config, String>),
    /// Signed out.
    Left(Result<Option<String>, String>),
    RootChanged(Option<String>),
    /// Ask macOS for a permission (Screen Recording: true), once the panel says it is asked.
    Permission(bool),
    /// Quit gracefully once a launcher is waiting to reopen this bundle.
    Reopen,
    Local(State),
    LocalReply(Request, Result<Reply, String>),
}

pub struct InProcess {
    proxy: EventLoopProxy<UiEvent>,
    /// This Mac's config; None while signed out.
    cfg: Option<Config>,
    /// The running host's queue and settings.
    control: Option<Wake<Control>>,
    settings: Option<Arc<Settings>>,
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
    after_leave: Option<Request>,
    signal_thread: Option<std::thread::JoinHandle<()>>,
}

impl InProcess {
    pub fn new(proxy: EventLoopProxy<UiEvent>, _at_login: bool, _dev: bool) -> InProcess {
        InProcess { proxy, cfg: Config::load().ok(), control: None, settings: None, busy: false, error: String::new(), root_problem: None, settings_error: String::new(), permission_request: None, local: super::local::Panel::default(), local_handle: None, after_leave: None, signal_thread: None }
    }

    fn local_request(&mut self, action: Request) {
        let own = self.local.state.as_ref().is_some_and(|s| s.own);
        if (action == Request::Start && self.cfg.is_some() && !own)
            || (action == Request::Delete && (own || (self.cfg.is_none() && self.root_cleanup_pending())))
        {
            self.after_leave = Some(action);
            self.leave();
        } else {
            self.send_local(action);
        }
    }

    fn send_local(&mut self, action: Request) {
        if let Some(handle) = &self.local_handle {
            let proxy = self.proxy.clone();
            handle.request(action, move |result| {
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
        let (browser, watch) = match platform::workspace_browser(control.clone()) {
            Ok(workspace) => workspace,
            Err(e) => {
                self.settings_error = e.to_string();
                return;
            }
        };
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
            let _watch = watch;
            signal::run(params, requests);
        }).expect("spawn signal thread");
        self.signal_thread = Some(thread);
        self.control = Some(control);
        self.settings = Some(settings);
    }

    fn enroll(&mut self, server: String, user: String, password: String) {
        self.busy = true;
        self.error.clear();
        self.root_problem = None;
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let res = crate::enroll_with(&server, &user, &password, "", Config::kept().as_ref()).and_then(Config::settle).map_err(|e| format!("{e:#}"));
            let local = res.as_ref().ok().filter(|cfg| cfg.server_root.is_some()).cloned();
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Enrolled(res)));
            if let Some(cfg) = local {
                let error = crate::install_browser_trust(&cfg).err().map(|e| format!("{e:#}"));
                let _ = proxy.send_event(UiEvent::Backend(BackendEvent::RootChanged(error)));
            }
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
        let control = self.control.clone();
        let thread = self.signal_thread.take();
        std::thread::spawn(move || {
            let result = Config::sign_out().map(|()| {
                if let Some(control) = control {
                    let _ = control.send(Control::Quit);
                }
                if let Some(thread) = thread {
                    let _ = thread.join();
                }
                crate::config::dir().and_then(|dir| platform::remove_installed_server_root(&dir))
                    .err().map(|e| format!("{e:#}"))
            }).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Left(result)));
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
        self.local_handle = crate::config::dir().and_then(|dir| Handle::start(
            dir,
            move |s| { let _ = state.send_event(UiEvent::Backend(BackendEvent::Local(s))); },
            || platform::ui::set_login(true),
            move |cfg| {
                let address = cfg.server.clone();
                let _ = enrolled.send_event(UiEvent::Backend(BackendEvent::Enrolled(Ok(cfg))));
                let error = trust_hub(&address).err().map(|e| format!("{e:#}"));
                let _ = enrolled.send_event(UiEvent::Backend(BackendEvent::RootChanged(error)));
            },
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
            BackendEvent::Enrolled(res) => {
                self.busy = false;
                match res {
                    Ok(cfg) => {
                        self.busy = cfg.server_root.is_some();
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
            BackendEvent::Left(result) => {
                self.busy = false;
                self.root_problem = match result {
                    Ok(error) => error,
                    Err(e) => {
                        if let Some(action) = self.after_leave.take() {
                            self.local.replied(action, Err(e));
                        } else {
                            self.error = e;
                        }
                        return true;
                    }
                };
                self.cfg = None;
                self.control = None;
                self.settings = None;
                // Signed out: nothing here should start at login or keep the Mac awake.
                let _ = crate::power::prevent_sleep(false);
                self.settings_error = platform::ui::set_login(false).with_context(|| t("err.mac_login_off")).err().map(|e| format!("{e:#}")).unwrap_or_default();
                // No host runs now: nothing is shown as controlled.
                let _ = self.proxy.send_event(UiEvent::Status(None));
                if let Some(action) = self.after_leave.take() {
                    if let Some(error) = &self.root_problem {
                        self.local.replied(action, Err(error.clone()));
                    } else {
                        self.send_local(action);
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
        if let Some(c) = &self.control {
            let _ = c.send(Control::Quit);
        }
        self.local_handle.take();
    }
}

fn open_local(url: &str, address: &str) -> Result<()> {
    trust_hub(address)?;
    platform::ui::open_browser(url)
}

fn trust_hub(address: &str) -> Result<()> {
    let address = deskway_server_connection::Address::parse(address)?;
    let dir = crate::config::dir()?;
    let pem = address.fetch_root()?.context("the local root is missing")?;
    crate::config::write_private(&crate::config::root_path(&dir), pem.as_bytes())?;
    platform::install_hub_root(&dir, address.fingerprint().unwrap())
}
