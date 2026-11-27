//! Programs an AI client starts: through PowerShell, out of the service's job so they outlive
//! the agent (docs/architecture.md, "Workspace and process lifetime"); with pipes, or in a terminal of
//! their own (ConPTY). A terminal's console host is this process's child, so a terminal program
//! loses its console when the user worker restarts; a piped one runs on.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::platform::Pty;
use crate::platform::pty::{PtyEvents, PtyInput};
use crate::workspace::procs::{Program, Started};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Whether this process has administrator rights (the admin worker; a host run elevated).
fn elevated() -> bool {
    token_elevation().unwrap_or(false)
}

pub(super) fn token_elevation() -> io::Result<bool> {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let result = GetTokenInformation(token, TokenElevation, Some(&mut e as *mut _ as _), size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        let _ = CloseHandle(token);
        result?;
        Ok(e.TokenIsElevated != 0)
    }
}

/// `elevated`, asked of a process without administrator rights, is refused: on Windows the
/// process's own rights decide (the admin worker has them).
fn rights(elevated_asked: bool) -> std::io::Result<()> {
    if elevated_asked && !elevated() {
        return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "this process has no administrator rights"));
    }
    Ok(())
}

/// PowerShell for both AI programs and browser terminals.
fn powershell() -> &'static Path {
    static POWERSHELL: OnceLock<PathBuf> = OnceLock::new();
    POWERSHELL.get_or_init(|| {
        let path = std::env::var_os("PATH");
        // Resolve relative PATH entries before passing the executable to either launch API.
        let candidates = path.iter().flat_map(std::env::split_paths)
            .filter_map(|dir| std::path::absolute(dir.join("pwsh.exe")).ok());
        let program_files = super::known_folder(super::KnownFolder::ProgramFiles);
        let system = super::known_folder(super::KnownFolder::System);
        select_powershell(candidates, &program_files, &system, Path::is_file)
    })
}

fn select_powershell(path_candidates: impl IntoIterator<Item = PathBuf>, program_files: &Path, system: &Path, mut is_file: impl FnMut(&Path) -> bool) -> PathBuf {
    path_candidates.into_iter()
        .chain(std::iter::once(program_files.join(r"PowerShell\7\pwsh.exe")))
        .find(|candidate| is_file(candidate))
        .unwrap_or_else(|| system.join(r"WindowsPowerShell\v1.0\powershell.exe"))
}

#[cfg(test)]
#[test]
fn powershell_prefers_path_then_program_files_then_windows_powershell() {
    let program_files = Path::new(r"C:\Program Files");
    let system = Path::new(r"D:\Windows\System32");
    let first = r"C:\tools\pwsh.exe";
    let second = r"C:\other tools\pwsh.exe";
    let installed = r"C:\Program Files\PowerShell\7\pwsh.exe";
    for (files, expected) in [
        (&[first, second, installed][..], first),
        (&[second, installed], second),
        (&[installed], installed),
        (&[], r"D:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
    ] {
        let selected = select_powershell([first, second].map(PathBuf::from), program_files, system, |candidate| {
            files.iter().any(|file| candidate == Path::new(file))
        });
        assert_eq!(selected, Path::new(expected), "files: {files:?}");
    }
}

fn powershell_line(args: &str) -> OsString {
    let mut line = OsString::from("\"");
    line.push(powershell());
    line.push("\" ");
    line.push(args);
    line
}

/// Starts `command` with its output piped. Out of this process's job when the job allows it
/// (the service's does); a job that does not (a terminal's, in a dev run) keeps it.
pub fn start_program(command: &str, cwd: Option<&str>, elevated: bool) -> std::io::Result<Child> {
    rights(elevated)?;
    // Its output in UTF-8, as it is read: PowerShell and the programs it starts (which take the
    // console's code page from it) would otherwise write in the system's (GBK and the like).
    let command = format!("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n{command}");
    let build = |flags: u32| {
        let mut c = Command::new(powershell());
        c.args(["-NoProfile", "-NonInteractive", "-Command", &command]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).creation_flags(flags);
        if let Some(dir) = cwd {
            c.current_dir(dir);
        }
        c
    };
    build(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB).spawn().or_else(|_| build(CREATE_NO_WINDOW).spawn())
}

/// Starts `command` (PowerShell itself, when there is none) in a terminal of `cols` × `rows`.
pub fn start_terminal(command: Option<&str>, cwd: Option<&str>, cols: u16, rows: u16, elevated: bool) -> std::io::Result<Started> {
    rights(elevated)?;
    let mut line = powershell_line("-NoLogo -NoProfile");
    if command.is_none() {
        // A plain prompt: PSReadLine redraws the line with cursor moves, which read as text
        // without them (`proc_read`) come out as the same input more than once.
        line.push(" -NoExit -Command Remove-Module PSReadLine -ErrorAction SilentlyContinue");
    }
    if let Some(c) = command {
        // As UTF-16 in base64: the command line takes it whatever quotes it has.
        let utf16: Vec<u8> = c.encode_utf16().flat_map(u16::to_le_bytes).collect();
        line.push(format!(" -EncodedCommand {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, utf16)));
    }
    let (program, output) = start_console(&line, cwd.map(OsStr::new), cols, rows, None)?;
    Ok(Started { program: Box::new(program), output: vec![Box::new(output)] })
}

fn start_console(line: &OsStr, cwd: Option<&OsStr>, cols: u16, rows: u16, browser: Option<(&Job, &dyn Fn() -> bool)>) -> io::Result<(Terminal, File)> {
    use windows::Win32::System::Threading::{ResumeThread, CREATE_SUSPENDED};
    let (cols, rows) = (i16::try_from(cols).map_err(io::Error::other)?, i16::try_from(rows).map_err(io::Error::other)?);
    unsafe {
        let (mut in_read, mut in_write, mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default(), HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_read, &mut in_write, None, 0)?;
        let (in_read, input) = (File::from_raw_handle(in_read.0), File::from_raw_handle(in_write.0));
        CreatePipe(&mut out_read, &mut out_write, None, 0)?;
        let (output, out_write) = (File::from_raw_handle(out_read.0), File::from_raw_handle(out_write.0));
        let console = CreatePseudoConsole(COORD { X: cols, Y: rows }, HANDLE(in_read.as_raw_handle()), HANDLE(out_write.as_raw_handle()), 0)?;
        // The console holds its own copies.
        drop(in_read);
        drop(out_write);

        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut attr_buf = vec![0u8; size];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as _);
        let started = (|| -> std::io::Result<PROCESS_INFORMATION> {
            InitializeProcThreadAttributeList(Some(attrs), 1, None, &mut size)?;
            UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, Some(console.0 as *const _), size_of::<HPCON>(), None, None)?;
            let mut si = STARTUPINFOEXW::default();
            si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            si.lpAttributeList = attrs;
            // No standard handles of this process: only the terminal's.
            si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            (si.StartupInfo.hStdInput, si.StartupInfo.hStdOutput, si.StartupInfo.hStdError) = (INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE);
            let dir: Option<Vec<u16>> = cwd.map(|d| d.encode_wide().chain([0]).collect());
            // A process that ignores Ctrl-C hands that down to its children; the shell must not.
            windows::Win32::System::Console::SetConsoleCtrlHandler(None, false)?;
            let create = |flags: u32| {
                let mut cmd: Vec<u16> = line.encode_wide().chain([0]).collect();
                let mut pi = PROCESS_INFORMATION::default();
                CreateProcessW(None, Some(PWSTR(cmd.as_mut_ptr())), None, None, false, EXTENDED_STARTUPINFO_PRESENT | PROCESS_CREATION_FLAGS(flags), None, dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())), &si.StartupInfo, &mut pi).map(|()| pi)
            };
            if let Some((job, allowed)) = browser {
                let pi = create(CREATE_SUSPENDED.0)?;
                let resume = AssignProcessToJobObject(job.0, pi.hProcess).map_err(io::Error::from).and_then(|()| {
                    if !allowed() {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    if ResumeThread(pi.hThread) == u32::MAX {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
                if let Err(error) = resume {
                    let _ = TerminateProcess(pi.hProcess, 1);
                    let _ = CloseHandle(pi.hProcess);
                    let _ = CloseHandle(pi.hThread);
                    return Err(error);
                }
                Ok(pi)
            } else {
                Ok(create(CREATE_BREAKAWAY_FROM_JOB).or_else(|_| create(0))?)
            }
        })();
        DeleteProcThreadAttributeList(attrs);
        let pi = match started {
            Ok(pi) => pi,
            Err(e) => {
                drop(output);
                ClosePseudoConsole(console);
                return Err(e);
            }
        };
        let _ = CloseHandle(pi.hThread);
        let program = Terminal { process: pi.hProcess, pid: pi.dwProcessId, console: Some(console), input: Some(input) };
        Ok((program, output))
    }
}

/// A program in a terminal: its process, its console (closed once it ends, so its output
/// reaches its end), and the console's input.
struct Terminal {
    process: HANDLE,
    pid: u32,
    console: Option<HPCON>,
    input: Option<File>,
}

// The handles are this program's own, used under the table's lock.
unsafe impl Send for Terminal {}

impl Terminal {
    fn close_console(&mut self) {
        if let Some(c) = self.console.take() {
            unsafe { ClosePseudoConsole(c) };
        }
    }
}

impl Program for Terminal {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn exit(&mut self) -> Option<i32> {
        let mut code = 0u32;
        unsafe { GetExitCodeProcess(self.process, &mut code) }.ok()?;
        if code == STILL_ACTIVE.0 as u32 {
            return None;
        }
        self.close_console();
        Some(code as i32)
    }

    fn kill(&mut self) -> std::io::Result<()> {
        kill_tree(self.pid);
        let _ = unsafe { TerminateProcess(self.process, 1) };
        Ok(())
    }

    fn input(&mut self) -> Option<&mut dyn Write> {
        self.input.as_mut().map(|input| input as &mut dyn Write)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close_console();
        let _ = unsafe { CloseHandle(self.process) };
    }
}

#[cfg(test)]
#[test]
fn administrator_rights_are_asked_only_of_a_process_that_has_them() {
    for start in [|| start_program("exit", None, true).map(drop), || start_terminal(None, None, 80, 24, true).map(drop)] {
        let r = start();
        assert_eq!(r.as_ref().err().map(std::io::Error::kind), (!elevated()).then_some(std::io::ErrorKind::Unsupported), "{:?}", r.err());
    }
}

/// Ends a program and every program it started.
pub fn kill_tree(pid: u32) {
    let _ = Command::new("taskkill.exe").args(["/T", "/F", "/PID", &pid.to_string()]).creation_flags(CREATE_NO_WINDOW).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

struct Job(HANDLE);

unsafe impl Send for Job {}
// Windows permits concurrent operations on this job; the handle stays open while shared.
unsafe impl Sync for Job {}

impl Job {
    fn new() -> io::Result<Self> {
        unsafe {
            let job = Self(CreateJobObjectW(None, None)?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(job.0, JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32)?;
            Ok(job)
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub(super) fn open_pty(command: Option<&str>, rows: u16, cols: u16, allowed: impl Fn() -> bool + Send + Sync + 'static) -> io::Result<Box<dyn Pty>> {
    if !allowed() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let home = std::env::home_dir().ok_or(io::ErrorKind::NotFound)?;
    let line = command.map_or_else(|| powershell_line("-NoLogo -NoProfile"), OsString::from);
    browser_pty(&line, &home, rows, cols, allowed)
}

fn browser_pty(line: &OsStr, home: &Path, rows: u16, cols: u16, allowed: impl Fn() -> bool + Send + Sync + 'static) -> io::Result<Box<dyn Pty>> {
    let job = Job::new()?;
    let (terminal, output) = start_console(line, Some(home.as_os_str()), cols, rows, Some((&job, &allowed)))?;
    Ok(Box::new(BrowserPty {
        terminal, output: Some(output), input: None, job: Arc::new(job),
        console: Arc::default(), allowed: Arc::new(allowed),
    }))
}

struct BrowserPty {
    terminal: Terminal,
    output: Option<File>,
    input: Option<PtyInput>,
    job: Arc<Job>,
    console: Arc<Mutex<Option<HPCON>>>,
    allowed: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Write for BrowserPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.input.as_ref().ok_or(io::ErrorKind::NotConnected)?.write(bytes);
        if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            && unsafe { WaitForSingleObject(self.terminal.process, 0) } == WAIT_OBJECT_0
        {
            // The reader still owns any final output and will report the exit in order.
            return Err(io::ErrorKind::WouldBlock.into());
        }
        result
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl Pty for BrowserPty {
    fn listen(&mut self, events: PtyEvents) -> io::Result<()> {
        let output = self.output.take().ok_or(io::ErrorKind::AlreadyExists)?;
        let mut writer = self.terminal.input.take().expect("new terminal has input");
        let process = unsafe { BorrowedHandle::borrow_raw(self.terminal.process.0) }.try_clone_to_owned()?;
        let allowed = self.allowed.clone();
        self.input = Some(PtyInput::start(move |bytes| {
            if allowed() { writer.write_all(bytes)?; }
            Ok(())
        }, events.writable.clone())?);

        let (status, exited) = mpsc::channel();
        let job = self.job.clone();
        let console = self.console.clone();
        *console.lock().unwrap() = self.terminal.console.take();
        let waiter = std::thread::Builder::new().name("conpty-exit".into()).spawn(move || {
            let process = HANDLE(process.as_raw_handle());
            let result = unsafe {
                if WaitForSingleObject(process, INFINITE) != WAIT_OBJECT_0 {
                    Err(io::Error::last_os_error())
                } else {
                    let mut code = 0;
                    GetExitCodeProcess(process, &mut code).map(|()| code as i32).map_err(io::Error::from)
                }
            };
            let cleanup = unsafe { TerminateJobObject(job.0, 1) }.map_err(io::Error::from);
            let _ = status.send(result.and_then(|code| cleanup.map(|()| code)));
            let closing = console.lock().unwrap().take();
            // Older ConPTY versions wait for the reader to drain the output here.
            if let Some(closing) = closing { unsafe { ClosePseudoConsole(closing) }; }
        });
        if let Err(error) = waiter {
            self.terminal.console = self.console.lock().unwrap().take();
            return Err(error);
        }
        std::thread::Builder::new().name("conpty-output".into()).spawn(move || {
            events.read(output, || exited.recv().map_err(io::Error::other)?);
        })?;
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let console = self.console.lock().unwrap();
        let Some(console) = console.or(self.terminal.console) else { return Ok(()); };
        let size = COORD {
            X: i16::try_from(cols).map_err(io::Error::other)?,
            Y: i16::try_from(rows).map_err(io::Error::other)?,
        };
        Ok(unsafe { ResizePseudoConsole(console, size) }?)
    }

    fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> { self.resize(rows, cols) }

    fn terminate(&mut self) -> io::Result<bool> {
        match unsafe { WaitForSingleObject(self.terminal.process, 0) } {
            WAIT_OBJECT_0 => Ok(false),
            WAIT_TIMEOUT => {
                unsafe { TerminateJobObject(self.job.0, 1)? };
                Ok(true)
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for BrowserPty {
    fn drop(&mut self) {
        let _ = unsafe { TerminateJobObject(self.job.0, 1) };
        drop(self.output.take());
    }
}


#[cfg(test)]
#[test]
fn a_suspended_shell_is_rejected_if_the_console_locks_during_start() {
    let home = std::env::temp_dir();
    let line = powershell_line("-NoLogo -NoProfile");
    let error = browser_pty(&line, &home, 24, 80, || false).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[cfg(test)]
fn wait_for_conpty_output(events: &mpsc::Receiver<crate::platform::pty::PtyEvent>, output: &mut Vec<u8>, expected: &str) {
    use crate::platform::pty::PtyEvent;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(8);
    while !String::from_utf8_lossy(output).contains(expected) {
        match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(PtyEvent::Bytes(bytes)) => output.extend(bytes),
            other => panic!("missing {expected:?} in {:?}, output event: {other:?}", String::from_utf8_lossy(output)),
        }
    }
}

/// A shell that keeps no history, so tests leave the account's own untouched.
#[cfg(test)]
fn plain_shell() -> OsString {
    powershell_line("-NoLogo -NoProfile -NoExit -Command Remove-Module PSReadLine -ErrorAction SilentlyContinue")
}

#[cfg(test)]
#[test]
fn browser_conpty_exchanges_utf8_and_resizes() {
    let dir = std::env::temp_dir();
    let line = plain_shell();
    let mut pty = browser_pty(&line, &dir, 24, 80, || true).unwrap();
    let (listener, events) = crate::platform::pty::test_events();
    pty.listen(listener).unwrap();
    let mut output = Vec::new();
    wait_for_conpty_output(&events, &mut output, "PS ");
    pty.write_all("Write-Output ('\u{6587}' + '\u{5b57}'); Write-Output ('width=' + $Host.UI.RawUI.WindowSize.Width)\r".as_bytes()).unwrap();
    wait_for_conpty_output(&events, &mut output, "\u{6587}\u{5b57}");
    wait_for_conpty_output(&events, &mut output, "width=80");
    pty.resize(31, 95).unwrap();
    pty.write_all(b"Write-Output ('width=' + $Host.UI.RawUI.WindowSize.Width)\r").unwrap();
    wait_for_conpty_output(&events, &mut output, "width=95");
    use crate::platform::pty::PtyEvent;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    pty.write_all(b"exit 17\r").unwrap();
    loop {
        match events.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(PtyEvent::Bytes(_)) => {}
            Ok(PtyEvent::Exit(code)) => { assert_eq!(code, 17); break; }
            other => panic!("expected exit: {other:?}"),
        }
    }
    assert_eq!(events.recv_timeout(std::time::Duration::from_secs(2)).unwrap_err(), mpsc::RecvTimeoutError::Disconnected);
    assert!(!pty.terminate().unwrap());
}

#[cfg(test)]
#[test]
fn browser_conpty_ctrl_c_interrupts_powershell() {
    let dir = std::env::temp_dir();
    let mut pty = browser_pty(&plain_shell(), &dir, 24, 120, || true).unwrap();
    let (listener, events) = crate::platform::pty::test_events();
    pty.listen(listener).unwrap();
    let mut output = Vec::new();
    wait_for_conpty_output(&events, &mut output, "PS ");
    pty.write_all(b"Write-Output ('sleep-' + 'started'); Start-Sleep -Seconds 60\r").unwrap();
    wait_for_conpty_output(&events, &mut output, "sleep-started");
    output.clear();
    pty.write_all(b"\x03").unwrap();
    wait_for_conpty_output(&events, &mut output, "PS ");
    pty.write_all(b"Write-Output ('interrupt-' + 'returned')\r").unwrap();
    wait_for_conpty_output(&events, &mut output, "interrupt-returned");
}
