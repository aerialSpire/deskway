//! The Linux host runs in the foreground or under systemd, with an in-process workspace.
use std::cell::Cell;
use std::env;
use std::fs::File;
use std::io::{self, Write};
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::auth::ai::{saved_ai_state, stop_saved_ai, AiState};
use crate::config::{self, Config};
use crate::i18n::{t, tf};
use crate::settings::Settings;
use crate::signal::{self, Control};
use crate::workspace::Local;

mod programs;
mod lock;
use super::unix::{open_host_lock, try_lock_host};
pub use programs::{kill_tree, session_members, start_program, start_terminal};

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

/// Own the state directory. The file names no process until a running host records itself.
pub fn lock() -> Result<File> {
    let file = open_host_lock(&config::dir()?, true)?;
    match try_lock_host(&file) {
        Ok(()) => {
            file.set_len(0)?;
            Ok(file)
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(e).context(t("cli.host_in_use")),
        Err(e) => Err(e.into()),
    }
}

pub fn stop_ai() -> Result<()> {
    let stopped = stop().with_context(|| t("cli.ai_stop_unconfirmed"))?;
    println!("{}", t(if stopped { "panel.ai_stopped" } else { "cli.ai_not_on" }));
    Ok(())
}

fn stop() -> Result<bool> {
    let dir = config::dir()?;
    let file = open_host_lock(&dir, true)?;
    match try_lock_host(&file) {
        Ok(()) => return stop_saved_ai(&dir),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(e.into()),
    }
    // The running host is asked whatever the saved state says: its memory is what counts.
    let before = saved_ai_state(&dir);
    lock::signal_stop(&file)?;
    let was_on = matches!(before?, AiState::On { .. });
    let deadline = Instant::now() + Duration::from_secs(2);
    while was_on && matches!(saved_ai_state(&dir)?, AiState::On { .. }) {
        if Instant::now() >= deadline {
            return Err(io::Error::from(io::ErrorKind::TimedOut).into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(was_on)
}

pub fn run() -> Result<()> {
    let (control, requests) = mpsc::channel();
    // Whoever finds this process through the lock file may signal it at once.
    receive_signals(control.clone())?;
    let mut lock = lock()?;
    lock::record_pid(&mut lock)?;
    let cfg = Config::load()?;
    let (settings, awake) = Settings::start(config::dir()?, &cfg, None, control);
    awake?;
    let waiting = Cell::new(false);
    signal::run(signal::Params {
        cfg, view_only: false, desktop: None,
        notify: Box::new(move |status| {
            if let (false, Some(code)) = (waiting.replace(status.waiting), status.approval_code) {
                let _ = writeln!(io::stdout(), "{}", tf("cli.approve_computer", &[("server", status.server), ("code", code)]));
            }
        }),
        settings: Arc::new(settings), workspace: Some(Arc::new(Local::default())), admin: None,
    }, requests);
    Ok(())
}

fn host_signals() -> libc::sigset_t {
    let mut signals = MaybeUninit::uninit();
    // sigemptyset initializes the set before the calls that use it.
    unsafe {
        libc::sigemptyset(signals.as_mut_ptr());
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGINT);
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGTERM);
        libc::sigaddset(signals.as_mut_ptr(), libc::SIGUSR1);
        signals.assume_init()
    }
}

/// Block these signals before starting any host threads, then receive them synchronously.
fn receive_signals(control: Sender<Control>) -> io::Result<()> {
    let signals = host_signals();
    // Spawned threads inherit this mask. The wait below is the only consumer of these signals.
    let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    std::thread::Builder::new().name("signals".into()).spawn(move || {
        let mut received = 0;
        loop {
            let error = unsafe { libc::sigwait(&signals, &mut received) };
            if error != 0 {
                eprintln!("signal wait: {}", io::Error::from_raw_os_error(error));
                break;
            }
            if received != libc::SIGUSR1 || control.send(Control::StopAi).is_err() {
                break;
            }
        }
        let _ = control.send(Control::Quit);
    })?;
    Ok(())
}
