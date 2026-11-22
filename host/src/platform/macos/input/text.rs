//! Delivers committed text while a plain keyboard layout is selected. A complete Unicode
//! scalar travels in each event, including both UTF-16 units of a supplementary character.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use dispatch2::{DispatchQueue, DispatchTime};
use objc2_core_foundation::{CFRetained, CFString, CFType};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventTapLocation};

use crate::input::keys::{KEYS, NO};
use crate::input::no_mods;
use wire::types::Mods;

use super::{kTISPropertyInputSourceID, read_layout, translate_uchr, TISGetInputSourceProperty};

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> *mut c_void;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISSelectInputSource(source: *const c_void) -> i32;
}

// WebKit drops Unicode sent with an unknown key code. A valid key code carries the
// committed characters even when the active keyboard layout has no key for them.
const UNICODE_KEY: u16 = 0; // kVK_ANSI_A
const RETURN_KEY: u16 = 0x24;
// Quartz posts asynchronously. Keep the layout selected across a burst of text so queued
// events have time to reach the focused app before its input method is enabled again.
const TEXT_SETTLE: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct LayoutKeys {
    data: Vec<u8>,
    keys: HashMap<char, u16>,
}

impl LayoutKeys {
    fn update(&mut self, data: Vec<u8>) {
        if data == self.data {
            return;
        }
        self.data = data;
        self.keys.clear();
        for (shift, alt) in [(false, false), (true, false), (false, true), (true, true)] {
            let mods = Mods { shift, alt, ..no_mods() };
            for key in KEYS.iter().filter(|k| k.mac != NO) {
                if let Some(typed) = translate_uchr(&self.data, key.mac, &mods).filter(|t| !t.dead) {
                    let mut chars = typed.text.chars();
                    if let (Some(ch), None) = (chars.next(), chars.next()) {
                        self.keys.entry(ch).or_insert(key.mac);
                    }
                }
            }
        }
    }
}

struct Selection<S> {
    previous: S,
    temporary: String,
    // No deadline while the input worker is posting a new batch.
    until: Option<Instant>,
}

thread_local! {
    // The main queue owns the retained Text Input Source references and their restoration.
    static SELECTION: RefCell<Option<Selection<CFRetained<CFType>>>> = const { RefCell::new(None) };
}

fn input_source(copy: unsafe extern "C" fn() -> *mut c_void) -> Option<CFRetained<CFType>> {
    let source = NonNull::new(unsafe { copy() })?;
    // Both TIS copy functions return an owned Core Foundation reference.
    Some(unsafe { CFRetained::from_raw(source.cast()) })
}

fn source_id(source: &CFRetained<CFType>) -> Option<String> {
    let raw = CFRetained::as_ptr(source).as_ptr().cast();
    let id = unsafe { TISGetInputSourceProperty(raw, kTISPropertyInputSourceID) };
    unsafe { (id as *const CFString).as_ref() }.map(ToString::to_string)
}

fn select(source: &CFRetained<CFType>) -> bool {
    let status = unsafe { TISSelectInputSource(CFRetained::as_ptr(source).as_ptr().cast()) };
    if status != 0 {
        eprintln!("input: could not select the keyboard input source: {status}");
    }
    status == 0
}

fn ascii_selection(selection: &mut Option<Selection<CFRetained<CFType>>>) {
    let Some(current) = input_source(TISCopyCurrentKeyboardInputSource) else { return };
    let Some(id) = source_id(&current) else { return };
    if let Some(held) = selection.as_mut().filter(|held| held.temporary == id) {
        held.until = None;
        return;
    }
    // If the person already selected another source, their new choice is the one to keep.
    *selection = None;
    let Some(ascii) = input_source(TISCopyCurrentASCIICapableKeyboardLayoutInputSource) else { return };
    let Some(temporary) = source_id(&ascii) else { return };
    if id != temporary && select(&ascii) {
        *selection = Some(Selection { previous: current, temporary, until: None });
    }
}

fn due<S>(selection: &Option<Selection<S>>, now: Instant) -> bool {
    selection.as_ref().and_then(|held| held.until).is_some_and(|until| now >= until)
}

fn take_due<S>(selection: &mut Option<Selection<S>>, current: Option<&str>, now: Instant) -> Option<S> {
    if due(selection, now) {
        return selection.take().filter(|held| current == Some(&held.temporary)).map(|held| held.previous);
    }
    None
}

fn restore() {
    SELECTION.with_borrow_mut(|selection| {
        if !due(selection, Instant::now()) {
            return;
        }
        let current = input_source(TISCopyCurrentKeyboardInputSource).as_ref().and_then(source_id);
        if let Some(previous) = take_due(selection, current.as_deref(), Instant::now()) {
            select(&previous);
        }
    });
}

/// Post from the input worker, using the main queue only to manage the input source.
pub(super) fn send(text: &str, layout: &mut LayoutKeys, source: Option<&CGEventSource>) -> Option<Instant> {
    let mut held_source = false;
    let mut data = Vec::new();
    let main = DispatchQueue::main();
    main.exec_sync(|| {
        SELECTION.with_borrow_mut(|selection| {
            ascii_selection(selection);
            held_source = selection.is_some();
        });
        // Key codes must describe the layout we will actually post under.
        if let Some((_, active)) = read_layout() {
            data = active;
        }
    });
    layout.update(data);
    keystrokes(text, &layout.keys, |key, units| post(source, key, units));
    if !held_source {
        return None;
    }
    let until = Instant::now() + TEXT_SETTLE;
    main.exec_async(move || {
        SELECTION.with_borrow_mut(|selection| {
            if let Some(held) = selection {
                held.until = Some(until);
            }
        });
        let remaining = until.saturating_duration_since(Instant::now());
        let _ = DispatchQueue::main().after(DispatchTime::try_from(remaining).unwrap(), restore);
    });
    Some(until)
}

/// Real key presses and layout queries must see the user's layout again.
pub(super) fn finish(until: Instant) {
    std::thread::sleep(until.saturating_duration_since(Instant::now()));
    DispatchQueue::main().exec_sync(restore);
}

fn post(source: Option<&CGEventSource>, key: u16, units: &[u16]) {
    for down in [true, false] {
        if let Some(event) = event(source, key, down, units) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
}

fn event(source: Option<&CGEventSource>, key: u16, down: bool, units: &[u16]) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new_keyboard_event(source, key, down)?;
    // Committed text carries no shortcut modifiers. Quartz copies the given UTF-16 units.
    CGEvent::set_flags(Some(&event), CGEventFlags::empty());
    unsafe { CGEvent::keyboard_set_unicode_string(Some(&event), units.len() as _, units.as_ptr()) };
    Some(event)
}

fn keystrokes(text: &str, keys: &HashMap<char, u16>, mut emit: impl FnMut(u16, &[u16])) {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        // A physical Return is an editing command. Normalize CRLF to one press.
        if matches!(ch, '\r' | '\n') {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            emit(RETURN_KEY, &[b'\r' as u16]);
        } else {
            emit(keys.get(&ch).copied().unwrap_or(UNICODE_KEY), ch.encode_utf16(&mut [0; 2]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_text_uses_valid_keys_and_one_return_per_line_end() {
        let keys = HashMap::from([('L', 37)]);
        let mut sent = Vec::new();
        keystrokes("中文L\n🙂\r\n末\r", &keys, |key, units| {
            sent.push((key, String::from_utf16(units).unwrap()));
        });
        assert_eq!(sent, [
            (UNICODE_KEY, "中".into()), (UNICODE_KEY, "文".into()), (37, "L".into()),
            (RETURN_KEY, "\r".into()), (UNICODE_KEY, "🙂".into()),
            (RETURN_KEY, "\r".into()), (UNICODE_KEY, "末".into()), (RETURN_KEY, "\r".into()),
        ]);
    }

    #[test]
    fn quartz_events_preserve_the_key_and_utf16_payload() {
        use objc2_core_graphics::CGEventField;

        for text in ["中", "🙂"] {
            let mut units = [0; 2];
            let units = text.chars().next().unwrap().encode_utf16(&mut units);
            for down in [true, false] {
                let event = event(None, UNICODE_KEY, down, units).unwrap();
                let key = CGEvent::integer_value_field(Some(&event), CGEventField::KeyboardEventKeycode);
                let mut actual = 0;
                let mut buf = [0; 2];
                unsafe { CGEvent::keyboard_get_unicode_string(Some(&event), buf.len() as _, &mut actual, buf.as_mut_ptr()) };
                assert_eq!(key, i64::from(UNICODE_KEY));
                assert_eq!(actual as usize, units.len());
                assert_eq!(String::from_utf16(&buf[..actual as usize]).unwrap(), text);
            }
        }
    }

    #[test]
    fn extending_a_text_burst_keeps_the_previous_source_until_the_last_post() {
        let now = Instant::now();
        let mut selection = Some(Selection { previous: "Pinyin", temporary: "ABC".into(), until: None });
        assert_eq!(take_due(&mut selection, Some("ABC"), now), None);
        selection.as_mut().unwrap().until = Some(now + TEXT_SETTLE);
        assert_eq!(take_due(&mut selection, Some("ABC"), now), None);
        assert_eq!(take_due(&mut selection, Some("ABC"), now + TEXT_SETTLE), Some("Pinyin"));
        assert!(selection.is_none());
    }

    #[test]
    fn a_manual_source_change_takes_precedence_over_restoration() {
        let now = Instant::now();
        let mut selection = Some(Selection { previous: "Pinyin", temporary: "ABC".into(), until: Some(now) });
        assert_eq!(take_due(&mut selection, Some("Japanese"), now), None);
        assert!(selection.is_none());
    }
}
