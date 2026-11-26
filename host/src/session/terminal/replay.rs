//! Recent output, trimmed only at UTF-8 and terminal parser boundaries.

use std::collections::VecDeque;
use std::fmt::Write;

pub(super) const LIMIT: usize = 256 * 1024;
const MODE_LIMIT: usize = 64;

#[derive(Default, PartialEq)]
enum Escape {
    #[default]
    Ground,
    Esc,
    Intermediate,
    Csi,
    CsiPrivate,
    CsiOther,
    Osc,
    String,
}

#[derive(Default)]
struct Boundary {
    escape: Escape,
    utf8: u8,
    c1: bool,
    params: Vec<u32>,
    modes: VecDeque<(u32, bool)>,
}

impl Boundary {
    fn ground(&self) -> bool {
        self.utf8 == 0 && self.escape == Escape::Ground
    }

    fn advance(&mut self, byte: u8) {
        if (0x80..=0xbf).contains(&byte) {
            if self.utf8 != 0 {
                self.utf8 -= 1;
                if self.c1 && byte < 0xa0 {
                    self.control(byte);
                }
            }
            return;
        }
        self.c1 = byte == 0xc2;
        self.utf8 = match byte {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => 0,
        };
        if byte < 0x80 {
            self.control(byte);
        }
    }

    // Track the ground transitions of the VT parser. Strings include OSC, DCS, SOS, PM
    // and APC. ST can be ESC backslash or the UTF-8 encoding of the C1 control.
    fn control(&mut self, byte: u8) {
        use Escape::*;
        self.escape = match (&self.escape, byte) {
            (_, 0x1b) => Esc,
            (_, 0x18 | 0x1a | 0x80..=0x8f | 0x91..=0x97 | 0x99 | 0x9a | 0x9c)
            | (Osc, 0x07) => Ground,
            (_, 0x9b) | (Esc, b'[') => Csi,
            (_, 0x9d) | (Esc, b']') => Osc,
            (_, 0x90 | 0x98 | 0x9e | 0x9f) | (Esc, b'P' | b'X' | b'^' | b'_') => String,
            (Esc, 0x20..=0x2f) => Intermediate,
            (Esc, b'c') => {
                self.modes.clear();
                Ground
            }
            (Csi, b'?') => {
                self.params.clear();
                self.params.push(0);
                CsiPrivate
            }
            (CsiPrivate, b'0'..=b'9') => {
                let param = self.params.last_mut().unwrap();
                *param = param.saturating_mul(10).saturating_add(u32::from(byte - b'0'));
                CsiPrivate
            }
            (CsiPrivate, b';') if self.params.len() < MODE_LIMIT => {
                self.params.push(0);
                CsiPrivate
            }
            (CsiPrivate, b'h' | b'l') => {
                // Keep the latest setting in order: different numbers can control the
                // same emulator state, such as the mouse reporting protocol.
                for &mode in &self.params {
                    self.modes.retain(|&(number, _)| number != mode);
                    if self.modes.len() == MODE_LIMIT {
                        self.modes.pop_front();
                    }
                    self.modes.push_back((mode, byte == b'h'));
                }
                Ground
            }
            (Csi | CsiPrivate, 0x20..=0x3f) => CsiOther,
            (Esc | Intermediate, 0x30..=0x7e) | (Csi | CsiPrivate | CsiOther, 0x40..=0x7e) => Ground,
            _ => return,
        };
    }
}

#[derive(Default)]
pub(super) struct History {
    pub(super) bytes: VecDeque<u8>,
    pub(super) dropped: bool,
    head: Boundary,
}

impl History {
    pub(super) fn mode_prefix(&self) -> Vec<u8> {
        let mut prefix = String::new();
        for &(mode, set) in &self.head.modes {
            let final_byte = if set { 'h' } else { 'l' };
            write!(prefix, "\x1b[?{mode}{final_byte}").unwrap();
        }
        prefix.into_bytes()
    }

    pub(super) fn append(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.bytes.len() == LIMIT {
                self.head.advance(self.bytes.pop_front().unwrap());
                self.dropped = true;
            }
            while !self.head.ground() && !self.bytes.is_empty() {
                self.head.advance(self.bytes.pop_front().unwrap());
            }
            if self.head.ground() {
                self.bytes.push_back(byte);
            } else {
                // A sequence can be longer than the whole buffer.
                self.head.advance(byte);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_keeps_whole_characters_and_sequences() {
        for prefix in [
            &b"x"[..], "é".as_bytes(), "😀".as_bytes(), b"\x1b[12;40H", b"\x1b(B",
            b"\x1b]2;title\x07", b"\x1b]2;title\x1b\\", b"\x1bP1$qtext\x1b\\",
            "\u{009b}31m".as_bytes(), "\u{009d}0;title\u{009c}".as_bytes(),
        ] {
            let mut replay = History::default();
            let tail = vec![b'x'; LIMIT + 1 - prefix.len()];
            for byte in prefix {
                replay.append(&[*byte]);
            }
            replay.append(&tail[..tail.len() - 1]);
            assert_eq!(replay.bytes.len(), LIMIT);
            assert!(!replay.dropped);
            replay.append(&tail[tail.len() - 1..]);
            assert!(replay.dropped, "{prefix:?}");
            assert!(replay.bytes.iter().copied().eq(tail), "{prefix:?}");
        }
    }

    #[test]
    fn a_string_longer_than_the_buffer_is_discarded_through_its_terminator() {
        let mut replay = History::default();
        replay.append(b"\x1b]2;");
        replay.append(&vec![b'x'; LIMIT * 2]);
        assert!(replay.dropped);
        assert!(replay.bytes.is_empty());
        replay.append(b"\x1b");
        replay.append(b"\\prompt");
        assert!(replay.bytes.iter().copied().eq(*b"prompt"));
    }

    #[test]
    fn replay_restores_private_modes_at_its_start() {
        for (discarded, retained, prefix) in [
            ("\x1b[?2004h", "", "\x1b[?2004h"),
            ("\x1b[?2004h\x1b[?2004l", "", "\x1b[?2004l"),
            ("old", "\x1b[?2004h", ""),
            ("\x1b[?1;1049;1002h\x1b[?25l", "\x1b[?1049l", "\x1b[?1h\x1b[?1049h\x1b[?1002h\x1b[?25l"),
            ("\x1b[?1003h\x1b[?1002h\x1b[?1003l", "", "\x1b[?1002h\x1b[?1003l"),
            ("\u{009b}?2004h\x1b[?25\x18\x1b]text[?1h\x07", "", "\x1b[?2004h"),
            ("\x1b[?2004h\x1bc", "", ""),
        ] {
            let mut replay = History::default();
            replay.append(discarded.as_bytes());
            replay.append(&vec![b'x'; LIMIT - retained.len()]);
            replay.append(retained.as_bytes());
            assert!(replay.dropped);
            assert_eq!(replay.mode_prefix(), prefix.as_bytes(), "{discarded:?}");
            assert!(replay.bytes.range(LIMIT - retained.len()..).copied().eq(retained.bytes()));
        }
    }
}
