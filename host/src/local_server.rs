//! Owns the saved local-service switch, server child and automatic hub enrollment.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use deskway_server_connection::{root_der, Address};
use serde::{Deserialize, Serialize};
use wire::crypto::{hex, sha256};

use crate::config::Config;
use crate::kept::{Child, Kept};

pub const DATA_DIR: &str = "local-server";
pub const SERVER_EXE: &str = if cfg!(windows) {
    "deskway-server.exe"
} else {
    "deskway-server"
};
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct Setting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guide: Option<u16>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Preferences>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fingerprint: Option<String>,
    #[cfg(windows)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

impl Setting {
    fn ports(&self) -> Option<(u16, u16)> {
        self.https.zip(self.guide)
    }

    fn remember_bound(&mut self, dir: &Path, ports: (u16, u16), fingerprint: String) -> Result<()> {
        if self.ports().is_some() && self.fingerprint.as_ref() == Some(&fingerprint) {
            return Ok(());
        }
        let (https, guide) = self.ports().unwrap_or(ports);
        let bound = Self {
            https: Some(https),
            guide: Some(guide),
            fingerprint: Some(fingerprint),
            ..self.clone()
        };
        Config::set_local_service(dir, &bound)?;
        *self = bound;
        Ok(())
    }

    pub fn enable(&mut self, dir: &Path, previous: Preferences) -> Result<()> {
        if !self.enabled && self.previous.is_none() {
            self.previous = Some(previous);
        }
        self.enabled = true;
        Config::set_local_service(dir, self)
    }

    pub fn disable(
        &mut self,
        dir: &Path,
        set_autostart: impl FnOnce(bool) -> Result<()>,
    ) -> Result<()> {
        // Save the off switch before any preference restoration can fail.
        let disabled = Self {
            enabled: false,
            ..self.clone()
        };
        Config::set_local_service(dir, &disabled)?;
        *self = disabled;
        // Keep the snapshot on disk if restoring the OS setting has to be retried.
        if let Some(previous) = self.previous {
            set_autostart(previous.autostart)?;
            let restored = Self {
                previous: None,
                ..self.clone()
            };
            Config::set_local_service(dir, &restored)?;
            *self = restored;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preferences {
    pub autostart: bool,
    pub prevent_sleep: bool,
}

impl Preferences {
    pub fn read(dir: &Path, autostart: bool) -> Result<Self> {
        Ok(Self {
            autostart,
            prevent_sleep: Config::prevent_sleep_setting(dir)?,
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    Start,
    Stop,
    StopAndDelete,
    Retry,
    Setup,
    Open,
    Usage,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    Off,
    Starting,
    Setup,
    Running,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct State {
    pub available: bool,
    pub enabled: bool,
    pub own: bool,
    pub has_data: bool,
    phase: Phase,
    port: Option<u16>,
    error: Option<String>,
    setup_error: Option<String>,
}

impl State {
    pub fn badge(&self) -> Option<bool> {
        match self.phase {
            Phase::Running | Phase::Setup => Some(true),
            Phase::Failed => Some(false),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Done,
    Browser { url: String, address: String },
    Usage { computers: usize, browsers: usize },
}

type Respond = Box<dyn FnOnce(Result<Reply, String>) + Send>;

enum Message {
    Request(Request, Option<Preferences>, Respond),
    Shutdown,
}

pub struct Handle {
    tx: Sender<Message>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    pub fn start(
        dir: PathBuf,
        publish: impl Fn(State) + Send + 'static,
        autostart: impl Fn(bool) -> Result<()> + Send + 'static,
        read_autostart: impl Fn() -> bool + Send + 'static,
        enrolled: impl Fn(Config) + Send + 'static,
        sign_out: impl Fn(&Path) -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("local-server".into())
            .spawn(move || {
                let mut server = Server::new(dir);
                let mut last = None;
                loop {
                    let request = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Message::Request(request, previous, reply)) => {
                            Some((request, previous, reply))
                        }
                        Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                    };
                    let complete = request.and_then(|(request, previous, reply)| {
                        server.request(
                            request,
                            previous,
                            reply,
                            &autostart,
                            &read_autostart,
                            &sign_out,
                        )
                    });
                    server.tick(&autostart, &enrolled);
                    let state = server.state();
                    if last.as_ref() != Some(&state) {
                        publish(state.clone());
                        last = Some(state);
                    }
                    if let Some((reply, result)) = complete {
                        reply(result.map_err(|e| format!("{e:#}")));
                    }
                    if !matches!(server.phase, Phase::Starting) {
                        if let Some(reply) = server.starting.take() {
                            reply(Ok(Reply::Done));
                        }
                    }
                }
            })?;
        Ok(Self {
            tx,
            thread: Some(thread),
        })
    }

    pub fn request(
        &self,
        request: Request,
        previous: Option<Preferences>,
        reply: impl FnOnce(Result<Reply, String>) + Send + 'static,
    ) {
        if let Err(mpsc::SendError(Message::Request(_, _, reply))) =
            self.tx
                .send(Message::Request(request, previous, Box::new(reply)))
        {
            reply(Err(crate::i18n::t("local.failed")));
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn data_dir(dir: &Path) -> PathBuf {
    dir.join(DATA_DIR)
}

pub fn remove_data(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(data_dir(dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).context("remove the local service data")
        }
        _ => Ok(()),
    }
}

/// Check the whole tree before removing anything. In particular, a Windows file held without
/// delete sharing would otherwise stop the recursive removal after it had erased other files.
fn check_delete(path: &Path, parent: bool) -> std::io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Err(e) if parent && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        use windows::Win32::Storage::FileSystem::*;
        fs::OpenOptions::new()
            .access_mode(DELETE.0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(path)?;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    };
    #[cfg(unix)]
    let reparse = meta.file_type().is_symlink();
    if meta.is_dir() && !reparse {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(path.as_os_str().as_bytes())?;
            if unsafe { libc::access(name.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        for entry in fs::read_dir(path)? {
            check_delete(&entry?.path(), false)?;
        }
    }
    #[cfg(windows)]
    if meta.is_file() && meta.permissions().readonly() && !reparse {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is read-only", path.display()),
        ));
    }
    Ok(())
}

/// The supervisor (or the elevated Windows command) has stopped the child and saved the
/// switch. Identify the hub enrollment while its CA is still available.
pub fn delete_stopped_data(dir: &Path, sign_out: impl FnOnce() -> Result<()>) -> Result<()> {
    let clear = match Config::load_from(dir) {
        Ok(cfg) if is_own(dir, &cfg.server) => {
            // Keep identifying this enrollment if deleting succeeds but clearing it must be retried.
            if let Some(mut setting) =
                Config::local_service(dir)?.filter(|s| s.fingerprint.is_none())
            {
                let address = Address::parse(&cfg.server)?;
                setting.fingerprint = address.fingerprint().map(str::to_owned);
                Config::set_local_service(dir, &setting)?;
            }
            true
        }
        Ok(_) => false,
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            true
        }
        Err(e) => return Err(e),
    };
    delete_then_clear(
        || {
            check_delete(&data_dir(dir), true)
                .context("check the local service data can be deleted")?;
            remove_data(dir)
        },
        || {
            if clear {
                sign_out()?;
            }
            Ok(())
        },
    )
}

fn delete_then_clear(
    delete: impl FnOnce() -> Result<()>,
    clear: impl FnOnce() -> Result<()>,
) -> Result<()> {
    delete()?;
    clear()
}

pub fn is_own(dir: &Path, address: &str) -> bool {
    if let Ok(Some(setting)) = Config::local_service(dir) {
        if let Some((port, pin)) = setting.https.zip(setting.fingerprint.as_deref()) {
            return loopback(port, pin).is_ok_and(|own| own.saved() == address);
        }
    }
    own_address(dir).is_ok_and(|own| own.saved() == address)
}

pub fn own_address(dir: &Path) -> Result<Address> {
    let (https, _) = Config::local_service(dir)?
        .and_then(|setting| setting.ports())
        .context("local service has no assigned ports")?;
    let pem = fs::read_to_string(data_dir(dir).join("local-ca").join("ca.crt"))?;
    let der = root_der(&pem)?;
    loopback(https, &hex(&sha256(&der)))
}

fn loopback(port: u16, pin: &str) -> Result<Address> {
    Address::parse(&format!("https://localhost:{port}#ca={pin}"))
}

fn choose_ports(after: Option<(u16, u16)>, free: impl Fn(u16, u16) -> bool) -> Result<(u16, u16)> {
    std::iter::once((443, 80))
        .chain((8443..=u16::MAX).zip(8080..))
        .find(|&(https, guide)| {
            after.is_none_or(|(previous, _)| https > previous) && free(https, guide)
        })
        .context("no free local service ports")
}

fn free_pair(https: u16, guide: u16) -> bool {
    let ipv6 = TcpListener::bind(("::", 0)).is_ok();
    let free = |address| {
        TcpListener::bind((address, https))
            .and_then(|_https| TcpListener::bind((address, guide)))
            .is_ok()
    };
    free("0.0.0.0") && (!ipv6 || free("::"))
}

struct Server {
    dir: PathBuf,
    exe: Option<PathBuf>,
    setting: Option<Setting>,
    candidate: Option<(u16, u16)>,
    kept: Kept<Process>,
    phase: Phase,
    port: Option<u16>,
    error: Option<String>,
    ready: Option<Ready>,
    next_setup: Option<Instant>,
    setup_error: Option<String>,
    setup_failures: u32,
    force: bool,
    starting: Option<Respond>,
    usage: Option<(Instant, Respond)>,
}

struct Ready {
    hostname: String,
    address: Address,
}

fn browser_origin(hostname: &str, port: u16, resolves: impl Fn(&str, u16) -> bool) -> String {
    let name = format!("{hostname}.local");
    let host = if resolves(&name, port) {
        name.as_str()
    } else {
        "localhost"
    };
    format!("https://{host}:{port}")
}

impl Server {
    fn new(dir: PathBuf) -> Self {
        let setting = Config::local_service(&dir).unwrap_or_else(|e| {
            eprintln!("read local service setting: {e:#}");
            None
        });
        if setting.as_ref().is_some_and(|s| !s.enabled) {
            if let Err(e) =
                Config::prevent_sleep_setting(&dir).and_then(crate::platform::prevent_sleep)
            {
                eprintln!("restore sleep prevention: {e:#}");
            }
        }
        Self {
            dir,
            exe: std::env::current_exe()
                .ok()
                .map(|p| p.with_file_name(SERVER_EXE))
                .filter(|p| p.is_file()),
            setting,
            candidate: None,
            kept: Kept::new("local service"),
            phase: Phase::Off,
            port: None,
            error: None,
            ready: None,
            next_setup: None,
            setup_error: None,
            setup_failures: 0,
            force: true,
            starting: None,
            usage: None,
        }
    }

    fn state(&self) -> State {
        State {
            available: self.exe.is_some(),
            enabled: self.setting.as_ref().is_some_and(|s| s.enabled),
            own: Config::load_from(&self.dir).is_ok_and(|c| is_own(&self.dir, &c.server)),
            has_data: data_dir(&self.dir).is_dir(),
            phase: self.phase,
            port: self.port,
            error: self
                .error
                .clone()
                .or_else(|| self.kept.last_error().map(str::to_owned)),
            setup_error: self.setup_error.clone(),
        }
    }

    fn request(
        &mut self,
        request: Request,
        previous: Option<Preferences>,
        reply: Respond,
        autostart: &impl Fn(bool) -> Result<()>,
        read_autostart: &impl Fn() -> bool,
        sign_out: &impl Fn(&Path) -> Result<()>,
    ) -> Option<(Respond, Result<Reply>)> {
        let result = match request {
            Request::Start => self.start(previous, read_autostart),
            Request::Retry => self.retry(),
            Request::Stop | Request::StopAndDelete => self.stop(autostart).and_then(|()| {
                if request == Request::StopAndDelete {
                    delete_stopped_data(&self.dir, || sign_out(&self.dir))?;
                }
                Ok(Reply::Done)
            }),
            Request::Setup | Request::Open => self.link(request),
            Request::Usage => {
                if let Some(process) = self.kept.child() {
                    let own = Config::load_from(&self.dir)
                        .ok()
                        .filter(|c| is_own(&self.dir, &c.server));
                    if self.usage.is_none()
                        && process
                            .ask_usage(own.as_ref().map(|c| c.id.as_str()))
                            .is_ok()
                    {
                        self.usage = Some((Instant::now() + Duration::from_secs(2), reply));
                        return None;
                    }
                }
                Ok(Reply::Done)
            }
        };
        if matches!(request, Request::Start | Request::Retry) && result.is_ok() {
            if let Some(previous) = self.starting.replace(reply) {
                previous(Ok(Reply::Done));
            }
            return None;
        }
        Some((reply, result))
    }

    fn retry(&mut self) -> Result<Reply> {
        ensure!(
            self.setting.as_ref().is_some_and(|s| s.enabled),
            crate::i18n::t("local.failed")
        );
        if self.ready.is_some() && self.setup_error.is_some() {
            self.next_setup = Some(Instant::now());
            return Ok(Reply::Done);
        }
        self.kept.stop();
        self.candidate = None;
        self.ready = None;
        self.port = None;
        self.force = true;
        self.error = None;
        self.phase = Phase::Starting;
        Ok(Reply::Done)
    }

    fn start(
        &mut self,
        previous: Option<Preferences>,
        read_autostart: &impl Fn() -> bool,
    ) -> Result<Reply> {
        ensure!(self.exe.is_some(), crate::i18n::t("local.unavailable"));
        ensure!(
            !Config::load_from(&self.dir).is_ok_and(|c| !is_own(&self.dir, &c.server)),
            crate::i18n::t("local.leave_first")
        );
        let mut setting = self.setting.clone().unwrap_or_default();
        let previous = match previous {
            Some(previous) => previous,
            None => Preferences::read(&self.dir, read_autostart())?,
        };
        setting.enable(&self.dir, previous)?;
        self.setting = Some(setting);
        if self.ready.is_none() {
            self.phase = Phase::Starting;
        }
        self.force = true;
        Ok(Reply::Done)
    }

    fn stop(&mut self, autostart: &impl Fn(bool) -> Result<()>) -> Result<()> {
        self.kept.stop();
        let previous = self.setting.as_ref().and_then(|s| s.previous);
        let disabled = self
            .setting
            .as_mut()
            .map_or(Ok(()), |saved| saved.disable(&self.dir, autostart));
        self.candidate = None;
        self.ready = None;
        self.phase = Phase::Off;
        self.port = None;
        self.error = None;
        self.reset_setup();
        if let Some((_, reply)) = self.usage.take() {
            reply(Ok(Reply::Done));
        }
        let awake = previous.map_or_else(
            || Config::load_from(&self.dir).is_ok_and(|cfg| cfg.prevent_sleep),
            |prior| prior.prevent_sleep,
        );
        let sleep = crate::platform::prevent_sleep(awake);
        disabled.and(sleep)
    }

    fn admin_link(&self, origin: &str, args: &[&str]) -> Result<String> {
        let exe = self.exe.as_ref().context("no local server program")?;
        check_program(exe)?;
        let result = command(exe)
            .arg("admin-link")
            .args(args)
            .arg("-data")
            .arg(data_dir(&self.dir).join("state.json"))
            .args(["-url", origin])
            .output()?;
        ensure!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
        Ok(String::from_utf8(result.stdout)?.trim().to_owned())
    }

    fn link(&self, request: Request) -> Result<Reply> {
        let ready = self
            .ready
            .as_ref()
            .context(crate::i18n::t("local.failed"))?;
        let mode = if request == Request::Setup {
            "-if-needed"
        } else {
            "-login"
        };
        let port = self
            .setting
            .as_ref()
            .and_then(|s| s.https)
            .context("no local HTTPS port")?;
        let origin = browser_origin(&ready.hostname, port, |name, port| {
            (name, port)
                .to_socket_addrs()
                .is_ok_and(|mut addrs| addrs.next().is_some())
        });
        let url = self.admin_link(&origin, &[mode])?;
        if url.is_empty() {
            return Ok(Reply::Done);
        }
        ensure!(
            url.starts_with(&format!("{origin}/setup#c=")),
            "invalid local browser link"
        );
        Ok(Reply::Browser {
            url,
            address: ready.address.saved(),
        })
    }

    fn apply(&mut self, autostart: &impl Fn(bool) -> Result<()>) {
        if self.force {
            self.force = false;
            let result = autostart(true).and_then(|()| crate::platform::prevent_sleep(true));
            if let Err(e) = result {
                self.error = Some(format!("{e:#}"));
                self.phase = Phase::Failed;
            }
        }
    }

    fn tick(&mut self, autostart: &impl Fn(bool) -> Result<()>, enrolled: &impl Fn(Config)) {
        if !self.setting.as_ref().is_some_and(|s| s.enabled) {
            return;
        }
        self.apply(autostart);
        if let Err(e) = self.tick_once() {
            self.kept.stop();
            self.ready = None;
            self.reset_setup();
            self.error = Some(format!("{e:#}"));
            self.phase = Phase::Failed;
        }
        self.poll_setup(Instant::now(), |server| server.check_setup(enrolled));
        if self
            .usage
            .as_ref()
            .is_some_and(|(deadline, _)| *deadline <= Instant::now())
        {
            self.usage.take().unwrap().1(Ok(Reply::Done));
        }
    }

    fn poll_setup(&mut self, now: Instant, check: impl FnOnce(&mut Self) -> Result<()>) {
        if self.ready.is_none() || !self.next_setup.is_some_and(|next| next <= now) {
            return;
        }
        self.next_setup = None;
        match check(self) {
            Ok(()) => {
                self.setup_error = None;
                self.setup_failures = 0;
            }
            Err(e) => {
                self.setup_error = Some(format!("{e:#}"));
                self.phase = Phase::Setup;
                self.next_setup =
                    Some(Instant::now() + crate::kept::retry_delay(self.setup_failures));
                self.setup_failures = self.setup_failures.saturating_add(1);
            }
        }
    }

    fn reset_setup(&mut self) {
        self.next_setup = None;
        self.setup_error = None;
        self.setup_failures = 0;
    }

    fn tick_once(&mut self) -> Result<()> {
        if self.port.is_some() || self.error.is_some() {
            return Ok(());
        }
        let Some(exe) = self.exe.as_ref() else {
            self.phase = Phase::Failed;
            return Ok(());
        };
        let setting = self.setting.as_mut().unwrap();
        let saved = setting.ports();
        let ports = match saved.or(self.candidate) {
            Some(ports) => ports,
            None => {
                let ports = choose_ports(None, free_pair)?;
                self.candidate = Some(ports);
                ports
            }
        };
        let dir = data_dir(&self.dir);
        // Read buffered output first, including a bind error from an exited child.
        let messages = self.kept.child().map(Process::messages).unwrap_or_default();
        for message in messages {
            match message {
                Output::Port { port } => {
                    self.kept.stop();
                    if saved.is_some() {
                        self.port = Some(port);
                        self.phase = Phase::Failed;
                    } else {
                        self.candidate = Some(choose_ports(Some(ports), free_pair)?);
                    }
                    return Ok(());
                }
                Output::Ready {
                    version,
                    hostname,
                    fingerprint,
                } => {
                    let address = ready_address(version, ports.0, &fingerprint)?;
                    setting.remember_bound(&self.dir, ports, fingerprint)?;
                    self.ready = Some(Ready { hostname, address });
                    self.next_setup = Some(Instant::now());
                }
                Output::Usage {
                    computers,
                    browsers,
                } => {
                    if let Some((_, reply)) = self.usage.take() {
                        reply(Ok(Reply::Usage {
                            computers,
                            browsers,
                        }));
                    }
                }
            }
        }
        self.kept.keep((), || Process::spawn(exe, &dir, ports));
        if self.kept.child().is_none() {
            self.ready = None;
            self.reset_setup();
            self.phase = if self.kept.failures() >= 3 {
                Phase::Failed
            } else {
                Phase::Starting
            };
        } else if self.ready.is_none() && self.phase == Phase::Off {
            self.phase = Phase::Starting;
        }
        if self.ready.is_none() {
            if let Some(child) = self.kept.child() {
                if child.started.elapsed() > Duration::from_secs(12) {
                    let _ = child.process.kill();
                }
            }
        }
        Ok(())
    }

    fn check_setup(&mut self, enrolled: &impl Fn(Config)) -> Result<()> {
        let ready = self.ready.as_ref().unwrap();
        match Config::load_from(&self.dir) {
            Ok(cfg) => {
                ensure!(
                    cfg.server == ready.address.saved(),
                    crate::i18n::t("local.leave_first")
                );
                cfg.connection()?;
                self.phase = Phase::Running;
                return Ok(());
            }
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(e) => return Err(e),
        }
        let origin = format!(
            "https://localhost:{}",
            self.setting.as_ref().unwrap().https.unwrap()
        );
        if !self
            .admin_link(&origin, &["-if-needed", "-check"])?
            .is_empty()
        {
            self.phase = Phase::Setup;
            self.next_setup = Some(Instant::now() + Duration::from_secs(2));
            return Ok(());
        }
        let link = self.admin_link(&origin, &["-enroll"])?;
        let token = link
            .strip_prefix(&format!("{origin}/api/computers/enroll#c="))
            .filter(|token| !token.is_empty())
            .context("invalid local enrollment link")?;
        let cfg = crate::enroll(
            &ready.address.saved(),
            "",
            None,
            crate::Enrollment::Link(token),
        )?;
        let cfg = match Config::settle_in(&self.dir, cfg.clone()) {
            Ok(cfg) => cfg,
            Err(e) => {
                let _ = crate::withdraw(&cfg);
                return Err(e);
            }
        };
        enrolled(cfg);
        self.phase = Phase::Running;
        Ok(())
    }
}

fn ready_address(version: u32, port: u16, fingerprint: &str) -> Result<Address> {
    ensure!(
        version == VERSION,
        "local server control version {version} is unsupported"
    );
    loopback(port, fingerprint)
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Output {
    Ready {
        version: u32,
        hostname: String,
        fingerprint: String,
    },
    Port {
        port: u16,
    },
    Usage {
        computers: usize,
        browsers: usize,
    },
}

struct Process {
    process: std::process::Child,
    input: Option<ChildStdin>,
    output: Receiver<Output>,
    read: Option<std::thread::JoinHandle<()>>,
    started: Instant,
}

fn check_program(exe: &Path) -> Result<()> {
    #[cfg(windows)]
    if crate::config::dir()? == crate::service::machine_dir() {
        crate::service::install::check_server_program(exe)?;
    }
    let _ = exe;
    Ok(())
}

fn command(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    }
    command
}

impl Process {
    fn spawn(exe: &Path, dir: &Path, (https, guide): (u16, u16)) -> Result<Self> {
        check_program(exe)?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
        let name: String = crate::shown_name("")
            .chars()
            .filter(|c| !c.is_control())
            .take(40)
            .collect();
        let mut process = command(exe)
            .args([
                "-local",
                "-host-supervised",
                "-listen",
                &format!(":{https}"),
                "-guide-listen",
                &format!(":{guide}"),
            ])
            .arg("-data")
            .arg(dir.join("state.json"))
            .args(["-local-name", &name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = process.stdin.take();
        let stdout = process.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        let read = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str(&line) {
                    if tx.send(message).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self {
            process,
            input,
            output,
            read: Some(read),
            started: Instant::now(),
        })
    }

    fn messages(&mut self) -> Vec<Output> {
        if !self.running() {
            self.finish_reading();
        }
        self.output.try_iter().collect()
    }

    fn ask_usage(&mut self, own: Option<&str>) -> Result<()> {
        let request = own.map_or_else(|| "usage\n".to_owned(), |id| format!("usage {id}\n"));
        self.input
            .as_mut()
            .context("local service has no control pipe")?
            .write_all(request.as_bytes())?;
        Ok(())
    }

    fn finish_reading(&mut self) {
        if let Some(read) = self.read.take() {
            let _ = read.join();
        }
    }
}

impl Child for Process {
    fn running(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        // Closing stdin lets the server withdraw mDNS and close its connections.
        let deadline = Instant::now() + Duration::from_secs(5);
        self.input.take();
        while self.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.running() {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
        self.finish_reading();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_choice_prefers_standard_ports_then_the_first_whole_pair() {
        for (available, expected) in [
            (vec![443, 80, 8443, 8080], (443, 80)),
            (vec![443, 8443, 8080], (8443, 8080)),
            (vec![8443, 8081, 8444], (8444, 8081)),
        ] {
            assert_eq!(
                choose_ports(None, |a, b| available.contains(&a)
                    && available.contains(&b))
                .unwrap(),
                expected
            );
        }
        assert_eq!(
            choose_ports(Some((443, 80)), |_, _| true).unwrap(),
            (8443, 8080)
        );
        assert_eq!(
            choose_ports(Some((8443, 8080)), |_, _| true).unwrap(),
            (8444, 8081)
        );
    }

    #[test]
    fn ports_are_only_persisted_after_binding_and_then_stay_fixed() {
        let dir = std::env::temp_dir().join(format!(
            "deskway-test-ports-{}",
            hex(&wire::crypto::random::<8>())
        ));
        fs::create_dir(&dir).unwrap();
        let prior = Preferences {
            autostart: false,
            prevent_sleep: true,
        };
        let mut setting = Setting::default();
        setting.enable(&dir, prior).unwrap();
        let candidate = choose_ports(None, |_, _| true).unwrap();
        assert_eq!(Config::local_service(&dir).unwrap().unwrap().ports(), None);
        let next = choose_ports(Some(candidate), |_, _| true).unwrap();
        setting.remember_bound(&dir, next, "aa".repeat(32)).unwrap();
        assert_eq!(
            Config::local_service(&dir).unwrap().unwrap().ports(),
            Some(next)
        );
        setting
            .remember_bound(&dir, candidate, "bb".repeat(32))
            .unwrap();
        assert_eq!(
            Config::local_service(&dir).unwrap().unwrap().ports(),
            Some(next)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabling_restores_saved_preferences_even_after_a_restart() {
        let dir = std::env::temp_dir().join(format!(
            "deskway-test-preferences-{}",
            hex(&wire::crypto::random::<8>())
        ));
        fs::create_dir(&dir).unwrap();
        for prior in [
            Preferences {
                autostart: false,
                prevent_sleep: true,
            },
            Preferences {
                autostart: true,
                prevent_sleep: false,
            },
        ] {
            Setting::default().enable(&dir, prior).unwrap();
            assert!(Config::prevent_sleep_setting(&dir).unwrap());
            let mut setting = Config::local_service(&dir).unwrap().unwrap();
            assert!(setting
                .disable(&dir, |_| anyhow::bail!("startup write failed"))
                .is_err());
            let mut setting = Config::local_service(&dir).unwrap().unwrap();
            assert!(!setting.enabled);
            assert_eq!(setting.previous, Some(prior));
            setting
                .disable(&dir, |on| {
                    assert_eq!(on, prior.autostart);
                    Ok(())
                })
                .unwrap();
            assert_eq!(
                Config::prevent_sleep_setting(&dir).unwrap(),
                prior.prevent_sleep
            );
            assert_eq!(Config::local_service(&dir).unwrap().unwrap().previous, None);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn browser_link_uses_the_local_name_only_if_resolvable() {
        assert_eq!(
            browser_origin("desk", 8443, |name, port| name == "desk.local"
                && port == 8443),
            "https://desk.local:8443"
        );
        assert_eq!(
            browser_origin("desk", 8443, |_, _| false),
            "https://localhost:8443"
        );
    }

    #[test]
    fn setup_errors_back_off_without_failing_the_server() {
        let dir = std::env::temp_dir().join(format!(
            "deskway-test-setup-{}",
            hex(&wire::crypto::random::<8>())
        ));
        let mut server = Server::new(dir);
        server.ready = Some(Ready {
            hostname: "desk".into(),
            address: loopback(8443, &"aa".repeat(32)).unwrap(),
        });
        let now = Instant::now();
        server.next_setup = Some(now);
        server.poll_setup(now, |_| anyhow::bail!("enrolment failed"));
        assert_eq!(server.phase, Phase::Setup);
        assert!(server.error.is_none() && server.ready.is_some());
        assert_eq!(server.setup_error.as_deref(), Some("enrolment failed"));
        let retry = server.next_setup.unwrap();
        assert!(retry >= now + Duration::from_secs(1));
        server.poll_setup(now, |_| panic!("retried before the backoff"));
        server.poll_setup(retry, |server| {
            server.phase = Phase::Running;
            Ok(())
        });
        assert_eq!(server.phase, Phase::Running);
        assert!(server.setup_error.is_none());
    }

    #[test]
    fn failed_deletion_leaves_sign_in_and_trust_alone_for_retry() {
        let cleared = std::cell::Cell::new(false);
        let clear = || {
            cleared.set(true);
            Ok(())
        };
        assert!(delete_then_clear(|| anyhow::bail!("data in use"), clear).is_err());
        assert!(!cleared.get());
        delete_then_clear(|| Ok(()), clear).unwrap();
        assert!(cleared.get());
    }

    #[cfg(windows)]
    #[test]
    fn locked_data_file_is_reported_before_deleting_anything() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!(
            "deskway-test-delete-{}",
            hex(&wire::crypto::random::<8>())
        ));
        let data = data_dir(&dir);
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("state.json"), "state").unwrap();
        let locked = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(data.join("held"))
            .unwrap();
        assert!(delete_stopped_data(&dir, || panic!("cleared trust with a locked file")).is_err());
        assert_eq!(
            fs::read_to_string(data.join("state.json")).unwrap(),
            "state"
        );
        drop(locked);
        let cleared = std::cell::Cell::new(false);
        delete_stopped_data(&dir, || {
            assert!(!data.exists());
            cleared.set(true);
            Ok(())
        })
        .unwrap();
        assert!(cleared.get());
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn child_messages_follow_the_local_host_contract() {
        let def: serde_json::Value =
            serde_json::from_str(include_str!("../../proto/local-host.json")).unwrap();
        assert_eq!(def["version"], VERSION);
        assert_eq!(def["stdin"]["usage"], "usage");
        assert_eq!(def["stdin"]["usage_excluding_hub"], "usage H1");
        let frame =
            |kind: &str| serde_json::from_value::<Output>(def["stdout"][kind].clone()).unwrap();
        let Output::Ready {
            version,
            hostname,
            fingerprint,
        } = frame("ready")
        else {
            panic!("ready frame")
        };
        assert_eq!(hostname, "desk");
        assert!(ready_address(version, 18599, &fingerprint)
            .unwrap()
            .is_pinned());
        assert!(matches!(frame("port"), Output::Port { port: 443 }));
        assert!(matches!(
            frame("usage"),
            Output::Usage {
                computers: 3,
                browsers: 2
            }
        ));
        assert!(matches!(
            frame("usage_excluding_hub"),
            Output::Usage {
                computers: 2,
                browsers: 2
            }
        ));
    }
}
