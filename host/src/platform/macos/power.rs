//! Keep-awake on macOS: IOKit power assertions, which also show in `pmset -g assertions`.

use anyhow::Result;
use objc2_core_foundation::CFString;

use crate::power::{Hold, Need};

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(kind: &CFString, level: u32, name: &CFString, id: &mut u32) -> i32;
    fn IOPMAssertionDeclareUserActivity(name: &CFString, user_type: u32, id: &mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// kIOPMAssertionLevelOn.
const ON: u32 = 255;
/// kIOPMUserActiveRemote.
const REMOTE_USER: u32 = 1;

pub struct PowerRequest {
    ids: Vec<u32>,
}

impl Hold for PowerRequest {
    fn hold(need: Need) -> Result<Self> {
        let name = CFString::from_str(&format!("{} keeps this Mac reachable", crate::product::NAME));
        let mut request = PowerRequest { ids: Vec::with_capacity(3) };
        for (wanted, kind) in [(need.system, "PreventUserIdleSystemSleep"), (need.display, "PreventUserIdleDisplaySleep")] {
            if wanted {
                let mut id = 0;
                let status = unsafe { IOPMAssertionCreateWithName(&CFString::from_str(kind), ON, &name, &mut id) };
                // Dropping `request` releases what was already taken.
                anyhow::ensure!(status == 0, "power assertion {kind} failed: {status:#x}");
                request.ids.push(id);
            }
        }
        // A display-idle assertion keeps the display awake once on. A new remote session
        // also needs to wake an already sleeping display before capture starts.
        if need.display {
            let mut id = 0;
            let status = unsafe { IOPMAssertionDeclareUserActivity(&name, REMOTE_USER, &mut id) };
            if status == 0 {
                request.ids.push(id);
            } else {
                eprintln!("cannot wake the display for remote capture: {status:#x}");
            }
        }
        Ok(request)
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}
