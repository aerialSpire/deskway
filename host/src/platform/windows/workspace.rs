
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HLOCAL, HWND, LPARAM, LRESULT, LocalFree, WPARAM};
use windows::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::platform::{Desk, WorkspaceState};
use crate::workspace::{self, Browser};

fn session_id() -> Option<u32> {
    static SESSION: OnceLock<Option<u32>> = OnceLock::new();
    *SESSION.get_or_init(|| {
        let mut id = 0;
        unsafe { ProcessIdToSessionId(std::process::id(), &mut id) }.ok().map(|()| id)
    })
}

fn ordinary() -> bool {
    static ORDINARY: OnceLock<bool> = OnceLock::new();
    *ORDINARY.get_or_init(|| matches!(super::programs::token_elevation(), Ok(false))
        && !crate::service::ipc::running_as_system())
}

fn session_info(id: u32) -> Option<WTSINFOEX_LEVEL1_W> {
    let mut buf = PWSTR::null();
    let mut len = 0;
    unsafe {
        WTSQuerySessionInformationW(None, id, WTSSessionInfoEx, &mut buf, &mut len).ok()?;
        let info = if len as usize >= size_of::<WTSINFOEXW>() && !buf.is_null() {
            let info = buf.0.cast::<WTSINFOEXW>().read_unaligned();
            (info.Level == 1).then(|| info.Data.WTSInfoExLevel1)
        } else { None };
        WTSFreeMemory(buf.0.cast());
        info
    }
}

fn console_user(id: u32) -> Option<String> {
    let read = |field| {
        let mut buf = PWSTR::null();
        let mut len = 0;
        unsafe {
            WTSQuerySessionInformationW(None, id, field, &mut buf, &mut len).ok()?;
            let result = (!buf.is_null()).then(|| buf.to_string().ok()).flatten();
            WTSFreeMemory(buf.0.cast());
            result
        }
    };
    let name = HSTRING::from(format!("{}\\{}", read(WTSDomainName)?, read(WTSUserName)?));
    unsafe {
        let mut sid_len = 0;
        let mut domain_len = 0;
        let mut use_kind = SID_NAME_USE::default();
        let _ = LookupAccountNameW(PCWSTR::null(), &name, None, &mut sid_len, None, &mut domain_len, &mut use_kind);
        if sid_len == 0 { return None; }
        let mut sid = vec![0u32; (sid_len as usize).div_ceil(4)];
        let mut domain = vec![0u16; domain_len as usize];
        let sid = PSID(sid.as_mut_ptr().cast());
        LookupAccountNameW(PCWSTR::null(), &name, Some(sid), &mut sid_len,
            Some(PWSTR(domain.as_mut_ptr())), &mut domain_len, &mut use_kind).ok()?;
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(sid, &mut text).ok()?;
        let value = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        value
    }
}

pub fn current() -> WorkspaceState {
    let Some(id) = session_id() else { return WorkspaceState::NO_USER; };
    if !ordinary() || id != unsafe { WTSGetActiveConsoleSessionId() } {
        return WorkspaceState::NO_USER;
    }
    static LOGON: OnceLock<Option<u64>> = OnceLock::new();
    let Some(info) = session_info(id) else {
        // Fail closed without throwing away a shell for a failed status query.
        return WorkspaceState { user: LOGON.get().copied().flatten(), locked: true };
    };
    if info.UserName[0] == 0 || info.LogonTime <= 0 {
        return WorkspaceState::NO_USER;
    }
    let logon = info.LogonTime as u64;
    if *LOGON.get_or_init(|| {
        let own = crate::service::ipc::own_sid().ok()?;
        (console_user(id)? == own).then_some(logon)
    }) != Some(logon) {
        return WorkspaceState::NO_USER;
    }
    WorkspaceState {
        user: Some(logon),
        locked: info.SessionFlags != WTS_SESSIONSTATE_UNLOCK as i32 || super::desktop::input() != Desk::Default,
    }
}

pub fn sweep_home() {
    if current().refusal(false).is_none() {
        workspace::sweep_home(&|| current().refusal(false).is_none());
    }
}

pub fn browser(test_lock: Option<PathBuf>) -> Browser {
    let state = Arc::new(move || {
        let mut state = current();
        if test_lock.as_ref().is_some_and(|path| std::fs::read_to_string(path).is_ok_and(|s| s.trim() == "locked")) {
            state.locked = true;
        }
        state
    });
    workspace::local_browser(Arc::new(super::programs::open_pty), state)
}

thread_local! {
    static CHANGED: std::cell::RefCell<Option<Box<dyn FnMut(WorkspaceState)>>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TIMER || msg == WM_WTSSESSION_CHANGE {
        let mut state = current();
        if msg == WM_WTSSESSION_CHANGE && session_id() == Some(lparam.0 as u32) {
            match wparam.0 as u32 {
                WTS_SESSION_LOCK => state.locked = true,
                WTS_SESSION_LOGOFF => state = WorkspaceState::NO_USER,
                _ => {}
            }
        }
        CHANGED.with(|changed| {
            if let Some(changed) = changed.borrow_mut().as_mut() {
                changed(state);
            }
        });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub struct WorkspaceWatch(u32);

impl WorkspaceWatch {
    /// WTS covers lock and sign-in transitions. The input desktop is also sampled for secure
    /// prompts, which do not generate a WTS lock event.
    pub fn start(mut changed: impl FnMut(WorkspaceState) + Send + 'static) -> std::io::Result<Self> {
        let (ready, thread) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("workspace-watch".into()).spawn(move || unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let class = WNDCLASSW {
                lpfnWndProc: Some(proc), hInstance: instance.into(),
                lpszClassName: w!("DeskwayWorkspaceWatch"), ..Default::default()
            };
            RegisterClassW(&class);
            let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("DeskwayWorkspaceWatch"), w!(""),
                WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance.into()), None) else { return; };
            let registered = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok();
            SetTimer(Some(hwnd), 1, 100, None);
            let mut previous = current();
            changed(previous);
            CHANGED.with(|slot| *slot.borrow_mut() = Some(Box::new(move |state| {
                if state != previous {
                    previous = state;
                    changed(state);
                }
            })));
            let _ = ready.send(GetCurrentThreadId());
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                DispatchMessageW(&msg);
            }
            if registered { let _ = WTSUnRegisterSessionNotification(hwnd); }
            let _ = DestroyWindow(hwnd);
        })?;
        thread.recv().map(Self).map_err(std::io::Error::other)
    }
}

impl Drop for WorkspaceWatch {
    fn drop(&mut self) {
        let _ = unsafe { PostThreadMessageW(self.0, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}
