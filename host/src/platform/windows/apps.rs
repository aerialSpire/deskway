//! Apps and windows on Windows (`workspace::apps`): the top-level windows a person sees
//! (EnumWindows, without tool and cloaked ones), the Start menu's shortcuts, ShellExecuteEx, and
//! WM_CLOSE or TerminateProcess. Run as the person, in their session (the user worker).

use std::path::Path;

use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, STILL_ACTIVE, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId, GetExitCodeProcess, GetProcessId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
use windows::Win32::UI::Shell::{FOLDERID_CommonPrograms, FOLDERID_Programs, SHGetKnownFolderPath, ShellExecuteExW, KNOWN_FOLDER_FLAG, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, PostMessageW, SetForegroundWindow, SetWindowPos, ShowWindow, GWL_EXSTYLE, GW_OWNER, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use crate::workspace::apps::{App, OpenError, Place, Window, WindowAction};

/// The windows a person sees as windows: visible, titled, not a tool window or an owned one
/// (unless it asks to be on the taskbar), not cloaked (a suspended app's).
pub fn windows() -> Vec<Window> {
    let mut found: Vec<HWND> = Vec::new();
    unsafe extern "system" fn each(hwnd: HWND, l: LPARAM) -> BOOL {
        unsafe { (*(l.0 as *mut Vec<HWND>)).push(hwnd) };
        true.into()
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    }
    let focused = unsafe { GetForegroundWindow() };
    found.into_iter().filter(|h| shown(*h)).map(|h| describe(h, h == focused)).collect()
}

fn shown(h: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || GetWindowTextLengthW(h) == 0 {
            return false;
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        let owned = GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        if ex & WS_EX_TOOLWINDOW.0 != 0 || (owned && ex & WS_EX_APPWINDOW.0 == 0) {
            return false;
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
        cloaked == 0
    }
}

fn describe(h: HWND, focused: bool) -> Window {
    unsafe {
        let mut title = vec![0u16; GetWindowTextLengthW(h) as usize + 1];
        let n = GetWindowTextW(h, &mut title);
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        Window {
            id: h.0 as usize as u64,
            title: String::from_utf16_lossy(&title[..n.max(0) as usize]),
            app: program(pid).unwrap_or_default(),
            pid,
            x: r.left,
            y: r.top,
            w: r.right - r.left,
            h: r.bottom - r.top,
            minimized: IsIconic(h).as_bool(),
            maximized: IsZoomed(h).as_bool(),
            focused,
        }
    }
}

/// A program's file name (notepad.exe).
fn program(pid: u32) -> Option<String> {
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(p);
        let path = ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))?;
        Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned())
    }
}

pub fn act(id: u64, action: WindowAction, place: Option<Place>) -> Result<(), String> {
    let h = HWND(id as usize as *mut _);
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return Err("the window is gone".into());
        }
        match action {
            WindowAction::Focus => focus(h)?,
            WindowAction::Minimize => {
                let _ = ShowWindow(h, SW_MINIMIZE);
            }
            WindowAction::Maximize => {
                let _ = ShowWindow(h, SW_MAXIMIZE);
            }
            WindowAction::Restore => {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            WindowAction::Close => PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)).map_err(|e| e.to_string())?,
            WindowAction::Move => {
                let p = place.ok_or("where to")?;
                if IsZoomed(h).as_bool() || IsIconic(h).as_bool() {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                let (w, hh) = p.size.unwrap_or((0, 0));
                let flags = SWP_NOZORDER | SWP_NOACTIVATE | if p.size.is_none() { SWP_NOSIZE } else { Default::default() };
                SetWindowPos(h, None, p.x, p.y, w, hh, flags).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// Brings `h` to the front, and says so only once it is: typing after a focus that did not
/// happen would land in whatever window had it (a person's chat, their document). Windows lets a
/// background process take the foreground only while its input is attached to the foreground
/// window's thread, so it is attached for the moment it asks; no keys are pressed.
unsafe fn focus(h: HWND) -> Result<(), String> {
    if IsIconic(h).as_bool() {
        let _ = ShowWindow(h, SW_RESTORE);
    }
    let front = || GetForegroundWindow() == h;
    if !SetForegroundWindow(h).as_bool() || !front() {
        let theirs = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let mine = GetCurrentThreadId();
        let attached = theirs != 0 && theirs != mine && AttachThreadInput(mine, theirs, true).as_bool();
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if attached {
            let _ = AttachThreadInput(mine, theirs, false);
        }
    }
    for _ in 0..20 {
        if front() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err("Windows kept another window in front: this one is not focused, so nothing typed now would reach it".into())
}

/// The Start menu's shortcuts, the person's and everyone's: what the person would open.
pub fn apps() -> Vec<App> {
    let mut out: Vec<App> = Vec::new();
    for folder in [FOLDERID_Programs, FOLDERID_CommonPrograms] {
        let Ok(p) = (unsafe { SHGetKnownFolderPath(&folder, KNOWN_FOLDER_FLAG(0), None) }) else { continue };
        let root = unsafe { p.to_string() }.unwrap_or_default();
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _)) };
        walk(Path::new(&root), 3, &mut out);
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    out
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<App>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            if depth > 0 {
                walk(&path, depth - 1, out);
            }
        } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("lnk") || x.eq_ignore_ascii_case("url")) {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            // Uninstallers and readmes are shortcuts too, but nothing to open.
            if !name.to_lowercase().contains("uninstall") {
                out.push(App { name, path: path.to_string_lossy().into_owned() });
            }
        }
    }
}

/// Opens `target` as Explorer would (a shortcut, program, file, folder or address), with
/// `args`; the program's id when it started one.
pub fn open(target: &str, args: &[String]) -> Result<Option<u32>, OpenError> {
    let file = HSTRING::from(target);
    let params = HSTRING::from(args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "));
    let verb = HSTRING::from("open");
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: if args.is_empty() { PCWSTR::null() } else { PCWSTR(params.as_ptr()) },
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
            return Err(if matches!(e.code().0 as u32 & 0xffff, 2 | 3) { OpenError::NotFound } else { OpenError::Failed(e.message()) });
        }
        if info.hProcess.is_invalid() {
            return Ok(None);
        }
        let pid = GetProcessId(info.hProcess);
        let _ = CloseHandle(info.hProcess);
        Ok((pid != 0).then_some(pid))
    }
}

/// An argument as a program's command line takes it.
fn quote(a: &str) -> String {
    if !a.is_empty() && !a.contains([' ', '\t', '"']) {
        return a.to_string();
    }
    format!("\"{}\"", a.replace('"', "\\\""))
}

/// The programs called `name` (notepad, or notepad.exe; any case).
pub fn pids_named(name: &str) -> Vec<u32> {
    let want = name.to_lowercase();
    let want = want.strip_suffix(".exe").unwrap_or(&want).to_string();
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                let exe = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
                if exe.strip_suffix(".exe").unwrap_or(&exe) == want {
                    out.push(e.th32ProcessID);
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Asks the programs `pids` to end: their windows closed, or ended outright. How many windows
/// were asked (or programs ended).
pub fn quit(pids: &[u32], force: bool) -> usize {
    if force {
        return pids.iter().filter(|p| terminate(**p)).count();
    }
    let mut asked = 0;
    for w in windows().into_iter().filter(|w| pids.contains(&w.pid)) {
        if unsafe { PostMessageW(Some(HWND(w.id as usize as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok() {
            asked += 1;
        }
    }
    asked
}

fn terminate(pid: u32) -> bool {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_TERMINATE, false, pid) else { return false };
        let done = TerminateProcess(p, 1).is_ok();
        let _ = CloseHandle(p);
        done
    }
}

pub fn alive(pid: u32) -> bool {
    unsafe {
        let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let running = GetExitCodeProcess(p, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(p);
        running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;

    #[test]
    fn arguments_are_quoted_only_when_they_must_be() {
        for (a, want) in [("plain", "plain"), ("two words", "\"two words\""), ("", "\"\""), ("say \"hi\"", "\"say \\\"hi\\\"\"")] {
            assert_eq!(quote(a), want, "{a}");
        }
    }

    /// A window of this test's own, far off the screen so the person never sees it: listed,
    /// moved, minimized and restored, then closed.
    #[test]
    fn a_window_is_listed_moved_minimized_and_closed() {
        let (tx, rx) = std::sync::mpsc::channel();
        let title = format!("deskway-test-{}", std::process::id());
        let t = title.clone();
        let owner = std::thread::spawn(move || own_window(&t, tx));
        let id = rx.recv().unwrap();
        let find = || windows().into_iter().find(|w| w.id == id);
        let w = find().expect("listed");
        assert_eq!((w.title.as_str(), w.pid, w.app.ends_with(".exe")), (title.as_str(), std::process::id(), true));
        act(id, WindowAction::Move, Some(Place { x: -20000, y: -20000, size: Some((300, 200)) })).unwrap();
        let w = find().unwrap();
        assert_eq!((w.x, w.y, w.w, w.h), (-20000, -20000, 300, 200));
        act(id, WindowAction::Minimize, None).unwrap();
        assert!(find().unwrap().minimized);
        act(id, WindowAction::Restore, None).unwrap();
        assert!(!find().unwrap().minimized);
        act(id, WindowAction::Close, None).unwrap();
        owner.join().unwrap();
        assert!(find().is_none(), "closed");
        assert!(act(id, WindowAction::Close, None).is_err(), "gone");
    }

    /// Shows a plain window off screen without taking the focus, sends its id, and runs its
    /// messages until it is closed.
    fn own_window(title: &str, id: std::sync::mpsc::Sender<u64>) {
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe extern "system" fn proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> windows::Win32::Foundation::LRESULT {
            if m == WM_DESTROY {
                unsafe { PostQuitMessage(0) };
            }
            unsafe { DefWindowProcW(h, m, w, l) }
        }
        unsafe {
            let class = HSTRING::from("deskway-test-window");
            let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: GetModuleHandleW(None).unwrap().into(), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
            RegisterClassW(&wc);
            let h = CreateWindowExW(Default::default(), PCWSTR(class.as_ptr()), &HSTRING::from(title), WS_OVERLAPPEDWINDOW, -21000, -21000, 200, 100, None, None, None, None).unwrap();
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            id.send(h.0 as usize as u64).unwrap();
            let mut m = MSG::default();
            while GetMessageW(&mut m, None, 0, 0).as_bool() {
                let _ = DispatchMessageW(&m);
            }
        }
    }

    /// A program of this test's own, with no window: closing it as a person would cannot, ending
    /// it does; then it is no longer alive and has no name to be found by.
    #[test]
    fn a_program_is_found_by_name_and_ended() {
        let mut child = std::process::Command::new("powershell.exe").args(["-NoProfile", "-Command", "Start-Sleep 30"]).creation_flags(0x0800_0000).spawn().unwrap();
        let pid = child.id();
        assert!(alive(pid) && pids_named("POWERSHELL").contains(&pid) && pids_named("powershell.exe").contains(&pid));
        assert_eq!(quit(&[pid], false), 0, "no window to close");
        assert_eq!(quit(&[pid], true), 1);
        child.wait().unwrap();
        assert!(!alive(pid));
    }

    #[test]
    fn the_start_menu_has_apps_and_a_missing_file_is_not_found() {
        assert!(apps().iter().all(|a| a.path.to_lowercase().ends_with(".lnk") || a.path.to_lowercase().ends_with(".url")));
        assert_eq!(open(r"C:\no\such\deskway-file.exe", &[]), Err(OpenError::NotFound));
    }
}
