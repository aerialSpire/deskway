//! Keep-awake on Windows: a power request (`PowerCreateRequest`), which also shows in
//! `powercfg /requests` with its reason.

use std::ffi::c_void;
use std::ptr::null_mut;

use anyhow::Result;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
    PowerRequestSystemRequired, PowerSetRequest, PowerSettingRegisterNotification,
    PowerSettingUnregisterNotification, DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
    POWER_REQUEST_TYPE, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING, POWER_REQUEST_CONTEXT_VERSION,
};
use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

use crate::platform::PowerState;
use crate::power::{Hold, Need};

pub struct PowerNotifications(Vec<HPOWERNOTIFY>);

impl PowerNotifications {
    pub fn register() -> Result<Self> {
        let mut notifications = Self(Vec::with_capacity(2));
        // The callback form works from the network service and from a foreground host.
        let mut subscription = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS { Callback: Some(power_changed), Context: null_mut() };
        let recipient = HANDLE(std::ptr::from_mut(&mut subscription).cast());
        for guid in [GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING] {
            let mut handle = null_mut();
            unsafe { PowerSettingRegisterNotification(&guid, DEVICE_NOTIFY_CALLBACK, recipient, &mut handle) }.ok()?;
            notifications.0.push(HPOWERNOTIFY(handle as isize));
        }
        Ok(notifications)
    }

    pub fn state(&self) -> PowerState {
        let mut state = SYSTEM_POWER_STATUS::default();
        if unsafe { GetSystemPowerStatus(&mut state) }.is_ok()
            && state.ACLineStatus == 0
            && state.BatteryFlag != 128 // No system battery.
            && state.BatteryLifePercent <= 100
        {
            PowerState::Battery(state.BatteryLifePercent)
        } else {
            PowerState::External
        }
    }
}

impl Drop for PowerNotifications {
    fn drop(&mut self) {
        for &handle in &self.0 {
            let _ = unsafe { PowerSettingUnregisterNotification(handle) };
        }
    }
}

unsafe extern "system" fn power_changed(_: *const c_void, _: u32, _: *const c_void) -> u32 {
    crate::power::power_source_changed();
    0
}

pub struct PowerRequest {
    handle: HANDLE,
    set: Vec<POWER_REQUEST_TYPE>,
}

// The handle is a kernel object usable from any thread.
unsafe impl Send for PowerRequest {}

impl Hold for PowerRequest {
    fn hold(need: Need) -> Result<Self> {
        let mut reason: Vec<u16> = format!("{} keeps this computer reachable\0", crate::product::NAME).encode_utf16().collect();
        let context = REASON_CONTEXT {
            Version: POWER_REQUEST_CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(reason.as_mut_ptr()) },
        };
        let handle = unsafe { PowerCreateRequest(&context)? };
        let mut request = PowerRequest { handle, set: Vec::new() };
        for (wanted, kind) in [(need.system, PowerRequestSystemRequired), (need.display, PowerRequestDisplayRequired)] {
            if wanted {
                unsafe { PowerSetRequest(handle, kind)? };
                request.set.push(kind);
            }
        }
        Ok(request)
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        unsafe {
            for kind in self.set.drain(..) {
                let _ = PowerClearRequest(self.handle, kind);
            }
            let _ = CloseHandle(self.handle);
        }
    }
}
