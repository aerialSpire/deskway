mod accessibility;
pub mod apps;
mod capture;
pub mod elements;
mod clipboard;
mod desk;
mod displays;
mod encoder;
mod files;
mod input;
mod power;
mod programs;
mod windows;
pub mod ui;

pub use capture::ScreenCapture as NativeCapture;
pub use clipboard::Clipboard as NativeClipboard;
pub use clipboard::{set_text as set_clipboard_text, text as clipboard_text};
pub use programs::{kill_tree, start_program, start_terminal};
pub use displays::DisplayWatch;
pub use power::{PowerNotifications, PowerRequest};
pub use encoder::Encoder as NativeEncoder;
pub use files::Files as NativeFiles;
pub use input::MacInput as NativeInput;

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut, NonNull};
use std::sync::OnceLock;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGPreflightScreenCaptureAccess;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: &CFDictionary) -> bool;
    static kAXTrustedCheckOptionPrompt: &'static CFString;
}

#[link(name = "SystemConfiguration", kind = "framework")]
extern "C" {
    fn SCDynamicStoreCopyComputerName(
        store: *const c_void,
        encoding: *mut u32,
    ) -> Option<NonNull<CFString>>;
}

pub fn computer_name() -> Option<String> {
    // Null uses a temporary system configuration session. The Copy result is retained.
    let name = unsafe { SCDynamicStoreCopyComputerName(null(), null_mut()) }?;
    let name = unsafe { CFRetained::from_raw(name) }.to_string();
    (!name.trim().is_empty()).then_some(name)
}

pub struct Permissions {
    pub screen: bool,
    pub accessibility: bool,
}

impl Permissions {
    pub fn ready(&self) -> bool {
        self.screen && self.accessibility
    }
}

pub fn permissions() -> Permissions {
    Permissions {
        screen: CGPreflightScreenCaptureAccess(),
        accessibility: unsafe { AXIsProcessTrusted() },
    }
}

pub fn full_disk_access() -> bool {
    // The system TCC database is present even on macOS versions without per-user databases.
    // Opening it read-only checks the process's access without a prompt or reading any records.
    File::open("/Library/Application Support/com.apple.TCC/TCC.db").is_ok()
}

pub fn request_accessibility() {
    let prompt = unsafe { kAXTrustedCheckOptionPrompt };
    let options =
        CFDictionary::from_slices(&[prompt as &CFType], &[CFBoolean::new(true) as &CFType]);
    // macOS presents its own prompt for this process. The return value is the
    // pre-prompt state, so the panel checks permission again on focus.
    unsafe { AXIsProcessTrustedWithOptions(options.as_opaque()) };
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/Deskway"))
}

/// The system's UI language tag, most preferred first (`zh-Hant-TW`, `en-US` …). macOS keeps
/// the order of preferred languages in the global AppleLanguages default; the environment is
/// the fallback (it is usually unset for apps started from Finder).
pub fn system_language() -> Option<String> {
    if let Ok(out) = std::process::Command::new("/usr/bin/defaults").args(["read", "-g", "AppleLanguages"]).output() {
        let text = String::from_utf8_lossy(&out.stdout);
        // Prints a list like ( "zh-Hant-TW", "en-US" ).
        if let Some(first) = text.split(['(', ')', ',', '\n']).map(|s| s.trim().trim_matches('"')).find(|s| !s.is_empty()) {
            return Some(first.to_string());
        }
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"].iter().filter_map(|v| std::env::var(v).ok()).find(|v| !v.is_empty() && v != "C" && v != "POSIX")
}

/// Commands on macOS run in a terminal: the message goes there.
pub fn message(title: &str, text: &str) {
    println!("{title}: {text}");
}

pub fn attach_console() {}
pub fn platform_init() {}

/// Locking when a session ends is a Windows setting; the Mac panel does not offer it.
pub fn lock_screen() {}

/// Start at login is the panel's login item on macOS, not a call from a session.
pub fn set_autostart(_on: bool) -> anyhow::Result<()> {
    Ok(())
}

pub fn first_instance() -> bool {
    static LOCK: OnceLock<File> = OnceLock::new();
    let lock = (|| -> anyhow::Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(crate::config::dir()?.join("host.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(file)
    })();
    match lock {
        Ok(file) => LOCK.set(file).is_ok(),
        Err(error) => {
            eprintln!("cannot lock Deskway host: {error}");
            false
        }
    }
}
