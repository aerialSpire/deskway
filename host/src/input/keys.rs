//! Every key a browser names (`KeyboardEvent.code`), with its native code on each platform:
//! Windows PC set-1 scan codes (0xE0xx = extended) and macOS HIToolbox virtual key codes.
//! Values follow Chromium's `dom_code_data.inc` (the USB HID usage table as browsers see it).
//! Keys follow the physical position, so the host's own layout and input method still apply.
//!
//! The table is plain data on purpose: both columns are checked by unit tests on every
//! platform, and character resolution searches it in order (main block before the keypad).

/// No native key on that platform.
pub const NO: u16 = 0xffff;

/// One key: its browser name, Windows scan code and macOS virtual key code.
#[derive(Debug, PartialEq, Eq)]
pub struct Key {
    pub code: &'static str,
    pub win: u16,
    pub mac: u16,
}

const fn k(code: &'static str, win: u16, mac: u16) -> Key {
    Key { code, win, mac }
}

/// Row order matters for character resolution: the first key that types a character wins.
pub static KEYS: &[Key] = &[
    // Letters.
    k("KeyA", 0x1e, 0x00), k("KeyB", 0x30, 0x0b), k("KeyC", 0x2e, 0x08), k("KeyD", 0x20, 0x02),
    k("KeyE", 0x12, 0x0e), k("KeyF", 0x21, 0x03), k("KeyG", 0x22, 0x05), k("KeyH", 0x23, 0x04),
    k("KeyI", 0x17, 0x22), k("KeyJ", 0x24, 0x26), k("KeyK", 0x25, 0x28), k("KeyL", 0x26, 0x25),
    k("KeyM", 0x32, 0x2e), k("KeyN", 0x31, 0x2d), k("KeyO", 0x18, 0x1f), k("KeyP", 0x19, 0x23),
    k("KeyQ", 0x10, 0x0c), k("KeyR", 0x13, 0x0f), k("KeyS", 0x1f, 0x01), k("KeyT", 0x14, 0x11),
    k("KeyU", 0x16, 0x20), k("KeyV", 0x2f, 0x09), k("KeyW", 0x11, 0x0d), k("KeyX", 0x2d, 0x07),
    k("KeyY", 0x15, 0x10), k("KeyZ", 0x2c, 0x06),
    // Digits.
    k("Digit1", 0x02, 0x12), k("Digit2", 0x03, 0x13), k("Digit3", 0x04, 0x14), k("Digit4", 0x05, 0x15),
    k("Digit5", 0x06, 0x17), k("Digit6", 0x07, 0x16), k("Digit7", 0x08, 0x1a), k("Digit8", 0x09, 0x1c),
    k("Digit9", 0x0a, 0x19), k("Digit0", 0x0b, 0x1d),
    // Punctuation and the international keys of ISO, JIS and ABNT keyboards.
    k("Minus", 0x0c, 0x1b), k("Equal", 0x0d, 0x18), k("BracketLeft", 0x1a, 0x21),
    k("BracketRight", 0x1b, 0x1e), k("Backslash", 0x2b, 0x2a), k("Semicolon", 0x27, 0x29),
    k("Quote", 0x28, 0x27), k("Backquote", 0x29, 0x32), k("Comma", 0x33, 0x2b),
    k("Period", 0x34, 0x2f), k("Slash", 0x35, 0x2c), k("IntlBackslash", 0x56, 0x0a),
    k("IntlRo", 0x73, 0x5e), k("IntlYen", 0x7d, 0x5d),
    // Editing and whitespace.
    k("Space", 0x39, 0x31), k("Enter", 0x1c, 0x24), k("Tab", 0x0f, 0x30), k("Backspace", 0x0e, 0x33),
    k("Escape", 0x01, 0x35),
    // Modifiers and locks.
    k("ShiftLeft", 0x2a, 0x38), k("ShiftRight", 0x36, 0x3c), k("ControlLeft", 0x1d, 0x3b),
    k("ControlRight", 0xe01d, 0x3e), k("AltLeft", 0x38, 0x3a), k("AltRight", 0xe038, 0x3d),
    k("MetaLeft", 0xe05b, 0x37), k("MetaRight", 0xe05c, 0x36), k("CapsLock", 0x3a, 0x39),
    k("Fn", NO, 0x3f), k("ContextMenu", 0xe05d, 0x6e),
    // Function keys. A PC keyboard on a Mac reports PrintScreen, ScrollLock and Pause as F13–F15.
    k("F1", 0x3b, 0x7a), k("F2", 0x3c, 0x78), k("F3", 0x3d, 0x63), k("F4", 0x3e, 0x76),
    k("F5", 0x3f, 0x60), k("F6", 0x40, 0x61), k("F7", 0x41, 0x62), k("F8", 0x42, 0x64),
    k("F9", 0x43, 0x65), k("F10", 0x44, 0x6d), k("F11", 0x57, 0x67), k("F12", 0x58, 0x6f),
    k("F13", 0x64, 0x69), k("F14", 0x65, 0x6b), k("F15", 0x66, 0x71), k("F16", 0x67, 0x6a),
    k("F17", 0x68, 0x40), k("F18", 0x69, 0x4f), k("F19", 0x6a, 0x50), k("F20", 0x6b, 0x5a),
    k("F21", 0x6c, NO), k("F22", 0x6d, NO), k("F23", 0x6e, NO), k("F24", 0x76, NO),
    k("PrintScreen", 0xe037, 0x69), k("ScrollLock", 0x46, 0x6b), k("Pause", 0x45, 0x71),
    // Navigation. Mac keyboards have Help where PC ones have Insert.
    k("Insert", 0xe052, 0x72), k("Help", 0xe03b, 0x72), k("Delete", 0xe053, 0x75),
    k("Home", 0xe047, 0x73), k("End", 0xe04f, 0x77), k("PageUp", 0xe049, 0x74),
    k("PageDown", 0xe051, 0x79), k("ArrowUp", 0xe048, 0x7e), k("ArrowDown", 0xe050, 0x7d),
    k("ArrowLeft", 0xe04b, 0x7b), k("ArrowRight", 0xe04d, 0x7c),
    // Keypad. A Mac keypad has Clear where a PC one has Num Lock.
    k("NumLock", 0xe045, 0x47), k("NumpadClear", NO, 0x47), k("Numpad0", 0x52, 0x52),
    k("Numpad1", 0x4f, 0x53), k("Numpad2", 0x50, 0x54), k("Numpad3", 0x51, 0x55),
    k("Numpad4", 0x4b, 0x56), k("Numpad5", 0x4c, 0x57), k("Numpad6", 0x4d, 0x58),
    k("Numpad7", 0x47, 0x59), k("Numpad8", 0x48, 0x5b), k("Numpad9", 0x49, 0x5c),
    k("NumpadDecimal", 0x53, 0x41), k("NumpadDivide", 0xe035, 0x4b),
    k("NumpadMultiply", 0x37, 0x43), k("NumpadSubtract", 0x4a, 0x4e), k("NumpadAdd", 0x4e, 0x45),
    k("NumpadEnter", 0xe01c, 0x4c), k("NumpadEqual", 0x59, 0x51), k("NumpadComma", 0x7e, 0x5f),
    // Input method keys (Japanese, Korean).
    k("KanaMode", 0x70, 0x68), k("Lang1", 0x72, 0x68), k("Lang2", 0x71, 0x66),
    // (Lang5 shares its scan code with F24 and is left out.)
    k("Lang3", 0x78, NO), k("Lang4", 0x77, NO),
    k("Convert", 0x79, NO), k("NonConvert", 0x7b, NO),
    // Media, volume and browser keys.
    k("AudioVolumeMute", 0xe020, 0x4a), k("AudioVolumeDown", 0xe02e, 0x49),
    k("AudioVolumeUp", 0xe030, 0x48), k("MediaTrackNext", 0xe019, NO),
    k("MediaTrackPrevious", 0xe010, NO), k("MediaStop", 0xe024, NO),
    k("MediaPlayPause", 0xe022, NO), k("MediaSelect", 0xe06d, NO), k("Eject", 0xe02c, NO),
    k("LaunchMail", 0xe06c, NO), k("LaunchApp1", 0xe06b, NO), k("LaunchApp2", 0xe021, NO),
    k("BrowserSearch", 0xe065, NO), k("BrowserHome", 0xe032, NO), k("BrowserBack", 0xe06a, NO),
    k("BrowserForward", 0xe069, NO), k("BrowserStop", 0xe068, NO),
    k("BrowserRefresh", 0xe067, NO), k("BrowserFavorites", 0xe066, NO),
    // Editing keys of some keyboards, and power.
    k("Undo", 0xe008, NO), k("Paste", 0xe00a, NO), k("Cut", 0xe017, NO), k("Copy", 0xe018, NO),
    k("Power", 0xe05e, NO), k("Sleep", 0xe05f, NO), k("WakeUp", 0xe063, NO),
];

/// The key a browser code names, if any.
pub fn find(code: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| k.code == code)
}

/// The modifier keys by state they set, left first (the one pressed when a state is wanted).
pub const SHIFT: [&str; 2] = ["ShiftLeft", "ShiftRight"];
pub const CONTROL: [&str; 2] = ["ControlLeft", "ControlRight"];
pub const ALT: [&str; 2] = ["AltLeft", "AltRight"];
pub const META: [&str; 2] = ["MetaLeft", "MetaRight"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn codes_are_unique_and_every_key_exists_somewhere() {
        let mut seen = HashSet::new();
        for key in KEYS {
            assert!(seen.insert(key.code), "{} twice", key.code);
            assert!(key.win != NO || key.mac != NO, "{} has no native key", key.code);
        }
    }

    #[test]
    fn windows_scan_codes_are_distinct_set_1_codes() {
        let mut seen = HashSet::new();
        for key in KEYS.iter().filter(|k| k.win != NO) {
            assert!(key.win & 0xff != 0 && (key.win >> 8 == 0 || key.win >> 8 == 0xe0), "{}: {:#x}", key.code, key.win);
            assert!(seen.insert(key.win), "{}: scan code {:#x} twice", key.code, key.win);
        }
    }

    #[test]
    fn mac_virtual_keys_are_within_the_hitoolbox_range() {
        for key in KEYS.iter().filter(|k| k.mac != NO) {
            assert!(key.mac <= 0x7f, "{}: {:#x}", key.code, key.mac);
        }
        // Keys that share a Mac key by design (Help/Insert, Clear/NumLock, the Kana keys, the
        // PC keys a Mac reads as F13–F15).
        let shared: HashSet<&str> = ["Help", "Insert", "NumLock", "NumpadClear", "KanaMode", "Lang1", "F13", "PrintScreen", "F14", "ScrollLock", "F15", "Pause"].into();
        let mut seen = std::collections::HashMap::new();
        for key in KEYS.iter().filter(|k| k.mac != NO && !shared.contains(k.code)) {
            assert!(seen.insert(key.mac, key.code).is_none(), "{}: mac key {:#x} twice", key.code, key.mac);
        }
    }

    #[test]
    fn known_keys_map_as_chromium_does() {
        for (code, win, mac) in [
            ("KeyA", 0x1e, 0x00), ("Digit9", 0x0a, 0x19), ("IntlBackslash", 0x56, 0x0a),
            ("IntlYen", 0x7d, 0x5d), ("IntlRo", 0x73, 0x5e), ("Lang1", 0x72, 0x68),
            ("Lang2", 0x71, 0x66), ("NumpadEnter", 0xe01c, 0x4c), ("NumpadEqual", 0x59, 0x51),
            ("NumpadComma", 0x7e, 0x5f), ("F13", 0x64, 0x69), ("F24", 0x76, NO),
            ("F20", 0x6b, 0x5a), ("Pause", 0x45, 0x71), ("PrintScreen", 0xe037, 0x69),
            ("NumLock", 0xe045, 0x47), ("ArrowUp", 0xe048, 0x7e), ("Delete", 0xe053, 0x75),
            ("ControlLeft", 0x1d, 0x3b), ("MetaLeft", 0xe05b, 0x37), ("AltRight", 0xe038, 0x3d),
            ("AudioVolumeUp", 0xe030, 0x48), ("MediaPlayPause", 0xe022, NO),
            ("BrowserBack", 0xe06a, NO), ("Convert", 0x79, NO), ("KanaMode", 0x70, 0x68),
        ] {
            let key = find(code).unwrap_or_else(|| panic!("{code} missing"));
            assert_eq!((key.win, key.mac), (win, mac), "{code}");
        }
        assert!(find("Unidentified").is_none());
    }

    #[test]
    fn control_and_command_keep_their_own_keys() {
        // Keys pass through as positions: Ctrl is Control and Meta is Win/Command everywhere.
        assert_eq!(find("ControlLeft").unwrap().mac, 0x3b);
        assert_eq!(find("MetaLeft").unwrap().mac, 0x37);
        assert_eq!(find("MetaLeft").unwrap().win, 0xe05b);
    }
}
