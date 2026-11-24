//! Installing the services. Runs elevated, once, with the person's approval (UAC).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::*;
use windows::Win32::Security::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Services::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Shell::*;
use windows_service::service::*;
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use crate::i18n::t;
use super::net::{AGENT_SERVICE, NET_SERVICE};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn program_dir() -> PathBuf {
    crate::platform::known_folder(crate::platform::KnownFolder::ProgramFiles).join("Deskway")
}

pub fn is_elevated() -> bool {
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

/// A process started by `elevate_start`, kept as an integer so it can cross threads.
pub struct Elevated(isize);

/// The elevated copy answers in the same language as the window that asked.
fn elevated_args(args: &str) -> HSTRING {
    HSTRING::from(format!("{args} --lang {}", crate::i18n::current().code()))
}

/// Starts this program again as administrator: returns once the person answered the UAC
/// prompt. "cancelled" when they said no.
pub fn elevate_start(args: &str) -> Result<Elevated> {
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = elevated_args(args);
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE,
            lpVerb: windows::core::w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: 0,
            ..Default::default()
        };
        if let Err(e) = ShellExecuteExW(&mut info) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                bail!("cancelled (1223)");
            }
            return Err(e.into());
        }
        Ok(Elevated(info.hProcess.0 as isize))
    }
}

/// Waits for an elevated process and returns its exit code.
pub fn wait_exit(p: Elevated) -> u32 {
    unsafe {
        let h = HANDLE(p.0 as _);
        WaitForSingleObject(h, INFINITE);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(h, &mut code);
        let _ = CloseHandle(h);
        code
    }
}

/// Starts this program again as administrator (the UAC prompt) and waits for it.
pub fn elevate(args: &str) -> Result<u32> {
    let exe = HSTRING::from(std::env::current_exe()?.as_os_str());
    let params = elevated_args(args);
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NO_CONSOLE,
            lpVerb: windows::core::w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: 0,
            ..Default::default()
        };
        ShellExecuteExW(&mut info).with_context(|| t("err.no_admin"))?;
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 0u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        Ok(code)
    }
}

fn sid_of(account: &str) -> Result<String> {
    unsafe {
        let mut sid = vec![0u8; 128];
        let mut sid_len = sid.len() as u32;
        let mut domain = vec![0u16; 128];
        let mut domain_len = domain.len() as u32;
        let mut kind = SID_NAME_USE::default();
        LookupAccountNameW(None, &HSTRING::from(account), Some(PSID(sid.as_mut_ptr() as _)), &mut sid_len, Some(PWSTR(domain.as_mut_ptr())), &mut domain_len, &mut kind)
            .with_context(|| format!("look up {account}"))?;
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(PSID(sid.as_mut_ptr() as _), &mut s)?;
        let out = s.to_string()?;
        let _ = LocalFree(Some(HLOCAL(s.0 as _)));
        Ok(out)
    }
}

/// The SID that owns a file or folder.
pub fn owner_of(path: &Path) -> Result<String> {
    unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let r = GetNamedSecurityInfoW(&HSTRING::from(path.as_os_str()), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, &mut sd);
        if r != ERROR_SUCCESS {
            bail!("read owner of {}: {r:?}", path.display());
        }
        let mut s = PWSTR::null();
        let c = ConvertSidToStringSidW(owner, &mut s);
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        c?;
        let out = s.to_string()?;
        let _ = LocalFree(Some(HLOCAL(s.0 as _)));
        Ok(out)
    }
}

const ADMINS: &str = "S-1-5-32-544";
const SYSTEM: &str = "S-1-5-18";

/// A folder only SYSTEM or administrators could have made. Anything else found at that path
/// (for example made in advance by a normal user, with files they still own inside) is moved
/// aside, never adopted.
fn fresh_dir(path: &Path, sddl: &str) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        let owner = owner_of(path)?;
        let is_link = std::fs::symlink_metadata(path)?.file_type().is_symlink();
        if is_link || (owner != ADMINS && owner != SYSTEM) {
            let aside = path.with_extension(format!("untrusted-{}", std::process::id()));
            eprintln!("{} belongs to {owner}; moving it to {}", path.display(), aside.display());
            std::fs::rename(path, &aside).with_context(|| format!("move aside {}", path.display()))?;
        }
    }
    if std::fs::symlink_metadata(path).is_err() {
        // Created with its final permissions, so there is no moment when others could write.
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None)?;
            let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
            let r = windows::Win32::Storage::FileSystem::CreateDirectoryW(&HSTRING::from(path.as_os_str()), Some(&sa));
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            r.with_context(|| format!("create {}", path.display()))?;
        }
    }
    protect(path, sddl)
}

/// Replaces a folder's owner and permissions, dropping anything inherited, and pushes the new
/// permissions down to everything inside.
fn protect(path: &Path, sddl: &str) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        bail!("{} is a link, refusing to use it", path.display());
    }
    unsafe {
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut sd, None)?;
        let (mut present, mut defaulted) = (windows::core::BOOL(0), windows::core::BOOL(0));
        let mut dacl: *mut ACL = std::ptr::null_mut();
        GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)?;
        let mut owner = PSID::default();
        GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted)?;
        let r = SetNamedSecurityInfoW(
            &HSTRING::from(path.as_os_str()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            Some(owner),
            None,
            Some(dacl),
            None,
        );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        if r != ERROR_SUCCESS {
            bail!("set permissions on {}: {:?}", path.display(), r);
        }
    }
    Ok(())
}

/// Data folder: SYSTEM and administrators full control, the network service may change files,
/// everyone else nothing.
fn data_sddl() -> Result<String> {
    let net = sid_of(&format!(r"NT SERVICE\{NET_SERVICE}"))?;
    Ok(format!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;{net})"))
}

fn service_sddl() -> Result<String> {
    let net = sid_of(&format!(r"NT SERVICE\{NET_SERVICE}"))?;
    Ok(format!("O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;{net})"))
}

/// The service's own folder: the network service may only read it, so it cannot plant a link
/// where the SYSTEM service writes.
/// Checked by the SYSTEM service at every start: it writes only into this folder, so the folder
/// must be one only SYSTEM or administrators made.
pub fn secure_service_dir() -> Result<()> {
    let dir = crate::service::machine_dir();
    for d in [dir.clone(), dir.join("service")] {
        let owner = owner_of(&d)?;
        if (owner != ADMINS && owner != SYSTEM) || std::fs::symlink_metadata(&d)?.file_type().is_symlink() {
            bail!("{} is owned by {owner}; reinstall {}", d.display(), crate::product::NAME);
        }
    }
    protect(&dir.join("service"), &service_sddl()?)
}

fn set_sid_and_privileges(svc: &windows_service::service::Service, restricted: bool, privileges: &[&str]) -> Result<()> {
    unsafe {
        let h = SC_HANDLE(svc.raw_handle() as _);
        let sid = SERVICE_SID_INFO { dwServiceSidType: if restricted { 3 } else { SERVICE_SID_TYPE_UNRESTRICTED } };
        ChangeServiceConfig2W(h, SERVICE_CONFIG_SERVICE_SID_INFO, Some(&sid as *const _ as _)).context("service SID type")?;
        // A double-NUL-terminated list. Everything else is stripped when the service starts.
        let mut list: Vec<u16> = privileges.iter().flat_map(|p| p.encode_utf16().chain([0])).collect();
        list.push(0);
        let info = SERVICE_REQUIRED_PRIVILEGES_INFOW { pmszRequiredPrivileges: PWSTR(list.as_mut_ptr()) };
        ChangeServiceConfig2W(h, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, Some(&info as *const _ as _)).context("service privileges")?;
    }
    Ok(())
}

fn create_or_update(m: &ServiceManager, info: &ServiceInfo, description: &str) -> Result<windows_service::service::Service> {
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG | ServiceAccess::DELETE;
    let svc = match m.open_service(&info.name, access) {
        Ok(s) => {
            s.change_config(info)?;
            s
        }
        Err(_) => m.create_service(info, access)?,
    };
    svc.set_description(description)?;
    svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) }; 3]),
    })?;
    Ok(svc)
}

/// Stops a service and waits until it has. Its recovery actions go first: a version that ends
/// without reporting Stopped looks crashed, and the service manager would start it again 5 s
/// later, in the middle of an install or an uninstall. Installing sets them again.
fn stop_and_wait(m: &ServiceManager, name: &str) {
    if let Ok(s) = m.open_service(name, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG) {
        let _ = s.update_failure_actions(ServiceFailureActions { reset_period: ServiceFailureResetPeriod::Never, reboot_msg: None, command: None, actions: Some(Vec::new()) });
        let _ = s.stop();
        for _ in 0..50 {
            if s.query_status().map(|st| st.current_state == ServiceState::Stopped).unwrap_or(true) {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Deskway";

fn set_string(key: &str, name: &str, value: &str) -> Result<()> {
    let data: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let r = unsafe { RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(name), REG_SZ.0, Some(data.as_ptr() as _), (data.len() * 2) as u32) };
    if r != ERROR_SUCCESS {
        bail!("registry {name}: {r:?}");
    }
    Ok(())
}

fn set_dword(key: &str, name: &str, value: u32) -> Result<()> {
    let r = unsafe { RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(key), &HSTRING::from(name), REG_DWORD.0, Some(&value as *const u32 as _), 4) };
    if r != ERROR_SUCCESS {
        bail!("registry {name}: {r:?}");
    }
    Ok(())
}

/// The entry in Settings → Apps, so Deskway is removed like any other program.
fn register_uninstall(exe: &Path) -> Result<()> {
    let quoted = format!("\"{}\"", exe.display());
    set_string(UNINSTALL_KEY, "DisplayName", crate::product::NAME)?;
    set_string(UNINSTALL_KEY, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    set_string(UNINSTALL_KEY, "Publisher", "Deskway")?;
    set_string(UNINSTALL_KEY, "DisplayIcon", &format!("{quoted},0"))?;
    set_string(UNINSTALL_KEY, "InstallLocation", &program_dir().display().to_string())?;
    // Opens the uninstall window, which asks for administrator rights itself.
    set_string(UNINSTALL_KEY, "UninstallString", &format!("{quoted} uninstall"))?;
    set_dword(UNINSTALL_KEY, "NoModify", 1)?;
    set_dword(UNINSTALL_KEY, "NoRepair", 1)?;
    let size_kb = std::fs::metadata(exe).map(|m| (m.len() / 1024) as u32).unwrap_or(0);
    set_dword(UNINSTALL_KEY, "EstimatedSize", size_kb)?;
    Ok(())
}

/// The Start menu entry for everyone on this computer, so the program can be found by searching.
fn start_menu_link() -> PathBuf {
    start_menu_dir().join(format!("{}.lnk", crate::product::NAME))
}

fn start_menu_dir() -> PathBuf {
    let base = unsafe {
        SHGetKnownFolderPath(&FOLDERID_CommonPrograms, KNOWN_FOLDER_FLAG(0), None)
            .ok()
            .map(|p| {
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
                PathBuf::from(s)
            })
            .unwrap_or_else(|| crate::platform::known_folder(crate::platform::KnownFolder::ProgramData).join(r"Microsoft\Windows\Start Menu\Programs"))
    };
    base
}

fn create_start_menu_link(exe: &Path) -> Result<()> {
    use windows::Win32::System::Com::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetWorkingDirectory(&HSTRING::from(exe.parent().unwrap_or(exe).as_os_str()))?;
        link.SetDescription(&HSTRING::from(t("install.shortcut_desc")))?;
        link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
        let file: IPersistFile = windows::core::Interface::cast(&link)?;
        let target = start_menu_link();
        let _ = std::fs::remove_file(&target);
        // The entry under the program's earlier name, before it was Deskway Host.
        let _ = std::fs::remove_file(start_menu_dir().join("Deskway.lnk"));
        file.Save(&HSTRING::from(target.as_os_str()), true)?;
    }
    Ok(())
}

fn set_run(value: Option<&str>) -> Result<()> {
    unsafe {
        let r = match value {
            Some(v) => {
                let data: Vec<u16> = v.encode_utf16().chain([0]).collect();
                RegSetKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(RUN_KEY), &HSTRING::from("Deskway"), REG_SZ.0, Some(data.as_ptr() as _), (data.len() * 2) as u32)
            }
            None => RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(RUN_KEY), &HSTRING::from("Deskway")),
        };
        if r != ERROR_SUCCESS && !(value.is_none() && r == ERROR_FILE_NOT_FOUND) {
            bail!("autostart entry: {r:?}");
        }
    }
    Ok(())
}

/// Whether the services start with Windows. None when they are not installed or unreadable.
/// Reading needs no administrator.
pub fn autostart() -> Option<bool> {
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
    let s = m.open_service(NET_SERVICE, ServiceAccess::QUERY_CONFIG).ok()?;
    Some(s.query_config().ok()?.start_type == ServiceStartType::AutoStart)
}

/// Starts with Windows, or only when opened. Only the start type changes. Needs the right to
/// change the services, which the SYSTEM agent and administrators have.
pub fn set_autostart(on: bool) -> Result<()> {
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)?;
        let r = (|| -> Result<()> {
            for name in [NET_SERVICE, AGENT_SERVICE] {
                let s = OpenServiceW(scm, &HSTRING::from(name), SERVICE_CHANGE_CONFIG)?;
                let start = if on { SERVICE_AUTO_START } else { SERVICE_DEMAND_START };
                let r = ChangeServiceConfigW(s, ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE), start, SERVICE_ERROR(SERVICE_NO_CHANGE), PCWSTR::null(), PCWSTR::null(), None, PCWSTR::null(), PCWSTR::null(), PCWSTR::null(), PCWSTR::null());
                let _ = CloseServiceHandle(s);
                r.with_context(|| format!("change {name}"))?;
            }
            Ok(())
        })();
        let _ = CloseServiceHandle(scm);
        r
    }
}

/// Opened by hand after automatic start was turned off: it starts with Windows again, and
/// runs now. Administrator.
pub fn start_services() -> Result<()> {
    set_autostart(true)?;
    run_services(&ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?)
}

/// Starts both services, the network one first, unless one already runs.
fn run_services(m: &ServiceManager) -> Result<()> {
    for name in [NET_SERVICE, AGENT_SERVICE] {
        let s = m.open_service(name, ServiceAccess::QUERY_STATUS | ServiceAccess::START)?;
        if s.query_status()?.current_state != ServiceState::Running {
            s.start(&[] as &[&str]).with_context(|| format!("start {name}"))?;
        }
    }
    Ok(())
}

/// Asks every signed-in user's panel to quit, through the network service that each is
/// connected to. Returns once they have (or after a few seconds); a service that is not
/// running has no panels to ask.
fn close_panels(m: &ServiceManager) {
    if let Ok(s) = m.open_service(NET_SERVICE, ServiceAccess::QUERY_STATUS | ServiceAccess::USER_DEFINED_CONTROL) {
        if s.query_status().is_ok_and(|st| st.current_state == ServiceState::Running) {
            if let Err(e) = s.notify(super::net::CLOSE_PANELS) {
                eprintln!("panels not asked to quit: {e}");
            }
        }
    }
}

/// Closes whatever still holds `files` (a panel that did not quit, an older version) with
/// Restart Manager. Spared: this process and `keep`, the installer window that started it.
fn release(files: &[PathBuf], keep: Option<u32>) {
    let spare: Vec<u32> = [Some(std::process::id()), keep].into_iter().flatten().collect();
    match super::files_in_use::close_holders(files, &spare) {
        Ok(0) => {}
        Ok(n) => eprintln!("closed {n} programs using Deskway's files"),
        Err(e) => eprintln!("cannot close the programs using Deskway's files: {e:#}"),
    }
}

/// `lang`, when given, is saved as the installed host's language (the installer's choice).
/// `keep` is the installer window that asked: it stays open.
pub fn install(keep: Option<u32>, lang: Option<String>) -> Result<()> {
    if !is_elevated() {
        bail!(t("err.need_admin"));
    }
    let src = std::env::current_exe()?;
    let dir = program_dir();
    let exe = dir.join("deskway-host.exe");
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    // The panels first, while the service they listen to still runs.
    close_panels(&m);
    stop_and_wait(&m, AGENT_SERVICE);
    stop_and_wait(&m, NET_SERVICE);
    std::fs::create_dir_all(&dir)?;
    if src != exe {
        release(&[exe.clone()], keep);
        std::fs::copy(&src, &exe).context("copy program")?;
    }

    let data = crate::service::machine_dir();

    let net_info = ServiceInfo {
        name: OsString::from(NET_SERVICE),
        // Service names and descriptions stay English whatever the language: administrators
        // and tools read them, often far from this computer's own settings.
        display_name: OsString::from("Deskway Network"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec![OsString::from("net")],
        dependencies: vec![],
        account_name: Some(OsString::from(format!(r"NT SERVICE\{NET_SERVICE}"))),
        account_password: None,
    };
    let net = create_or_update(&m, &net_info, "The networking part of Deskway, running under a low-privilege account.")?;
    // Tested on Windows 11: SChannel (TLS, and DTLS for WebRTC) fails with SEC_E_NO_CREDENTIALS
    // under a write-restricted SID or without SeImpersonatePrivilege. Everything else is stripped.
    set_sid_and_privileges(&net, false, &["SeChangeNotifyPrivilege", "SeCreateGlobalPrivilege", "SeImpersonatePrivilege"])?;

    let agent_info = ServiceInfo {
        name: OsString::from(AGENT_SERVICE),
        display_name: OsString::from("Deskway Agent"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe.clone(),
        launch_arguments: vec![OsString::from("service")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let agent = create_or_update(&m, &agent_info, "Lets Deskway control the lock screen and sign-in screen too. Does not use the network.")?;
    set_sid_and_privileges(&agent, false, &["SeTcbPrivilege", "SeAssignPrimaryTokenPrivilege", "SeIncreaseQuotaPrivilege", "SeChangeNotifyPrivilege"])?;

    // Secured before anything is written into them.
    fresh_dir(&data, &data_sddl()?)?;
    let service_dir = data.join("service");
    let _ = std::fs::remove_dir_all(&service_dir);
    fresh_dir(&service_dir, &service_sddl()?)?;
    // Signing in is the panel's (`sign_in`): installing only installs. An update keeps the
    // service's own data, which only administrators and the service can change.
    let path = data.join("host.json");
    if let Some(lang) = lang.filter(|l| crate::i18n::Lang::parse(l).is_some() && path.exists()) {
        let mut cfg: crate::config::Config = serde_json::from_slice(&std::fs::read(&path)?)?;
        cfg.lang = Some(lang);
        crate::config::write_private(&path, &serde_json::to_vec_pretty(&cfg)?)?;
    }
    // Settings left in the user's profile would be confusing: the service no longer reads them.
    set_run(Some(&format!("\"{}\" --at-login", exe.display())))?;
    register_uninstall(&exe)?;
    if let Err(e) = create_start_menu_link(&exe) {
        eprintln!("Start menu entry not created: {e:#}");
    }

    drop((net, agent));
    run_services(&m)
}

/// Signs the installed service in with the sign-in a person made in their panel, kept in their
/// own folder (`from`): it is settled in the service's folder, which the service is waiting on
/// (`Config::settle_in`: the computer signed out there comes back, or a new one starts clean).
/// Administrator.
pub fn sign_in(from: &std::path::Path) -> Result<()> {
    if !is_elevated() {
        bail!(t("err.need_admin"));
    }
    let new = crate::config::Config::load_from(from).context("read the sign-in")?;
    crate::config::Config::settle_in(&crate::service::machine_dir(), new).map(drop)
}

/// Signs the installed service out: the services start again, waiting for a sign-in. The
/// server and the service's folder keep this computer for the same account to take back
/// (`Config::sign_out`). Administrator.
pub fn sign_out() -> Result<()> {
    if !is_elevated() {
        bail!(t("err.need_admin"));
    }
    crate::config::use_dir(crate::service::machine_dir());
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    stop_and_wait(&m, AGENT_SERVICE);
    stop_and_wait(&m, NET_SERVICE);
    crate::config::Config::sign_out()?;
    run_services(&m)
}

/// Removes the program folder. Whatever else still uses it is closed first; what cannot go
/// now (this uninstaller's own program, and the window that started it) goes at the next
/// restart.
fn remove_program(keep: Option<u32>) {
    let dir = program_dir();
    if !dir.ends_with("Deskway") {
        return;
    }
    release(&super::files_in_use::files_under(&dir), keep);
    super::files_in_use::remove_tree(&dir, &mut |p| super::files_in_use::delete_at_restart(p));
}

/// `keep` is the uninstaller window that asked: it stays open to say it is done.
pub fn uninstall(purge: bool, keep: Option<u32>) -> Result<()> {
    if !is_elevated() {
        bail!(t("err.need_admin"));
    }
    let m = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    // The panels first, while the service they listen to still runs.
    close_panels(&m);
    for name in [AGENT_SERVICE, NET_SERVICE] {
        stop_and_wait(&m, name);
        if let Ok(s) = m.open_service(name, ServiceAccess::DELETE) {
            s.delete()?;
        }
    }
    set_run(None)?;
    unsafe {
        let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(UNINSTALL_KEY));
    }
    let _ = std::fs::remove_file(start_menu_link());
    remove_program(keep);
    let data = crate::service::machine_dir();
    let _ = std::fs::remove_dir_all(data.join("service"));
    if purge {
        std::fs::remove_dir_all(&data).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })?;
    }
    Ok(())
}
