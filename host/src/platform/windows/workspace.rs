
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HLOCAL, HWND, LPARAM, LRESULT, LocalFree, WPARAM};
use windows::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::*;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::platform::{Desk, WorkspaceState};
use crate::workspace::{self, Browser, BrowserState};

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
    console_state(true)
}

fn console_state(check_desktop: bool) -> WorkspaceState {
    let Some(id) = session_id() else { return WorkspaceState::NO_USER; };
    if !ordinary() {
        return WorkspaceState::NO_USER;
    }
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    if console != id && console != u32::MAX {
        return WorkspaceState::NO_USER;
    }
    static LOGON: OnceLock<u64> = OnceLock::new();
    let info = session_info(id);
    if LOGON.get().is_none() {
        if let Some(info) = info.as_ref().filter(|info| info.UserName[0] != 0 && info.LogonTime > 0) {
            if crate::service::ipc::own_sid().ok().is_some_and(|own| console_user(id).as_ref() == Some(&own)) {
                let _ = LOGON.set(info.LogonTime as u64);
            }
        }
    }
    classify(id, console, LOGON.get().copied(), info.as_ref().map(|info| {
        let logon = (info.UserName[0] != 0 && info.LogonTime > 0).then_some(info.LogonTime as u64);
        let unlocked = console == id && info.SessionFlags == WTS_SESSIONSTATE_UNLOCK as i32
            && (!check_desktop || super::desktop::input() == Desk::Default);
        (logon, unlocked)
    }))
}

fn classify(id: u32, console: u32, user: Option<u64>, status: Option<(Option<u64>, bool)>) -> WorkspaceState {
    if (console != id && console != u32::MAX) || user.is_none() {
        return WorkspaceState::NO_USER;
    }
    match status {
        Some((logon, _)) if logon != user => WorkspaceState::NO_USER,
        _ => WorkspaceState { user, locked: console != id || !status.is_some_and(|(_, unlocked)| unlocked) },
    }
}

pub fn sweep_home() {
    if current().refusal(false).is_none() {
        workspace::sweep_home(&|| current().refusal(false).is_none());
    }
}

pub fn browser(test_lock: Option<PathBuf>, command: Option<String>) -> Browser {
    let line = command.unwrap_or_else(super::programs::browser_command);
    let state = Arc::new(move || {
        let mut state = current();
        if test_lock.as_ref().is_some_and(|path| std::fs::read_to_string(path).is_ok_and(|s| s.trim() == "locked")) {
            state.locked = true;
        }
        state
    });
    let current = state.clone();
    let open = Arc::new(move |rows, cols| {
        let state = current.clone();
        let user = state().user;
        super::programs::open_pty(&line, rows, cols, move || {
            let now = state();
            now.user == user && now.refusal(false).is_none()
        })
    });
    workspace::local_browser(open, state)
}

thread_local! {
    static CHANGED: std::cell::RefCell<Option<Box<dyn FnMut(WatchEvent)>>> = const { std::cell::RefCell::new(None) };
}

const WM_WORKSPACE_USE: u32 = WM_APP;
const DESKTOP_TIMER: usize = 1;

enum WatchEvent {
    Timer,
    Session(Option<u32>),
    Activity(bool),
}

fn check(event: WatchEvent) {
    CHANGED.with(|slot| {
        if let Some(changed) = slot.borrow_mut().as_mut() { changed(event); }
    });
}

/// The user worker is in use while answering a request or holding an open browser resource.
#[derive(Default)]
pub struct WorkspaceActivity {
    state: Mutex<UseState>,
}

#[derive(Default)]
struct UseState {
    count: usize,
    changed: Option<Box<dyn Fn(bool) + Send>>,
}

impl UseState {
    fn set_count(&mut self, count: usize) {
        let was_in_use = self.count != 0;
        self.count = count;
        let in_use = count != 0;
        if was_in_use != in_use {
            if let Some(changed) = &self.changed { changed(in_use); }
        }
    }
}

impl WorkspaceActivity {
    pub fn begin(self: &Arc<Self>) -> WorkspaceUse {
        let mut state = self.state.lock().unwrap();
        let count = state.count + 1;
        state.set_count(count);
        WorkspaceUse(self.clone())
    }

    pub(crate) fn watch(&self, changed: impl Fn(bool) + Send + 'static) -> bool {
        let mut state = self.state.lock().unwrap();
        state.changed = Some(Box::new(changed));
        state.count != 0
    }
}

pub struct WorkspaceUse(Arc<WorkspaceActivity>);

impl Drop for WorkspaceUse {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        let count = state.count - 1;
        state.set_count(count);
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == DESKTOP_TIMER => check(WatchEvent::Timer),
        WM_WTSSESSION_CHANGE => {
            check(WatchEvent::Session((session_id() == Some(lparam.0 as u32)).then_some(wparam.0 as u32)));
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

pub struct WorkspaceWatch {
    thread: u32,
    activity: Option<Arc<WorkspaceActivity>>,
}

impl WorkspaceWatch {
    /// WTS covers lock and sign-in transitions. The input desktop is also sampled for secure
    /// prompts, which do not generate a WTS lock event.
    pub fn start(state: Arc<BrowserState>, changed: impl FnMut(WorkspaceState) + Send + 'static) -> std::io::Result<Self> {
        Self::watch(move |_| state(), None, changed)
    }

    pub fn for_worker(activity: Arc<WorkspaceActivity>, changed: impl FnMut(WorkspaceState) + Send + 'static) -> std::io::Result<Self> {
        Self::watch(console_state, Some(activity), changed)
    }

    fn watch(
        state: impl Fn(bool) -> WorkspaceState + Send + 'static,
        activity: Option<Arc<WorkspaceActivity>>,
        mut changed: impl FnMut(WorkspaceState) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (ready, thread) = std::sync::mpsc::channel();
        let watched = activity.clone();
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
            let thread = GetCurrentThreadId();
            // Without WTS notifications, poll continuously for lock and sign-in changes.
            let initially_in_use = !registered || watched.is_none_or(|activity| activity.watch(move |in_use| {
                let _ = PostThreadMessageW(thread, WM_WORKSPACE_USE, WPARAM(usize::from(in_use)), LPARAM(0));
            }));
            let mut in_use = false;
            let mut previous = None;
            CHANGED.with(|slot| *slot.borrow_mut() = Some(Box::new(move |event| {
                let session = match event {
                    WatchEvent::Timer if !in_use => return,
                    WatchEvent::Timer => None,
                    WatchEvent::Session(event) => event,
                    WatchEvent::Activity(active) => {
                        if in_use != active {
                            in_use = active;
                            if active {
                                SetTimer(Some(hwnd), DESKTOP_TIMER, 100, None);
                            } else {
                                let _ = KillTimer(Some(hwnd), DESKTOP_TIMER);
                            }
                        }
                        None
                    }
                };
                // Idle availability follows WTS. A secure prompt can close without a WTS event.
                let mut next = state(in_use);
                match session {
                    Some(WTS_SESSION_LOCK) => next.locked = true,
                    Some(WTS_SESSION_LOGOFF) => next = WorkspaceState::NO_USER,
                    _ => {}
                }
                if Some(next) != previous {
                    previous = Some(next);
                    changed(next);
                }
            })));
            check(WatchEvent::Activity(initially_in_use));
            let _ = ready.send(thread);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if msg.message == WM_WORKSPACE_USE {
                    check(WatchEvent::Activity(msg.wParam.0 != 0));
                } else {
                    DispatchMessageW(&msg);
                }
            }
            if registered { let _ = WTSUnRegisterSessionNotification(hwnd); }
            let _ = DestroyWindow(hwnd);
        })?;
        thread.recv().map(|thread| Self { thread, activity }).map_err(std::io::Error::other)
    }
}

#[cfg(test)]
#[test]
fn desktop_timer_is_wanted_while_workspace_is_in_use() {
    let activity = Arc::new(WorkspaceActivity::default());
    let (tx, changes) = std::sync::mpsc::channel();
    assert!(!activity.watch(move |wanted| tx.send(wanted).unwrap()));
    let mut uses = [None, None, None];
    for (slot, active, wanted) in [
        (0, true, Some(true)),
        (1, true, None),
        (0, false, None),
        (2, true, None),
        (1, false, None),
        (2, false, Some(false)),
        (0, true, Some(true)),
        (0, false, Some(false)),
    ] {
        uses[slot] = active.then(|| activity.begin());
        assert_eq!(changes.try_recv().ok(), wanted, "slot={slot}, active={active}");
        assert!(changes.try_recv().is_err());
    }
}

#[cfg(test)]
#[test]
fn a_detached_console_preserves_only_a_live_sign_in() {
    let locked = WorkspaceState { user: Some(10), locked: true };
    for (console, info, expected) in [
        (1, Some((Some(10), true)), WorkspaceState { user: Some(10), locked: false }),
        (1, Some((Some(10), false)), locked),
        (1, None, locked),
        (u32::MAX, Some((Some(10), true)), locked),
        (u32::MAX, None, locked),
        (u32::MAX, Some((None, false)), WorkspaceState::NO_USER),
        (1, Some((Some(11), true)), WorkspaceState::NO_USER),
        (2, Some((Some(10), true)), WorkspaceState::NO_USER),
    ] {
        assert_eq!(classify(1, console, Some(10), info), expected);
    }
}

impl Drop for WorkspaceWatch {
    fn drop(&mut self) {
        if let Some(activity) = &self.activity {
            activity.state.lock().unwrap().changed = None;
        }
        let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}
