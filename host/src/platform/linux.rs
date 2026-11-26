//! The Linux host runs in the foreground with no desktop or workspace.
use std::env;
use std::cell::Cell;
use std::fs::File;
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc};

use anyhow::Result;

use crate::config::{self, Config};
use crate::i18n::{t, tf};
use crate::settings::Settings;
use crate::signal::{self, Control};

pub fn config_dir() -> Option<PathBuf> {
    let absolute = |name| env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute());
    absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".local/state"))).map(|state| state.join("deskway"))
}

pub fn computer_name() -> Option<String> {
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

pub fn system_language() -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|key| env::var(key).ok().filter(|value| !value.is_empty()))
}

pub fn prevent_sleep(_: bool) -> Result<()> {
    Ok(())
}

pub fn lock() -> Result<File> {
    super::unix::lock_host().map_err(|e| {
        if e.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::WouldBlock) {
            e.context(t("cli.host_in_use"))
        } else {
            e
        }
    })
}

pub fn run(cfg: Config) -> Result<()> {
    let (control, requests) = mpsc::channel();
    let (settings, awake) = Settings::start(config::dir()?, &cfg, None, control.clone());
    awake?;
    quit_on_signal(control)?;
    let waiting = Cell::new(false);
    signal::run(signal::Params {
        cfg, view_only: false, desktop: None,
        notify: Box::new(move |status| {
            if let (false, Some(code)) = (waiting.replace(status.waiting), status.approval_code) {
                let _ = writeln!(io::stdout(), "{}", tf("cli.approve_computer", &[("server", status.server), ("code", code)]));
            }
        }),
        settings: Arc::new(settings), workspace: None, admin: None,
    }, requests);
    Ok(())
}

/// Block these signals before starting any host threads, then receive them synchronously.
fn quit_on_signal(control: Sender<Control>) -> io::Result<()> {
    let mut signals = MaybeUninit::uninit();
    // sigemptyset initializes the set, and all three calls use its valid storage.
    let signals = unsafe {
        libc::sigemptyset(signals.as_mut_ptr());
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGINT);
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGTERM);
        signals.assume_init()
    };
    // Spawned threads inherit this mask. The wait below is the only consumer of these signals.
    let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    std::thread::Builder::new().name("signals".into()).spawn(move || {
        let mut received = 0;
        let error = unsafe { libc::sigwait(&signals, &mut received) };
        if error != 0 {
            eprintln!("signal wait: {}", io::Error::from_raw_os_error(error));
        }
        let _ = control.send(Control::Quit);
    })?;
    Ok(())
}
