//! The user worker: the person signed in at the console, for the browser and AI workspace
//! (`workspace`). The `Deskway` service starts it as that person (`launch_worker`) and gives it
//! the key of this run on its standard input; it connects to the network service's worker pipe
//! and answers there until the person signs out. Its program table outlives the pipe: the
//! network service restarting finds the same programs.
//!
//! The admin worker is the same, with the person's administrator token, while the AI switch
//! allows administrator rights: its own key and pipe, and its programs numbered apart
//! (`procs::ADMIN_FIRST`).

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::ipc::{self, Pipe};
use crate::workspace::{procs, Local};
use crate::workspace::Workspace;

/// Which worker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// The person, with the rights they have.
    User,
    /// The person as an administrator (their elevated token).
    Admin,
}

impl Kind {
    /// Where the network service reads the key the worker of this run was given. A new one
    /// every start, written by the SYSTEM service into its own folder (`secure_service_dir`).
    pub fn key_path(self) -> PathBuf {
        crate::service::machine_dir().join("service").join(match self {
            Kind::User => "worker.key",
            Kind::Admin => "worker-admin.key",
        })
    }

    pub fn pipe(self) -> &'static str {
        match self {
            Kind::User => ipc::WORKER_PIPE,
            Kind::Admin => ipc::WORKER_ADMIN_PIPE,
        }
    }

    /// Its command line after the program, and what logs call it.
    pub fn args(self) -> &'static str {
        match self {
            Kind::User => "worker",
            Kind::Admin => "worker --admin",
        }
    }

    pub fn what(self) -> &'static str {
        match self {
            Kind::User => "worker",
            Kind::Admin => "admin worker",
        }
    }
}

pub fn run(kind: Kind) -> Result<()> {
    let mut key = Vec::new();
    std::io::stdin().take(64).read_to_end(&mut key).context("read the key")?;
    if key.len() != 32 {
        bail!("started without a key: only the Deskway service starts the worker");
    }
    if let Some(dir) = dirs_home() {
        let _ = std::env::set_current_dir(&dir);
        let log = if kind == Kind::Admin { "worker-admin.log" } else { "worker.log" };
        super::log_to(&dir.join("AppData").join("Local").join("Deskway").join(log));
    }
    if kind == Kind::User {
        crate::platform::workspace::sweep_home();
    }
    let activity = (kind == Kind::User).then(|| Arc::new(crate::platform::workspace::WorkspaceActivity::default()));
    let ws: Arc<dyn Workspace> = Arc::new(match kind {
        Kind::User => Local::default(),
        Kind::Admin => Local::numbered_from(procs::ADMIN_FIRST),
    });
    let mut delay = Duration::from_secs(1);
    loop {
        let r = Pipe::connect(kind.pipe()).context("network service not running").and_then(|pipe| {
            ipc::handshake_client(&pipe, &key)?;
            eprintln!("{}: connected", kind.what());
            delay = Duration::from_secs(1);
            crate::workspace::remote::serve(Arc::new(pipe), ws.clone(), activity.clone())
        });
        if let Err(e) = r {
            eprintln!("{}: {e:#}", kind.what());
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(10));
    }
}

/// The person's own folder, where programs start and the log goes.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from).filter(|p| p.is_dir())
}
