//! `control`: the reliable, ordered data channel ("input").

use super::types::{Display, Format, FormatKind, Labels, Locks, Mods, ClipboardError, Reason};
use super::{wire_enum, wire_values};

wire_values! {
    pub enum CaptureState { "no-display" => NoDisplay, "ready" => Ready }
}

wire_values! {
    pub enum Owner { "host" => Host, "browser" => Browser }
}

wire_enum! {
    /// Every message on the control channel, both directions.
    pub enum Control (tag "t") {
        "hello" => Hello { "version" version: u32, "caps" caps: Vec<String> },
        "resume" => Resume { "id" id: String, "k" k: String },
        "end" => End {
            "reason" reason: Reason,
            #[serde(skip_serializing_if = "Option::is_none")]
            "by" by: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "same" same: Option<bool>,
        },
        "k" => Key { "c" code: String, "d" down: bool },
        "kc" => Char { "ch" ch: String, "mods" mods: Mods },
        "x" => Text { "v" text: String },
        "sync" => Sync { "locks" locks: Locks, "mods" mods: Mods },
        "b" => Button { "b" button: u8, "d" down: bool, "x" x: f64, "y" y: f64 },
        "w" => Wheel { "dx" dx: f64, "dy" dy: f64 },
        // The last move of a motion, repeated reliably (see `moves`).
        "m" => MoveTo { "x" x: f64, "y" y: f64, "s" seq: u64 },
        "mr" => MoveBy { "dx" dx: f64, "dy" dy: f64, "s" seq: u64 },
        "lc" => LocalCursor { "on" on: bool },
        "view" => View { "w" w: u32, "h" h: u32, "maxW" max_w: u32, "maxH" max_h: u32, "fps" fps: u32 },
        "display" => ChooseDisplay { "v" id: String },
        "capture-keyframe" => CaptureKeyframe {},
        "displays" => Displays { "list" list: Vec<Display>, "cur" cur: String },
        "capture" => Capture { "v" state: CaptureState },
        "desk" => Desk { "v" name: String },
        "cs" => CursorShape { "w" w: u32, "h" h: u32, "hx" hx: u32, "hy" hy: u32, "d" rgba: String },
        "layout" => Layout { "name" name: String, "labels" labels: Labels, "shift" shift: Labels },
        "clipshare" => ClipShare { "on" on: bool },
        "cb" => Clipboard {
            #[serde(skip_serializing_if = "Option::is_none")]
            "gen" gen: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "owner" owner: Option<Owner>,
            "formats" formats: Vec<Format>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "text" text: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "id" id: Option<String>,
        },
        "cbreq" => ClipboardRequest {
            "gen" gen: u64,
            "format" format: FormatKind,
            "id" id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            "i" i: Option<u32>,
        },
        "cbdata" => ClipboardData {
            "gen" gen: u64,
            "format" format: FormatKind,
            "id" id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            "text" text: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "html" html: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "size" size: Option<u64>,
        },
        "cberr" => ClipboardFailed { "gen" gen: u64, "id" id: String, "why" why: ClipboardError },
    }
}
