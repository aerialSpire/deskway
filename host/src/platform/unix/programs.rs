//! Unix programs use a login shell, with pipes or a controlling terminal of their own.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::i18n::t;
use crate::platform::Pty;
use crate::platform::pty::{PtyEvents, PtyInput};
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

pub fn browser_pty(mut command: Command, rows: u16, cols: u16, allowed: impl Fn() -> bool + Send + Sync + 'static) -> io::Result<Box<dyn Pty>> {
    let home = std::env::home_dir().ok_or(io::ErrorKind::NotFound)?;
    command.current_dir(home);
    Ok(Box::new(BrowserPty::start(command, rows, cols, allowed)?))
}

struct BrowserPty {
    master: Option<File>,
    child: Arc<Mutex<Option<Child>>>,
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    input: Option<PtyInput>,
    listening: bool,
}

impl BrowserPty {
    fn start(command: Command, rows: u16, cols: u16, allowed: impl Fn() -> bool + Send + Sync + 'static) -> io::Result<Self> {
        let (Terminal { child, master }, ()) = Terminal::start(command, cols, rows, |master| {
            if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
                return Err(io::Error::last_os_error());
            }
            if !allowed() { return Err(io::ErrorKind::PermissionDenied.into()); }
            Ok(())
        })?;
        Ok(Self { master: Some(master), child: Arc::new(Mutex::new(Some(child))), allowed: Arc::new(allowed), input: None, listening: false })
    }
}

fn wait_ready(file: &File, events: libc::c_short) -> io::Result<()> {
    let mut fd = libc::pollfd { fd: file.as_raw_fd(), events, revents: 0 };
    loop {
        if unsafe { libc::poll(&mut fd, 1, -1) } > 0 { return Ok(()); }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
    }
}

struct PtyReader(File);

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => wait_ready(&self.0, libc::POLLIN)?,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(0), // The last slave closed.
                result => return result,
            }
        }
    }
}

impl Write for BrowserPty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.input.as_ref().ok_or(io::ErrorKind::NotConnected)?.write(buf);
        if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            && self.child.lock().unwrap().as_ref().is_none_or(|child| exited(child.id() as libc::pid_t).unwrap_or(false))
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl Pty for BrowserPty {
    fn listen(&mut self, events: PtyEvents) -> io::Result<()> {
        if self.listening { return Err(io::ErrorKind::AlreadyExists.into()); }
        let master = self.master.as_ref().unwrap();
        let reader = PtyReader(master.try_clone()?);
        let mut writer = master.try_clone()?;
        let allowed = self.allowed.clone();
        self.input = Some(PtyInput::start(move |mut bytes| {
            while !bytes.is_empty() && allowed() {
                match writer.write(bytes) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(written) => bytes = &bytes[written..],
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => wait_ready(&writer, libc::POLLOUT)?,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }, events.writable.clone())?);

        let pid = self.child.lock().unwrap().as_ref().unwrap().id() as libc::pid_t;
        let child = self.child.clone();
        let (status, exited) = mpsc::channel();
        std::thread::Builder::new().name("pty-exit".into()).spawn(move || {
            let wait = wait_for_exit(pid, 0);
            let mut slot = child.lock().unwrap();
            let mut child = slot.take().unwrap();
            // Keep the session leader waitable until its job-control groups are gone.
            let result = wait.and_then(|_| {
                let cleanup = kill_session(pid);
                let status = child.wait()?;
                cleanup?;
                Ok(status.code().unwrap_or_else(|| -status.signal().expect("process exit signal")))
            });
            let _ = status.send(result);
        })?;
        self.listening = true;
        std::thread::Builder::new().name("pty-output".into()).spawn(move || {
            events.read(reader, || exited.recv().map_err(io::Error::other)?);
        })?;
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        set_size(self.master.as_ref().unwrap().as_raw_fd(), size(cols, rows))
    }

    fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let fd = self.master.as_ref().unwrap().as_raw_fd();
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

    fn terminate(&mut self) -> io::Result<bool> {
        let child = self.child.lock().unwrap();
        let Some(child) = &*child else { return Ok(false); };
        let pid = child.id() as libc::pid_t;
        if exited(pid)? { return Ok(false); }
        kill_session(pid).map(|()| true)
    }
}

impl Drop for BrowserPty {
    fn drop(&mut self) {
        let mut slot = self.child.lock().unwrap();
        if let Some(child) = slot.as_mut() {
            if let Err(e) = kill_session(child.id() as libc::pid_t) {
                eprintln!("{}: {e}", t("terminal.close_failed"));
                let _ = child.kill();
            }
            if !self.listening {
                // No reader can drain this terminal during cleanup.
                self.master = None;
                self.input = None;
                let _ = child.wait();
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
    wait_for_exit(pid, libc::WNOHANG)
}

fn wait_for_exit(pid: libc::pid_t, flags: libc::c_int) -> io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT | flags) } == 0 {
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
    }
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

    use crate::platform::pty::{test_events, PtyEvent};

    fn read_until(events: &mpsc::Receiver<PtyEvent>, output: &mut Vec<u8>, end: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(4);
        while !output.ends_with(end) {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(PtyEvent::Bytes(bytes)) => output.extend(bytes),
                other => panic!("PTY did not produce the expected bytes: {other:?}"),
            }
        }
    }

    fn read_exit(events: &mpsc::Receiver<PtyEvent>, output: &mut Vec<u8>) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(PtyEvent::Bytes(bytes)) => output.extend(bytes),
                Ok(PtyEvent::Exit(code)) => return code,
                other => panic!("PTY did not exit: {other:?}"),
            }
        }
    }

    #[test]
    fn browser_pty_keeps_raw_output_input_size_and_exit_status() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "stty -echo; printf '\\033[31mready\\033[0m\\n'; stty size; read -r line; stty size; printf '%s\\n' \"$line\"; exit 17"]);
        let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
        let (listener, events) = test_events();
        pty.listen(listener).unwrap();
        let mut output = Vec::new();
        read_until(&events, &mut output, b"24 80\r\n");
        pty.resize(31, 93).unwrap();
        pty.write_all(b"last line\n").unwrap();
        assert_eq!(read_exit(&events, &mut output), 17);
        assert_eq!(output, b"\x1b[31mready\x1b[0m\r\n24 80\r\n31 93\r\nlast line\r\n");
    }

    #[test]
    fn browser_pty_notifies_the_foreground_program_at_the_same_size() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap 'echo redraw' WINCH; echo ready; while :; do sleep 0.1; done"]);
        let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
        let (listener, events) = test_events();
        pty.listen(listener).unwrap();
        let mut output = Vec::new();
        read_until(&events, &mut output, b"ready\r\n");
        pty.redraw(24, 80).unwrap();
        read_until(&events, &mut output, b"redraw\r\n");
    }

    #[test]
    fn browser_pty_exit_kills_background_jobs() {
        let mut command = Command::new("/bin/sh");
        command.args(["-ic", "trap '' HUP; sleep 300 </dev/null >/dev/null 2>&1 & echo $!; exit 17"]);
        let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
        let id = pty.child.lock().unwrap().as_ref().unwrap().id() as libc::pid_t;
        let (listener, events) = test_events();
        pty.listen(listener).unwrap();
        let mut output = Vec::new();
        assert_eq!(read_exit(&events, &mut output), 17);
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
