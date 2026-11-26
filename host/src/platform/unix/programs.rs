//! Unix programs use a login shell, with pipes or a controlling terminal of their own.

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

    #[cfg(target_os = "linux")]
    pub fn resize(&self, rows: u16, cols: u16) -> io::Result<()> {
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

/// Signal the terminal's session, including the job-control groups the shell did not join.
/// The caller holds the Program lock, so the session leader cannot be reaped by a concurrent
/// tool call while its ID is in use here.
fn kill_session(id: libc::pid_t) -> io::Result<()> {
    // End the shell and its own group first so it cannot start more work during the scan.
    let mut error = signal(-id).err();
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
