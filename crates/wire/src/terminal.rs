//! The reliable ordered terminal channel. Binary messages carry raw PTY bytes.

use crate::{wire_enum, wire_values};

pub const CHANNEL: &str = "terminal";
pub const FRAME: usize = 16 * 1024;
pub const OUTPUT_WINDOW: usize = 128 * 1024;
pub const INPUT_WINDOW: usize = 64 * 1024;

wire_values! {
    pub enum TerminalError {
        "open" => Open,
        "io" => Io,
        "protocol" => Protocol,
    }
}

wire_enum! {
    pub enum Terminal (tag "t") {
        "open" => Open { "rows" rows: u16, "cols" cols: u16 },
        "opened" => Opened {},
        "resize" => Resize { "rows" rows: u16, "cols" cols: u16 },
        "ack" => Ack { "bytes" bytes: usize },
        "close" => Close {},
        "exit" => Exit { "code" code: i32 },
        "error" => Error { "why" why: TerminalError },
    }
}
