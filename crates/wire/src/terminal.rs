//! The reliable ordered terminal channel. Binary messages carry raw PTY bytes.

use crate::{wire_enum, wire_values};

pub const CHANNEL: &str = "terminal";
pub const FRAME: usize = crate::DATA_CHUNK;
pub const OUTPUT_WINDOW: usize = 128 * 1024;
pub const INPUT_WINDOW: usize = 64 * 1024;

wire_values! {
    pub enum TerminalError {
        "open" => Open,
        "close" => Close,
        "io" => Io,
        "protocol" => Protocol,
    }
}

wire_values! {
    pub enum DetachReason {
        "taken" => Taken,
    }
}

wire_enum! {
    pub enum Terminal (tag "t") {
        "open" => Open {
            "rows" rows: u16, "cols" cols: u16,
            #[serde(skip_serializing_if = "Option::is_none")]
            "id" id: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "new" new: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "take" take: Option<bool>,
        },
        "opened" => Opened {
            #[serde(skip_serializing_if = "Option::is_none")]
            "id" id: Option<u64>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "replay" replay: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "dropped" dropped: Option<bool>,
        },
        "resize" => Resize { "rows" rows: u16, "cols" cols: u16 },
        "ack" => Ack { "bytes" bytes: usize },
        "close" => Close {},
        "terminate" => Terminate {},
        "closed" => Closed {},
        "detached" => Detached { "reason" reason: DetachReason },
        "exit" => Exit { "code" code: i32 },
        "error" => Error { "why" why: TerminalError },
    }
}
