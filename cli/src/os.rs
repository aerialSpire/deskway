//! What `deskway` needs of the system besides its key: private state and a proxy process
//! that outlives the one that started it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// Starts `deskway proxy` on its own: no window, no terminal, nothing tying it to this process.
/// What it says goes to proxy.log beside its other files.
pub fn start_proxy() -> Result<()> {
    let log = std::fs::File::create(crate::store::dir()?.join("proxy.log")).context("the proxy's log")?;
    // The program file as it is now, which an update may have replaced since this one started.
    let mut c = Command::new(crate::update::program());
    c.arg("proxy").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    detach(&mut c);
    c.spawn().context("start the proxy")?;
    Ok(())
}

#[cfg(windows)]
fn detach(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, HANDLE_FLAGS};
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
    // The proxy must not hold this process's own pipes open.
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            if let Ok(h) = GetStdHandle(which) {
                let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
}

#[cfg(unix)]
fn detach(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    c.process_group(0);
}

#[cfg(windows)]
pub fn default_state_dir() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Deskway").join("cli")).context("LOCALAPPDATA is not set")
}

#[cfg(target_os = "macos")]
pub fn default_state_dir() -> Result<PathBuf> {
    std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support/Deskway/cli")).context("HOME is not set")
}

#[cfg(target_os = "linux")]
pub fn default_state_dir() -> Result<PathBuf> {
    linux_state_dir(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

#[cfg(target_os = "linux")]
fn linux_state_dir(xdg: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Result<PathBuf> {
    let absolute = |value: Option<std::ffi::OsString>| value.map(PathBuf::from).filter(|p| p.is_absolute());
    absolute(xdg)
        .or_else(|| absolute(home).map(|p| p.join(".local/state")))
        .map(|p| p.join("deskway/cli"))
        .context("neither XDG_STATE_HOME nor HOME is an absolute path")
}

pub fn create_private_dir(path: &Path) -> Result<()> {
    create_dir(path).with_context(|| format!("create {}", path.display()))
}

#[cfg(windows)]
fn create_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    set_unix_mode(path, 0o700)
}

#[cfg(unix)]
pub fn set_unix_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub fn set_unix_mode(_: &Path, _: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn linux_uses_absolute_xdg_state_or_home() {
        for (xdg, home, expected) in [
            (Some("/state"), Some("/home/me"), Some("/state/deskway/cli")),
            (Some("/state"), None, Some("/state/deskway/cli")),
            (None, Some("/home/me"), Some("/home/me/.local/state/deskway/cli")),
            (Some(""), Some("/home/me"), Some("/home/me/.local/state/deskway/cli")),
            (Some("relative"), Some("/home/me"), Some("/home/me/.local/state/deskway/cli")),
            (None, Some("relative"), None),
            (None, None, None),
        ] {
            assert_eq!(linux_state_dir(xdg.map(Into::into), home.map(Into::into)).ok(), expected.map(PathBuf::from));
        }
    }
}
