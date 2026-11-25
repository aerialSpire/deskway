//! Keep-awake on macOS: IOKit power assertions, which also show in `pmset -g assertions`.

use std::ffi::{c_char, c_int};

use anyhow::{ensure, Result};
use block2::{Block, RcBlock};
use dispatch2::DispatchQueue;
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};

use crate::platform::PowerState;
use crate::power::{Hold, Need};

extern "C" {
    fn notify_register_dispatch(
        name: *const c_char,
        token: &mut c_int,
        queue: &DispatchQueue,
        handler: &Block<dyn Fn(c_int)>,
    ) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(kind: &CFString, level: u32, name: &CFString, id: &mut u32) -> i32;
    fn IOPMAssertionDeclareUserActivity(name: &CFString, user_type: u32, id: &mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
    fn IOPSCopyPowerSourcesInfo() -> Option<CFRetained<CFType>>;
    fn IOPSCopyPowerSourcesList(info: &CFType) -> Option<CFRetained<CFArray<CFType>>>;
    fn IOPSGetPowerSourceDescription<'a>(info: &'a CFType, source: &CFType) -> Option<&'a CFDictionary<CFString, CFType>>;
}

pub struct PowerNotifications(c_int);

impl PowerNotifications {
    pub fn register() -> Result<Self> {
        let mut token = 0;
        let callback = RcBlock::new(|_: c_int| crate::power::power_source_changed());
        // IOPowerSources.h documents kIOPSNotifyTimeRemaining for dispatch delivery.
        let name = c"com.apple.system.powersources.timeremaining";
        let status = unsafe { notify_register_dispatch(name.as_ptr(), &mut token, DispatchQueue::main(), &callback) };
        ensure!(status == 0, "notify_register_dispatch failed: {status}");
        Ok(Self(token))
    }

    pub fn state(&self) -> PowerState {
        let Some(info) = (unsafe { IOPSCopyPowerSourcesInfo() }) else { return PowerState::External };
        let Some(sources) = (unsafe { IOPSCopyPowerSourcesList(&info) }) else { return PowerState::External };
        // IOPSKeys.h defines these dictionary keys and values. Apple reports capacity in percent.
        let kind = CFString::from_str("Type");
        let internal = CFString::from_str("InternalBattery");
        let state = CFString::from_str("Power Source State");
        let battery = CFString::from_str("Battery Power");
        let capacity = CFString::from_str("Current Capacity");
        for source in sources.iter() {
            let Some(description) = (unsafe { IOPSGetPowerSourceDescription(&info, &source) }) else { continue };
            if description.get(&kind).as_deref() != Some(internal.as_ref()) {
                continue;
            }
            if description.get(&state).as_deref() == Some(battery.as_ref()) {
                let percent = description.get(&capacity).and_then(|value| value.downcast::<CFNumber>().ok()?.as_i32());
                if let Some(percent @ 0..=100) = percent {
                    return PowerState::Battery(percent as u8);
                }
            }
            break;
        }
        PowerState::External
    }
}

impl Drop for PowerNotifications {
    fn drop(&mut self) {
        unsafe { notify_cancel(self.0) };
    }
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
