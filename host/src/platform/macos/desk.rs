//! What the person at the Mac sees.
//!
//! The Window Server's session dictionary (`CGSessionCopyCurrentDictionary`) says whether this
//! user's session is on the console and whether its screen is locked. That is the signal; no
//! guessing from the frontmost application.

use std::io;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;

use crate::platform::{unix::programs, Desk, WorkspaceState};
use crate::signal::Control;
use crate::wake::Wake;
use crate::workspace::{self, Browser};

/// This session's desktop: `Default` when it is on the console and unlocked; `Winlogon` when
/// the lock screen or the login window (another user, fast user switching) is showing.
pub fn current() -> Desk {
    let Some(session) = CGSessionCopyCurrentDictionary() else {
        return Desk::Winlogon;
    };
    // The session dictionary is keyed by CFString.
    let session = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(session) };
    let flag = |key: &str| {
        session
            .get(&CFString::from_str(key))
            .and_then(|value| value.downcast::<CFBoolean>().ok())
            .is_some_and(|value| value.as_bool())
    };
    classify(flag("kCGSSessionOnConsoleKey"), flag("CGSSessionScreenIsLocked"))
}

fn classify(on_console: bool, locked: bool) -> Desk {
    if on_console && !locked {
        Desk::Default
    } else {
        Desk::Winlogon
    }
}

fn workspace_state() -> WorkspaceState {
    browser_state(current())
}

fn browser_state(desk: Desk) -> WorkspaceState {
    WorkspaceState { locked: desk != Desk::Default, ..WorkspaceState::READY }
}

pub(crate) fn workspace_browser(control: Wake<Control>) -> io::Result<(Browser, WorkspaceWatch)> {
    let browser = workspace::local_browser(
        Arc::new(|rows, cols| programs::browser_pty(programs::shell("/bin/zsh", None, None), rows, cols, || !workspace_state().locked)),
        Arc::new(workspace_state),
    );
    workspace::sweep_home(&|| !workspace_state().locked);

    let (stop, stopped) = mpsc::channel();
    let thread = thread::Builder::new().name("workspace-watch".into()).spawn(move || {
        let mut previous = None;
        loop {
            let next = workspace_state();
            if previous != Some(next) {
                if control.send(Control::WorkspaceChanged(next)).is_err() { break; }
                previous = Some(next);
            }
            if !matches!(stopped.recv_timeout(Duration::from_millis(100)), Err(mpsc::RecvTimeoutError::Timeout)) { break; }
        }
    })?;
    Ok((browser, WorkspaceWatch { stop, thread: Some(thread) }))
}

pub(crate) struct WorkspaceWatch {
    stop: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for WorkspaceWatch {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unlocked_session_on_the_console_is_the_desktop() {
        use wire::types::Reason;

        for (on_console, locked, desk) in [
            (true, false, Desk::Default),
            (true, true, Desk::Winlogon),
            (false, false, Desk::Winlogon),
            (false, true, Desk::Winlogon),
        ] {
            assert_eq!(classify(on_console, locked), desk);
            let workspace = browser_state(desk);
            assert_eq!(workspace.user, WorkspaceState::READY.user);
            assert_eq!(workspace.refusal(false), (desk != Desk::Default).then_some(Reason::Locked));
            assert_eq!(workspace.refusal(true), Some(Reason::ViewOnly));
        }
    }
}
