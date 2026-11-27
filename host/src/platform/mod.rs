//! The computer a session controls, behind one interface.
//!
//! [`Platform`] is what a session asks of the computer; [`PlatformEvent`] is what the computer
//! reports back. [`Local`] carries it out in this process (the macOS app, the Windows agent);
//! [`Remote`] forwards it over the agent pipe (the Windows network service). Every method is
//! declared once, in the list below, which also makes its [`Call`] variant and the proxy's
//! forwarding: a new capability is one line here, one event, and its `Local` side.
//!
//! A session acts on the computer only while it holds it (docs/session-lifecycle.md,
//! "Authority"). The session that started video last holds it; every act of a session (`held`
//! below) carries the session's id, and `Local` obeys it only from the session holding the
//! computer, so an ended or replaced session's last acts never reach its successor, however
//! late they arrive.

use serde::{Deserialize, Serialize};
use wire::types::{ClipboardError, FormatKind, Item, Labels, Locks, Mods};

use crate::display::DisplayInfo;

mod features;
pub use features::{BrowserSession, DeskParams, DesktopFeatures, Features, OpenPty, WorkspaceState};
pub(crate) mod pty;
pub use pty::Pty;

#[cfg(all(test, desktop_host))]
pub const TEST_DESKTOP: DesktopFeatures = DESKTOP;

#[cfg(all(test, target_os = "linux"))]
pub const TEST_DESKTOP: DesktopFeatures = DesktopFeatures {
    // Desktop-independent signaling tests use a stand-in for the session thread.
    caps: |_| vec![wire::caps::DESK.into()],
    spawn: |_, _| std::sync::mpsc::channel().0,
    ai_awake: |_| {},
};

#[cfg(desktop_host)]
pub const DESKTOP: DesktopFeatures = DesktopFeatures { caps: crate::session::desk::caps, spawn: crate::session::desk::spawn, ai_awake: crate::power::ai_session };

#[cfg(desktop_host)]
pub use crate::power::prevent_sleep;

#[cfg(desktop_host)]
mod local;
#[cfg(desktop_host)]
pub use local::Local;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::without_echo;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

/// The OS power state relevant to the keep-awake setting.
#[cfg(desktop_host)]
#[derive(Clone, Copy, Debug)]
pub enum PowerState {
    External,
    Battery(u8),
}

#[cfg(windows)]
pub mod remote;
#[cfg(windows)]
pub use remote::Remote;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64")))]
mod macos;
#[cfg(all(target_os = "macos", any(target_arch = "aarch64", target_arch = "x86_64")))]
pub use self::macos::*;

#[cfg(all(target_os = "macos", not(any(target_arch = "aarch64", target_arch = "x86_64"))))]
compile_error!("Deskway for macOS requires Apple silicon or 64-bit Intel");

pub fn facts_name(name: &str) -> String {
    #[cfg(windows)]
    return name.to_string();
    #[cfg(not(windows))]
    crate::shown_name(name)
}

/// Declares the trait, the serialized form of each call, how a call is carried out on any
/// `Platform`, which calls set state a newly connected agent must be told again, and which are
/// a session's acts: those `held` take the acting session's id first.
macro_rules! platform {
    (
        held { $( $(#[$hdoc:meta])* fn $hname:ident($($harg:ident: $hty:ty),*) => $hvariant:ident $(, keep $hslot:ident)?; )* }
        free { $( $(#[$doc:meta])* fn $name:ident($($arg:ident: $ty:ty),*) => $variant:ident $(, keep $slot:ident)? $(, held $owner:ident)?; )* }
    ) => {
        pub trait Platform: Send + Sync {
            $( $(#[$hdoc])* fn $hname(&self, session: &str, $($harg: $hty),*); )*
            $( $(#[$doc])* fn $name(&self, $($arg: $ty),*); )*
        }

        /// One `Platform` method call, as it crosses the agent pipe.
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
        pub enum Call {
            $( $hvariant { session: String, $($harg: $hty),* }, )*
            $( $variant { $($arg: $ty),* }, )*
        }

        impl Call {
            /// Carries the call out.
            #[cfg(any(windows, test))]
            pub fn apply(self, p: &dyn Platform) {
                match self {
                    $( Call::$hvariant { session, $($harg),* } => p.$hname(&session, $($harg),*), )*
                    $( Call::$variant { $($arg),* } => p.$name($($arg),*), )*
                }
            }

            /// The state this call sets, if any: the latest call per slot is replayed, in
            /// order, to an agent that connects later.
            #[cfg(any(windows, test))]
            pub fn slot(&self) -> Option<&'static str> {
                match self {
                    $( Call::$hvariant { .. } => None $(.or(Some(stringify!($hslot))))?, )*
                    $( Call::$variant { .. } => None $(.or(Some(stringify!($slot))))?, )*
                }
            }

            /// The session whose act this is; None for what concerns the whole computer.
            pub fn held_by(&self) -> Option<&str> {
                match self {
                    $( Call::$hvariant { session, .. } => Some(session), )*
                    $( Call::$variant { $($owner,)? .. } => None $(.or(Some($owner.as_str())))?, )*
                }
            }
        }

        /// Forwards every method as its `Call`.
        pub trait Forward {
            fn forward(&self, call: Call);
        }

        impl<T: Forward + Send + Sync> Platform for T {
            $( fn $hname(&self, session: &str, $($harg: $hty),*) { self.forward(Call::$hvariant { session: session.to_string(), $($harg),* }) } )*
            $( fn $name(&self, $($arg: $ty),*) { self.forward(Call::$variant { $($arg),* }) } )*
        }
    };
}

platform! {
    held {
    // ---- video ----
    /// The session ended: capture stops, held input is released, a transfer half done is
    /// removed and the computer may sleep again.
    fn video_stop() => VideoStop, keep video_stop;
    /// The viewer lost something and needs a keyframe.
    fn keyframe() => Keyframe;
    /// The network's estimate, in bits per second.
    fn bitrate(bps: u32) => Bitrate;
    /// What this computer sends is queuing up (`desk::pace`): encoding waits until it drains,
    /// then encodes the screen as it is then.
    fn network_behind(behind: bool) => NetworkBehind;
    /// What the viewer accepts (`video::Constraints`; 0 = not known): the encoded size follows.
    fn view_constraints(w: u32, h: u32, max_w: u32, max_h: u32, fps: u32, level: u8) => ViewConstraints, keep view;
    /// The viewer draws the pointer itself: report it instead of drawing it into the video.
    fn local_cursor(on: bool) => LocalCursor, keep local_cursor;
    /// Captures this display (`DisplayInfo::id`) from now on.
    fn choose_display(id: String) => ChooseDisplay, keep display;

    // ---- input ----
    /// A position on the pointer display, 0..1 each way; `seq` is the browser's move sequence.
    fn mouse_move(x: f64, y: f64, seq: u64) => MouseMove;
    /// Relative motion in pixels (Pointer Lock), in the same sequence.
    fn mouse_move_by(dx: f64, dy: f64, seq: u64) => MouseMoveBy;
    fn mouse_button(button: u8, down: bool) => MouseButton;
    fn wheel(dx: f64, dy: f64) => Wheel;
    /// A key by its position (`KeyboardEvent.code`).
    fn key(code: String, down: bool) => Key;
    /// One character, typed with this computer's layout, with these modifiers held.
    fn char_key(ch: String, mods: Mods) => Char;
    fn text(text: String) => Text;
    /// Toggle keys and modifiers as the browser has them.
    fn sync_keys(locks: Locks, mods: Mods) => SyncKeys;
    /// The display that pointer positions refer to.
    fn pointer_display(display: DisplayInfo) => PointerDisplay;
    /// Lifts every key and button still held.
    fn release_all() => ReleaseAll;
    /// Answered `InputDone` under `id` once everything sent before it has been carried out, and
    /// the program in front has had a moment to take it in (an AI client's type and key).
    fn input_done(id: String) => InputDone;

    // ---- clipboard (see `clipboard`) ----
    /// Watch and serve the clipboard (sharing on and a session running) or not.
    fn clipboard_share(on: bool) => ClipboardShare, keep clipboard_share;
    /// The browser owns the clipboard: placeholders for its formats (delayed rendering).
    fn clipboard_announce(gen: u64, formats: Vec<ClipFormat>) => ClipboardAnnounce, keep clipboard_owner;
    /// Answers a `ClipboardRender`; neither text nor html: the paste gets nothing.
    fn clipboard_provide(gen: u64, format: FormatKind, text: Option<String>, html: Option<String>) => ClipboardProvide;
    /// Reads one format of this computer's clipboard for the browser (`i`: which file).
    fn clipboard_read(gen: u64, format: FormatKind, id: String, i: Option<u32>) => ClipboardRead;

    // ---- files ----
    /// A message on the browser's "file" channel (see `transfer`).
    fn file_message(text: String) => FileMessage;
    fn file_data(data: Bytes) => FileData;
    }

    free {
    /// Session `session` takes the computer. Video starts on `display` (primary if unavailable
    /// or omitted) at the viewer's `view` constraints. The computer stays awake and input starts
    /// from its own key state. A session that held it before loses it.
    fn video_start(session: String, view: Constraints, display: Option<String>) => VideoStart, keep video_start;
    /// One round of display previews, for the device list.
    fn thumbnails(on: bool) => Thumbnails, keep thumbnails;
    /// The AI client's session beside the person's (`None`: none): its input is carried out.
    fn ai_session(session: Option<String>) => AiSession;
    /// One still of a display (`None`: the primary), at most `max` wide, for an AI client:
    /// answered `Snapshot` or `SnapshotFailed` under `id`.
    fn snapshot(id: String, display: Option<String>, max: u32) => Snapshot;

    // ---- desktop ----
    fn lock() => Lock;
    /// Lock by itself if the controlling side disappears during a session.
    fn auto_lock(on: bool) => AutoLock, keep auto_lock;

    // ---- system ----
    /// Start with the computer or not.
    fn autostart(on: bool) => AutoStart, keep autostart;
    /// The language of the notes shown at this computer: a catalog code, or None for the system's.
    fn language(code: Option<String>) => Language, keep language;
    /// Applies the enrollment's browser trust using the agent's protected certificate files.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn sync_server_root() => SyncServerRoot, keep server_root;
    }
}

impl Call {
    /// Input: the only acts an AI client's session may make on the computer, beside the
    /// person's. A call not named here is not, so a new one is the person's until said.
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            Call::MouseMove { .. } | Call::MouseMoveBy { .. } | Call::MouseButton { .. } | Call::Wheel { .. } | Call::Key { .. } | Call::Char { .. } | Call::Text { .. } | Call::PointerDisplay { .. } | Call::ReleaseAll { .. } | Call::InputDone { .. }
        )
    }
}

/// Answers the platform gives under an id (an AI client's screenshot), each to the one that
/// asked: it expects one before asking, and the router hands it over.
#[derive(Default)]
pub struct Replies(std::sync::Mutex<std::collections::HashMap<String, std::sync::mpsc::Sender<PlatformEvent>>>);

impl Replies {
    pub fn expect(&self, id: &str) -> std::sync::mpsc::Receiver<PlatformEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.0.lock().unwrap().insert(id.to_string(), tx);
        rx
    }

    /// Hands `e` to whoever expects it; gives it back when it is no answer anyone waits for.
    pub fn answer(&self, e: PlatformEvent) -> Option<PlatformEvent> {
        let id = match &e {
            PlatformEvent::Snapshot { id, .. } | PlatformEvent::SnapshotFailed { id } | PlatformEvent::InputDone { id } => id.clone(),
            _ => return Some(e),
        };
        if let Some(tx) = self.0.lock().unwrap().remove(&id) {
            let _ = tx.send(e);
        }
        None
    }
}

/// One clipboard format, as it crosses the agent pipe. The same fields as the wire `Format`,
/// without its skipped fields (the pipe's encoding is not self-describing).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClipFormat {
    pub kind: FormatKind,
    pub size: Option<u64>,
    pub w: Option<u32>,
    pub h: Option<u32>,
    pub items: Option<Vec<Item>>,
}

impl ClipFormat {
    pub fn of(kind: FormatKind) -> ClipFormat {
        ClipFormat { kind, size: None, w: None, h: None, items: None }
    }
}

/// What the viewer accepts. Zero means "not known".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Constraints {
    /// Viewport, device pixels.
    pub w: u32,
    pub h: u32,
    /// Largest size the viewer decodes smoothly (MediaCapabilities), as a box.
    pub max_w: u32,
    pub max_h: u32,
    /// Frames a second the viewer takes: what it decodes smoothly at that size, or fewer when
    /// the person prefers a sharper picture to a smoother one.
    pub fps: u32,
    /// `level_idc` from the negotiated `profile-level-id`.
    pub level: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorShape {
    pub w: usize,
    pub h: usize,
    pub hx: i32,
    pub hy: i32,
    #[serde(with = "crate::platform::raw_bytes")]
    pub rgba: Vec<u8>,
}

/// A line for the person at this computer, beside "… is in control".
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TransferNote {
    pub text: String,
    /// The folder the files went to, for an “Open folder” button.
    pub folder: Option<String>,
}

/// What the computer reports.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PlatformEvent {
    /// One encoded H.264 access unit (Annex B).
    Frame(Bytes),
    /// The display being captured, among all attached.
    Capturing { list: Vec<DisplayInfo>, current: DisplayInfo },
    /// A new encoder runs: its size, and the most that size can use (bits per second).
    EncodedSize { w: u32, h: u32, max_bitrate: u32 },
    /// False while no display can be captured (none attached); true again with the next frame.
    CaptureAvailable(bool),
    CursorShape(CursorShape),
    /// `seq`: the last pointer move applied when the position was read.
    CursorPosition { x: f64, y: f64, visible: bool, seq: u64 },
    /// The keyboard layout in use, with each key's label without and with Shift.
    LayoutChanged { name: String, labels: Labels, shift: Labels },
    /// A JPEG preview of one display.
    Thumbnail { display: String, jpeg: Bytes },
    /// The still asked for under `id`: JPEG, `w`×`h`, of `display`.
    Snapshot { id: String, display: String, jpeg: Bytes, w: u32, h: u32 },
    /// The still asked for under `id` could not be taken.
    SnapshotFailed { id: String },
    /// The input sent before `input_done(id)` has been carried out.
    InputDone { id: String },
    ServerRootSynced { error: Option<String> },
    /// Every display attached.
    DisplaysChanged(Vec<DisplayInfo>),
    /// The desktop that has input now.
    Desktop(Desk),
    /// For the browser's "file" channel.
    FileMessage(String),
    /// Bytes for the browser's "file" channel, after the `meta` they belong to.
    FileData(Bytes),
    /// Something was copied on this computer (never reported while sharing is off).
    ClipboardChanged { formats: Vec<ClipFormat>, text: Option<String> },
    /// The answer to `clipboard_announce`: the placeholders are in place (`why` None), or not.
    ClipboardAnnounced { gen: u64, why: Option<ClipboardError> },
    /// Something on this computer pastes from one of the browser's placeholders.
    ClipboardRender { gen: u64, format: FormatKind },
    /// Answers `clipboard_read`: inline, or `size` bytes that follow as `meta`, `FileData`, `fin`.
    ClipboardData { gen: u64, format: FormatKind, id: String, name: Option<String>, text: Option<String>, html: Option<String>, size: Option<u64> },
    /// A `clipboard_read` cannot be answered.
    ClipboardFailed { gen: u64, id: String, why: ClipboardError },
    /// A line for the person at this computer about a transfer.
    TransferNote(Option<TransferNote>),
}

/// What the person at the computer is looking at. Windows shows the lock screen, the sign-in
/// screen and UAC prompts on desktops of their own.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Desk {
    /// The signed-in user's desktop.
    Default,
    /// Lock screen, sign-in screen or Ctrl+Alt+Del screen.
    Winlogon,
    /// A UAC prompt or another secure desktop.
    Other,
}

impl Desk {
    pub fn as_str(self) -> &'static str {
        match self {
            Desk::Default => "default",
            Desk::Winlogon => "winlogon",
            Desk::Other => "other",
        }
    }
}

/// Binary data (video frames, file chunks): raw bytes on the agent pipe, never a list of numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        raw_bytes::serialize(&self.0, s)
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        raw_bytes::deserialize(d).map(Bytes)
    }
}

/// `#[serde(with = "crate::platform::raw_bytes")]` for byte fields: one byte string, not a
/// sequence of single bytes.
pub mod raw_bytes {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(data: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(data)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        struct Raw;
        impl<'de> Visitor<'de> for Raw {
            type Value = Vec<u8>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("bytes")
            }
            fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }
            fn visit_byte_buf<E: Error>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<u8>, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 16));
                while let Some(b) = seq.next_element()? {
                    out.push(b);
                }
                Ok(out)
            }
        }
        d.deserialize_byte_buf(Raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The agent pipe carries `Call` and `PlatformEvent` (externally tagged, in the pipe's own
    /// encoding). The messages the definition marks current are those, by name and fields; the
    /// others describe a pipe not built yet.
    #[test]
    fn the_current_agent_pipe_messages_are_these_calls_and_events() {
        use std::collections::{BTreeMap, BTreeSet};
        let f = ClipFormat::of(FormatKind::Text);
        let mine: BTreeMap<String, BTreeSet<String>> = [
            serde_json::to_value(Call::VideoStart { session: "s1".into(), view: Constraints::default(), display: Some("A".into()) }).unwrap(),
            serde_json::to_value(Call::ViewConstraints { session: "s1".into(), w: 1, h: 1, max_w: 1, max_h: 1, fps: 1, level: 1 }).unwrap(),
            serde_json::to_value(Call::ClipboardShare { session: "s1".into(), on: true }).unwrap(),
            serde_json::to_value(Call::ClipboardAnnounce { session: "s1".into(), gen: 1, formats: vec![f.clone()] }).unwrap(),
            serde_json::to_value(Call::ClipboardProvide { session: "s1".into(), gen: 1, format: FormatKind::Text, text: None, html: None }).unwrap(),
            serde_json::to_value(Call::ClipboardRead { session: "s1".into(), gen: 1, format: FormatKind::Text, id: "r".into(), i: None }).unwrap(),
            serde_json::to_value(PlatformEvent::EncodedSize { w: 1, h: 1, max_bitrate: 1 }).unwrap(),
            serde_json::to_value(PlatformEvent::ClipboardChanged { formats: vec![f], text: None }).unwrap(),
            serde_json::to_value(PlatformEvent::ClipboardAnnounced { gen: 1, why: None }).unwrap(),
            serde_json::to_value(PlatformEvent::ClipboardRender { gen: 1, format: FormatKind::Html }).unwrap(),
            serde_json::to_value(PlatformEvent::ClipboardData { gen: 1, format: FormatKind::Text, id: "r".into(), name: None, text: None, html: None, size: None }).unwrap(),
            serde_json::to_value(PlatformEvent::ClipboardFailed { gen: 1, id: "r".into(), why: ClipboardError::Gone }).unwrap(),
        ]
        .into_iter()
        .map(|v| {
            let (name, body) = v.as_object().unwrap().iter().next().map(|(k, b)| (k.clone(), b.clone())).unwrap();
            (name, body.as_object().unwrap().keys().cloned().collect())
        })
        .collect();
        assert_eq!(mine, wire::contract::current("ipc"));
    }

    #[derive(Default)]
    struct Log(Mutex<Vec<Call>>);
    impl Forward for Log {
        fn forward(&self, call: Call) {
            self.0.lock().unwrap().push(call);
        }
    }

    #[test]
    fn every_method_is_one_call_and_back() {
        let log = Log::default();
        log.key("s1", "KeyA".into(), true);
        log.file_data("s1", Bytes(vec![0, 1, 255]));
        log.video_start("s1".into(), Constraints::default(), None);
        log.thumbnails(true);
        let calls = std::mem::take(&mut *log.0.lock().unwrap());
        assert_eq!(calls[0], Call::Key { session: "s1".into(), code: "KeyA".into(), down: true });
        assert_eq!(calls.iter().map(Call::held_by).collect::<Vec<_>>(), [Some("s1"), Some("s1"), None, None], "a session's acts carry it");
        // Carried out on another platform, the same calls arrive.
        let again = Log::default();
        for c in calls.clone() {
            c.apply(&again);
        }
        assert_eq!(*again.0.lock().unwrap(), calls);
    }

    #[test]
    fn clipboard_calls_cross_the_pipe_encoding() {
        // The pipe's encoding is not self-describing: optional fields must survive it.
        let opts = || bincode::DefaultOptions::new();
        use bincode::Options;
        let c = Call::ClipboardAnnounce { session: "s1".into(), gen: 7, formats: vec![ClipFormat { size: Some(3), ..ClipFormat::of(FormatKind::Image) }, ClipFormat::of(FormatKind::Text)] };
        let back: Call = opts().deserialize(&opts().serialize(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let e = PlatformEvent::ClipboardData { gen: 2, format: FormatKind::Files, id: "g".into(), name: Some("a.pdf".into()), text: None, html: None, size: Some(9) };
        let back: PlatformEvent = opts().deserialize(&opts().serialize(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn only_state_setting_calls_have_a_slot() {
        // Separate slots: a newer session's start and an older one's stop both replay, in order.
        assert_eq!(Call::VideoStart { session: "s".into(), view: Default::default(), display: None }.slot(), Some("video_start"));
        assert_eq!(Call::VideoStop { session: "s".into() }.slot(), Some("video_stop"));
        assert_eq!(Call::AutoLock { on: true }.slot(), Some("auto_lock"));
        assert_eq!(Call::Key { session: "s".into(), code: "KeyA".into(), down: true }.slot(), None);
        assert_eq!(Call::Keyframe { session: "s".into() }.slot(), None);
    }
}
