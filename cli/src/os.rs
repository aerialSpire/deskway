//! What `deskway` needs of the system besides its key (`keys`): starting the proxy as a process
//! of its own that outlives the one that started it.

use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// Starts `deskway proxy` on its own: no window, no terminal, nothing tying it to this process.
/// What it says goes to proxy.log beside its other files.
pub fn start_proxy() -> Result<()> {
    let log = std::fs::File::create(crate::store::dir()?.join("proxy.log")).context("the proxy's log")?;
    // The program file as it is now, which an update may have replaced since this one started.
    let mut c = Command::new(crate::update::program());
    c.arg("proxy").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, HANDLE_FLAGS};
        use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
        // The proxy outlives this process: it must not hold this process's own pipes open
        // (an AI client would never see `deskway mcp` close them).
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe {
                if let Ok(h) = GetStdHandle(which) {
                    let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
                }
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c.spawn().context("start the proxy")?;
    Ok(())
}
