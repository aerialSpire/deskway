//! Linux command setup and process enumeration.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command};

use crate::i18n::t;
use crate::platform::Pty;
use crate::platform::unix::programs;
use crate::workspace::procs::{Program, Started};

use super::host_signals;

pub use programs::kill_tree;

pub fn start_program(command: &str, cwd: Option<&str>, elevated: bool) -> io::Result<Child> {
    programs::pipe(shell(Some(command), cwd, elevated)?)
}

pub fn start_terminal(command: Option<&str>, cwd: Option<&str>, cols: u16, rows: u16, elevated: bool) -> io::Result<Started> {
    programs::terminal(shell(command, cwd, elevated)?, cols, rows)
}

pub(super) fn open_pty(rows: u16, cols: u16) -> io::Result<Box<dyn Pty>> {
    let home = std::env::home_dir().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
    let mut command = shell(None, None, false)?;
    command.current_dir(home);
    Ok(Box::new(BrowserPty::start(command, rows, cols)?))
}

struct BrowserPty {
    terminal: programs::Terminal,
    status: Option<i32>,
}

impl BrowserPty {
    fn start(command: Command, rows: u16, cols: u16) -> io::Result<Self> {
        let (terminal, ()) = programs::Terminal::start(command, cols, rows, |master: &File| {
            if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })?;
        Ok(Self { terminal, status: None })
    }

    fn exited(&self) -> io::Result<bool> {
        // Leave the leader waitable so the kernel cannot reuse its ID before the jobs are gone.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::waitid(libc::P_PID, self.terminal.pid(), &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn finish(&mut self) -> io::Result<()> {
        let cleanup = self.terminal.kill();
        if cleanup.is_err() && self.terminal.child.kill().is_err() && self.terminal.child.try_wait()?.is_none() {
            return cleanup;
        }
        let status = self.terminal.child.wait()?;
        self.status = Some(status.code().unwrap_or_else(|| -status.signal().expect("process exit signal")));
        cleanup
    }
}

impl Read for BrowserPty {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.terminal.master.read(buf) {
            Err(e) if e.raw_os_error() == Some(libc::EIO) => Ok(0), // The last slave closed.
            result => result,
        }
    }
}

impl Write for BrowserPty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.terminal.master.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.terminal.master.flush()
    }
}

impl Pty for BrowserPty {
    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        self.terminal.resize(rows, cols)
    }

    fn exit(&mut self) -> io::Result<Option<i32>> {
        if self.status.is_none() && self.exited()? {
            self.finish()?;
        }
        Ok(self.status)
    }
}

impl Drop for BrowserPty {
    fn drop(&mut self) {
        if self.status.is_none() {
            if let Err(e) = self.finish() {
                eprintln!("terminal cleanup failed: {e}");
            }
        }
    }
}

fn shell(command: Option<&str>, cwd: Option<&str>, elevated: bool) -> io::Result<Command> {
    if elevated {
        return Err(io::Error::new(io::ErrorKind::Unsupported, t("mcp.elevation_unavailable")));
    }
    let mut c = programs::shell("/bin/sh", command, cwd);
    let signals = host_signals();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(mut check: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(4);
        while !check() {
            assert!(Instant::now() < until, "PTY did not reach the expected state");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn read(pty: &mut BrowserPty, output: &mut Vec<u8>) -> bool {
        let mut buf = [0; 64];
        match pty.read(&mut buf) {
            Ok(0) => true,
            Ok(n) => { output.extend_from_slice(&buf[..n]); false }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
            Err(e) => panic!("PTY read: {e}"),
        }
    }

    #[test]
    fn browser_pty_keeps_raw_output_input_size_and_exit_status() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "stty -echo; printf '\\033[31mready\\033[0m\\n'; stty size; read -r line; stty size; printf '%s\\n' \"$line\"; exit 17"]);
        let mut pty = BrowserPty::start(command, 24, 80).unwrap();
        let mut output = Vec::new();
        wait(|| {
            read(&mut pty, &mut output);
            output.ends_with(b"24 80\r\n")
        });
        pty.resize(31, 93).unwrap();
        pty.write_all(b"last line\n").unwrap();
        wait(|| pty.exit().unwrap() == Some(17));
        wait(|| read(&mut pty, &mut output));
        assert_eq!(output, b"\x1b[31mready\x1b[0m\r\n24 80\r\n31 93\r\nlast line\r\n");
    }

    #[test]
    fn browser_pty_drop_kills_jobs_even_after_the_shell_exits() {
        let mut command = Command::new("/bin/sh");
        command.args(["-ic", "trap '' HUP; sleep 300 </dev/null >/dev/null 2>&1 & echo $!; exit 17"]);
        let mut pty = BrowserPty::start(command, 24, 80).unwrap();
        let id = pty.terminal.pid() as libc::pid_t;
        wait(|| pty.exited().unwrap());
        let jobs: Vec<_> = session_members(id).unwrap().into_iter().filter(|&pid| pid != id).collect();
        assert_eq!(jobs.len(), 1);
        assert_ne!(unsafe { libc::getpgid(jobs[0]) }, id);
        drop(pty);
        assert!(session_members(id).unwrap().is_empty());
    }
}
