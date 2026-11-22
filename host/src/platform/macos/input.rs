//! Input into macOS through Quartz events.
//!
//! Every event carries the modifier flags of the modifier keys this backend holds down, set on
//! the event itself: nothing is inherited from the system's modifier state, so a release the
//! system did not take in (or one lost on the way) cannot stick to the keys that follow. Caps
//! Lock is the HID system's own: it is set there and read from there.
//! The keyboard layout comes from Text Input Sources (on the main thread, as they require) and
//! is translated with `UCKeyTranslate`.

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2_app_kit::NSEvent;
use objc2_core_foundation::{
    CFData, CFDictionary, CFNotificationCenter, CFNotificationName, CFNotificationSuspensionBehavior, CFRetained,
    CFString, CFType, CGPoint, CGRect,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayIsActive, CGEvent, CGEventField, CGEventFlags as Flags, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType as Kind, CGMainDisplayID, CGMouseButton, CGScrollEventUnit,
};

use crate::display::DisplayInfo;
use crate::input::keys::{Key, NO};
use crate::input::{Backend, KeyState, Lock, Typed};
use wire::types::Mods;

mod text;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: &CFString) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: &'static CFString;
    static kTISPropertyInputSourceID: &'static CFString;
    static kTISNotifySelectedKeyboardInputSourceChanged: &'static CFString;
    fn UCKeyTranslate(
        layout: *const u8,
        code: u16,
        action: u16,
        modifiers: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: usize,
        length: *mut usize,
        chars: *mut u16,
    ) -> i32;
    fn LMGetKbdType() -> u8;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOHIDSetModifierLockState(connect: u32, selector: i32, state: bool) -> i32;
}

const CAPS_LOCK: u16 = 0x39;
const UC_KEY_ACTION_DOWN: u16 = 0;
const UC_NO_DEAD_KEYS: u32 = 1;
/// Bumped by the system's "keyboard layout changed" notification; read on each key.
static LAYOUT_GENERATION: AtomicU64 = AtomicU64::new(1);

unsafe extern "C-unwind" fn layout_changed(
    _: *mut CFNotificationCenter,
    _: *mut c_void,
    _: *const CFNotificationName,
    _: *const c_void,
    _: *const CFDictionary,
) {
    LAYOUT_GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Listens for layout changes, once, from the main run loop.
fn watch_layout() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        DispatchQueue::main().exec_async(|| {
            if let Some(center) = CFNotificationCenter::distributed_center() {
                unsafe {
                    center.add_observer(
                        std::ptr::null(),
                        Some(layout_changed),
                        Some(kTISNotifySelectedKeyboardInputSourceChanged),
                        std::ptr::null(),
                        CFNotificationSuspensionBehavior::DeliverImmediately,
                    )
                };
            }
        });
    });
}

/// The current layout's id and `uchr` data. Text Input Sources must be used on the main thread.
fn read_layout() -> Option<(String, Vec<u8>)> {
    unsafe {
        let source = NonNull::new(TISCopyCurrentKeyboardLayoutInputSource())?;
        // The copy is ours to release.
        let source: CFRetained<CFType> = CFRetained::from_raw(source.cast());
        let raw = CFRetained::as_ptr(&source).as_ptr().cast::<c_void>();
        let id = (TISGetInputSourceProperty(raw, kTISPropertyInputSourceID) as *const CFString).as_ref().map(|s| s.to_string()).unwrap_or_default();
        let data = (TISGetInputSourceProperty(raw, kTISPropertyUnicodeKeyLayoutData) as *const CFData).as_ref().map(|d| d.to_vec()).unwrap_or_default();
        Some((id, data))
    }
}

fn current_layout() -> Option<(String, Vec<u8>)> {
    let mut out = None;
    DispatchQueue::main().exec_sync(|| out = read_layout());
    out
}

/// Carbon modifier bits, shifted as `UCKeyTranslate` takes them.
fn carbon_modifiers(m: &Mods) -> u32 {
    (m.meta as u32) | (m.shift as u32) << 1 | (m.alt as u32) << 3 | (m.ctrl as u32) << 4
}

fn translate_uchr(data: &[u8], code: u16, m: &Mods) -> Option<Typed> {
    if data.is_empty() {
        return None;
    }
    let run = |options: u32| {
        let (mut dead, mut len, mut buf) = (0u32, 0usize, [0u16; 8]);
        let status = unsafe {
            UCKeyTranslate(data.as_ptr(), code, UC_KEY_ACTION_DOWN, carbon_modifiers(m), u32::from(LMGetKbdType()), options, &mut dead, buf.len(), &mut len, buf.as_mut_ptr())
        };
        (status == 0).then(|| (String::from_utf16_lossy(&buf[..len.min(buf.len())]), dead))
    };
    let (text, dead) = run(0)?;
    if !text.is_empty() {
        return Some(Typed { text, dead: false });
    }
    // Nothing typed but a dead key state: an accent key. Its own character labels it.
    if dead != 0 {
        let (text, _) = run(UC_NO_DEAD_KEYS)?;
        return (!text.is_empty()).then_some(Typed { text, dead: true });
    }
    None
}

/// Sets Caps Lock in the HID system, as the key on a keyboard does.
fn set_caps_lock(on: bool) {
    unsafe {
        let service = IOServiceGetMatchingService(0, IOServiceMatching(c"IOHIDSystem".as_ptr()));
        if service == 0 {
            return;
        }
        let mut connect = 0u32;
        #[allow(deprecated)]
        let task = libc::mach_task_self();
        // kIOHIDParamConnectType, kIOHIDCapsLockState.
        if IOServiceOpen(service, task, 1, &mut connect) == 0 {
            if IOHIDSetModifierLockState(connect, 1, on) != 0 {
                eprintln!("cannot set Caps Lock");
            }
            IOServiceClose(connect);
        }
        IOObjectRelease(service);
    }
}

/// Flags a key carries by itself on Apple keyboards (not modifier state).
fn intrinsic_flags(code: &str) -> Flags {
    if code.starts_with("Arrow") {
        Flags::MaskNumericPad | Flags::MaskSecondaryFn
    } else if code.starts_with("Numpad") || code == "NumLock" {
        Flags::MaskNumericPad
    } else if (code.starts_with('F') && code[1..].parse::<u8>().is_ok()) || matches!(code, "Home" | "End" | "PageUp" | "PageDown" | "Delete" | "Help" | "Insert") {
        Flags::MaskSecondaryFn
    } else {
        Flags::empty()
    }
}

fn is_modifier(mac: u16) -> bool {
    matches!(mac, 0x36..=0x3e | 0x3f)
}

/// The flag a modifier key (by its Mac key code) holds while down.
fn modifier_flag(mac: u16) -> Flags {
    match mac {
        0x38 | 0x3c => Flags::MaskShift,
        0x3b | 0x3e => Flags::MaskControl,
        0x3a | 0x3d => Flags::MaskAlternate,
        0x37 | 0x36 => Flags::MaskCommand,
        0x3f => Flags::MaskSecondaryFn,
        _ => Flags::empty(),
    }
}

/// The flags of these modifier keys held down.
fn held_flags(held: &[u16]) -> Flags {
    held.iter().fold(Flags::empty(), |f, &mac| f | modifier_flag(mac))
}

struct Button {
    quartz: CGMouseButton,
    down: Kind,
    up: Kind,
    drag: Kind,
}

const BUTTONS: [Button; 3] = [
    Button { quartz: CGMouseButton::Left, down: Kind::LeftMouseDown, up: Kind::LeftMouseUp, drag: Kind::LeftMouseDragged },
    Button { quartz: CGMouseButton::Center, down: Kind::OtherMouseDown, up: Kind::OtherMouseUp, drag: Kind::OtherMouseDragged },
    Button { quartz: CGMouseButton::Right, down: Kind::RightMouseDown, up: Kind::RightMouseUp, drag: Kind::RightMouseDragged },
];

#[derive(Clone, Copy)]
struct Click {
    position: CGPoint,
    time: Instant,
    count: i64,
    dragged: bool,
}

/// The layout as last loaded for physical key presses.
struct Layout {
    generation: u64,
    id: String,
    data: Vec<u8>,
}

pub struct MacInput {
    source: Option<CFRetained<CGEventSource>>,
    position: CGPoint,
    /// Mouse buttons held, in the order pressed.
    held: Vec<usize>,
    /// Modifier keys held down here (Mac key codes): the flags every event carries.
    modifiers: Vec<u16>,
    clicks: [Option<Click>; 3],
    double_click: Duration,
    wheel: (f64, f64),
    display_id: Option<u32>,
    layout: Option<Layout>,
    text_layout: text::LayoutKeys,
    text_until: Option<Instant>,
}

impl MacInput {
    pub fn new() -> Self {
        watch_layout();
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState);
        let event = CGEvent::new(source.as_deref());
        Self {
            position: CGEvent::location(event.as_deref()),
            source,
            held: Vec::new(),
            modifiers: Vec::new(),
            clicks: [None; 3],
            double_click: Duration::from_secs_f64(NSEvent::doubleClickInterval()),
            wheel: (0.0, 0.0),
            display_id: None,
            layout: None,
            text_layout: text::LayoutKeys::default(),
            text_until: None,
        }
    }

    /// The flags an event carries: the modifiers held here, Caps Lock as the HID system has it,
    /// and what the key itself carries (`extra`).
    fn flags(&self, extra: Flags) -> Flags {
        let caps = if CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).contains(Flags::MaskAlphaShift) { Flags::MaskAlphaShift } else { Flags::empty() };
        held_flags(&self.modifiers) | caps | extra
    }

    fn display_bounds(&self) -> CGRect {
        let display = self.display_id.filter(|&id| CGDisplayIsActive(id)).unwrap_or_else(|| CGMainDisplayID());
        CGDisplayBounds(display)
    }

    fn post_mouse(&self, kind: Kind, button: &Button, click_count: i64, delta: Option<(f64, f64)>) {
        if let Some(event) = CGEvent::new_mouse_event(self.source.as_deref(), kind, self.position, button.quartz) {
            CGEvent::set_flags(Some(&event), self.flags(Flags::empty()));
            CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, click_count);
            if let Some((dx, dy)) = delta {
                // Games and 3D programs read the motion from these, not from the position.
                CGEvent::set_double_value_field(Some(&event), CGEventField::MouseEventDeltaX, dx);
                CGEvent::set_double_value_field(Some(&event), CGEventField::MouseEventDeltaY, dy);
            }
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }

    /// Moves the pointer, dragging with the button held, if any.
    fn move_to(&mut self, position: CGPoint, delta: Option<(f64, f64)>) {
        if position == self.position && delta.is_none() {
            return;
        }
        self.position = position;
        if let Some(&button) = self.held.last() {
            if let Some(click) = &mut self.clicks[button] {
                if !near(click.position, position) {
                    click.dragged = true;
                }
            }
            let count = self.clicks[button].map_or(1, |click| click.count);
            self.post_mouse(BUTTONS[button].drag, &BUTTONS[button], count, delta);
        } else {
            self.post_mouse(Kind::MouseMoved, &BUTTONS[0], 0, delta);
        }
    }

    /// The layout, loaded again when the system says it changed.
    fn layout(&mut self) -> Option<&Layout> {
        let generation = LAYOUT_GENERATION.load(Ordering::Acquire);
        if self.layout.as_ref().is_none_or(|l| l.generation != generation) {
            let (id, data) = current_layout().unwrap_or_default();
            self.layout = Some(Layout { generation, id, data });
        }
        self.layout.as_ref()
    }

    fn finish_text(&mut self) {
        if let Some(until) = self.text_until.take() {
            text::finish(until);
            self.layout = None;
        }
    }
}

impl Backend for MacInput {
    fn mouse_move(&mut self, x: f64, y: f64) {
        let position = display_point(self.display_bounds(), x, y);
        self.move_to(position, None);
    }

    fn mouse_move_by(&mut self, dx: f64, dy: f64) {
        let b = self.display_bounds();
        let position = CGPoint::new(
            (self.position.x + dx).clamp(b.origin.x, b.origin.x + (b.size.width - 1.0).max(0.0)),
            (self.position.y + dy).clamp(b.origin.y, b.origin.y + (b.size.height - 1.0).max(0.0)),
        );
        self.move_to(position, Some((dx, dy)));
    }

    fn set_display(&mut self, display: &DisplayInfo) {
        self.display_id = super::displays::cg_id(&display.id);
    }

    fn mouse_button(&mut self, button: u8, down: bool) {
        let index = usize::from(button);
        let Some(button) = BUTTONS.get(index) else {
            return;
        };
        let count = if down {
            if self.held.contains(&index) {
                return;
            }
            let now = Instant::now();
            let count = self.clicks[index]
                .filter(|click| !click.dragged && now.duration_since(click.time) <= self.double_click && near(click.position, self.position))
                .map_or(1, |click| click.count + 1);
            self.clicks[index] = Some(Click { position: self.position, time: now, count, dragged: false });
            self.held.push(index);
            count
        } else {
            let Some(i) = self.held.iter().position(|&b| b == index) else {
                return;
            };
            self.held.remove(i);
            self.clicks[index].map_or(1, |click| click.count)
        };
        self.post_mouse(if down { button.down } else { button.up }, button, count, None);
    }

    fn wheel(&mut self, dx: f64, dy: f64) {
        // Quartz uses positive deltas for up and left.
        self.wheel.0 = (self.wheel.0 - dx).clamp(i32::MIN as f64, i32::MAX as f64);
        self.wheel.1 = (self.wheel.1 - dy).clamp(i32::MIN as f64, i32::MAX as f64);
        let (x, y) = (self.wheel.0 as i32, self.wheel.1 as i32);
        self.wheel.0 -= f64::from(x);
        self.wheel.1 -= f64::from(y);
        if (x, y) != (0, 0) {
            if let Some(event) = CGEvent::new_scroll_wheel_event2(self.source.as_deref(), CGScrollEventUnit::Pixel, 2, y, x, 0) {
                CGEvent::set_flags(Some(&event), self.flags(Flags::empty()));
                CGEvent::set_location(Some(&event), self.position);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
            }
        }
    }

    fn key(&mut self, key: &Key, down: bool, repeat: bool) -> bool {
        self.finish_text();
        if key.mac == NO {
            return false;
        }
        if key.mac == CAPS_LOCK {
            // A posted Caps Lock key does not toggle the lock; the HID system does.
            if down && !repeat {
                let on = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState).contains(Flags::MaskAlphaShift);
                set_caps_lock(!on);
            }
            return true;
        }
        let modifier = is_modifier(key.mac);
        if repeat && modifier {
            return true;
        }
        // A modifier's own event carries its flag going down and not coming up, as a keyboard's.
        if modifier {
            self.modifiers.retain(|&m| m != key.mac);
            if down {
                self.modifiers.push(key.mac);
            }
        }
        let Some(event) = CGEvent::new_keyboard_event(self.source.as_deref(), key.mac, down) else {
            return true;
        };
        if repeat {
            CGEvent::set_integer_value_field(Some(&event), CGEventField::KeyboardEventAutorepeat, 1);
        }
        CGEvent::set_flags(Some(&event), self.flags(intrinsic_flags(key.code)));
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        true
    }

    fn text(&mut self, text: &str) {
        self.text_until = text::send(text, &mut self.text_layout, self.source.as_deref());
    }

    fn key_state(&mut self) -> KeyState {
        let f = CGEventSource::flags_state(CGEventSourceStateID::HIDSystemState);
        KeyState {
            caps: Some(f.contains(Flags::MaskAlphaShift)),
            // A Mac has neither Num Lock nor Scroll Lock.
            num: None,
            scroll: None,
            mods: Some(Mods {
                shift: f.contains(Flags::MaskShift),
                ctrl: f.contains(Flags::MaskControl),
                alt: f.contains(Flags::MaskAlternate),
                meta: f.contains(Flags::MaskCommand),
            }),
        }
    }

    fn set_lock(&mut self, lock: Lock, on: bool) {
        if lock == Lock::Caps {
            set_caps_lock(on);
        }
    }

    fn layout_id(&mut self) -> Option<u64> {
        self.finish_text();
        self.layout().map(|l| l.generation)
    }

    fn layout_name(&mut self) -> String {
        let id = self.layout().map(|l| l.id.clone()).unwrap_or_default();
        id.rsplit('.').next().unwrap_or_default().to_string()
    }

    fn translate(&mut self, key: &Key, mods: &Mods) -> Option<Typed> {
        if key.mac == NO {
            return None;
        }
        translate_uchr(&self.layout()?.data, key.mac, mods)
    }
}

fn near(a: CGPoint, b: CGPoint) -> bool {
    (a.x - b.x).abs() <= 4.0 && (a.y - b.y).abs() <= 4.0
}

fn display_point(bounds: CGRect, x: f64, y: f64) -> CGPoint {
    // Quartz and browser coordinates both run down from the top edge.
    // Clamp the lower/right edges to the last point inside the display.
    fn axis(origin: f64, size: f64, value: f64) -> f64 {
        let value = if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) };
        origin + (size * value).min((size - 1.0).max(0.0))
    }
    CGPoint::new(axis(bounds.origin.x, bounds.size.width, x), axis(bounds.origin.y, bounds.size.height, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::CGSize;

    #[test]
    fn maps_display_points_with_origin_and_clamps_edges() {
        let bounds = CGRect::new(CGPoint::new(-100.0, 50.0), CGSize::new(1920.0, 1080.0));
        assert_eq!(display_point(bounds, 0.5, 0.5), CGPoint::new(860.0, 590.0));
        assert_eq!(display_point(bounds, -0.2, f64::NAN), bounds.origin);
        assert_eq!(display_point(bounds, 1.0, 1.3), CGPoint::new(1819.0, 1129.0));
    }

    #[test]
    fn carbon_modifier_bits() {
        let m = |shift, ctrl, alt, meta| Mods { shift, ctrl, alt, meta };
        assert_eq!(carbon_modifiers(&m(true, false, false, false)), 2);
        assert_eq!(carbon_modifiers(&m(false, false, true, false)), 8);
        assert_eq!(carbon_modifiers(&m(false, true, false, true)), 17);
    }

    /// The flags are the modifiers held here, never what the system remembers: Control and
    /// Command released here leave a plain F plain.
    #[test]
    fn events_carry_the_modifiers_held_here() {
        assert_eq!(held_flags(&[]), Flags::empty());
        assert_eq!(held_flags(&[0x3b, 0x37]), Flags::MaskControl | Flags::MaskCommand);
        assert_eq!(held_flags(&[0x3e, 0x36, 0x3c, 0x3d]), Flags::MaskControl | Flags::MaskCommand | Flags::MaskShift | Flags::MaskAlternate);
        assert_eq!(held_flags(&[0x3f]), Flags::MaskSecondaryFn);
        assert_eq!(modifier_flag(0x03), Flags::empty(), "F is no modifier");
    }

    #[test]
    fn keys_carry_their_own_flags_only() {
        assert_eq!(intrinsic_flags("ArrowUp"), Flags::MaskNumericPad | Flags::MaskSecondaryFn);
        assert_eq!(intrinsic_flags("Numpad5"), Flags::MaskNumericPad);
        assert_eq!(intrinsic_flags("F12"), Flags::MaskSecondaryFn);
        assert_eq!(intrinsic_flags("KeyF"), Flags::empty());
        assert_eq!(intrinsic_flags("Fn"), Flags::empty());
    }
}
