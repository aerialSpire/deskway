//! `file`: the reliable data channel for files and clipboard data. Text messages are these;
//! binary messages are the bytes of the one transfer active in that direction.

use super::types::{Item, TransferError, TransferKind};
use super::wire_enum;

wire_enum! {
    /// Every text message on the file channel, both directions.
    pub enum File (tag "t") {
        "put" => Put {
            "id" id: String,
            "kind" kind: TransferKind,
            "items" items: Vec<Item>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "gen" gen: Option<u64>,
        },
        "ready" => Ready { "id" id: String },
        "done" => Done { "id" id: String, "names" names: Vec<String> },
        "meta" => Meta {
            "id" id: String,
            "name" name: String,
            "size" size: u64,
            #[serde(skip_serializing_if = "Option::is_none")]
            "mime" mime: Option<String>,
            "gen" gen: u64,
        },
        "fin" => Fin { "id" id: String },
        "cancel" => Cancel { "id" id: String },
        "err" => Err { "id" id: String, "why" why: TransferError },
    }
}
