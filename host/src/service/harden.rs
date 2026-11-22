//! Process mitigations for the processes that run with service privileges. Each one is best
//! effort: an older Windows that lacks a policy still runs, and says which one it skipped.

use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Threading::*;

fn policy(p: PROCESS_MITIGATION_POLICY, flags: u32, name: &str) {
    // Each of these policies is a single 32-bit field of flags.
    if let Err(e) = unsafe { SetProcessMitigationPolicy(p, &flags as *const u32 as _, 4) } {
        eprintln!("mitigation {name} unavailable: {e}");
    }
}

/// For both privileged processes.
fn common() {
    unsafe {
        // DLLs only from System32 and the admin-only install folder, never the current folder or PATH.
        if let Err(e) = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32 | LOAD_LIBRARY_SEARCH_APPLICATION_DIR) {
            eprintln!("mitigation dll-directories unavailable: {e}");
        }
    }
    // No AppInit DLLs, global hooks, legacy IMEs or Winsock LSPs injected into the process.
    policy(ProcessExtensionPointDisablePolicy, 0b1, "extension-points");
    // Using a closed or wrong handle crashes instead of touching some other object.
    policy(ProcessStrictHandleCheckPolicy, 0b11, "strict-handles");
    // No images from network shares or low-integrity files; System32 copies win over same-named DLLs.
    policy(ProcessImageLoadPolicy, 0b111, "image-load");
    // Does not follow junctions or links that a non-administrator created.
    policy(ProcessRedirectionTrustPolicy, 0b1, "redirection-guard");
}

/// The SYSTEM agent. Dynamic code and signature policies stay off: GPU drivers and hardware
/// encoders may need them.
pub fn agent() {
    common();
}

/// The network service.
pub fn network() {
    common();
}

/// The LocalSystem service, and administrator commands that write machine-wide files.
pub fn privileged() {
    common();
}
