//! File browsing on the data-only browser session's reliable files channel.

use crate::{wire_enum, wire_struct, wire_values};

pub const CHANNEL: &str = "browse";
pub const MAX_BYTES: usize = 64 * 1024;

wire_values! {
    pub enum FileKind {
        "folder" => Folder,
        "file" => File,
        "link" => Link,
        "special" => Special,
    }
}

wire_values! {
    pub enum BrowseError {
        "not_found" => NotFound,
        "denied" => Denied,
        "exists" => Exists,
        "not_empty" => NotEmpty,
        "invalid_name" => InvalidName,
        "invalid_path" => InvalidPath,
        "busy" => Busy,
        "io" => Io,
    }
}

wire_struct! {
    pub struct FileEntry {
        "name" name: String,
        "kind" kind: FileKind,
        "size" size: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        "modified" modified: Option<i64>,
        "mode" mode: u32,
        "owner" owner: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "target" target: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "opens" opens: Option<bool>,
    }
}

wire_enum! {
    pub enum Browse (tag "t") {
        "list" => List {
            #[serde(skip_serializing_if = "Option::is_none")]
            "path" path: Option<Vec<String>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "cursor" cursor: Option<String>,
        },
        "mkdir" => Mkdir { "path" path: Vec<String>, "name" name: String },
        "rename" => Rename { "path" path: Vec<String>, "entry" entry: String, "name" name: String },
        "delete" => Delete { "path" path: Vec<String>, "entry" entry: String },
        "listing" => Listing {
            "path" path: Vec<String>,
            "entries" entries: Vec<FileEntry>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "cursor" cursor: Option<String>,
        },
        "changed" => Changed {},
        "error" => Error { "why" why: BrowseError },
    }
}
