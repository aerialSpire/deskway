//! The network service (`DeskwayNet`): signaling and sessions, using the agent's platform
//! through [`Remote`]; the pipe the desktop panels use to reach it, whose requests its
//! [`Settings`] carry out.
//!
//! The panel runs as whoever is signed in, so the pipe takes only what the person at the
//! computer may decide: end the session, the auto-lock, keep-awake, automatic start and
//! shared-clipboard switches, the language, adding a device and taking devices away.
//! Signing the computer in or out, and uninstalling, change files only administrators can
//! write (the panel asks for an elevated copy of this program).
//!
//! Installed but not signed in, the service waits for a sign-in and tells the panels so.

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows_service::service::UserEventCode;

use super::ipc::{self, Pipe, AGENT_PIPE, UI_PIPE};
use crate::config::Config;
use crate::local_server;
use crate::platform::{Platform, PlatformEvent, Remote};
use crate::settings::{PanelRequest, Settings};
use crate::signal::{self, Control, Status};
use crate::wake::Wake;

/// The host's queue, so the service can end it when it stops.
static HOST: Mutex<Option<Wake<Control>>> = Mutex::new(None);
static STOPPING: AtomicBool = AtomicBool::new(false);

pub const NET_SERVICE: &str = "DeskwayNet";
pub const AGENT_SERVICE: &str = "Deskway";

/// Sent to this service by the installer and the uninstaller (`ControlService`, administrators
/// only): every panel is asked to quit, so the program file is no longer in use.
pub const CLOSE_PANELS: UserEventCode = unsafe { UserEventCode::from_unchecked(128) };

/// What the service tells a panel.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ToPanel {
    Status(Status),
    /// This computer is not signed in: the panel offers to sign it in, and names the computer
    /// signed out here, if any, so the same account signing in takes it back.
    SignedOut(Option<crate::config::Kept>),
    /// Deskway is being installed again or removed: the panel quits.
    Exit,
    /// The agent's last browser trust error, if any.
    RootProblem(Option<String>),
    Local(local_server::State),
    LocalOwner(Result<String, String>),
    LocalAccess(bool),
    LocalReply(local_server::Request, Result<local_server::Reply, String>),
    DevLeft(Result<(), String>),
}

/// Runs the network service until the process is stopped.
pub fn run(dev: bool, view_only: bool) -> Result<()> {
    if !dev {
        crate::config::use_dir(crate::service::machine_dir());
        super::harden::network();
        super::KillJob::keep_service_children()?;
    }
    crate::i18n::from_config();
    let for_panels = Arc::new(Mutex::new(None::<Arc<Settings>>));

    let (events, from_agent) = channel();
    let host_events = Arc::new(Mutex::new(None::<std::sync::mpsc::Sender<PlatformEvent>>));
    let to_host = host_events.clone();
    let platform = Remote::new(events);
    std::thread::spawn(move || {
        for event in from_agent {
            if let PlatformEvent::ServerRootSynced { error } = event {
                PANELS.root_problem(error);
            } else if let Some(host) = to_host.lock().unwrap().as_ref() {
                let _ = host.send(event);
            }
        }
    });
    serve_agent(dev, platform.clone())?;
    let dir = crate::config::dir()?;
    let agent = platform.clone();
    let apply_settings = for_panels.clone();
    let local = Arc::new(local_server::Handle::start(
        dir.clone(), |state| PANELS.local(state),
        move |on| {
            if !dev && super::install::autostart() != Some(on) {
                agent.autostart(on);
                let until = std::time::Instant::now() + Duration::from_secs(3);
                while super::install::autostart() != Some(on) {
                    anyhow::ensure!(std::time::Instant::now() < until, "automatic startup setting was not applied");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            Ok(())
        },
        move || !dev && super::install::autostart().unwrap_or(false),
        |_| {},
        dev_clear_sign_in,
        move |cfg| {
            if let (Some(settings), Some(cfg)) = (apply_settings.lock().unwrap().clone(), cfg) {
                if let Err(e) = settings.apply_saved(cfg) {
                    eprintln!("{e:#}");
                }
            }
        },
    )?);
    let watch = Arc::downgrade(&local);
    super::watch::watch_file(&dir, "host.json", move || {
        let Some(local) = watch.upgrade() else { return false };
        local.changed();
        true
    })?;
    serve_ui(dev, for_panels.clone(), Arc::downgrade(&local), platform.clone())?;
    while let Some(cfg) = signed_in() {
        let (to_host, platform_events) = channel();
        *host_events.lock().unwrap() = Some(to_host);
        run_host(dev, view_only, &dir, cfg, platform.clone(), platform_events, &for_panels)?;
        *for_panels.lock().unwrap() = None;
        *HOST.lock().unwrap() = None;
        *host_events.lock().unwrap() = None;
        if !dev {
            break;
        }
    }
    Ok(())
}

fn run_host(
    dev: bool, view_only: bool, dir: &std::path::Path, cfg: Config,
    platform: Arc<Remote>, platform_events: Receiver<PlatformEvent>,
    for_panels: &Mutex<Option<Arc<Settings>>>,
) -> Result<()> {
    crate::i18n::from_config();
    PANELS.status(Status::offline(&cfg));
    let (control_tx, control_rx) = signal::control_channel();
    *HOST.lock().unwrap() = Some(control_tx.clone());

    let (settings, awake) = Settings::start(dir.to_path_buf(), &cfg, Some(platform.clone()), control_tx.clone());
    if let Err(e) = awake {
        eprintln!("{e:#}");
    }
    let settings = Arc::new(settings);
    *for_panels.lock().unwrap() = Some(settings.clone());

    // Run by hand (`--dev`), this process is the person; as the service it is not, and the
    // person's workspace is the user worker's.
    // As the service, the admin worker too, while it runs (`worker::Kind::Admin`); by hand, this
    // process is all there is, and its own rights decide (`platform::start_program`).
    let (workspace, admin, browser): (
        Arc<dyn crate::workspace::Workspace>, Option<Arc<dyn crate::workspace::Workspace>>, crate::workspace::Browser,
    ) = if dev {
        let lock = std::env::var_os("DESKWAY_TEST_WORKSPACE_LOCK").map(std::path::PathBuf::from);
        if lock.is_none() {
            crate::platform::workspace::sweep_home();
        }
        let command = std::env::var("DESKWAY_TEST_WORKSPACE_SHELL").ok();
        (Arc::new(crate::workspace::Local::default()), None, crate::platform::workspace::browser(lock, command))
    } else {
        use super::worker::Kind;
        use crate::workspace::remote::Remote;
        let user = Arc::new(Remote::user(control_tx.clone()));
        let admin = Arc::new(Remote::new(true));
        serve_worker(user.clone(), Kind::User)?;
        serve_worker(admin.clone(), Kind::Admin)?;
        let browser = user.browser();
        (user, Some(admin), browser)
    };
    let _watch = if dev {
        Some(crate::platform::workspace::WorkspaceWatch::start(browser.state.clone(), move |state| {
            let _ = control_tx.send(Control::WorkspaceChanged(state));
        })?)
    } else {
        None
    };
    let workspace = Some(workspace);
    let desktop = Some(signal::DesktopParams { features: crate::platform::DESKTOP, platform, events: platform_events, can_host: Box::new(|| true) });
    let params = signal::Params { cfg, view_only, desktop, browser: Some(browser), notify: Box::new(|st| PANELS.status(st)), settings, workspace, admin };
    if !STOPPING.load(Ordering::Acquire) {
        signal::run(params, control_rx);
    }
    Ok(())
}

/// The sign-in, waited for: until a person signs this computer in from a panel, the panels are
/// told it is signed out. Signing out restarts the service, which waits here again.
fn signed_in() -> Option<Config> {
    while !STOPPING.load(Ordering::Acquire) {
        if let Ok(cfg) = Config::load() {
            if cfg.connection().is_ok() {
                return Some(cfg);
            }
        }
        PANELS.signed_out(Config::kept());
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

/// The panels connected now, and what they were last told.
struct Panels {
    list: Mutex<Vec<Arc<Pipe>>>,
    left: Condvar,
    latest: Mutex<Option<ToPanel>>,
    root_problem: Mutex<Option<String>>,
    local: Mutex<Option<local_server::State>>,
}

static PANELS: Panels = Panels { list: Mutex::new(Vec::new()), left: Condvar::new(), latest: Mutex::new(None), root_problem: Mutex::new(None), local: Mutex::new(None) };

impl Panels {
    fn local(&self, state: local_server::State) {
        *self.local.lock().unwrap() = Some(state.clone());
        self.send(&ToPanel::Local(state));
    }

    fn root_problem(&self, error: Option<String>) {
        *self.root_problem.lock().unwrap() = error.clone();
        self.send(&ToPanel::RootProblem(error));
    }

    fn status(&self, st: Status) {
        *self.latest.lock().unwrap() = Some(ToPanel::Status(st.clone()));
        self.send(&ToPanel::Status(st));
    }

    /// Said once, not every time the sign-in is looked for.
    fn signed_out(&self, kept: Option<crate::config::Kept>) {
        let mut latest = self.latest.lock().unwrap();
        if matches!(&*latest, Some(ToPanel::SignedOut(k)) if *k == kept) {
            return;
        }
        let m = ToPanel::SignedOut(kept);
        *latest = Some(m.clone());
        drop(latest);
        self.send(&m);
    }

    /// Sent outside the lock; a panel that stopped reading is dropped.
    fn send(&self, m: &ToPanel) {
        let now: Vec<Arc<Pipe>> = self.list.lock().unwrap().clone();
        let gone: Vec<Arc<Pipe>> = now.into_iter().filter(|p| p.send_json(m).is_err()).collect();
        for p in gone {
            self.leave(&p);
        }
    }

    fn join(&self, p: Arc<Pipe>) {
        if let Some(m) = self.latest.lock().unwrap().clone() {
            let _ = p.send_json(&m);
        }
        let root_problem = self.root_problem.lock().unwrap();
        let _ = p.send_json(&ToPanel::RootProblem(root_problem.clone()));
        if let Some(state) = self.local.lock().unwrap().clone() {
            let _ = p.send_json(&ToPanel::Local(state));
        }
        self.list.lock().unwrap().push(p);
    }

    fn leave(&self, p: &Arc<Pipe>) {
        self.list.lock().unwrap().retain(|c| !Arc::ptr_eq(c, p));
        self.left.notify_all();
    }

    /// Asks every panel to quit and waits until they have closed their pipes, or `wait` ends.
    fn close_all(&self, wait: Duration) -> usize {
        self.send(&ToPanel::Exit);
        let list = self.list.lock().unwrap();
        let (list, _) = self.left.wait_timeout_while(list, wait, |l| !l.is_empty()).unwrap();
        list.len()
    }
}

/// The service is stopping: the session in progress ends, and its browser is told so instead
/// of trying to reconnect.
pub fn stop() {
    STOPPING.store(true, Ordering::Release);
    if let Some(host) = HOST.lock().unwrap().as_ref() {
        let _ = host.send(Control::Quit);
    }
}

/// The installer asks (`CLOSE_PANELS`): every panel quits by itself.
pub fn close_panels() {
    let left = PANELS.close_all(Duration::from_secs(10));
    eprintln!("panels asked to quit; {left} still connected");
}

/// Waits for the agent on its pipe, one connection at a time, for as long as the service runs.
fn serve_agent(dev: bool, platform: Arc<Remote>) -> Result<()> {
    let path = super::agent::key_path(dev)?;
    if dev {
        ipc::load_key(&path, true)?;
    }
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", ipc::own_sid()?);
    std::thread::Builder::new().name("agent-link".into()).spawn(move || {
        // Created once and kept: between agents the name stays ours, so no other program can
        // take it while the console session changes.
        let pipe = loop {
            match Pipe::create(&ipc::pipe_name(AGENT_PIPE, dev), &sddl) {
                Ok(p) => break Arc::new(p),
                Err(e) => {
                    eprintln!("agent link: {e:#} (another program holds the agent pipe; remote control is unavailable until it exits)");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        };
        loop {
            let r = pipe.accept().and_then(|()| {
                // The key is read for every connection: the service writes a new one when it starts.
                let key = ipc::load_key(&path, dev)?;
                ipc::handshake_server(&pipe, &key)?;
                eprintln!("agent link: agent connected");
                let served = platform.attach(pipe.clone());
                eprintln!("agent link: agent left");
                served
            });
            if let Err(e) = r {
                eprintln!("agent link: {e:#}");
            }
            pipe.disconnect();
            std::thread::sleep(Duration::from_millis(200));
        }
    })?;
    Ok(())
}

/// A worker's pipe: interactive users may connect (the worker is the person) but not serve it;
/// the key of this run (`worker::Kind::key_path`) tells the worker from anyone else.
fn serve_worker(remote: Arc<crate::workspace::remote::Remote>, kind: super::worker::Kind) -> Result<()> {
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})(A;;0x12019b;;;IU)", ipc::own_sid()?);
    std::thread::Builder::new().name(format!("{}-link", kind.what())).spawn(move || {
        let pipe = loop {
            match Pipe::create(kind.pipe(), &sddl) {
                Ok(p) => break Arc::new(p),
                Err(e) => {
                    eprintln!("{} link: {e:#}", kind.what());
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        };
        loop {
            let r = pipe.accept().and_then(|()| {
                let key = ipc::load_key(&kind.key_path(), false)?;
                ipc::handshake_server(&pipe, &key)?;
                eprintln!("{} link: connected", kind.what());
                let served = remote.attach(pipe.clone());
                eprintln!("{} link: left", kind.what());
                served
            });
            if let Err(e) = r {
                eprintln!("{} link: {e:#}", kind.what());
            }
            pipe.disconnect();
            std::thread::sleep(Duration::from_millis(200));
        }
    })?;
    Ok(())
}

fn dev_clear_sign_in(dir: &std::path::Path) -> Result<()> {
    if let Some(host) = HOST.lock().unwrap().as_ref() {
        let _ = host.send(Control::Quit);
    }
    crate::config::remove_sign_in_root(dir).and(crate::platform::sync_dev_browser_root(dir, None))
}

fn serve_ui(dev: bool, settings: Arc<Mutex<Option<Arc<Settings>>>>, local: Weak<local_server::Handle>, agent: Arc<Remote>) -> Result<()> {
    let ui_name = ipc::pipe_name(UI_PIPE, dev);
    // SYSTEM and this service have full access. Signed-in (interactive) users may read and
    // write, but 0x12019B leaves out FILE_CREATE_PIPE_INSTANCE: they cannot add servers of their own.
    let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})(A;;0x12019b;;;IU)", ipc::own_sid()?);
    std::thread::Builder::new().name("ui-pipe".into()).spawn(move || {
        // The first instance claims the name; one listening instance always holds it after that.
        let mut next = loop {
            match Pipe::create_shared(&ui_name, &sddl) {
                Ok(p) => break p,
                Err(e) => {
                    eprintln!("ui pipe: {e:#}");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        };
        loop {
            if next.accept().is_err() {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            let pipe = Arc::new(next);
            next = loop {
                match Pipe::create_more(&ui_name, &sddl) {
                    Ok(p) => break p,
                    Err(e) => {
                        eprintln!("ui pipe: {e:#}");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            };
            PANELS.join(pipe.clone());
            let settings = settings.clone();
            let local = local.clone();
            let agent = agent.clone();
            std::thread::spawn(move || {
                let admin_link_allowed = || -> Result<bool> {
                    let owner = if dev {
                        Some(ipc::own_sid()?)
                    } else {
                        Config::local_service(&super::machine_dir())?.and_then(|s| s.owner)
                    };
                    pipe.allows_admin_link(owner.as_deref())
                };
                while let Ok(r) = pipe.recv_json::<PanelRequest>() {
                    if r == PanelRequest::LocalOwner {
                        let owner = local_server::localized(pipe.client_sid(), "local.owner_failed")
                            .map_err(|e| e.to_string());
                        let _ = pipe.send_json(&ToPanel::LocalOwner(owner));
                        continue;
                    }
                    if r == PanelRequest::LocalAccess {
                        let _ = pipe.send_json(&ToPanel::LocalAccess(admin_link_allowed().unwrap_or(false)));
                        continue;
                    }
                    if let PanelRequest::Local(request) = r {
                        use local_server::Request::*;
                        if matches!(request, Start | StopAndDelete) && !dev {
                            let _ = pipe.send_json(&ToPanel::LocalReply(request, Err(crate::i18n::t("err.need_admin"))));
                        } else if matches!(request, Setup | Open) && !admin_link_allowed().unwrap_or(false) {
                            let _ = pipe.send_json(&ToPanel::LocalReply(request, Err(crate::i18n::t("local.link_restricted"))));
                        } else if let Some(local) = local.upgrade() {
                            let reply = pipe.clone();
                            local.request(request, None, move |r| {
                                let _ = reply.send_json(&ToPanel::LocalReply(request, r));
                            });
                        }
                        continue;
                    }
                    if r == PanelRequest::SyncLocalRoot {
                        agent.sync_server_root();
                        continue;
                    }
                    if r == PanelRequest::DevLeave {
                        if dev {
                            let result = crate::config::dir()
                                .and_then(|dir| Config::sign_out().and_then(|()| dev_clear_sign_in(&dir)))
                                .map_err(|e| format!("{e:#}"));
                            let _ = pipe.send_json(&ToPanel::DevLeft(result));
                        }
                        continue;
                    }
                    // Not signed in yet: there is nothing to change.
                    let Some(settings) = settings.lock().unwrap().clone() else { continue };
                    if let Err(e) = settings.handle(r) {
                        eprintln!("{e:#}");
                    }
                }
                PANELS.leave(&pipe);
            });
        }
    })?;
    Ok(())
}

/// Whether the system service is installed, in which case the panel is only a remote for it.
pub fn installed() -> bool {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|m| m.open_service(NET_SERVICE, ServiceAccess::QUERY_STATUS))
        .is_ok()
}

/// The panel's end of the pipe.
pub struct UiClient {
    pipe: Arc<Pipe>,
}

impl UiClient {
    /// Connects, and checks the pipe really belongs to the running network service: a program
    /// that grabbed the name first could otherwise show a false status.
    pub fn connect(dev: bool) -> Result<(UiClient, Receiver<ToPanel>)> {
        let pipe = Arc::new(Pipe::connect(&ipc::pipe_name(UI_PIPE, dev)).with_context(|| crate::i18n::t("status.service_down"))?);
        if !dev {
            let expected = service_pid(NET_SERVICE)?;
            anyhow::ensure!(pipe.server_pid()? == expected, crate::i18n::t("err.pipe_taken"));
        }
        let (tx, rx) = channel();
        let p = pipe.clone();
        std::thread::spawn(move || {
            while let Ok(m) = p.recv_json::<ToPanel>() {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
        pipe.send_json(&PanelRequest::LocalAccess)?;
        Ok((UiClient { pipe }, rx))
    }

    pub fn send(&self, r: PanelRequest) -> Result<()> {
        self.pipe.send_json(&r)
    }
}

pub fn service_pid(name: &str) -> Result<u32> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let s = m.open_service(name, ServiceAccess::QUERY_STATUS)?;
    s.query_status()?.process_id.with_context(|| crate::i18n::t("status.service_down"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_messages_round_trip() {
        let exit = serde_json::to_string(&ToPanel::Exit).unwrap();
        assert!(matches!(serde_json::from_str::<ToPanel>(&exit).unwrap(), ToPanel::Exit));
        let kept = crate::config::Kept { server: "https://s.example".into(), id: "H1".into(), user: Some("me".into()) };
        let out = serde_json::to_string(&ToPanel::SignedOut(Some(kept.clone()))).unwrap();
        assert!(matches!(serde_json::from_str::<ToPanel>(&out).unwrap(), ToPanel::SignedOut(Some(k)) if k == kept));
        let st = Status { prevent_sleep: true, ..Status::default() };
        let back: ToPanel = serde_json::from_str(&serde_json::to_string(&ToPanel::Status(st)).unwrap()).unwrap();
        assert!(matches!(back, ToPanel::Status(s) if s.prevent_sleep));
    }
}
