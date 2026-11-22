//! Programs an AI client starts: through the person's login shell, in a process group of their
//! own so they outlive the app's session and can be ended with what they started; with pipes,
//! or in a terminal of their own (a pseudo-terminal, as their session's controlling one).

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::workspace::procs::{Program, Started};

/// Starts `command` with its output piped. `elevated`: as an administrator, through the
/// system's own prompt, which the person at the Mac answers (password or Touch ID) every time;
/// its output comes when it ends.
pub fn start_program(command: &str, cwd: Option<&str>, elevated: bool) -> std::io::Result<Child> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut c = if elevated {
        let line = match cwd {
            Some(dir) => format!("cd {} && {command}", sh_quote(dir)),
            None => command.to_string(),
        };
        let script = format!("do shell script {} with administrator privileges", applescript_string(&line));
        let mut c = Command::new("/usr/bin/osascript");
        c.args(["-e", &script]);
        c
    } else {
        let mut c = Command::new(shell);
        c.args(["-lc", command]);
        c
    };
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    c.spawn()
}

/// A string as an AppleScript literal.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A word as the shell takes it, whatever it holds.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Starts `command` (the login shell itself, when there is none) in a terminal of `cols` × `rows`.
/// Not as an administrator: the system's prompt cannot sit in a terminal.
pub fn start_terminal(command: Option<&str>, cwd: Option<&str>, cols: u16, rows: u16, elevated: bool) -> std::io::Result<Started> {
    if elevated {
        return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "a terminal cannot be started as an administrator on a Mac; use run with elevated"));
    }
    let (mut master, mut slave) = (0, 0);
    let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &size as *const _ as *mut _) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut c = Command::new(shell);
    match command {
        Some(cmd) => c.args(["-lc", cmd]),
        None => c.arg("-l"),
    };
    c.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave)).env("TERM", "xterm-256color");
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    // A session of its own with the terminal as its controlling one. Interactive shells
    // allocate separate process groups for their foreground and background jobs.
    unsafe {
        c.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = c.spawn()?;
    // The terminal's other end stays only with the program.
    drop(c);
    let output = master.try_clone()?;
    Ok(Started { program: Box::new(Terminal { child, master }), output: vec![Box::new(output)] })
}

/// A program in a terminal, and the terminal's end that types into it.
struct Terminal {
    child: Child,
    master: File,
}

impl Program for Terminal {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn exit(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().map(|s| s.code().unwrap_or(-1))
    }

    fn kill(&mut self) -> io::Result<()> {
        kill_session(self.child.id() as libc::pid_t)
    }

    fn input(&mut self) -> Option<&mut dyn Write> {
        Some(&mut self.master)
    }
}

/// Signal the terminal's session, including the job-control groups the shell did not join.
/// The caller holds the Program lock, so the session leader cannot be reaped by a concurrent
/// tool call while its ID is in use here.
fn kill_session(id: libc::pid_t) -> io::Result<()> {
    // End the shell and its own group first so it cannot start more work during the scan.
    signal(-id)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let mut remaining = false;
        for pid in process_ids()? {
            if pid > 1 && unsafe { libc::getsid(pid) } == id {
                signal(pid)?;
                // A job could have forked while the process list was being collected.
                remaining = true;
            }
        }
        if !remaining {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "terminal jobs are still present"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn signal(pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
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

/// Ends a program and every program in its group.
pub fn kill_tree(pid: u32) {
    let _ = Command::new("/bin/kill").args(["-KILL", "--", &format!("-{pid}")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}
