//! Following the input desktop. Windows shows the lock screen, the sign-in screen and UAC
//! prompts on separate desktops. Only a SYSTEM process in the console session can open them;
//! a thread must attach to the desktop that has input before it can capture or inject there.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::*;

use crate::platform::Desk;

fn name_of(h: HANDLE) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    unsafe { GetUserObjectInformationW(h, UOI_NAME, Some(buf.as_mut_ptr() as _), (buf.len() * 2) as u32, Some(&mut needed)).ok()? };
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

fn classify(name: Option<&str>) -> Desk {
    match name {
        Some(n) if n.eq_ignore_ascii_case("Default") => Desk::Default,
        // A process that cannot even open the input desktop is looking at a secure one.
        Some(n) if n.eq_ignore_ascii_case("Winlogon") => Desk::Winlogon,
        None => Desk::Winlogon,
        Some(_) => Desk::Other,
    }
}

/// The desktop receiving input now.
pub fn input() -> Desk {
    unsafe {
        match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) {
            Ok(h) => {
                let name = name_of(HANDLE(h.0));
                let _ = CloseDesktop(h);
                classify(name.as_deref())
            }
            Err(_) => Desk::Winlogon,
        }
    }
}

thread_local! {
    static ATTACHED: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}

/// Locks the console. LockWorkStation only works for the signed-in user's own processes, so
/// as SYSTEM the console session is disconnected instead, which shows the lock screen and
/// keeps every program running.
pub fn lock() {
    unsafe {
        if crate::service::ipc::running_as_system() {
            let mut session = 0u32;
            if windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(std::process::id(), &mut session).is_ok()
                && windows::Win32::System::RemoteDesktop::WTSDisconnectSession(None, session, false).is_ok()
            {
                return;
            }
        }
        if let Err(e) = windows::Win32::System::Shutdown::LockWorkStation() {
            eprintln!("cannot lock: {e}");
        }
    }
}

/// Attaches the calling thread to the input desktop if it is on another one. The thread must
/// own no windows or hooks. Returns true if the thread moved.
pub fn follow() -> bool {
    unsafe {
        let Ok(input) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(0x10000000)) else { return false };
        let want = name_of(HANDLE(input.0));
        let have = GetThreadDesktop(windows::Win32::System::Threading::GetCurrentThreadId()).ok().and_then(|h| name_of(HANDLE(h.0)));
        if want.is_none() || want == have {
            let _ = CloseDesktop(input);
            return false;
        }
        if SetThreadDesktop(input).is_err() {
            let _ = CloseDesktop(input);
            return false;
        }
        // The handle stays open while the thread uses the desktop; the previous one can close.
        let old = ATTACHED.with(|a| a.replace(input.0 as isize));
        if old != 0 {
            let _ = CloseDesktop(HDESK(old as _));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_map_to_kinds() {
        assert_eq!(classify(Some("Default")), Desk::Default);
        assert_eq!(classify(Some("Winlogon")), Desk::Winlogon);
        assert_eq!(classify(Some("Screen-saver")), Desk::Other);
        assert_eq!(classify(None), Desk::Winlogon);
    }
}
