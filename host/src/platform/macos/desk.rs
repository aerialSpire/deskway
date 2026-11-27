//! What the person at the Mac sees.
//!
//! The Window Server's session dictionary (`CGSessionCopyCurrentDictionary`) says whether this
//! user's session is on the console and whether its screen is locked. That is the signal; no
//! guessing from the frontmost application.

use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass};
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification, NSWorkspaceSessionDidResignActiveNotification};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSNotificationSuspensionBehavior, NSObject, NSObjectProtocol, NSString};

use crate::platform::{unix::programs, Desk, WorkspaceState};
use crate::signal::Control;
use crate::wake::Wake;
use crate::workspace::{self, Browser};

use super::programs::shell;

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

pub(crate) fn workspace_browser(control: Wake<Control>) -> (Browser, WorkspaceWatch) {
    let browser = workspace::local_browser(
        Arc::new(|rows, cols| programs::browser_pty(shell(None, None), rows, cols, || !workspace_state().locked)),
        Arc::new(workspace_state),
    );
    workspace::sweep_home(&|| !workspace_state().locked);

    (browser, WorkspaceWatch::new(control))
}

struct ObserverState {
    control: Wake<Control>,
    previous: Mutex<Option<WorkspaceState>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = ObserverState]
    struct WorkspaceObserver;

    unsafe impl NSObjectProtocol for WorkspaceObserver {}

    impl WorkspaceObserver {
        #[unsafe(method(workspaceChanged:))]
        fn changed(&self, _: &NSNotification) {
            self.refresh(false);
        }

        #[unsafe(method(workspaceRevoked:))]
        fn revoked(&self, _: &NSNotification) {
            self.refresh(true);
        }
    }
);

impl WorkspaceObserver {
    fn refresh(&self, revoked: bool) {
        let mut previous = self.ivars().previous.lock().unwrap();
        let next = workspace_state();
        if revoked {
            // The screen may have unlocked by the time the run loop handles the lock.
            let locked = WorkspaceState { locked: true, ..next };
            *previous = Some(locked);
            let _ = self.ivars().control.send(Control::WorkspaceChanged(locked));
        }
        if *previous != Some(next) {
            *previous = Some(next);
            let _ = self.ivars().control.send(Control::WorkspaceChanged(next));
        }
    }
}

pub(crate) struct WorkspaceWatch {
    distributed: Retained<NSDistributedNotificationCenter>,
    workspace: Retained<NSNotificationCenter>,
    observer: Retained<WorkspaceObserver>,
}

impl WorkspaceWatch {
    fn new(control: Wake<Control>) -> Self {
        let distributed = NSDistributedNotificationCenter::defaultCenter();
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        // The panel's main run loop receives distributed notifications, even while hidden.
        let observer: Retained<WorkspaceObserver> = unsafe {
            msg_send![super(WorkspaceObserver::alloc().set_ivars(ObserverState { control, previous: Mutex::default() })), init]
        };
        let selector = sel!(workspaceChanged:);
        let revoke = sel!(workspaceRevoked:);
        for (name, handler) in [("com.apple.screenIsLocked", revoke), ("com.apple.screenIsUnlocked", selector)] {
            unsafe {
                distributed.addObserver_selector_name_object_suspensionBehavior(
                    &observer, handler, Some(&NSString::from_str(name)), None, NSNotificationSuspensionBehavior::DeliverImmediately,
                );
            }
        }
        unsafe {
            for name in [NSWorkspaceSessionDidBecomeActiveNotification, NSWorkspaceDidWakeNotification] {
                workspace.addObserver_selector_name_object(&observer, selector, Some(name), None);
            }
            workspace.addObserver_selector_name_object(&observer, revoke, Some(NSWorkspaceSessionDidResignActiveNotification), None);
        }
        observer.refresh(false);
        Self { distributed, workspace, observer }
    }
}

impl Drop for WorkspaceWatch {
    fn drop(&mut self) {
        unsafe {
            self.distributed.removeObserver_name_object(&self.observer, None, None);
            self.workspace.removeObserver(&self.observer);
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
