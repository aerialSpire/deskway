//! The Mac's administrator prompt and native process enumeration.

use std::io;
use std::process::{Child, Command};

use crate::platform::unix::programs;
use crate::workspace::procs::Started;

pub use programs::kill_tree;

/// For an elevated command, the person at the Mac answers the system's prompt each time.
pub fn start_program(command: &str, cwd: Option<&str>, elevated: bool) -> io::Result<Child> {
    if !elevated {
        return programs::pipe(shell(Some(command), cwd));
    }
    let line = match cwd {
        Some(dir) => format!("cd {} && {command}", sh_quote(dir)),
        None => command.to_string(),
    };
    let script = format!("do shell script {} with administrator privileges", applescript_string(&line));
    let mut c = Command::new("/usr/bin/osascript");
    c.args(["-e", &script]);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    programs::pipe(c)
}

/// A string as an AppleScript literal.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A word as the shell takes it, whatever it holds.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The system's administrator prompt cannot sit in a terminal.
pub fn start_terminal(command: Option<&str>, cwd: Option<&str>, cols: u16, rows: u16, elevated: bool) -> io::Result<Started> {
    if elevated {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "a terminal cannot be started as an administrator on a Mac; use run with elevated"));
    }
    programs::terminal(shell(command, cwd), cols, rows)
}

pub(super) fn shell(command: Option<&str>, cwd: Option<&str>) -> Command {
    programs::shell("/bin/zsh", command, cwd, std::convert::identity)
}

pub fn session_members(id: libc::pid_t) -> io::Result<Vec<libc::pid_t>> {
    let mut pids = process_ids()?;
    pids.retain(|&pid| pid > 1 && unsafe { libc::getsid(pid) } == id);
    Ok(pids)
}

fn process_ids() -> io::Result<Vec<libc::pid_t>> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut ids = vec![0; count as usize + 32];
    loop {
        let bytes = i32::try_from(std::mem::size_of_val(ids.as_slice())).map_err(io::Error::other)?;
        let written = unsafe { libc::proc_listallpids(ids.as_mut_ptr().cast(), bytes) };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        if (written as usize) < ids.len() {
            ids.truncate(written as usize);
            return Ok(ids);
        }
        ids.resize(ids.len() * 2, 0);
    }
}
