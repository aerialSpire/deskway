//! The person signed in at the console, for file work done on their behalf by the SYSTEM
//! agent: files from the controller are written, and files sent to it are read, with that
//! person's own rights, never with SYSTEM's.

use std::path::PathBuf;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{DuplicateTokenEx, RevertToSelf, SecurityImpersonation, TokenImpersonation, TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, SetThreadToken, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};

/// An impersonation token of the console user, taken from their Explorer.
fn console_token() -> Option<HANDLE> {
    unsafe {
        let session = WTSGetActiveConsoleSessionId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut pid = None;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case("explorer.exe") {
                    let mut s = 0u32;
                    if ProcessIdToSessionId(e.th32ProcessID, &mut s).is_ok() && s == session {
                        pid = Some(e.th32ProcessID);
                        break;
                    }
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid?).ok()?;
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY | TOKEN_DUPLICATE, &mut token);
        let _ = CloseHandle(process);
        opened.ok()?;
        let mut imp = HANDLE::default();
        let dup = DuplicateTokenEx(token, TOKEN_QUERY | TOKEN_IMPERSONATE, None, SecurityImpersonation, TokenImpersonation, &mut imp);
        let _ = CloseHandle(token);
        dup.ok()?;
        Some(imp)
    }
}

/// Runs `f` as the console user. Not running as SYSTEM (development), it runs as is.
/// With nobody signed in, nothing runs and the result is an error.
pub fn as_user<T>(f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
    with_token(|_| f())
}

fn with_token<T>(f: impl FnOnce(Option<HANDLE>) -> std::io::Result<T>) -> std::io::Result<T> {
    if !crate::service::ipc::running_as_system() {
        return f(None);
    }
    let Some(token) = console_token() else { return Err(std::io::Error::other("nobody is signed in")) };
    unsafe {
        if SetThreadToken(None, Some(token)).is_err() {
            let _ = CloseHandle(token);
            return Err(std::io::Error::other("cannot act as the signed-in user"));
        }
    }
    let r = f(Some(token));
    unsafe {
        let _ = RevertToSelf();
        let _ = CloseHandle(token);
    }
    r
}

/// The console user's Downloads folder.
pub fn downloads() -> Option<PathBuf> {
    with_token(|token| unsafe {
        let p: PWSTR = SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), token).map_err(std::io::Error::other)?;
        let path = PathBuf::from(p.to_string().unwrap_or_default());
        CoTaskMemFree(Some(p.0 as _));
        Ok(path)
    })
    .ok()
    .filter(|p| p.is_absolute())
}
