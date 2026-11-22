//! Display identity and change notice on macOS.
//!
//! A `CGDirectDisplayID` can change when a display is re-plugged or the Mac restarts, so the
//! id Deskway reports is the display's UUID (`CGDisplayCreateUUIDFromDisplayID`), which stays
//! the same for the same display. Changes come from Quartz
//! (`CGDisplayRegisterReconfigurationCallback`), delivered on the main thread's run loop.

use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};

use objc2_core_foundation::{CFRetained, CFUUID};
use objc2_core_graphics::{
    CGDisplayChangeSummaryFlags, CGDisplayRegisterReconfigurationCallback, CGDisplayRemoveReconfigurationCallback, CGError,
    CGGetActiveDisplayList,
};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    // ColorSync, re-exported by ApplicationServices. Follows the Create rule.
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> Option<NonNull<CFUUID>>;
}

/// The stable id of a display, or None when it is gone.
pub fn uuid(display: u32) -> Option<String> {
    let raw = unsafe { CGDisplayCreateUUIDFromDisplayID(display) }?;
    let uuid = unsafe { CFRetained::from_raw(raw) };
    CFUUID::new_string(None, Some(&uuid)).map(|s| s.to_string())
}

/// The displays active now.
pub fn active() -> Vec<u32> {
    let mut count = 0;
    unsafe {
        if CGGetActiveDisplayList(0, null_mut(), &mut count) != CGError::Success {
            return Vec::new();
        }
        let mut ids = vec![0; count as usize];
        if CGGetActiveDisplayList(count, ids.as_mut_ptr(), &mut count) != CGError::Success {
            return Vec::new();
        }
        ids.truncate(count as usize);
        ids
    }
}

/// The `CGDirectDisplayID` now of the display with this stable id.
pub fn cg_id(id: &str) -> Option<u32> {
    active().into_iter().find(|&d| uuid(d).is_some_and(|u| u.eq_ignore_ascii_case(id)))
}

type Changed = Box<dyn Fn() + Send + Sync>;

/// Calls `changed` after every display reconfiguration (added, removed, mode, arrangement,
/// mirroring) until dropped.
pub struct DisplayWatch {
    changed: *mut Changed,
}

// The callback runs on the main thread; the closure it calls is Send + Sync.
unsafe impl Send for DisplayWatch {}
unsafe impl Sync for DisplayWatch {}

unsafe extern "C-unwind" fn reconfigured(_display: u32, flags: CGDisplayChangeSummaryFlags, info: *mut c_void) {
    // Each change is announced before and after; only the result counts.
    if flags.contains(CGDisplayChangeSummaryFlags::BeginConfigurationFlag) {
        return;
    }
    let changed = unsafe { &*(info as *const Changed) };
    changed();
}

impl DisplayWatch {
    pub fn start(changed: impl Fn() + Send + Sync + 'static) -> DisplayWatch {
        let changed: *mut Changed = Box::into_raw(Box::new(Box::new(changed)));
        let r = unsafe { CGDisplayRegisterReconfigurationCallback(Some(reconfigured), changed.cast()) };
        if r != CGError::Success {
            eprintln!("display watch: {r:?}; display changes are seen only when capture restarts");
        }
        DisplayWatch { changed }
    }
}

impl Drop for DisplayWatch {
    fn drop(&mut self) {
        unsafe {
            CGDisplayRemoveReconfigurationCallback(Some(reconfigured), self.changed.cast());
            drop(Box::from_raw(self.changed));
        }
    }
}
