//! Unix programs use a login shell, with pipes or a controlling terminal of their own.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::i18n::t;
use crate::platform::Pty;
use crate::workspace::procs::{Program, Started};

pub fn shell(default: &str, command: Option<&str>, cwd: Option<&str>) -> Command {
    let mut c = Command::new(std::env::var("SHELL").unwrap_or_else(|_| default.into()));
    match command {
        Some(command) => c.args(["-lc", command]),
        None => c.arg("-l"),
    };
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    c
}

pub fn pipe(mut c: Command) -> io::Result<Child> {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn()
}

fn open_pty(size: libc::winsize) -> io::Result<(File, OwnedFd)> {
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    let master = unsafe { libc::posix_openpt(flags) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { File::from_raw_fd(master) };
    let fd = master.as_raw_fd();
    if unsafe { libc::grantpt(fd) < 0 || libc::unlockpt(fd) < 0 } {
        return Err(io::Error::last_os_error());
    }

    #[cfg(target_os = "linux")]
    let slave = unsafe { libc::ioctl(fd, libc::TIOCGPTPEER, flags) };
    #[cfg(target_os = "macos")]
    let slave = {
        // Available since macOS 10.13.4. libc does not declare it on macOS.
        unsafe extern "C" {
            fn ptsname_r(fd: libc::c_int, buf: *mut libc::c_char, len: libc::size_t) -> libc::c_int;
        }
        let mut path = [0; libc::PATH_MAX as usize];
        if unsafe { ptsname_r(fd, path.as_mut_ptr(), path.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe { libc::open(path.as_ptr(), flags) }
    };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_size(slave.as_raw_fd(), size)?;
    Ok((master, slave))
}

fn size(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

fn set_size(fd: libc::c_int, size: libc::winsize) -> io::Result<()> {
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size as *const libc::winsize) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn terminal(c: Command, cols: u16, rows: u16) -> io::Result<Started> {
    Terminal::start(c, cols, rows, |master| master.try_clone()).map(|(program, output)| {
        Started { program: Box::new(program), output: vec![Box::new(output)] }
    })
}

/// A program in a terminal and its master descriptor.
pub struct Terminal {
    pub child: Child,
    pub master: File,
}

impl Terminal {
    pub fn start<T>(mut c: Command, cols: u16, rows: u16, configure: impl FnOnce(&File) -> io::Result<T>) -> io::Result<(Self, T)> {
        let (master, slave) = open_pty(size(cols, rows))?;
        let output = configure(&master)?;
        c.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave)).env("TERM", "xterm-256color");
        // A session of its own with the terminal as its controlling one. Interactive shells
        // allocate separate process groups for their foreground and background jobs.
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = c.spawn()?;
        Ok((Self { child, master }, output))
    }

    fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
        set_size(self.master.as_raw_fd(), size(cols, rows))
    }
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

pub fn browser_pty(mut command: Command, rows: u16, cols: u16, allowed: impl FnOnce() -> bool) -> io::Result<Box<dyn Pty>> {
    let home = std::env::home_dir().ok_or(io::ErrorKind::NotFound)?;
    command.current_dir(home);
    Ok(Box::new(BrowserPty::start(command, rows, cols, allowed)?))
}

struct BrowserPty {
    terminal: Terminal,
    status: Option<i32>,
    termination_incomplete: bool,
}

impl BrowserPty {
    fn start(command: Command, rows: u16, cols: u16, allowed: impl FnOnce() -> bool) -> io::Result<Self> {
        let (terminal, ()) = Terminal::start(command, cols, rows, |master: &File| {
            if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return Err(io::Error::last_os_error());
            }
            if !allowed() { return Err(io::ErrorKind::PermissionDenied.into()); }
            Ok(())
        })?;
        Ok(Self { terminal, status: None, termination_incomplete: false })
    }

    fn exited(&self) -> io::Result<bool> {
        exited(self.terminal.pid() as libc::pid_t)
    }

    fn finish(&mut self) -> io::Result<()> {
        let cleanup = self.terminal.kill();
        if cleanup.is_err() && self.terminal.child.kill().is_err() && self.terminal.child.try_wait()?.is_none() {
            return cleanup;
        }
        self.reap()?;
        cleanup
    }

    fn reap(&mut self) -> io::Result<()> {
        // macOS holds an exiting process until the output it left in the terminal is read.
        let status = loop {
            if let Some(status) = self.terminal.child.try_wait()? {
                break status;
            }
            if !matches!(self.terminal.master.read(&mut [0; 4096]), Ok(n) if n > 0) {
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        self.status = Some(status.code().unwrap_or_else(|| -status.signal().expect("process exit signal")));
        Ok(())
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

    fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let fd = self.terminal.master.as_raw_fd();
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if (rows, cols) != (size.ws_row, size.ws_col) {
            return self.resize(rows, cols);
        }
        // TIOCSWINSZ sends SIGWINCH only when the size changes.
        let group = unsafe { libc::tcgetpgrp(fd) };
        if group < 0 {
            return Err(io::Error::last_os_error());
        }
        if group > 0 && unsafe { libc::kill(-group, libc::SIGWINCH) } < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ESRCH) {
                return Err(e);
            }
        }
        Ok(())
    }

    fn exit(&mut self) -> io::Result<Option<i32>> {
        // A failed terminate can have killed the leader but left jobs behind. Keep
        // its ID reserved and the attachment open so the person can retry cleanup.
        if self.status.is_none() && !self.termination_incomplete && self.exited()? {
            self.finish()?;
        }
        Ok(self.status)
    }

    fn terminate(&mut self) -> io::Result<bool> {
        if self.status.is_some() || (!self.termination_incomplete && self.exited()?) {
            return Ok(false);
        }
        let result = self.terminal.kill().and_then(|()| self.reap());
        self.termination_incomplete = result.is_err();
        result.map(|()| true)
    }
}

impl Drop for BrowserPty {
    fn drop(&mut self) {
        if self.status.is_none() {
            if let Err(e) = self.finish() {
                eprintln!("{}: {e}", t("terminal.close_failed"));
            }
        }
    }
}

/// Signal the terminal's session, including the job-control groups the shell did not join.
/// The caller holds the Program lock, so the session leader cannot be reaped by a concurrent
/// tool call while its ID is in use here.
fn kill_session(id: libc::pid_t) -> io::Result<()> {
    // End the shell and its own group first so it cannot start more work during the scan.
    // macOS refuses to signal a group whose only member has already exited.
    let mut error = signal(-id).err().filter(|e| e.raw_os_error() != Some(libc::EPERM) || !exited(id).unwrap_or(false));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let jobs = crate::platform::session_members(id)?;
        if jobs.is_empty() {
            break;
        }
        for pid in jobs {
            if let Err(e) = signal(pid) {
                error.get_or_insert(e);
            }
        }
        if error.is_some() {
            break;
        }
        // A job could have forked while the process list was being collected.
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "terminal jobs are still present"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    error.map_or(Ok(()), Err)
}

/// Whether this child has exited. It stays waitable so the kernel cannot reuse its ID
/// before its jobs are gone.
fn exited(pid: libc::pid_t) -> io::Result<bool> {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

fn signal(pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) { Ok(()) } else { Err(error) }
}

/// Ends a piped program's process group.
pub fn kill_tree(pid: u32) {
    let _ = signal(-(pid as libc::pid_t));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::procs::Table;

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
        let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
        let mut output = Vec::new();
        wait(|| {
            read(&mut pty, &mut output);
            output.ends_with(b"24 80\r\n")
        });
        pty.resize(31, 93).unwrap();
        pty.write_all(b"last line\n").unwrap();
        wait(|| read(&mut pty, &mut output));
        wait(|| pty.exit().unwrap() == Some(17));
        assert_eq!(output, b"\x1b[31mready\x1b[0m\r\n24 80\r\n31 93\r\nlast line\r\n");
    }

    #[test]
    fn browser_pty_notifies_the_foreground_program_at_the_same_size() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap 'echo redraw' WINCH; echo ready; while :; do sleep 0.1; done"]);
        let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
        let mut output = Vec::new();
        wait(|| { read(&mut pty, &mut output); output.ends_with(b"ready\r\n") });
        pty.redraw(24, 80).unwrap();
        wait(|| { read(&mut pty, &mut output); output.ends_with(b"redraw\r\n") });
    }

    #[test]
    fn browser_pty_drop_kills_jobs_even_after_the_shell_exits() {
        let mut command = Command::new("/bin/sh");
        command.args(["-ic", "trap '' HUP; sleep 300 </dev/null >/dev/null 2>&1 & echo $!; exit 17"]);
        let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
        let id = pty.terminal.pid() as libc::pid_t;
        wait(|| { read(&mut pty, &mut Vec::new()); pty.exited().unwrap() });
        let jobs: Vec<_> = crate::platform::session_members(id).unwrap().into_iter().filter(|&pid| pid != id).collect();
        assert_eq!(jobs.len(), 1);
        assert_ne!(unsafe { libc::getpgid(jobs[0]) }, id);
        drop(pty);
        assert!(crate::platform::session_members(id).unwrap().is_empty());
    }

    #[test]
    fn both_pty_ends_are_close_on_exec() {
        let (master, slave) = open_pty(libc::winsize { ws_row: 30, ws_col: 100, ws_xpixel: 0, ws_ypixel: 0 }).unwrap();
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, libc::FD_CLOEXEC);
        }
    }

    fn still_running(job: libc::pid_t, session: libc::pid_t) -> bool {
        crate::platform::session_members(session).unwrap().contains(&job)
    }

    #[test]
    fn killing_a_program_ends_its_background_jobs() {
        const JOB: &str = "/bin/sleep 30 & printf '\\ndeskway_job=%s\\n' $!";
        for terminal in [false, true] {
            let table = Table::default();
            let started = if terminal {
                table.start(Some("exec /bin/sh -i"), None, 100, 30, false)
            } else {
                table.run(&format!("{JOB}; wait"), None, 0, false)
            }.unwrap();
            let id = started["id"].as_u64().unwrap();
            let leader = table.list()[0]["pid"].as_i64().unwrap() as libc::pid_t;
            if terminal {
                table.write(id, Some(JOB), &["enter".into()]).unwrap();
            }
            let session = unsafe { libc::getsid(leader) };
            assert!(session > 0);
            assert_eq!(session == leader, terminal);
            let until = Instant::now() + Duration::from_secs(4);
            let mut output = String::new();
            let mut at = 0;
            let job = loop {
                let result = table.read(id, at, 200).unwrap();
                at = result["next"].as_u64().unwrap();
                output.push_str(result["output"].as_str().unwrap());
                if let Some(pid) = output.lines().filter_map(|line| line.strip_prefix("deskway_job=")?.trim().parse::<libc::pid_t>().ok()).find(|pid| *pid > 1) {
                    break pid;
                }
                assert!(Instant::now() < until, "could not read the background job PID: {output:?}");
            };
            assert!(still_running(job, session));
            assert_eq!(unsafe { libc::getpgid(job) } == leader, !terminal, "interactive shells put jobs in a separate process group");
            let stopped = table.kill(id).unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            while still_running(job, session) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            let leaked = still_running(job, session);
            if leaked {
                unsafe { libc::kill(job, libc::SIGKILL) };
            }
            assert_eq!(stopped["running"], false);
            assert!(!leaked, "the background job survived proc_kill (terminal={terminal})");
        }
    }
}
