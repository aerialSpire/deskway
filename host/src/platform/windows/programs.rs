//! Programs an AI client starts: through PowerShell, out of the service's job so they outlive
//! the agent (docs/architecture.md, "Workspace and process lifetime"); with pipes, or in a terminal of
//! their own (ConPTY). A terminal's console host is this process's child, so a terminal program
//! loses its console when the user worker restarts; a piped one runs on.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::platform::Pty;
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

/// The shell commands run in: PowerShell 7 (`pwsh.exe`) where it is installed (on the PATH),
/// else Windows PowerShell, which every Windows has.
fn shell() -> &'static str {
    static SHELL: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| {
        let found = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("pwsh.exe").is_file()));
        if found { "pwsh.exe" } else { "powershell.exe" }
    })
}

/// Starts `command` with its output piped. Out of this process's job when the job allows it
/// (the service's does); a job that does not (a terminal's, in a dev run) keeps it.
pub fn start_program(command: &str, cwd: Option<&str>, elevated: bool) -> std::io::Result<Child> {
    rights(elevated)?;
    // Its output in UTF-8, as it is read: PowerShell and the programs it starts (which take the
    // console's code page from it) would otherwise write in the system's (GBK and the like).
    let command = format!("[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n{command}");
    let build = |flags: u32| {
        let mut c = Command::new(shell());
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
    let mut line = format!("{} -NoLogo -NoProfile", shell());
    if command.is_none() {
        // A plain prompt: PSReadLine redraws the line with cursor moves, which read as text
        // without them (`proc_read`) come out as the same input more than once.
        line.push_str(" -NoExit -Command Remove-Module PSReadLine -ErrorAction SilentlyContinue");
    }
    if let Some(c) = command {
        // As UTF-16 in base64: the command line takes it whatever quotes it has.
        let utf16: Vec<u8> = c.encode_utf16().flat_map(u16::to_le_bytes).collect();
        line.push_str(&format!(" -EncodedCommand {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, utf16)));
    }
    let (program, output) = start_console(&line, cwd.map(std::ffi::OsStr::new), cols, rows, None)?;
    Ok(Started { program: Box::new(program), output: vec![Box::new(output)] })
}

fn start_console(line: &str, cwd: Option<&std::ffi::OsStr>, cols: u16, rows: u16, browser: Option<(&Job, &dyn Fn() -> bool)>) -> io::Result<(Terminal, File)> {
    use std::os::windows::ffi::OsStrExt;
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
            let create = |flags: u32| {
                let mut cmd: Vec<u16> = line.encode_utf16().chain([0]).collect();
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

pub(super) fn open_pty(rows: u16, cols: u16, isolated: bool, allowed: impl Fn() -> bool + Send + Sync + 'static) -> io::Result<Box<dyn Pty>> {
    if !allowed() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let home = std::env::home_dir().ok_or(io::ErrorKind::NotFound)?;
    let pwsh = super::known_folder(super::KnownFolder::ProgramFiles).join("PowerShell").join("7").join("pwsh.exe");
    let mut line = if pwsh.is_file() {
        format!("\"{}\" -NoLogo -NoProfile", pwsh.display())
    } else {
        format!("{} -NoLogo -NoProfile", shell())
    };
    if isolated {
        // PSReadLine asks Windows for the roaming folder, which ignores APPDATA.
        line.push_str(r" -NoExit -Command Set-PSReadLineOption -HistorySavePath (Join-Path $env:APPDATA 'Microsoft\Windows\PowerShell\PSReadLine\ConsoleHost_history.txt')");
    }
    browser_pty(&line, &home, rows, cols, allowed)
}

fn browser_pty(line: &str, home: &std::path::Path, rows: u16, cols: u16, allowed: impl Fn() -> bool + Send + Sync + 'static) -> io::Result<Box<dyn Pty>> {
    let job = Job::new()?;
    let (mut terminal, output) = start_console(line, Some(home.as_os_str()), cols, rows, Some((&job, &allowed)))?;
    let mut writer = terminal.input.take().expect("new terminal has input");
    let (input, queued) = sync_channel::<Vec<u8>>(1);
    let allowed = std::sync::Arc::new(allowed);
    let pty = BrowserPty { terminal, output: Some(output), input, job, status: None, allowed: allowed.clone() };
    // ConPTY requires synchronous pipes. A short queue bounds the writes the registry
    // can acknowledge ahead of the console, and a blocked writer never holds its lock.
    std::thread::Builder::new().name("conpty-input".into()).spawn(move || {
        for bytes in queued {
            if allowed() && writer.write_all(&bytes).is_err() {
                break;
            }
        }
    })?;
    Ok(Box::new(pty))
}

struct BrowserPty {
    terminal: Terminal,
    output: Option<File>,
    input: SyncSender<Vec<u8>>,
    job: Job,
    status: Option<i32>,
    allowed: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Read for BrowserPty {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if !(self.allowed)() { return Err(io::ErrorKind::WouldBlock.into()); }
        self.read_output(bytes)
    }
}

impl BrowserPty {
    fn read_output(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let Some(output) = &mut self.output else { return Ok(0); };
        let mut pending = 0;
        match unsafe { PeekNamedPipe(HANDLE(output.as_raw_handle()), None, 0, None, Some(&mut pending), None) } {
            Ok(()) if pending > 0 => {
                let count = bytes.len().min(pending as usize);
                output.read(&mut bytes[..count])
            }
            Ok(()) => Err(io::ErrorKind::WouldBlock.into()),
            Err(e) if e.code() == windows::core::HRESULT::from_win32(windows::Win32::Foundation::ERROR_BROKEN_PIPE.0) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
}

impl Write for BrowserPty {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.input.try_send(bytes.to_vec()).map_err(|error| match error {
            TrySendError::Full(_) => io::ErrorKind::WouldBlock,
            TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

impl Pty for BrowserPty {
    fn drain(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.read_output(buf) }

    fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()> {
        let Some(console) = self.terminal.console else { return Ok(()); };
        let size = COORD {
            X: i16::try_from(cols).map_err(io::Error::other)?,
            Y: i16::try_from(rows).map_err(io::Error::other)?,
        };
        Ok(unsafe { ResizePseudoConsole(console, size) }?)
    }

    fn redraw(&mut self, rows: u16, cols: u16) -> io::Result<()> { self.resize(rows, cols) }

    fn exit(&mut self) -> io::Result<Option<i32>> {
        if self.status.is_none() {
            let mut code = 0;
            unsafe { GetExitCodeProcess(self.terminal.process, &mut code)? };
            if code != STILL_ACTIVE.0 as u32 {
                unsafe { TerminateJobObject(self.job.0, 1)? };
                if let Some(console) = self.terminal.console.take() {
                    let raw = console.0;
                    // Older ConPTY versions wait for output to be read before closing.
                    std::thread::spawn(move || unsafe { ClosePseudoConsole(HPCON(raw)) });
                }
                self.status = Some(code as i32);
            }
        }
        Ok(self.status)
    }

    fn terminate(&mut self) -> io::Result<bool> {
        if self.exit()?.is_some() {
            return Ok(false);
        }
        unsafe { TerminateJobObject(self.job.0, 1)? };
        Ok(true)
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
    let line = format!("{} -NoLogo -NoProfile", shell());
    let error = browser_pty(&line, &home, 24, 80, || false).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

#[cfg(test)]
#[test]
fn browser_conpty_exchanges_utf8_and_resizes() {
    use std::time::{Duration, Instant};

    let dir = std::env::temp_dir();
    let line = format!("{} -NoLogo -NoProfile -NoExit -Command Remove-Module PSReadLine -ErrorAction SilentlyContinue", shell());
    let mut pty = browser_pty(&line, &dir, 24, 80, || true).unwrap();
    let mut output = Vec::new();
    let mut wait = |pty: &mut dyn Pty, expected: &str| {
        let deadline = Instant::now() + Duration::from_secs(7);
        let mut buffer = [0; 1024];
        loop {
            match pty.read(&mut buffer) {
                Ok(n) => output.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
            if String::from_utf8_lossy(&output).contains(expected) { break; }
            assert!(Instant::now() < deadline, "missing {expected:?} in {:?}", String::from_utf8_lossy(&output));
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait(pty.as_mut(), "PS ");
    pty.write_all("Write-Output ('\u{6587}' + '\u{5b57}'); Write-Output ('width=' + $Host.UI.RawUI.WindowSize.Width)\r".as_bytes()).unwrap();
    wait(pty.as_mut(), "\u{6587}\u{5b57}");
    wait(pty.as_mut(), "width=80");
    pty.resize(31, 95).unwrap();
    pty.write_all(b"Write-Output ('width=' + $Host.UI.RawUI.WindowSize.Width)\r").unwrap();
    wait(pty.as_mut(), "width=95");
    assert!(pty.terminate().unwrap());
}
