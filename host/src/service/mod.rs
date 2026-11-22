//! Windows: the two services, the agent they run, the pipes between Deskway's processes and
//! installing it all. Windows has one process model: the network service and the agent.
//!
//! `Deskway` runs as LocalSystem. Its only job is to keep processes running in the console
//! session, inside a job that dies with the service: the agent, with a copy of its own token
//! moved to that session and every privilege the agent does not need removed; while someone is
//! signed in there, the user worker ([`worker`]) as that person; and while the AI switch also
//! allows administrator rights and that person is an administrator, the admin worker with
//! their elevated token. It has no network code and accepts no requests.
//!
//! `DeskwayNet` runs as the virtual account `NT SERVICE\DeskwayNet` and hosts [`net`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::*;
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Threading::*;
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub mod agent;
pub mod files_in_use;
pub mod harden;
pub mod install;
pub mod ipc;
pub mod net;
pub mod watch;
pub mod worker;

use net::{AGENT_SERVICE, NET_SERVICE};

/// The machine-wide folder of the installed services. Taken from the system, never from
/// environment variables, which a user can set for their own elevated programs.
pub fn machine_dir() -> PathBuf {
    crate::platform::known_folder(crate::platform::KnownFolder::ProgramData).join("Deskway")
}

define_windows_service!(ffi_agent_main, agent_main);
define_windows_service!(ffi_net_main, net_main);

pub fn run_agent_service() -> Result<()> {
    service_dispatcher::start(AGENT_SERVICE, ffi_agent_main).context("not started by the service manager")
}

pub fn run_net_service() -> Result<()> {
    service_dispatcher::start(NET_SERVICE, ffi_net_main).context("not started by the service manager")
}

/// Sends this process's stderr to a log file, keeping it under 1 MB.
pub fn log_to(path: &Path) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        unsafe {
            let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(f.as_raw_handle()));
        }
        // Stays open for the life of the process.
        std::mem::forget(f);
    }
}

fn status(state: ServiceState, accept: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    }
}

fn net_main(_args: Vec<OsString>) {
    log_to(&crate::service::machine_dir().join("net.log"));
    // Stopped is reported before the process ends: a process that just ends is a crash to the
    // service manager, which its recovery actions answer with a restart.
    let (tx, rx) = channel::<()>();
    let done = tx.clone();
    let handler = move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            net::stop();
            let _ = tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        // From the installer: returns once the panels have quit (or a few seconds passed).
        ServiceControl::UserEvent(code) if code == net::CLOSE_PANELS => {
            net::close_panels();
            ServiceControlHandlerResult::NoError
        }
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(h) = service_control_handler::register(NET_SERVICE, handler) else { return };
    let _ = h.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN));
    // It runs until it fails or the service manager stops it; its threads end with the process.
    std::thread::spawn(move || {
        if let Err(e) = net::run(false, false) {
            eprintln!("network service stopped: {e:#}");
        }
        let _ = done.send(());
    });
    let _ = rx.recv();
    // Lets the host tell the viewer before the process ends.
    std::thread::sleep(Duration::from_millis(300));
    let _ = h.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
}

fn agent_main(_args: Vec<OsString>) {
    harden::privileged();
    let dir = crate::service::machine_dir().join("service");
    log_to(&dir.join("service.log"));
    let (tx, rx) = channel::<bool>();
    let handler = move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::SessionChange(_) => {
            let _ = tx.send(false);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(h) = service_control_handler::register(AGENT_SERVICE, handler) else { return };
    let accept = ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN | ServiceControlAccept::SESSION_CHANGE;
    let _ = h.set_service_status(status(ServiceState::Running, accept));
    if let Err(e) = supervise(&dir, &rx) {
        eprintln!("service stopped: {e:#}");
    }
    let _ = h.set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()));
}

/// The installed program, which only administrators can replace.
fn installed_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let expected = install::program_dir().join("deskway-host.exe");
    if !exe.eq(&expected) {
        bail!("refusing to start an agent from {}: only {} is trusted", exe.display(), expected.display());
    }
    Ok(exe)
}

fn supervise(dir: &Path, rx: &std::sync::mpsc::Receiver<bool>) -> Result<()> {
    let exe = installed_exe()?;
    install::secure_service_dir()?;
    // A fresh pipe key every start, so a leaked old key stops working.
    let _ = std::fs::remove_file(dir.join("ipc.key.new"));
    ipc::load_key(&dir.join("ipc.key.new"), true)?;
    std::fs::rename(dir.join("ipc.key.new"), dir.join("ipc.key"))?;
    let job = KillJob::new()?;
    let mut agent = Kept::new("agent");
    let mut worker = Kept::new("worker");
    let mut admin = Kept::new("admin worker");
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(true) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Ok(false) | Err(RecvTimeoutError::Timeout) => {}
        }
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        if session == u32::MAX {
            continue; // between sessions
        }
        agent.keep(session, || launch_agent(&exe, session, &job));
        worker.keep(session, || launch_worker(&exe, session, &job, worker::Kind::User));
        // The switch is the network service's to take; it keeps it beside the trusted devices.
        if crate::auth::admin_allowed(&machine_dir()) {
            admin.keep(session, || launch_worker(&exe, session, &job, worker::Kind::Admin));
        } else {
            admin.stop();
        }
    }
}

/// One process kept running in the console session: started again when it exits or the
/// console moves to another session, waiting longer after each failed start.
struct Kept {
    what: &'static str,
    child: Option<(Child, u32)>,
    backoff: Duration,
    next_try: Instant,
    /// The last failure, said once rather than at every try (nobody signed in, for the worker).
    failed: Option<String>,
}

impl Kept {
    fn new(what: &'static str) -> Kept {
        Kept { what, child: None, backoff: Duration::from_secs(1), next_try: Instant::now(), failed: None }
    }

    fn keep(&mut self, session: u32, launch: impl FnOnce() -> Result<Child>) {
        if let Some((child, s)) = &self.child {
            if *s == session && child.running() {
                return;
            }
            if child.running() {
                eprintln!("console moved to session {session}; restarting the {}", self.what);
            } else {
                eprintln!("{} exited", self.what);
            }
            child.kill();
            self.child = None;
        }
        if Instant::now() < self.next_try {
            return;
        }
        match launch() {
            Ok(child) => {
                eprintln!("{} started in session {session}", self.what);
                self.child = Some((child, session));
                self.backoff = Duration::from_secs(1);
                self.failed = None;
            }
            Err(e) => {
                let e = format!("{e:#}");
                if self.failed.as_ref() != Some(&e) {
                    eprintln!("cannot start the {} in session {session}: {e}", self.what);
                }
                self.failed = Some(e);
                self.next_try = Instant::now() + self.backoff;
                self.backoff = (self.backoff * 2).min(Duration::from_secs(60));
            }
        }
    }

    /// No longer wanted: ended, and started at once when it is wanted again.
    fn stop(&mut self) {
        if let Some((child, _)) = self.child.take() {
            eprintln!("{} stopped", self.what);
            child.kill();
        }
        (self.failed, self.backoff, self.next_try) = (None, Duration::from_secs(1), Instant::now());
    }
}

struct Child(HANDLE);

impl Child {
    fn running(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_TIMEOUT }
    }
    fn kill(&self) {
        unsafe {
            let _ = TerminateProcess(self.0, 0);
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Every agent goes into this job; if the service dies, the job closes and the agent with it.
/// What an AI client starts may leave it (`platform::start_program`): those are the person's.
struct KillJob(HANDLE);

impl KillJob {
    fn new() -> Result<KillJob> {
        unsafe {
            let h = CreateJobObjectW(None, None)?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as _, size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32)?;
            Ok(KillJob(h))
        }
    }
}

struct Owned(HANDLE);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Removes privileges the agent never needs. It stays SYSTEM, which is what lets it open the
/// secure desktops, but cannot create tokens, act as the OS or load drivers.
fn strip_privileges(token: HANDLE) {
    // SeImpersonate: files from the controller are written, and files sent to it are read, as
    // the signed-in user rather than as SYSTEM.
    let keep = ["SeChangeNotifyPrivilege", "SeImpersonatePrivilege"];
    unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenPrivileges, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token, TokenPrivileges, Some(buf.as_mut_ptr() as _), len, &mut len).is_err() {
            return;
        }
        let list = &*(buf.as_ptr() as *const TOKEN_PRIVILEGES);
        let items = std::slice::from_raw_parts(list.Privileges.as_ptr(), list.PrivilegeCount as usize);
        for p in items {
            let mut name = [0u16; 64];
            let mut n = name.len() as u32;
            let _ = LookupPrivilegeNameW(None, &p.Luid, Some(PWSTR(name.as_mut_ptr())), &mut n);
            let name = String::from_utf16_lossy(&name[..n as usize]);
            if keep.contains(&name.as_str()) {
                continue;
            }
            let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: p.Luid, Attributes: SE_PRIVILEGE_REMOVED }] };
            let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        }
    }
}

/// The person's administrator token: the elevated one linked to theirs under UAC, or theirs when
/// it is elevated already; an error for someone who is not an administrator.
unsafe fn admin_token(user: HANDLE) -> Result<Owned> {
    use windows::Win32::Security::{TokenElevation, TokenElevationType, TokenElevationTypeLimited, TokenLinkedToken, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE, TOKEN_LINKED_TOKEN};
    unsafe {
        let mut kind = TOKEN_ELEVATION_TYPE(0);
        let mut len = 0u32;
        GetTokenInformation(user, TokenElevationType, Some(&mut kind as *mut _ as _), size_of::<TOKEN_ELEVATION_TYPE>() as u32, &mut len)?;
        if kind == TokenElevationTypeLimited {
            let mut linked = TOKEN_LINKED_TOKEN::default();
            GetTokenInformation(user, TokenLinkedToken, Some(&mut linked as *mut _ as _), size_of::<TOKEN_LINKED_TOKEN>() as u32, &mut len).context("their administrator token")?;
            return Ok(Owned(linked.LinkedToken));
        }
        let mut e = TOKEN_ELEVATION::default();
        GetTokenInformation(user, TokenElevation, Some(&mut e as *mut _ as _), size_of::<TOKEN_ELEVATION>() as u32, &mut len)?;
        if e.TokenIsElevated == 0 {
            bail!("the person signed in is not an administrator");
        }
        let mut dup = HANDLE::default();
        DuplicateTokenEx(user, TOKEN_ALL_ACCESS, None, SecurityAnonymous, TokenPrimary, &mut dup)?;
        Ok(Owned(dup))
    }
}

/// A worker (`worker::Kind`) as the person signed in at `session` (none: an error, tried again
/// later), with that person's environment. Its key goes where the network service reads it and
/// to the worker on its standard input, the one handle it inherits.
fn launch_worker(exe: &Path, session: u32, job: &KillJob, kind: worker::Kind) -> Result<Child> {
    use windows::Win32::Storage::FileSystem::WriteFile;
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
    unsafe {
        let mut user = HANDLE::default();
        WTSQueryUserToken(session, &mut user).context("nobody is signed in")?;
        let user = match kind {
            worker::Kind::User => Owned(user),
            worker::Kind::Admin => {
                let person = Owned(user);
                admin_token(person.0)?
            }
        };
        let mut env = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut env, Some(user.0), false).context("their environment")?;

        let inherit = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle: true.into() };
        let (mut read, mut write) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut read, &mut write, Some(&inherit), 0)?;
        let (read, write) = (Owned(read), Owned(write));
        SetHandleInformation(write.0, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;

        // Only the pipe's read end is inherited, whatever else this process holds.
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut attr_buf = vec![0u8; size];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as _);
        InitializeProcThreadAttributeList(Some(attrs), 1, None, &mut size)?;
        let handles = [read.0];
        UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, Some(handles.as_ptr() as _), size_of::<HANDLE>(), None, None)?;

        let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.lpDesktop = PWSTR(desktop.as_mut_ptr());
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = read.0;
        si.lpAttributeList = attrs;
        let exe_str = exe.to_string_lossy().to_string();
        let mut cmd: Vec<u16> = format!("\"{exe_str}\" {}\0", kind.args()).encode_utf16().collect();
        let dir = HSTRING::from(exe.parent().unwrap_or(exe).as_os_str());
        let mut pi = PROCESS_INFORMATION::default();
        let r = CreateProcessAsUserW(
            Some(user.0),
            &HSTRING::from(exe_str.as_str()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(env),
            &dir,
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attrs);
        let _ = DestroyEnvironmentBlock(env);
        r.context("CreateProcessAsUser")?;
        let child = Child(pi.hProcess);
        let started = (|| -> Result<()> {
            AssignProcessToJobObject(job.0, pi.hProcess).with_context(|| format!("assign the {} to the job", kind.what()))?;
            // A new key for every worker: the one before it no longer connects.
            let key = wire::crypto::random::<32>();
            crate::config::write_private(&kind.key_path(), &key)?;
            WriteFile(write.0, Some(&key), None, None).context("hand the worker its key")?;
            Ok(())
        })();
        drop(write);
        if let Err(e) = started {
            child.kill();
            let _ = CloseHandle(pi.hThread);
            return Err(e);
        }
        ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);
        Ok(child)
    }
}

fn launch_agent(exe: &Path, session: u32, job: &KillJob) -> Result<Child> {
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY, &mut own)?;
        let own = Owned(own);
        let mut primary = HANDLE::default();
        DuplicateTokenEx(own.0, TOKEN_ALL_ACCESS, None, SecurityAnonymous, TokenPrimary, &mut primary)?;
        let primary = Owned(primary);
        // Moving a token to another session needs the TCB privilege, which this service has.
        SetTokenInformation(primary.0, TokenSessionId, &session as *const u32 as _, 4).context("set session")?;
        strip_privileges(primary.0);

        // The agent may not start other programs.
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut attr_buf = vec![0u8; size];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as _);
        InitializeProcThreadAttributeList(Some(attrs), 1, None, &mut size)?;
        let child_policy: u32 = 1; // PROCESS_CREATION_CHILD_PROCESS_RESTRICTED
        UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize, Some(&child_policy as *const u32 as _), 4, None, None)?;

        let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.lpDesktop = PWSTR(desktop.as_mut_ptr());
        si.lpAttributeList = attrs;
        let exe_str = exe.to_string_lossy().to_string();
        let mut cmd: Vec<u16> = format!("\"{exe_str}\" agent\0").encode_utf16().collect();
        let dir = HSTRING::from(exe.parent().unwrap_or(exe).as_os_str());
        let mut pi = PROCESS_INFORMATION::default();
        let r = CreateProcessAsUserW(
            Some(primary.0),
            &HSTRING::from(exe_str.as_str()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            None,
            &dir,
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attrs);
        r.context("CreateProcessAsUser")?;
        let child = Child(pi.hProcess);
        if let Err(e) = AssignProcessToJobObject(job.0, pi.hProcess) {
            child.kill();
            let _ = CloseHandle(pi.hThread);
            return Err(e).context("assign agent to job");
        }
        ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);
        Ok(child)
    }
}
