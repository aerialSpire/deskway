//! Keep-awake on Windows: a power request (`PowerCreateRequest`), which also shows in
//! `powercfg /requests` with its reason.

use anyhow::Result;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Power::{PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetRequest, POWER_REQUEST_TYPE};
use windows::Win32::System::SystemServices::POWER_REQUEST_CONTEXT_VERSION;
use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};

use crate::power::{Hold, Need};

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
