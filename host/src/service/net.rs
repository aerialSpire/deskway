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
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use windows_service::service::UserEventCode;

use super::ipc::{self, Pipe, AGENT_PIPE, UI_PIPE};
use crate::config::Config;
use crate::platform::{Platform, PlatformEvent, Remote};
use crate::settings::{PanelRequest, Settings};
use crate::signal::{self, Control, Status};
use crate::wake::Wake;

/// The host's queue, so the service can end it when it stops.
static HOST: OnceLock<Wake<Control>> = OnceLock::new();

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
}

/// Runs the network service until the process is stopped.
pub fn run(dev: bool, view_only: bool) -> Result<()> {
    if !dev {
        crate::config::use_dir(crate::service::machine_dir());
        super::harden::network();
    }
    crate::i18n::from_config();
    // The panels can connect while the computer is not signed in yet; what they ask waits for it.
    let for_panels = Arc::new(OnceLock::new());
    serve_ui(dev, for_panels.clone())?;

    let (events, from_agent) = channel();
    let (to_host, platform_events) = channel();
    let platform = Remote::new(events);
    std::thread::spawn(move || {
        for event in from_agent {
            if let PlatformEvent::ServerRootSynced { error } = event {
                PANELS.root_problem(error);
            } else if to_host.send(event).is_err() {
                break;
            }
        }
    });
    // A signed-out service also needs the agent, to remove the previous root.
    platform.sync_server_root();
    serve_agent(dev, platform.clone())?;
    let cfg = signed_in();
    if cfg.server_root.is_some() {
        platform.sync_server_root();
    }
    crate::i18n::from_config();
    PANELS.status(Status::offline(&cfg));
    let (control_tx, control_rx) = signal::control_channel();
    let _ = HOST.set(control_tx.clone());

    let dir = crate::config::dir()?;
    let (settings, awake) = Settings::start(dir.clone(), &cfg, Some(platform.clone()), control_tx.clone());
    if let Err(e) = awake {
        eprintln!("{e:#}");
    }
    let settings = Arc::new(settings);
    let _ = for_panels.set(settings.clone());

    // An administrator's command line (`--set-lang`) changes the file, not the pipe.
    let watched = settings.clone();
    super::watch::watch_file(&dir, "host.json", move || {
        if let Err(e) = watched.reload() {
            eprintln!("{e:#}");
        }
        true
    })?;

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
    signal::run(params, control_rx);
    Ok(())
}

/// The sign-in, waited for: until a person signs this computer in from a panel, the panels are
/// told it is signed out. Signing out restarts the service, which waits here again.
fn signed_in() -> Config {
    loop {
        if let Ok(cfg) = Config::load() {
            return cfg;
        }
        PANELS.signed_out(Config::kept());
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The panels connected now, and what they were last told.
struct Panels {
    list: Mutex<Vec<Arc<Pipe>>>,
    left: Condvar,
    latest: Mutex<Option<ToPanel>>,
    root_problem: Mutex<Option<String>>,
}

static PANELS: Panels = Panels { list: Mutex::new(Vec::new()), left: Condvar::new(), latest: Mutex::new(None), root_problem: Mutex::new(None) };

impl Panels {
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
    if let Some(host) = HOST.get() {
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

fn serve_ui(dev: bool, settings: Arc<OnceLock<Arc<Settings>>>) -> Result<()> {
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
            std::thread::spawn(move || {
                while let Ok(r) = pipe.recv_json::<PanelRequest>() {
                    // Not signed in yet: there is nothing to change.
                    let Some(settings) = settings.get() else { continue };
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
    pub fn connect() -> Result<(UiClient, Receiver<ToPanel>)> {
        let pipe = Arc::new(Pipe::connect(UI_PIPE).with_context(|| crate::i18n::t("status.service_down"))?);
        let expected = service_pid(NET_SERVICE)?;
        anyhow::ensure!(pipe.server_pid()? == expected, crate::i18n::t("err.pipe_taken"));
        let (tx, rx) = channel();
        let p = pipe.clone();
        std::thread::spawn(move || {
            while let Ok(m) = p.recv_json::<ToPanel>() {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
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
