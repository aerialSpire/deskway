//! Reliable, ordered file contents on the data-only browser session.

use crate::{wire_enum, wire_values, DATA_CHUNK};

pub const CHANNEL: &str = "file-transfer";
pub const HEADER: usize = 12;
pub const FRAME: usize = DATA_CHUNK;
pub const CHUNK: usize = FRAME - HEADER;
pub const WINDOW: usize = 4 * CHUNK;

wire_values! {
    pub enum Error {
        "not_found" => NotFound,
        "denied" => Denied,
        "exists" => Exists,
        "invalid_name" => InvalidName,
        "invalid_path" => InvalidPath,
        "busy" => Busy,
        "io" => Io,
        "not_regular" => NotRegular,
        "changed" => Changed,
        "size" => Size,
        "limit" => Limit,
        "protocol" => Protocol,
        "cancelled" => Cancelled,
        "integrity" => Integrity,
        "expired" => Expired,
    }
}

wire_enum! {
    pub enum Message (tag "t") {
        "upload" => Upload { "id" id: u32, "path" path: Vec<String>, "name" name: String, "size" size: u64 },
        "download" => Download {
            "id" id: u32, "path" path: Vec<String>, "entry" entry: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            "limit" limit: Option<u64>,
        },
        "resume" => Resume { "id" id: u32 },
        "conflict" => Conflict {
            "id" id: u32, "size" size: u64, "modified" modified: i64,
            #[serde(skip_serializing_if = "Option::is_none")]
            "changed" changed: Option<bool>,
        },
        "replace" => Replace { "id" id: u32 },
        "ready" => Ready { "id" id: u32, "size" size: u64, "offset" offset: u64 },
        "start" => Start { "id" id: u32, "offset" offset: u64 },
        "ack" => Ack { "id" id: u32, "offset" offset: u64 },
        "finish" => Finish { "id" id: u32, "offset" offset: u64, "sha256" sha256: String },
        "done" => Done { "id" id: u32, "offset" offset: u64 },
        "cancel" => Cancel { "id" id: u32 },
        "error" => Error { "id" id: u32, "why" why: Error },
    }
}

impl Message {
    pub fn id(&self) -> u32 {
        match *self {
            Self::Upload { id, .. }
            | Self::Download { id, .. }
            | Self::Resume { id }
            | Self::Conflict { id, .. }
            | Self::Replace { id }
            | Self::Ready { id, .. }
            | Self::Start { id, .. }
            | Self::Ack { id, .. }
            | Self::Finish { id, .. }
            | Self::Done { id, .. }
            | Self::Cancel { id }
            | Self::Error { id, .. } => id,
        }
    }
}

pub fn header(id: u32, offset: u64) -> Vec<u8> {
    let mut frame = Vec::with_capacity(FRAME);
    frame.extend_from_slice(&id.to_be_bytes());
    frame.extend_from_slice(&offset.to_be_bytes());
    frame
}

pub fn frame(bytes: &[u8]) -> Option<(u32, u64, &[u8])> {
    if !(HEADER + 1..=FRAME).contains(&bytes.len()) {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes[..4].try_into().ok()?),
        u64::from_be_bytes(bytes[4..HEADER].try_into().ok()?),
        &bytes[HEADER..],
    ))
}
