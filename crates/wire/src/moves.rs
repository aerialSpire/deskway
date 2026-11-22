//! `move`: the unordered, lossy data channel for pointer motion.

use super::wire_enum;

wire_enum! {
    /// Every message on the move channel, both directions.
    pub enum Move (tag "t") {
        "m" => To { "x" x: f64, "y" y: f64, "s" seq: u64 },
        "mr" => By { "dx" dx: f64, "dy" dy: f64, "s" seq: u64 },
        "cp" => CursorPos { "x" x: f64, "y" y: f64, "v" visible: bool, "s" seq: u64 },
    }
}
