//! What the person at the Mac sees.
//!
//! The Window Server's session dictionary (`CGSessionCopyCurrentDictionary`) says whether this
//! user's session is on the console and whether its screen is locked. That is the signal; no
//! guessing from the frontmost application.

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;

use crate::platform::Desk;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_unlocked_session_on_the_console_is_the_desktop() {
        assert_eq!(classify(true, false), Desk::Default);
        assert_eq!(classify(true, true), Desk::Winlogon);
        assert_eq!(classify(false, false), Desk::Winlogon);
        assert_eq!(classify(false, true), Desk::Winlogon);
    }
}
