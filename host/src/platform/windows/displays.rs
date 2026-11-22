//! The displays attached to the desktop: DXGI outputs, each named by a stable id, and the
//! system's notice that they changed (`WM_DISPLAYCHANGE`).
//!
//! The id is made from the monitor's device path (`QueryDisplayConfig` target), so the same
//! monitor keeps its id across restarts and re-plugging, whatever `\\.\DISPLAYn` Windows gives
//! it this time. The path names hardware; the browser only needs a token, so it is hashed.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use windows::core::w;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::display::{self, DisplayInfo};

/// Lists displays attached to the desktop, left to right, then top to bottom.
pub fn list() -> Vec<DisplayInfo> {
    let ids = stable_ids();
    let mut out = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        for i in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            for j in 0.. {
                let Ok(output) = adapter.EnumOutputs(j) else { break };
                let Ok(desc) = output.GetDesc() else { continue };
                if let Some(d) = from_desc(&desc, &ids) {
                    out.push(d);
                }
            }
        }
    }
    display::sort(&mut out);
    out
}

/// One attached output, or None when it is detached or has no stable id.
pub fn from_desc(desc: &DXGI_OUTPUT_DESC, ids: &HashMap<String, String>) -> Option<DisplayInfo> {
    if !desc.AttachedToDesktop.as_bool() {
        return None;
    }
    let r = desc.DesktopCoordinates;
    let Some(id) = ids.get(&wide(&desc.DeviceName)) else {
        eprintln!("display {} has no display path; left out", wide(&desc.DeviceName));
        return None;
    };
    Some(DisplayInfo { id: id.clone(), x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top, primary: r.left == 0 && r.top == 0 })
}

fn wide(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
}

/// GDI device name (`\\.\DISPLAYn`, what DXGI reports) to stable id, for every active path.
/// A display shown on several monitors (duplicate mode) takes the first monitor's.
pub fn stable_ids() -> HashMap<String, String> {
    let mut out = HashMap::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return out;
        }
        for p in &paths[..np as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = p.sourceInfo.adapterId;
            source.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = p.targetInfo.adapterId;
            target.header.id = p.targetInfo.id;
            let path = if DisplayConfigGetDeviceInfo(&mut target.header) == 0 { wide(&target.monitorDevicePath) } else { String::new() };
            // A target without a monitor path (some virtual displays) is named by its adapter and
            // target, which last until the adapter is reinstalled.
            let name = if path.is_empty() { format!("target:{:08x}{:08x}:{}", p.targetInfo.adapterId.HighPart, p.targetInfo.adapterId.LowPart, p.targetInfo.id) } else { path };
            out.entry(wide(&source.viewGdiDeviceName)).or_insert_with(|| stable_id(&name));
        }
    }
    out
}

/// A short token for a monitor's device path. Device paths differ only in case at times.
fn stable_id(path: &str) -> String {
    let digest = Sha256::digest(path.to_lowercase().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Calls `changed` whenever the displays change (added, removed, resolution, arrangement), on
/// a thread of its own with a hidden window: `WM_DISPLAYCHANGE` goes to top-level windows only.
/// Stops when dropped.
pub struct DisplayWatch {
    thread: u32,
}

thread_local! {
    static CHANGED: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DISPLAYCHANGE {
        CHANGED.with(|c| {
            if let Some(f) = c.borrow().as_ref() {
                f();
            }
        });
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl DisplayWatch {
    pub fn start(changed: impl Fn() + Send + Sync + 'static) -> DisplayWatch {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("display-watch".into()).spawn(move || unsafe {
            let _ = tx.send(GetCurrentThreadId());
            CHANGED.with(|c| *c.borrow_mut() = Some(Box::new(changed)));
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: w!("DeskwayDisplayWatch"), ..Default::default() };
            // Already registered by an earlier watch in this process: fine.
            RegisterClassW(&class);
            // Never shown. A message-only window would not hear broadcasts.
            let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("DeskwayDisplayWatch"), w!(""), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(instance.into()), None) else {
                eprintln!("display watch: no window; display changes are seen only when capture restarts");
                return;
            };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            let _ = DestroyWindow(hwnd);
        });
        let thread = spawned.ok().and_then(|_| rx.recv().ok()).unwrap_or(0);
        DisplayWatch { thread }
    }
}

impl Drop for DisplayWatch {
    fn drop(&mut self) {
        if self.thread != 0 {
            unsafe {
                let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_short_stable_tokens() {
        let path = r"\\?\DISPLAY#GSM5B09#5&2a1b3c4d&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        let id = stable_id(path);
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, stable_id(&path.to_uppercase()), "the same monitor whatever the case");
        assert_ne!(id, stable_id(&path.replace("UID4352", "UID4353")), "two identical monitors differ");
    }

    #[test]
    fn every_attached_display_is_listed_with_a_stable_id() {
        // Whatever this machine has: each listed display's id comes from the display paths.
        let ids = stable_ids();
        for d in list() {
            assert!(ids.values().any(|v| *v == d.id), "{} not from a display path", d.id);
            assert!(!d.id.starts_with(r"\\.\"));
        }
    }
}
