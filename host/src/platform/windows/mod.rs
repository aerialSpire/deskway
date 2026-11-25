pub mod apps;
mod capture;
pub mod elements;
mod clipboard;
mod desktop;
mod dib;
mod displays;
mod encoder;
mod files;
mod input;
mod power;
mod programs;
pub mod ui;
mod user;

pub use capture::Duplication as NativeCapture;
pub use clipboard::Clipboard as NativeClipboard;
pub use clipboard::{set_text as set_clipboard_text, text as clipboard_text};
pub use programs::{kill_tree, start_program, start_terminal};
pub use displays::DisplayWatch;
pub use power::{PowerNotifications, PowerRequest};
pub use encoder::Encoder as NativeEncoder;
pub use files::Files as NativeFiles;
pub use input::WinInput as NativeInput;

use std::path::PathBuf;

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Deskway"))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".deskway")))
}

/// The computer's name as Windows shows it (its NetBIOS name, as `COMPUTERNAME` has it).
pub fn computer_name() -> Option<String> {
    use windows::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
    let mut len = 0u32;
    unsafe {
        let _ = GetComputerNameExW(ComputerNameNetBIOS, None, &mut len);
        let mut buf = vec![0u16; len as usize];
        GetComputerNameExW(ComputerNameNetBIOS, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len).ok()?;
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        (!name.trim().is_empty()).then_some(name)
    }
}

pub enum KnownFolder {
    ProgramData,
    ProgramFiles,
}

/// A folder as the system names it, never as environment variables do.
pub fn known_folder(which: KnownFolder) -> PathBuf {
    unsafe {
        use windows::Win32::UI::Shell::*;
        let id = match which {
            KnownFolder::ProgramData => FOLDERID_ProgramData,
            KnownFolder::ProgramFiles => FOLDERID_ProgramFiles,
        };
        if let Ok(p) = SHGetKnownFolderPath(&id, KNOWN_FOLDER_FLAG(0), None) {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    PathBuf::from(match which {
        KnownFolder::ProgramData => r"C:\ProgramData",
        KnownFolder::ProgramFiles => r"C:\Program Files",
    })
}

/// The system's UI language tag, most preferred first (`zh-TW`, `en-US` …).
pub fn system_language() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let (mut count, mut len) = (0u32, 0u32);
    unsafe {
        GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).ok()?;
        let mut buf = vec![0u16; len as usize];
        GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, Some(PWSTR(buf.as_mut_ptr())), &mut len).ok()?;
        // A list of NUL-terminated names, most preferred first.
        let first = buf.split(|c| *c == 0).next()?;
        (!first.is_empty()).then(|| String::from_utf16_lossy(first))
    }
}

/// A message box over everything, for a command started without a console.
pub fn message(title: &str, text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_OK, MB_SETFOREGROUND, MB_TOPMOST};
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_OK | MB_TOPMOST | MB_SETFOREGROUND);
    }
}

pub fn attach_console() {
    // The GUI subsystem has no console of its own; commands reuse the one they were started from.
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// Only one panel per user session.
pub fn first_instance() -> bool {
    unsafe {
        use windows::core::w;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;
        // Held for the life of the process on purpose.
        let created = CreateMutexW(None, false, w!("Local\\DeskwayHost"));
        created.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Real pixels, not DPI-virtualized ones: capture size and input mapping both depend on it.
pub fn platform_init() {
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn lock_screen() {
    desktop::lock();
}

/// The services' start type, which only the agent (SYSTEM) may change.
pub fn set_autostart(on: bool) -> anyhow::Result<()> {
    crate::service::install::set_autostart(on)
}
