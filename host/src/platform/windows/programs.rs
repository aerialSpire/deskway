//! Programs an AI client starts: through PowerShell, out of the service's job so they outlive
//! the agent (docs/architecture.md, "Workspace and process lifetime"); with pipes, or in a terminal of
//! their own (ConPTY). A terminal's console host is this process's child, so a terminal program
//! loses its console when the user worker restarts; a piped one runs on.

use std::fs::File;
use std::io::Write;
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::workspace::procs::{Program, Started};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// Whether this process has administrator rights (the admin worker; a host run elevated).
fn elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut e = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, Some(&mut e as *mut _ as _), size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok();
        let _ = CloseHandle(token);
        ok && e.TokenIsElevated != 0
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
    unsafe {
        let (mut in_read, mut in_write, mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default(), HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_read, &mut in_write, None, 0)?;
        CreatePipe(&mut out_read, &mut out_write, None, 0)?;
        let console = CreatePseudoConsole(COORD { X: cols as i16, Y: rows as i16 }, in_read, out_write, 0);
        // The console holds its own copies.
        let _ = CloseHandle(in_read);
        let _ = CloseHandle(out_write);
        let (input, output) = (File::from_raw_handle(in_write.0), File::from_raw_handle(out_read.0));
        let console = console?;

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
            let dir: Option<Vec<u16>> = cwd.map(|d| d.encode_utf16().chain([0]).collect());
            let create = |flags: u32| {
                let mut cmd: Vec<u16> = line.encode_utf16().chain([0]).collect();
                let mut pi = PROCESS_INFORMATION::default();
                CreateProcessW(None, Some(PWSTR(cmd.as_mut_ptr())), None, None, false, EXTENDED_STARTUPINFO_PRESENT | PROCESS_CREATION_FLAGS(flags), None, dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())), &si.StartupInfo, &mut pi).map(|()| pi)
            };
            Ok(create(CREATE_BREAKAWAY_FROM_JOB).or_else(|_| create(0))?)
        })();
        DeleteProcThreadAttributeList(attrs);
        let pi = match started {
            Ok(pi) => pi,
            Err(e) => {
                ClosePseudoConsole(console);
                return Err(e);
            }
        };
        let _ = CloseHandle(pi.hThread);
        let program = Terminal { process: pi.hProcess, pid: pi.dwProcessId, console: Some(console), input };
        Ok(Started { program: Box::new(program), output: vec![Box::new(output)] })
    }
}

/// A program in a terminal: its process, its console (closed once it ends, so its output
/// reaches its end), and the console's input.
struct Terminal {
    process: HANDLE,
    pid: u32,
    console: Option<HPCON>,
    input: File,
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
        Some(&mut self.input)
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
