//! The definition (`proto/messages.json`) and the golden strings (`proto/vectors.json`) as the
//! contract tests read them: this crate's own (`tests.rs`) and those of the crates that build on
//! these types, which check their own messages and signing against the same files.

use std::collections::{BTreeMap, BTreeSet};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

const MESSAGES: &str = include_str!("../../../proto/messages.json");
const VECTORS: &str = include_str!("../../../proto/vectors.json");

pub fn messages() -> Value {
    serde_json::from_str(MESSAGES).expect("messages.json parses")
}

pub fn vectors() -> Value {
    serde_json::from_str(VECTORS).expect("vectors.json parses")
}

/// The golden vector of one signed format (`deskway-…-v1`).
pub fn vector<'a>(all: &'a Value, format: &str) -> &'a Value {
    all["vectors"].as_array().unwrap().iter().find(|v| v["format"] == format).unwrap_or_else(|| panic!("no vector for {format}"))
}

/// One of a vector's string inputs.
pub fn input<'a>(v: &'a Value, k: &str) -> &'a str {
    v["inputs"][k].as_str().unwrap_or_else(|| panic!("input {k}"))
}

/// HMAC-SHA256 of `s`, base64, as the vectors write MACs.
pub fn mac_b64(key: &[u8], s: &str) -> String {
    let mut m = Hmac::<Sha256>::new_from_slice(key).unwrap();
    m.update(s.as_bytes());
    STANDARD.encode(m.finalize().into_bytes())
}

/// The messages of a channel the definition marks current, by name and fields. Channels whose
/// messages are Rust types of another crate (the agent pipe) are checked against this there.
pub fn current(channel: &str) -> BTreeMap<String, BTreeSet<String>> {
    messages()["channels"][channel]["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("no channel {channel}"))
        .iter()
        .filter(|m| m["status"] == "current")
        .map(|m| (m["type"].as_str().unwrap().to_string(), m["fields"].as_object().unwrap().keys().cloned().collect()))
        .collect()
}
