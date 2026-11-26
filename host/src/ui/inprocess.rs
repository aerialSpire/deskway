//! The panel's backend on macOS: the host runs in this process, with this computer's platform.
//! Signing in and out happens here too, as do the Mac's own settings: its permissions and the
//! login item.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tao::event_loop::EventLoopProxy;

use super::{Backend, PageMessage, UiEvent};
use crate::config::Config;
use crate::i18n::t;
use crate::platform;
use crate::settings::{PanelRequest, Settings};
use crate::signal::{Control, Status};

pub enum BackendEvent {
    /// Signing in finished: the new config, or why it failed.
    Enrolled(std::result::Result<Config, String>),
    /// Signed out.
    Left,
    /// Ask macOS for a permission (Screen Recording: true), once the panel says it is asked.
    Permission(bool),
    /// Quit gracefully once a launcher is waiting to reopen this bundle.
    Reopen,
}

pub struct InProcess {
    proxy: EventLoopProxy<UiEvent>,
    /// This Mac's config; None while signed out.
    cfg: Option<Config>,
    /// The running host's queue and settings.
    control: Option<Sender<Control>>,
    settings: Option<Arc<Settings>>,
    /// Signing in.
    busy: bool,
    /// Why signing in failed.
    error: String,
    /// Why the last Mac setting could not be changed.
    settings_error: String,
    permission_request: Option<&'static str>,
}

impl InProcess {
    pub fn new(proxy: EventLoopProxy<UiEvent>, _at_login: bool) -> InProcess {
        InProcess { proxy, cfg: Config::load().ok(), control: None, settings: None, busy: false, error: String::new(), settings_error: String::new(), permission_request: None }
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
        let (control, requests) = std::sync::mpsc::channel();
        let dir = match crate::config::dir() {
            Ok(d) => d,
            Err(e) => {
                self.settings_error = format!("{e:#}");
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
            browser: None,
            notify: Box::new(move |s| {
                let _ = proxy.send_event(UiEvent::Status(Some(s)));
            }),
            settings: settings.clone(),
            // This app is the person signed in.
            workspace: Some(std::sync::Arc::new(crate::workspace::Local::default())),
            // A Mac's own workspace asks the person there for administrator rights.
            admin: None,
        };
        std::thread::Builder::new().name("signal".into()).spawn(move || crate::signal::run(params, requests)).expect("spawn signal thread");
        self.control = Some(control);
        self.settings = Some(settings);
    }

    fn enroll(&mut self, server: String, user: String, password: String) {
        self.busy = true;
        self.error.clear();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let res = crate::enroll_with(&server, &user, &password, "", Config::kept().as_ref()).and_then(Config::settle).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UiEvent::Backend(BackendEvent::Enrolled(res)));
        });
    }

    /// Signs out here only: the server keeps this computer, offline, for the same account to
    /// take back (`Config::sign_out`).
    fn leave(&mut self) {
        self.quit();
        if self.cfg.is_some() {
            let _ = self.proxy.send_event(UiEvent::Backend(BackendEvent::Left));
        }
    }
}

impl Backend for InProcess {
    fn start(&mut self) -> bool {
        self.start_host();
        self.cfg.is_none() || !platform::permissions().ready()
    }

    fn extras(&self) -> Value {
        let permissions = platform::permissions();
        let login = platform::ui::login_status();
        json!({
            "enrolled": self.cfg.is_some(), "busy": self.busy, "error": self.error,
            "autostart": login.registered(),
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

    fn request(&mut self, r: PanelRequest) -> bool {
        let result = match r {
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
            PageMessage::Login { server, user, password } => {
                self.enroll(server, user, password);
                return true;
            }
            PageMessage::Leave => {
                self.leave();
                return false;
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
                        self.cfg = Some(cfg);
                        self.error.clear();
                        self.start_host();
                    }
                    Err(e) => self.error = e,
                }
                true
            }
            BackendEvent::Left => {
                self.cfg = None;
                self.control = None;
                self.settings = None;
                // Signed out: nothing here should start at login or keep the Mac awake.
                let _ = crate::power::prevent_sleep(false);
                self.settings_error = platform::ui::set_login(false).with_context(|| t("err.mac_login_off")).err().map(|e| format!("{e:#}")).unwrap_or_default();
                let _ = Config::sign_out();
                // No host runs now: nothing is shown as controlled.
                let _ = self.proxy.send_event(UiEvent::Status(None));
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
        }
    }

    fn quit(&mut self) {
        if let Some(c) = &self.control {
            let _ = c.send(Control::Quit);
        }
    }
}
