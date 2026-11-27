//! Unix programs use a login shell, with pipes or a controlling terminal of their own.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use wire::terminal::{FRAME, OUTPUT_WINDOW};

use crate::i18n::t;
use crate::platform::Pty;
use crate::platform::pty::{PtyEvent, PtyEvents};
use crate::workspace::procs::{Program, Started};

pub fn shell(default: &str, command: Option<&str>, cwd: Option<&str>) -> Command {
    let program = std::env::var("SHELL").unwrap_or_else(|_| default.into());
    let mut c = Command::new(&program);
    match command {
        Some(command) => {
            static ADDED: OnceLock<Vec<OsString>> = OnceLock::new();
            let added = ADDED.get_or_init(|| probe_paths(|| Command::new(&program)).unwrap_or_else(|error| {
                eprintln!("cannot read the interactive shell's PATH: {error}");
                Vec::new()
            }));
            if !added.is_empty() {
                c.env("PATH", append_paths(std::env::var_os("PATH"), added));
            }
            c.args(["-lc", command])
        }
        None => c.arg("-l"),
    };
    if let Some(dir) = cwd {
        c.current_dir(dir);
    }
    c
}

fn path_line(output: &[u8]) -> Option<&OsStr> {
    output.rsplit(|&byte| byte == b'\n').find(|line| !line.is_empty()).map(OsStr::from_bytes)
}

fn added_paths(login: &OsStr, interactive: &OsStr) -> Vec<OsString> {
    let entries = |path| std::env::split_paths(path).map(|dir| dir.into_os_string());
    let mut seen: HashSet<_> = entries(login).collect();
    entries(interactive).filter(|dir| seen.insert(dir.clone())).collect()
}

fn append_paths(path: Option<OsString>, added: &[OsString]) -> OsString {
    let mut entries = path.iter().chain(added);
    let mut path = entries.next().cloned().unwrap_or_default();
    for dir in entries {
        path.push(":");
        path.push(dir);
    }
    path
}

fn probe_paths(shell: impl Fn() -> Command) -> io::Result<Vec<OsString>> {
    let login = probe_path(shell(), "-lc")?;
    let interactive = probe_path(shell(), "-ilc")?;
    Ok(added_paths(&login, &interactive))
}

fn probe_path(mut shell: Command, flags: &str) -> io::Result<OsString> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut child = shell.args([flags, "/usr/bin/printenv PATH"])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0).spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let output = match read_probe_output(&mut stdout, child.id() as libc::pid_t, deadline) {
        Ok(output) => output,
        Err(error) => {
            kill_tree(child.id());
            let _ = child.wait();
            return Err(error);
        }
    };
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!("shell {flags} exited with {status}")));
    }
    path_line(&output).map(OsStr::to_os_string)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "shell did not print PATH"))
}

fn read_probe_output(stdout: &mut ChildStdout, pid: libc::pid_t, deadline: Instant) -> io::Result<Vec<u8>> {
    let remaining = || {
        let time = deadline.saturating_duration_since(Instant::now());
        if time.is_zero() { Err(io::ErrorKind::TimedOut) } else { Ok(time) }
    };
    let mut fd = libc::pollfd { fd: stdout.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let mut output = Vec::new();
    let mut bytes = [0; 4096];
    // A descendant can hold the pipe open after the shell exits. Keep the leader waitable
    // until the pipe closes so its group can still be killed if the deadline passes.
    loop {
        let timeout = remaining()?.as_millis().max(1) as libc::c_int;
        match unsafe { libc::poll(&mut fd, 1, timeout) } {
            0 => continue,
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
            }
            _ => match stdout.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            },
        }
    }
    // The shell can also close stdout before exiting.
    while !exited(pid)? {
        thread::sleep(remaining()?.min(Duration::from_millis(20)));
    }
    Ok(output)
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
    input: Option<PtyWriter>,
    reader: Option<(UnixStream, JoinHandle<()>)>,
    waiter: Option<JoinHandle<()>>,
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
        Ok(Self { master: Some(master), child: Arc::new(Mutex::new(Some(child))), allowed: Arc::new(allowed),
            input: None, reader: None, waiter: None })
    }
}

fn wait_ready(wake: &UnixStream, master: Option<&File>, events: libc::c_short) -> io::Result<bool> {
    let mut fds = [
        libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: master.map_or(-1, AsRawFd::as_raw_fd), events, revents: 0 },
    ];
    loop {
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } > 0 {
            return Ok(fds[0].revents != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted { return Err(error); }
    }
}

#[derive(Default)]
struct Input {
    attached: bool,
    closed: bool,
    pending: Vec<u8>,
    offset: usize,
}

struct PtyWriter {
    queue: Arc<Mutex<Input>>,
    wake: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl PtyWriter {
    fn start(mut master: File, allowed: Arc<dyn Fn() -> bool + Send + Sync>, writable: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let (wake, mut ready) = UnixStream::pair()?;
        wake.set_nonblocking(true)?;
        let queue = Arc::new(Mutex::new(Input::default()));
        let input = queue.clone();
        let thread = thread::Builder::new().name("pty-input".into()).spawn(move || {
            loop {
                let waiting = {
                    // Serialize the nonblocking write with detach, including partial writes.
                    let mut input = input.lock().unwrap();
                    if input.pending.is_empty() || !allowed() {
                        // Detach will discard this input and wake the writer.
                        false
                    } else {
                        match master.write(&input.pending[input.offset..]) {
                            Ok(0) => break,
                            Ok(n) => {
                                input.offset += n;
                                if input.offset < input.pending.len() { continue; }
                                input.pending.clear();
                                input.offset = 0;
                                writable();
                                false
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => true,
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                            Err(_) => break,
                        }
                    }
                };
                match wait_ready(&ready, waiting.then_some(&master), libc::POLLOUT) {
                    Ok(false) => {}
                    Ok(true) => match ready.read(&mut [0; 64]) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    },
                    Err(_) => break,
                }
            }
            input.lock().unwrap().closed = true;
            writable();
        })?;
        Ok(Self { queue, wake, thread: Some(thread) })
    }

    fn notify(&self) -> io::Result<()> {
        loop {
            match (&self.wake).write(&[0]) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn set_attached(&self, attached: bool) {
        let mut input = self.queue.lock().unwrap();
        input.attached = attached;
        if !attached {
            input.pending.clear();
            input.offset = 0;
            let _ = self.notify();
        }
    }

    fn write(&self, bytes: &[u8]) -> io::Result<usize> {
        let mut input = self.queue.lock().unwrap();
        if input.closed {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        if !input.attached || !input.pending.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let len = bytes.len().min(FRAME);
        self.notify()?;
        input.pending.extend_from_slice(&bytes[..len]);
        Ok(len)
    }
}

impl Drop for PtyWriter {
    fn drop(&mut self) {
        self.set_attached(false);
        let _ = self.wake.shutdown(Shutdown::Write);
        let _ = self.thread.take().unwrap().join();
    }
}

fn read_output(mut master: File, wake: UnixStream, status: mpsc::Receiver<io::Result<i32>>, mut events: PtyEvents) {
    let mut bytes = [0; FRAME];
    let mut readable = true;
    let mut exited = None;
    let mut final_bytes = 0;
    let result = loop {
        if exited.is_none() {
            match wait_ready(&wake, readable.then_some(&master), libc::POLLIN) {
                Ok(true) => match status.try_recv() {
                    Ok(result) => exited = Some(result),
                    Err(_) => return, // The registry dropped the PTY.
                },
                Ok(false) => {}
                Err(e) => break Err(e),
            }
        }
        // The shell's wait status ends the stream, even if another process still holds
        // the slave. Keep its last readable bytes ahead of the exit, with a bounded drain.
        if !readable || final_bytes >= OUTPUT_WINDOW {
            if let Some(exit) = exited.take() { break exit; }
            continue;
        }
        match master.read(&mut bytes) {
            Ok(0) => readable = false,
            Ok(len) => {
                if !(events.output)(PtyEvent::Bytes(bytes[..len].to_vec())) { return; }
                if exited.is_some() { final_bytes += len; }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if let Some(exit) = exited.take() { break exit; }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.raw_os_error() == Some(libc::EIO) => readable = false,
            Err(e) => break exited.take().unwrap_or(Err(e)),
        }
    };
    (events.output)(match result {
        Ok(code) => PtyEvent::Exit(code),
        Err(e) => PtyEvent::Error(e),
    });
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
        if self.waiter.is_some() { return Err(io::ErrorKind::AlreadyExists.into()); }
        let master = self.master.as_ref().unwrap();
        let reader = master.try_clone()?;
        let (wake, ready) = UnixStream::pair()?;
        let exit_wake = wake.try_clone()?;
        self.input = Some(PtyWriter::start(master.try_clone()?, self.allowed.clone(), events.writable.clone())?);

        let pid = self.child.lock().unwrap().as_ref().unwrap().id() as libc::pid_t;
        let child = self.child.clone();
        let (status, exited) = mpsc::channel();
        self.waiter = Some(thread::Builder::new().name("pty-exit".into()).spawn(move || {
            if let Err(e) = wait_for_exit(pid, 0) {
                eprintln!("{}: {e}", t("terminal.close_failed"));
            }
            let mut child = child.lock().unwrap().take().unwrap();
            // Keep the session leader waitable until cleanup has identified its job-control groups.
            if let Err(e) = kill_session(pid) {
                eprintln!("{}: {e}", t("terminal.close_failed"));
                let _ = child.kill();
            }
            let result = child.wait().map(|status| {
                status.code().unwrap_or_else(|| -status.signal().expect("process exit signal"))
            });
            let _ = status.send(result);
            let _ = exit_wake.shutdown(Shutdown::Write);
        })?);
        let reader = thread::Builder::new().name("pty-output".into()).spawn(move || read_output(reader, ready, exited, events))?;
        self.reader = Some((wake, reader));
        Ok(())
    }

    fn set_attached(&mut self, attached: bool) {
        if let Some(input) = &self.input { input.set_attached(attached); }
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
        self.master = None;
        self.input = None;
        if let Some((wake, reader)) = self.reader.take() {
            let _ = wake.shutdown(Shutdown::Write);
            let _ = reader.join();
        }
        {
            let mut slot = self.child.lock().unwrap();
            if let Some(child) = slot.as_mut() {
                if let Err(e) = kill_session(child.id() as libc::pid_t) {
                    eprintln!("{}: {e}", t("terminal.close_failed"));
                    let _ = child.kill();
                }
                if self.waiter.is_none() { let _ = child.wait(); }
            }
        }
        if let Some(waiter) = self.waiter.take() { let _ = waiter.join(); }
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

    #[test]
    fn probe_path_uses_the_last_non_empty_line() {
        for (output, expected) in [
            (&b""[..], None),
            (b"\n\n", None),
            (b"/bin:/usr/bin\n", Some(&b"/bin:/usr/bin"[..])),
            (b"hello from the rc file\n\n /extra:/bin \n\n", Some(&b" /extra:/bin "[..])),
            (b"startup\n/b\xffn", Some(&b"/b\xffn"[..])),
        ] {
            assert_eq!(path_line(output), expected.map(OsStr::from_bytes), "{output:?}");
        }
    }

    #[test]
    fn interactive_paths_exclude_login_entries_and_keep_order() {
        for (login, interactive, expected) in [
            ("/usr/bin:/bin", "/bin:/usr/bin", &[][..]),
            ("/usr/bin:/bin", "/z:/bin:/a:/z:/usr/bin:/a", &["/z", "/a"][..]),
            ("/usr/bin:", ":/extra:/extra:", &["/extra"][..]),
        ] {
            assert_eq!(added_paths(OsStr::new(login), OsStr::new(interactive)), expected, "{login:?}, {interactive:?}");
        }
    }

    #[test]
    fn interactive_paths_are_appended_to_the_host_path() {
        for (path, added, expected) in [
            (Some("/host:/bin"), &[][..], "/host:/bin"),
            (Some("/host:/bin"), &["/z", "/a"][..], "/host:/bin:/z:/a"),
            (None, &["/z", "/a"][..], "/z:/a"),
            (Some(""), &["/extra"][..], ":/extra"),
        ] {
            let added: Vec<_> = added.iter().map(OsString::from).collect();
            assert_eq!(append_paths(path.map(OsString::from), &added), expected, "{path:?}, {added:?}");
        }
    }

    #[test]
    fn probe_reads_the_interactive_shell_env_file() {
        let home = std::env::temp_dir().join(format!("deskway-shell-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        std::fs::create_dir(&home).unwrap();
        let rc = home.join("rc");
        let result = std::fs::write(&rc, "echo startup\nexport PATH=\"$PATH:${ENV%/*}/bin\"\n").and_then(|()| {
            probe_paths(|| {
                let mut shell = Command::new("/bin/sh");
                shell.env_clear().env("SHELL", "/bin/sh").env("HOME", &home).env("ENV", &rc).env("PATH", "/usr/bin:/bin");
                shell
            })
        });
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(result.unwrap(), [home.join("bin").into_os_string()]);
    }

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
        pty.set_attached(true);
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
    fn browser_pty_drops_input_on_detach() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut command = Command::new("/bin/sh");
        command.args(["-c", "stty -echo; echo ready; read -r line; printf '%s\\n' \"$line\""]);
        let allowed = Arc::new(AtomicBool::new(true));
        let check = allowed.clone();
        let mut pty = BrowserPty::start(command, 24, 80, move || check.load(Ordering::Acquire)).unwrap();
        let (listener, events) = test_events();
        pty.listen(listener).unwrap();
        let mut output = Vec::new();
        read_until(&events, &mut output, b"ready\r\n");
        pty.set_attached(true);
        allowed.store(false, Ordering::Release);
        pty.write_all(b"old attachment\n").unwrap();
        pty.set_attached(false);
        allowed.store(true, Ordering::Release);
        pty.set_attached(true);
        pty.write_all(b"current attachment\n").unwrap();
        assert_eq!(read_exit(&events, &mut output), 0);
        assert_eq!(output, b"ready\r\ncurrent attachment\r\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn browser_pty_reports_exit_and_closes_the_master_with_a_slave_still_open() {
        for exit in [false, true] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "stty -echo; echo ready; read -r line; echo last; exit 17"]);
            let mut pty = BrowserPty::start(command, 24, 80, || true).unwrap();
            let fd = pty.master.as_ref().unwrap().as_raw_fd();
            let slave = unsafe { libc::ioctl(fd, libc::TIOCGPTPEER, libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
            assert!(slave >= 0, "{}", io::Error::last_os_error());
            let slave = unsafe { OwnedFd::from_raw_fd(slave) };
            let (listener, events) = test_events();
            pty.listen(listener).unwrap();
            let mut output = Vec::new();
            read_until(&events, &mut output, b"ready\r\n");
            if exit {
                pty.set_attached(true);
                pty.write_all(b"done\n").unwrap();
                assert_eq!(read_exit(&events, &mut output), 17);
                assert_eq!(output, b"ready\r\nlast\r\n");
            }
            drop(pty);
            let mut status = libc::pollfd { fd: slave.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            assert_eq!(unsafe { libc::poll(&mut status, 1, 0) }, 1);
            assert_ne!(status.revents & libc::POLLHUP, 0, "every master descriptor closed");
            assert!(matches!(events.try_recv(), Err(mpsc::TryRecvError::Disconnected)));
        }
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
            let starter = crate::auth::ClientIdentity { device: "c1".into(), name: "Claude".into() };
            let started = if terminal {
                table.start(Some("exec /bin/sh -i"), None, 100, 30, false, starter)
            } else {
                table.run(&format!("{JOB}; wait"), None, 0, false, starter)
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
