//! Linux command setup and process enumeration.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

use crate::i18n::t;
use crate::platform::unix::programs;
use crate::workspace::procs::Started;

use super::quit_signals;

pub use programs::kill_tree;

pub fn start_program(command: &str, cwd: Option<&str>, elevated: bool) -> io::Result<Child> {
    programs::pipe(shell(Some(command), cwd, elevated)?)
}

pub fn start_terminal(command: Option<&str>, cwd: Option<&str>, cols: u16, rows: u16, elevated: bool) -> io::Result<Started> {
    programs::terminal(shell(command, cwd, elevated)?, cols, rows)
}

fn shell(command: Option<&str>, cwd: Option<&str>, elevated: bool) -> io::Result<Command> {
    if elevated {
        return Err(io::Error::new(io::ErrorKind::Unsupported, t("mcp.elevation_unavailable")));
    }
    let mut c = programs::shell("/bin/sh", command, cwd);
    let signals = quit_signals();
    // Children should receive signals that the host consumes through sigwait.
    unsafe {
        c.pre_exec(move || {
            if libc::sigprocmask(libc::SIG_UNBLOCK, &signals, std::ptr::null_mut()) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(c)
}

pub fn session_members(id: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<libc::pid_t>().ok()) else { continue };
        if pid <= 1 || unsafe { libc::getsid(pid) } != id {
            continue;
        }
        let status = match std::fs::read(entry.path().join("status")) {
            Ok(status) => status,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        // A zombie still belongs to its session until its parent reaps it.
        let state = status.split(|&b| b == b'\n').find_map(|line| line.strip_prefix(b"State:"));
        if !state.is_some_and(|s| matches!(s.trim_ascii_start().first(), Some(b'Z' | b'X'))) {
            pids.push(pid);
        }
    }
    Ok(pids)
}
