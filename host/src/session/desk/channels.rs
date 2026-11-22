//! What the session says and hears on the browser's data channels: "input" (reliable,
//! ordered: `Control`), "move" (neither: `Move`) and "file" (`File` and the bytes of transfers,
//! with what waits to be sent on it).

use std::collections::VecDeque;

use base64::Engine;
use serde::Serialize;
use str0m::channel::ChannelId;
use str0m::Rtc;

use crate::platform::PlatformEvent;
use wire::control::{CaptureState, Control};
use wire::file::File;
use wire::moves::Move;
use wire::types::TransferError;

/// File data kept ahead in the channel. The send buffer is shared with input and the other
/// channels, so files never fill it.
pub const FILE_AHEAD: usize = 256 * 1024;

/// A message on the "input" or "move" channel, read once. Moves come on both: the last move of
/// a motion is repeated on the reliable channel.
pub enum Incoming {
    Move(Move),
    Control(Control),
}

pub fn incoming(data: &[u8]) -> Option<Incoming> {
    if let Ok(m) = serde_json::from_slice::<Move>(data) {
        return Some(Incoming::Move(m));
    }
    serde_json::from_slice::<Control>(data).ok().map(Incoming::Control)
}

/// A control message for the shared clipboard.
pub fn is_clipboard(c: &Control) -> bool {
    matches!(c, Control::ClipShare { .. } | Control::Clipboard { .. } | Control::ClipboardRequest { .. } | Control::ClipboardData { .. } | Control::ClipboardFailed { .. })
}

pub fn capture_status(available: bool) -> Control {
    Control::Capture { state: if available { CaptureState::Ready } else { CaptureState::NoDisplay } }
}

pub fn displays_message(list: &[crate::display::DisplayInfo], current: &str) -> Control {
    Control::Displays { list: list.to_vec(), cur: current.to_string() }
}

fn text(m: &impl Serialize) -> String {
    serde_json::to_string(m).expect("channel message serializes")
}

/// A platform event as the browser hears it, and whether it goes on the reliable channel.
/// Events the session handles itself give `None`.
pub fn browser_message(e: &PlatformEvent) -> Option<(bool, String)> {
    Some(match e {
        PlatformEvent::CursorShape(s) => {
            let rgba = base64::engine::general_purpose::STANDARD.encode(&s.rgba);
            (true, text(&Control::CursorShape { w: s.w as u32, h: s.h as u32, hx: s.hx.max(0) as u32, hy: s.hy.max(0) as u32, rgba }))
        }
        PlatformEvent::CursorPosition { x, y, visible, seq } => (false, text(&Move::CursorPos { x: *x, y: *y, visible: *visible, seq: *seq })),
        PlatformEvent::LayoutChanged { name, labels, shift } => (true, text(&Control::Layout { name: name.clone(), labels: labels.clone(), shift: shift.clone() })),
        PlatformEvent::Desktop(d) => (true, text(&Control::Desk { name: d.as_str().to_string() })),
        PlatformEvent::CaptureAvailable(a) => (true, text(&capture_status(*a))),
        _ => return None,
    })
}

/// A file channel message from the browser, when it is text this host reads.
fn file_message(data: &[u8], binary: bool) -> Option<File> {
    if binary {
        return None;
    }
    serde_json::from_slice(data).ok()
}

/// The answer to a file channel message a view-only host does not take: files and pictures
/// sent to it. Clipboard answers (`put` with `gen`) and their bytes still go through: they
/// only answer a paste made at this computer. Bytes without an accepted `put` are dropped
/// by the transfer itself.
pub fn refuse_file(data: &[u8], binary: bool) -> Option<String> {
    match file_message(data, binary)? {
        File::Put { id, gen: None, .. } if !id.is_empty() && id.len() <= 64 => Some(text(&File::Err { id, why: TransferError::Unsupported })),
        _ => None,
    }
}

/// The id of a file channel `cancel` from the browser.
pub fn cancelled(data: &[u8], binary: bool) -> Option<String> {
    match file_message(data, binary)? {
        File::Cancel { id } => Some(id),
        _ => None,
    }
}

/// The id of a transfer to this computer that has ended (`done` or `err` to the browser).
pub fn transfer_ended(text: &str) -> Option<String> {
    match serde_json::from_str(text).ok()? {
        File::Done { id, .. } | File::Err { id, .. } => Some(id),
        _ => None,
    }
}

/// One message for the "file" channel, with the clipboard transfer it belongs to, if any.
struct Queued {
    binary: bool,
    data: Vec<u8>,
    of: Option<String>,
}

/// Messages for the "file" channel, sent as its buffer drains.
#[derive(Default)]
pub struct FileQueue {
    queue: VecDeque<Queued>,
    /// The clipboard data going out: bytes after its `meta` and up to its `fin` are its own.
    sending: Option<String>,
}

impl FileQueue {
    pub fn push(&mut self, text: String) {
        let of = match serde_json::from_str(&text) {
            Ok(File::Meta { id, .. }) => {
                self.sending = Some(id.clone());
                Some(id)
            }
            Ok(File::Fin { .. }) => self.sending.take(),
            _ => None,
        };
        self.queue.push_back(Queued { binary: false, data: text.into_bytes(), of });
    }

    pub fn push_bytes(&mut self, data: Vec<u8>) {
        self.queue.push_back(Queued { binary: true, data, of: self.sending.clone() });
    }

    /// The browser gave up clipboard data `id`: what is still queued of it is dropped.
    pub fn cancel(&mut self, id: &str) {
        self.queue.retain(|q| q.of.as_deref() != Some(id));
        if self.sending.as_deref() == Some(id) {
            self.sending = None;
        }
    }

    pub fn flush(&mut self, rtc: &mut Rtc, channel: Option<ChannelId>) {
        let Some(mut ch) = channel.and_then(|id| rtc.channel(id)) else { return };
        while let Some(q) = self.queue.front() {
            if ch.buffered_amount() > FILE_AHEAD {
                break;
            }
            match ch.write(q.binary, &q.data) {
                Ok(true) => {
                    self.queue.pop_front();
                }
                _ => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Desk;
    use crate::video::CursorShape;

    fn parsed(e: PlatformEvent) -> (bool, serde_json::Value) {
        let (reliable, text) = browser_message(&e).unwrap();
        (reliable, serde_json::from_str(&text).unwrap())
    }

    #[test]
    fn platform_events_become_browser_messages() {
        let (reliable, m) = parsed(PlatformEvent::CaptureAvailable(false));
        assert!(reliable);
        assert_eq!(m["v"], "no-display");
        assert_eq!(parsed(PlatformEvent::CaptureAvailable(true)).1["v"], "ready");
        assert_eq!(parsed(PlatformEvent::Desktop(Desk::Winlogon)).1["v"], "winlogon");
        let (reliable, m) = parsed(PlatformEvent::CursorPosition { x: 0.5, y: 0.25, visible: true, seq: 12 });
        assert!(!reliable, "positions go on the lossy channel");
        assert_eq!((m["t"].as_str(), m["x"].as_f64(), m["v"].as_bool(), m["s"].as_u64()), (Some("cp"), Some(0.5), Some(true), Some(12)));
        let labels: crate::input::Labels = [("KeyQ".to_string(), "a".to_string())].into();
        let shift: crate::input::Labels = [("KeyQ".to_string(), "A".to_string())].into();
        let (reliable, m) = parsed(PlatformEvent::LayoutChanged { name: "fr-FR".into(), labels, shift });
        assert!(reliable);
        assert_eq!((m["t"].as_str(), m["labels"]["KeyQ"].as_str(), m["shift"]["KeyQ"].as_str()), (Some("layout"), Some("a"), Some("A")));
        let m = parsed(PlatformEvent::CursorShape(CursorShape { w: 1, h: 1, hx: 0, hy: 0, rgba: vec![1, 2, 3, 4] })).1;
        assert_eq!((m["t"].as_str(), m["d"].as_str()), (Some("cs"), Some("AQIDBA==")));
        assert!(browser_message(&PlatformEvent::DisplaysChanged(Vec::new())).is_none());
    }

    #[test]
    fn a_view_only_host_refuses_files_by_id_but_takes_clipboard_answers() {
        let r: serde_json::Value = serde_json::from_str(&refuse_file(br#"{"t":"put","id":"p1","kind":"files","items":[]}"#, false).unwrap()).unwrap();
        assert_eq!((r["t"].as_str(), r["id"].as_str(), r["why"].as_str()), (Some("err"), Some("p1"), Some("unsupported")));
        assert!(refuse_file(b"chunk", true).is_none(), "bytes need no answer");
        assert!(refuse_file(br#"{"t":"put","id":"","kind":"files","items":[]}"#, false).is_none());
        assert!(refuse_file(br#"{"t":"put","id":"p2","kind":"image","items":[],"gen":4}"#, false).is_none());
        assert!(refuse_file(br#"{"t":"cancel","id":"p2"}"#, false).is_none());
    }

    #[test]
    fn channel_messages_are_recognized() {
        assert_eq!(cancelled(br#"{"t":"cancel","id":"g1"}"#, false).as_deref(), Some("g1"));
        assert_eq!(cancelled(br#"{"t":"cancel","id":"g1"}"#, true), None);
        assert_eq!(transfer_ended(r#"{"t":"done","id":"p1","names":[]}"#).as_deref(), Some("p1"));
        assert_eq!(transfer_ended(r#"{"t":"ready","id":"p1"}"#), None);
        let control = |s: &[u8]| match incoming(s) {
            Some(Incoming::Control(c)) => Some(c),
            _ => None,
        };
        assert!(is_clipboard(&control(br#"{"t":"cbreq","gen":1,"format":"text","id":"r"}"#).unwrap()) && is_clipboard(&control(br#"{"t":"clipshare","on":true}"#).unwrap()));
        assert!(!is_clipboard(&control(br#"{"t":"k","c":"KeyV","d":true}"#).unwrap()));
        assert!(matches!(incoming(br#"{"t":"m","x":0.5,"y":0.5,"s":3}"#), Some(Incoming::Move(Move::To { seq: 3, .. }))), "a move is a move on either channel");
        assert!(incoming(b"junk").is_none());
    }

    #[test]
    fn cancelled_clipboard_data_leaves_the_queue() {
        let mut q = FileQueue::default();
        q.push(r#"{"t":"done","id":"p0","names":[]}"#.into());
        q.push(r#"{"t":"meta","id":"g1","name":"a","size":2,"gen":1}"#.into());
        q.push_bytes(vec![1]);
        q.push_bytes(vec![2]);
        q.push(r#"{"t":"fin","id":"g1"}"#.into());
        q.push(r#"{"t":"meta","id":"g2","name":"b","size":1,"gen":1}"#.into());
        q.push_bytes(vec![3]);
        q.cancel("g1");
        let left: Vec<(bool, Vec<u8>)> = q.queue.iter().map(|x| (x.binary, x.data.clone())).collect();
        assert_eq!(left.len(), 3, "the other messages stay");
        assert!(left[0].1.starts_with(br#"{"t":"done""#));
        assert_eq!(left[2], (true, vec![3]));
    }
}
