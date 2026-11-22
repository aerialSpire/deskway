//! The agent process, split from the network service by privilege:
//!
//! - The agent runs as SYSTEM in the console session and carries out [`Local`]: it captures the
//!   screen, injects input and follows the input desktop onto the lock screen and UAC prompts.
//!   It has no network code.
//! - The network service runs as a low-privilege virtual account and reaches it as
//!   [`crate::platform::Remote`], over an authenticated pipe.

use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::{Context, Result};

use super::ipc::{self, Pipe, AGENT_PIPE};
use crate::platform::{remote, Local};

/// Where the pipe key lives. In service mode only SYSTEM and administrators can write it,
/// and the network service can read it.
pub fn key_path(dev: bool) -> Result<PathBuf> {
    Ok(if dev { crate::config::dir()?.join("ipc.key") } else { crate::service::machine_dir().join("service").join("ipc.key") })
}

/// Runs the agent until the process is stopped, reconnecting to the network service.
pub fn run(dev: bool) -> Result<()> {
    // Physical pixels, like the rest of the host: without this, on a scaled display (150%)
    // clicks, wheel and the reported pointer all land in the wrong place.
    crate::platform::platform_init();
    if !dev {
        super::harden::agent();
        super::log_to(&crate::service::machine_dir().join("service").join("agent.log"));
    }
    // The language of the notes written here comes from the network service (`Platform::language`).
    let path = key_path(dev)?;
    let mut delay = Duration::from_secs(1);
    loop {
        match serve_once(&path, dev) {
            Ok(()) => delay = Duration::from_secs(1),
            Err(e) => eprintln!("agent: {e:#}"),
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(10));
    }
}

fn serve_once(key_path: &std::path::Path, dev: bool) -> Result<()> {
    let pipe = Pipe::connect(&ipc::pipe_name(AGENT_PIPE, dev)).context("network service not running")?;
    let key = ipc::load_key(key_path, false)?;
    ipc::handshake_client(&pipe, &key)?;
    eprintln!("agent: connected");
    let (events, from_local) = channel();
    // Dropped when the network side vanishes, perhaps in the middle of a session: `Local`
    // ends that session.
    let local = Local::new(events);
    remote::serve(&pipe, &local, from_local)
}
