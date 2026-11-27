//! Owns the saved local-service switch, server child and automatic hub enrollment.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
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
const USAGE: &[u8] = b"usage\n";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub https: u16,
    pub guide: u16,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    Start,
    Stop,
    Retry,
    Delete,
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
    Request(Request, Respond),
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
        autostart: impl Fn() -> Result<()> + Send + 'static,
        enrolled: impl Fn(Config) + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("local-server".into())
            .spawn(move || {
                let mut server = Server::new(dir);
                let mut last = None;
                loop {
                    let request = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Message::Request(request, reply)) => Some((request, reply)),
                        Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                    };
                    let complete =
                        request.and_then(|(request, reply)| server.request(request, reply));
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
        reply: impl FnOnce(Result<Reply, String>) + Send + 'static,
    ) {
        if let Err(mpsc::SendError(Message::Request(_, reply))) =
            self.tx.send(Message::Request(request, Box::new(reply)))
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

pub fn is_own(dir: &Path, address: &str) -> bool {
    own_address(dir).is_ok_and(|own| own.saved() == address)
}

pub fn own_address(dir: &Path) -> Result<Address> {
    let setting = Config::local_service(dir)?.context("local service has no assigned ports")?;
    let pem = fs::read_to_string(data_dir(dir).join("local-ca").join("ca.crt"))?;
    let der = root_der(&pem)?;
    loopback(setting.https, &hex(&sha256(&der)))
}

fn loopback(port: u16, pin: &str) -> Result<Address> {
    Address::parse(&format!("https://localhost:{port}#ca={pin}"))
}

fn choose_ports(free: impl Fn(u16, u16) -> bool) -> Result<Setting> {
    let (https, guide) = std::iter::once((443, 80))
        .chain((8443..=u16::MAX).zip(8080..))
        .find(|&(https, guide)| free(https, guide))
        .context("no free local service ports")?;
    Ok(Setting {
        https,
        guide,
        enabled: true,
    })
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
    kept: Kept<Process>,
    phase: Phase,
    port: Option<u16>,
    error: Option<String>,
    ready: Option<Ready>,
    next_setup: Option<Instant>,
    force: bool,
    starting: Option<Respond>,
    usage: Option<(Instant, Respond)>,
}

struct Ready {
    origin: String,
    address: Address,
}

impl Server {
    fn new(dir: PathBuf) -> Self {
        let setting = Config::local_service(&dir).unwrap_or_else(|e| {
            eprintln!("read local service setting: {e:#}");
            None
        });
        Self {
            dir,
            exe: std::env::current_exe()
                .ok()
                .map(|p| p.with_file_name(SERVER_EXE))
                .filter(|p| p.is_file()),
            setting,
            kept: Kept::new("local service"),
            phase: Phase::Off,
            port: None,
            error: None,
            ready: None,
            next_setup: None,
            force: true,
            starting: None,
            usage: None,
        }
    }

    fn state(&self) -> State {
        State {
            available: self.exe.is_some(),
            enabled: self.setting.is_some_and(|s| s.enabled),
            own: Config::load_from(&self.dir).is_ok_and(|c| is_own(&self.dir, &c.server)),
            has_data: data_dir(&self.dir).is_dir(),
            phase: self.phase,
            port: self.port,
            error: self.error.clone(),
        }
    }

    fn request(&mut self, request: Request, reply: Respond) -> Option<(Respond, Result<Reply>)> {
        let result = match request {
            Request::Start => self.start(),
            Request::Retry => self.retry(),
            Request::Stop => self.stop(),
            Request::Delete => self.delete(),
            Request::Setup | Request::Open => self.link(request),
            Request::Usage => {
                if let Some(process) = self.kept.child() {
                    if self.usage.is_none() && process.ask_usage().is_ok() {
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
            self.setting.is_some_and(|s| s.enabled),
            crate::i18n::t("local.failed")
        );
        self.kept.stop();
        self.ready = None;
        self.port = None;
        self.force = true;
        self.error = None;
        self.phase = Phase::Starting;
        Ok(Reply::Done)
    }

    fn start(&mut self) -> Result<Reply> {
        ensure!(self.exe.is_some(), crate::i18n::t("local.unavailable"));
        ensure!(
            !Config::load_from(&self.dir).is_ok_and(|c| !is_own(&self.dir, &c.server)),
            crate::i18n::t("local.leave_first")
        );
        let setting = match self.setting {
            Some(saved) => Setting {
                enabled: true,
                ..saved
            },
            None => choose_ports(free_pair)?,
        };
        Config::set_local_service(&self.dir, setting)?;
        self.setting = Some(setting);
        if self.ready.is_none() {
            self.phase = Phase::Starting;
        }
        self.force = true;
        Ok(Reply::Done)
    }

    fn stop(&mut self) -> Result<Reply> {
        if let Some(saved) = self.setting {
            let disabled = Setting {
                enabled: false,
                ..saved
            };
            Config::set_local_service(&self.dir, disabled)?;
            self.setting = Some(disabled);
        }
        self.kept.stop();
        self.ready = None;
        self.phase = Phase::Off;
        self.port = None;
        self.error = None;
        self.next_setup = None;
        if let Some((_, reply)) = self.usage.take() {
            reply(Ok(Reply::Done));
        }
        if Config::load_from(&self.dir).is_err() {
            crate::platform::prevent_sleep(false)?;
        }
        Ok(Reply::Done)
    }

    fn delete(&self) -> Result<Reply> {
        ensure!(
            !self.setting.is_some_and(|s| s.enabled),
            crate::i18n::t("local.stop_first")
        );
        ensure!(
            !Config::load_from(&self.dir).is_ok_and(|c| is_own(&self.dir, &c.server)),
            crate::i18n::t("local.leave_first")
        );
        match fs::remove_dir_all(data_dir(&self.dir)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Reply::Done)
    }

    fn admin_link(&self, origin: &str, args: &[&str]) -> Result<String> {
        let exe = self.exe.as_ref().context("no local server program")?;
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
        let url = self.admin_link(&ready.origin, &[mode])?;
        if url.is_empty() {
            return Ok(Reply::Done);
        }
        ensure!(
            url.starts_with(&format!("{}/setup#c=", ready.origin)),
            "invalid local browser link"
        );
        Ok(Reply::Browser {
            url,
            address: ready.address.saved(),
        })
    }

    fn apply(&mut self, autostart: &impl Fn() -> Result<()>) {
        if self.force {
            self.force = false;
            let result = autostart().and_then(|()| crate::platform::prevent_sleep(true));
            if let Err(e) = result {
                self.error = Some(format!("{e:#}"));
                self.phase = Phase::Failed;
            }
        }
    }

    fn tick(&mut self, autostart: &impl Fn() -> Result<()>, enrolled: &impl Fn(Config)) {
        if !self.setting.is_some_and(|s| s.enabled) {
            return;
        }
        self.apply(autostart);
        self.tick_once();
        if self.next_setup.is_some_and(|next| next <= Instant::now()) && self.ready.is_some() {
            self.next_setup = None;
            if let Err(e) = self.check_setup(enrolled) {
                self.error = Some(format!("{e:#}"));
                self.phase = Phase::Failed;
            }
        }
        if self
            .usage
            .as_ref()
            .is_some_and(|(deadline, _)| *deadline <= Instant::now())
        {
            self.usage.take().unwrap().1(Ok(Reply::Done));
        }
    }

    fn tick_once(&mut self) {
        let Some(setting) = self.setting.filter(|s| s.enabled) else {
            return;
        };
        if self.port.is_some() || self.error.is_some() {
            return;
        }
        let Some(exe) = self.exe.as_ref() else {
            self.phase = Phase::Failed;
            return;
        };
        let dir = data_dir(&self.dir);
        // Read buffered output first, including a bind error from an exited child.
        let messages = self.kept.child().map(Process::messages).unwrap_or_default();
        for message in messages {
            match message {
                Output::Port { port } => {
                    self.port = Some(port);
                    self.phase = Phase::Failed;
                    self.kept.stop();
                    return;
                }
                Output::Ready {
                    version,
                    hostname,
                    fingerprint,
                } => match ready_address(version, setting.https, &fingerprint) {
                    Ok(address) => {
                        let origin = format!("https://{hostname}.local:{}", setting.https);
                        self.ready = Some(Ready { origin, address });
                        self.next_setup = Some(Instant::now());
                    }
                    Err(e) => {
                        self.error = Some(format!("{e:#}"));
                        self.phase = Phase::Failed;
                    }
                },
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
        self.kept.keep((), || Process::spawn(exe, &dir, setting));
        if self.kept.child().is_none() {
            self.ready = None;
            self.next_setup = None;
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
    }

    fn check_setup(&mut self, enrolled: &impl Fn(Config)) -> Result<()> {
        let ready = self.ready.as_ref().unwrap();
        if let Ok(cfg) = Config::load_from(&self.dir) {
            ensure!(
                cfg.server == ready.address.saved(),
                crate::i18n::t("local.leave_first")
            );
            self.phase = Phase::Running;
            return Ok(());
        }
        if !self
            .admin_link(&ready.origin, &["-if-needed", "-check"])?
            .is_empty()
        {
            self.phase = Phase::Setup;
            self.next_setup = Some(Instant::now() + Duration::from_secs(2));
            return Ok(());
        }
        let link = self.admin_link(&ready.origin, &["-enroll"])?;
        let token = link
            .strip_prefix(&format!("{}/api/computers/enroll#c=", ready.origin))
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
    fn spawn(exe: &Path, dir: &Path, setting: Setting) -> Result<Self> {
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
                &format!(":{}", setting.https),
                "-guide-listen",
                &format!(":{}", setting.guide),
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

    fn ask_usage(&mut self) -> Result<()> {
        self.input
            .as_mut()
            .context("local service has no control pipe")?
            .write_all(USAGE)?;
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
            let setting =
                choose_ports(|a, b| available.contains(&a) && available.contains(&b)).unwrap();
            assert_eq!((setting.https, setting.guide), expected);
        }
    }

    #[test]
    fn child_messages_follow_the_local_host_contract() {
        let def: serde_json::Value =
            serde_json::from_str(include_str!("../../proto/local-host.json")).unwrap();
        assert_eq!(def["version"], VERSION);
        assert_eq!(
            def["stdin"]["usage"],
            std::str::from_utf8(USAGE).unwrap().trim_end()
        );
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
    }
}
