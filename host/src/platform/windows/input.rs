use std::mem::size_of;
use std::time::{Duration, Instant};

use crate::display::DisplayInfo;
use crate::input::keys::{Key, NO};
use crate::input::{Backend, KeyState, Lock, Typed};
use wire::types::Mods;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct WinInput {
    wheel_x: f64,
    wheel_y: f64,
    /// Relative motion below a whole pixel, kept for the next move.
    rel_x: f64,
    rel_y: f64,
    display: Option<DisplayInfo>,
    followed: Option<Instant>,
    /// The layout of the window in front, as last looked up.
    hkl: HKL,
}

impl WinInput {
    pub fn new() -> Self {
        WinInput { wheel_x: 0.0, wheel_y: 0.0, rel_x: 0.0, rel_y: 0.0, display: None, followed: None, hkl: HKL::default() }
    }
}

fn send(i: INPUT) {
    unsafe {
        SendInput(&[i], size_of::<INPUT>() as i32);
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) {
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    });
}

fn keyboard(scan: u16, flags: KEYBD_EVENT_FLAGS) {
    send(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    });
}

/// A key by its set-1 scan code (0xE0xx = extended). Scan codes follow the physical position,
/// so the host's own layout and input method still apply.
fn scan_key(sc: u16, down: bool) {
    let mut flags = KEYEVENTF_SCANCODE;
    if sc >> 8 == 0xe0 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    keyboard(sc & 0xff, flags);
}

/// The layout of the thread that owns the window in front: what typing there uses.
fn foreground_layout() -> HKL {
    unsafe {
        let window = GetForegroundWindow();
        let thread = if window.is_invalid() { 0 } else { GetWindowThreadProcessId(window, None) };
        GetKeyboardLayout(thread)
    }
}

fn down(vk: VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(vk.0 as i32) }) < 0
}

fn toggled(vk: VIRTUAL_KEY) -> bool {
    (unsafe { GetKeyState(vk.0 as i32) }) & 1 != 0
}

impl Backend for WinInput {
    fn mouse_move(&mut self, x: f64, y: f64) {
        // Coordinates span the whole virtual desktop, so any display can be reached. The
        // process is per-monitor DPI aware, so these are all physical pixels.
        let (vx, vy, vw, vh) = unsafe {
            (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN))
        };
        let (dx, dy, dw, dh) = match &self.display {
            Some(d) => (d.x, d.y, d.w, d.h),
            None => unsafe { (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) },
        };
        let ax = absolute(x, dx, dw, vx, vw);
        let ay = absolute(y, dy, dh, vy, vh);
        mouse(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
    }

    fn mouse_move_by(&mut self, dx: f64, dy: f64) {
        // Relative input, as a mouse sends it: programs reading raw input see the motion.
        let (x, y) = whole(&mut self.rel_x, dx, &mut self.rel_y, dy);
        if (x, y) != (0, 0) {
            mouse(x, y, 0, MOUSEEVENTF_MOVE);
        }
    }

    fn set_display(&mut self, d: &DisplayInfo) {
        self.display = Some(d.clone());
    }

    fn secure_desktop(&self) -> bool {
        super::desktop::input() == crate::platform::Desk::Other
    }

    fn follow_input_desktop(&mut self) {
        // Input lands on the lock screen and UAC prompts only from a thread attached there.
        if self.followed.is_none_or(|t| t.elapsed() >= Duration::from_millis(200)) {
            self.followed = Some(Instant::now());
            super::desktop::follow();
        }
    }

    fn mouse_button(&mut self, button: u8, down: bool) {
        let f = match (button, down) {
            (0, true) => MOUSEEVENTF_LEFTDOWN,
            (0, false) => MOUSEEVENTF_LEFTUP,
            (1, true) => MOUSEEVENTF_MIDDLEDOWN,
            (1, false) => MOUSEEVENTF_MIDDLEUP,
            (2, true) => MOUSEEVENTF_RIGHTDOWN,
            (2, false) => MOUSEEVENTF_RIGHTUP,
            _ => return,
        };
        mouse(0, 0, 0, f);
    }

    fn wheel(&mut self, dx: f64, dy: f64) {
        // Browser delta is about 100 per notch where Windows expects 120, and its
        // positive vertical direction is the reverse. Remainders accumulate so
        // slow trackpad scrolling is not lost.
        self.wheel_y -= dy * 1.2;
        self.wheel_x += dx * 1.2;
        let (vy, vx) = (self.wheel_y as i32, self.wheel_x as i32);
        if vy != 0 {
            self.wheel_y -= vy as f64;
            mouse(0, 0, vy as u32, MOUSEEVENTF_WHEEL);
        }
        if vx != 0 {
            self.wheel_x -= vx as f64;
            mouse(0, 0, vx as u32, MOUSEEVENTF_HWHEEL);
        }
    }

    fn key(&mut self, key: &Key, down: bool, _repeat: bool) -> bool {
        if key.win == NO {
            return false;
        }
        scan_key(key.win, down);
        true
    }

    fn text(&mut self, s: &str) {
        for u in s.encode_utf16() {
            keyboard(u, KEYEVENTF_UNICODE);
            keyboard(u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
        }
    }

    fn key_state(&mut self) -> KeyState {
        KeyState {
            caps: Some(toggled(VK_CAPITAL)),
            num: Some(toggled(VK_NUMLOCK)),
            scroll: Some(toggled(VK_SCROLL)),
            mods: Some(Mods { shift: down(VK_SHIFT), ctrl: down(VK_CONTROL), alt: down(VK_MENU), meta: down(VK_LWIN) || down(VK_RWIN) }),
        }
    }

    fn set_lock(&mut self, lock: Lock, _on: bool) {
        // Windows toggles on each press: the caller only asks when the state differs.
        let code = match lock { Lock::Caps => "CapsLock", Lock::Num => "NumLock", Lock::Scroll => "ScrollLock" };
        if let Some(key) = crate::input::keys::find(code) {
            scan_key(key.win, true);
            scan_key(key.win, false);
        }
    }

    fn layout_id(&mut self) -> Option<u64> {
        self.hkl = foreground_layout();
        (!self.hkl.is_invalid()).then_some(self.hkl.0 as usize as u64)
    }

    fn layout_name(&mut self) -> String {
        // The language half of the layout handle names it, such as fr-FR.
        let lang = (self.hkl.0 as usize & 0xffff) as u32;
        let mut buf = [0u16; 85];
        let n = unsafe { windows::Win32::Globalization::LCIDToLocaleName(lang, Some(&mut buf), 0) };
        String::from_utf16_lossy(&buf[..(n.max(1) - 1) as usize])
    }

    fn translate(&mut self, key: &Key, mods: &Mods) -> Option<Typed> {
        if key.win == NO {
            return None;
        }
        let vk = unsafe { MapVirtualKeyExW(key.win as u32, MAPVK_VSC_TO_VK_EX, Some(self.hkl)) };
        if vk == 0 {
            return None;
        }
        let mut state = [0u8; 256];
        for (on, keys) in [(mods.shift, [VK_SHIFT, VK_LSHIFT]), (mods.ctrl, [VK_CONTROL, VK_LCONTROL]), (mods.alt, [VK_MENU, VK_LMENU])] {
            if on {
                for k in keys {
                    state[k.0 as usize] = 0x80;
                }
            }
        }
        let mut buf = [0u16; 8];
        // Flag 4: the kernel's dead-key state is left as it is, so typing on the computer is
        // not disturbed by this lookup.
        let n = unsafe { ToUnicodeEx(vk, key.win as u32 & 0xff, &state, &mut buf, 4, Some(self.hkl)) };
        match n {
            0 => None,
            n if n < 0 => Some(Typed { text: String::from_utf16_lossy(&buf[..1]), dead: true }),
            n => Some(Typed { text: String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]), dead: false }),
        }
    }
}

/// Adds a motion to its remainder and takes the whole pixels out of it.
fn whole(rx: &mut f64, dx: f64, ry: &mut f64, dy: f64) -> (i32, i32) {
    *rx += dx;
    *ry += dy;
    let (x, y) = (rx.trunc(), ry.trunc());
    *rx -= x;
    *ry -= y;
    (x as i32, y as i32)
}

/// Maps a 0..1 position on a display to Windows' absolute input units, 0..65535 across the
/// whole virtual desktop, where both ends are the outermost pixels.
fn absolute(p: f64, origin: i32, size: i32, desktop_origin: i32, desktop_size: i32) -> i32 {
    let px = origin as f64 + p * (size - 1).max(0) as f64;
    ((px - desktop_origin as f64) * 65535.0 / (desktop_size - 1).max(1) as f64).round().clamp(0.0, 65535.0) as i32
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_reaches_every_display_edge() {
        // Primary 3840 wide at 0, a second display 1920 wide to its left: desktop -1920..3840.
        assert_eq!(absolute(0.0, -1920, 1920, -1920, 5760), 0);
        assert_eq!(absolute(1.0, 0, 3840, -1920, 5760), 65535);
        // The primary display's left edge sits a third of the way in.
        let left = absolute(0.0, 0, 3840, -1920, 5760);
        assert!((left - 65535 * 1920 / 5759).abs() <= 1, "{left}");
        // Only one display: the same as the old primary-only mapping.
        assert_eq!(absolute(0.5, 0, 2561, 0, 2561), 32768);
        assert_eq!(absolute(1.0, 0, 100, 0, 100), 65535);
    }

    #[test]
    fn relative_motion_keeps_fractions_for_later() {
        let (mut rx, mut ry) = (0.0, 0.0);
        assert_eq!(whole(&mut rx, 0.6, &mut ry, -0.6), (0, 0));
        assert_eq!(whole(&mut rx, 0.6, &mut ry, -0.6), (1, -1));
        assert_eq!(whole(&mut rx, 3.0, &mut ry, 2.5), (3, 2));
    }

    #[test]
    fn this_computers_layout_types_and_is_named() {
        let mut w = WinInput::new();
        assert!(w.layout_id().is_some());
        let key = |c| crate::input::keys::find(c).unwrap();
        // Every Windows layout types a letter or digit on these keys, unshifted.
        for code in ["KeyF", "Digit5"] {
            let t = w.translate(key(code), &crate::input::no_mods()).expect(code);
            assert_eq!(t.text.chars().count(), 1, "{code}: {t:?}");
        }
        assert!(w.translate(key("F24"), &crate::input::no_mods()).is_none());
        assert!(w.layout_name().contains('-'), "{}", w.layout_name());
    }
}
