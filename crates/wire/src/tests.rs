//! Contract tests: the Rust wire types against `proto/messages.json`, and `challenge` against
//! the golden strings in `proto/vectors.json`. The host checks its agent pipe, its pairing and
//! its resume tokens against the same files (`platform`, `auth`).
//!
//! Samples are built from the definition itself (every field, and required fields only), so a
//! message added to the definition is tested without writing a sample by hand.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::challenge;
use super::contract::{input as s, mac_b64, messages as def, vector, vectors};
use super::control::{CaptureState, Control, Owner};
use super::file::File;
use super::moves::Move;
use super::signal::Signal;
use super::types::*;

// ---- samples from the definition ----

fn sample(def: &Value, field: &Value, full: bool) -> Value {
    if let Some(values) = field["enum"].as_array() {
        return values[0].clone();
    }
    let of = |name: &Value| sample(def, &json!({ "type": name }), full);
    match field["type"].as_str().expect("field type") {
        "string" => json!("x"),
        "integer" => json!(1),
        "number" => json!(1.5),
        "boolean" => json!(true),
        "any" | "object" => json!({}),
        "array" => json!([of(&field["items"])]),
        "map" => json!({ "KeyA": of(&field["values"]) }),
        name => {
            let t = &def["types"][name];
            assert!(t.is_object(), "unknown type {name}");
            object(def, &t["fields"], full)
        }
    }
}

fn object(def: &Value, fields: &Value, full: bool) -> Value {
    let mut out = Map::new();
    for (name, f) in fields.as_object().expect("fields") {
        if full || !f["optional"].as_bool().expect("optional is set") {
            out.insert(name.clone(), sample(def, f, full));
        }
    }
    Value::Object(out)
}

fn required(fields: &Value) -> Vec<String> {
    fields.as_object().unwrap().iter().filter(|(_, f)| f["optional"] == false).map(|(k, _)| k.clone()).collect()
}

/// Deserializes `v` as `T`, serializes it again and expects the same JSON: names, presence of
/// optional fields and value types all agree.
fn round_trip<T: Serialize + DeserializeOwned + Debug>(what: &str, v: &Value) -> T {
    let t: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{what}: {v} does not deserialize: {e}"));
    assert_eq!(&serde_json::to_value(&t).unwrap(), v, "{what}: round trip changes the message");
    t
}

fn must_fail<T: DeserializeOwned + Debug>(what: &str, v: &Value) {
    assert!(serde_json::from_value::<T>(v.clone()).is_err(), "{what}: {v} deserializes, but a required field is missing");
}

// ---- channels ----

fn check_channel<T: Serialize + DeserializeOwned + Debug>(channel: &str, table: &[(&str, &[&str])], kind: fn(&T) -> &'static str) {
    let def = def();
    let ch = &def["channels"][channel];
    let tag = ch["discriminator"].as_str().expect("discriminator");
    let messages: Vec<&Value> = ch["messages"].as_array().unwrap().iter().collect();

    let wanted: BTreeMap<String, BTreeSet<String>> = messages.iter().map(|m| (m["type"].as_str().unwrap().to_string(), m["fields"].as_object().unwrap().keys().cloned().collect())).collect();
    let have: BTreeMap<String, BTreeSet<String>> = table.iter().map(|(t, f)| (t.to_string(), f.iter().map(|s| s.to_string()).collect())).collect();
    assert_eq!(have, wanted, "{channel}: Rust messages and fields differ from messages.json");

    for m in messages {
        let ty = m["type"].as_str().unwrap();
        let what = format!("{channel} {ty}");
        for full in [true, false] {
            let mut v = object(&def, &m["fields"], full);
            v.as_object_mut().unwrap().insert(tag.into(), json!(ty));
            let t: T = round_trip(&what, &v);
            assert_eq!(kind(&t), ty, "{what}: wrong variant");
            if full {
                for r in required(&m["fields"]) {
                    let mut less = v.clone();
                    less.as_object_mut().unwrap().remove(&r);
                    must_fail::<T>(&format!("{what} without {r}"), &less);
                }
                for (name, f) in m["fields"].as_object().unwrap() {
                    for value in f["enum"].as_array().into_iter().flatten() {
                        let mut other = v.clone();
                        other[name] = value.clone();
                        round_trip::<T>(&format!("{what} {name}={value}"), &other);
                    }
                }
            }
        }
    }
}

#[test]
fn signaling_host_matches_the_definition() {
    check_channel::<Signal>("signaling.host", Signal::TYPES, Signal::kind);
}

#[test]
fn control_matches_the_definition() {
    check_channel::<Control>("control", Control::TYPES, Control::kind);
}

#[test]
fn move_matches_the_definition() {
    check_channel::<Move>("move", Move::TYPES, Move::kind);
}

#[test]
fn file_matches_the_definition() {
    check_channel::<File>("file", File::TYPES, File::kind);
}

// ---- shared types and value sets ----

fn check_struct<T: Serialize + DeserializeOwned + Debug>(name: &str, fields: &[&str]) {
    let def = def();
    let t = &def["types"][name];
    let wanted: BTreeSet<&str> = t["fields"].as_object().unwrap_or_else(|| panic!("no type {name}")).keys().map(String::as_str).collect();
    assert_eq!(fields.iter().copied().collect::<BTreeSet<_>>(), wanted, "{name}: fields differ");
    let full = object(&def, &t["fields"], true);
    round_trip::<T>(name, &full);
    round_trip::<T>(name, &object(&def, &t["fields"], false));
    for r in required(&t["fields"]) {
        let mut less = full.clone();
        less.as_object_mut().unwrap().remove(&r);
        must_fail::<T>(&format!("{name} without {r}"), &less);
    }
}

#[test]
fn shared_types_match_the_definition() {
    check_struct::<Display>("Display", Display::FIELDS);
    check_struct::<IceServer>("ICEServer", IceServer::FIELDS);
    check_struct::<View>("View", View::FIELDS);
    check_struct::<IceCandidate>("IceCandidate", IceCandidate::FIELDS);
    check_struct::<OfferAuth>("OfferAuth", OfferAuth::FIELDS);
    check_struct::<AnswerAuth>("AnswerAuth", AnswerAuth::FIELDS);
    check_struct::<Holder>("Holder", Holder::FIELDS);
    check_struct::<VouchEntry>("VouchEntry", VouchEntry::FIELDS);
    check_struct::<Vouch>("Vouch", Vouch::FIELDS);
    check_struct::<PairAuth>("PairAuth", PairAuth::FIELDS);
    check_struct::<PairedAuth>("PairedAuth", PairedAuth::FIELDS);
    check_struct::<LogEntry>("LogEntry", LogEntry::FIELDS);
    check_struct::<Mods>("Mods", Mods::FIELDS);
    check_struct::<Locks>("Locks", Locks::FIELDS);
    check_struct::<Item>("Item", Item::FIELDS);
    check_struct::<Format>("Format", Format::FIELDS);
    check_struct::<AiSwitch>("AiSwitch", AiSwitch::FIELDS);
}

fn values(v: &Value) -> Vec<&str> {
    v["enum"].as_array().unwrap_or_else(|| panic!("no enum in {v}")).iter().map(|x| x.as_str().unwrap()).collect()
}

fn field<'a>(def: &'a Value, channel: &str, ty: &str, name: &str) -> &'a Value {
    let m = def["channels"][channel]["messages"].as_array().unwrap().iter().find(|m| m["type"] == ty).unwrap_or_else(|| panic!("no {channel} {ty}"));
    &m["fields"][name]
}

#[test]
fn value_sets_match_the_definition() {
    let def = def();
    let t = |ty: &str, f: &str| def["types"][ty]["fields"][f].clone();
    assert_eq!(Reason::VALUES, values(field(&def, "control", "end", "reason")));
    assert_eq!(CaptureState::VALUES, values(field(&def, "control", "capture", "v")));
    assert_eq!(Owner::VALUES, values(field(&def, "control", "cb", "owner")));
    assert_eq!(ClipboardError::VALUES, values(field(&def, "control", "cberr", "why")));
    assert_eq!(ClipboardError::VALUES, values(field(&def, "ipc", "ClipboardFailed", "why")));
    assert_eq!(FormatKind::VALUES, values(field(&def, "control", "cbreq", "format")));
    assert_eq!(FormatKind::VALUES, values(&t("Format", "kind")));
    assert_eq!(TransferKind::VALUES, values(field(&def, "file", "put", "kind")));
    assert_eq!(TransferError::VALUES, values(field(&def, "file", "err", "why")));
    assert_eq!(TransferError::VALUES, values(field(&def, "ipc", "FilesFailed", "why")));
    assert_eq!(Reason::VALUES, values(field(&def, "signaling.host", "bye", "reason")));
    assert_eq!(Reason::VALUES, values(field(&def, "signaling.browser", "bye", "reason")));
    assert_eq!(AuthKind::VALUES, values(&t("OfferAuth", "k")));
    assert_eq!(LogOp::VALUES, values(&t("LogEntry", "op")));
    assert_eq!(Role::VALUES, values(&t("LogEntry", "role")));
    assert_eq!(AiState::VALUES, values(field(&def, "signaling.host", "status", "ai_state")));
    assert_eq!(AiState::VALUES, values(&t("AiView", "state")));
}

#[test]
fn version_and_capabilities_match_the_definition() {
    let def = def();
    let version: Value = serde_json::from_str(include_str!("../../../proto/version.json")).unwrap();
    assert_eq!(version["protocol_version"], super::PROTOCOL_VERSION);
    assert_eq!(version["protocol_version"], super::MIN_VERSION + 1);
    let host: Vec<&str> = def["capabilities"].as_array().unwrap().iter().filter(|c| c["who"] == "host").map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(super::caps::ALL, host);
}

// ---- golden vectors ----

fn sha_hex(s: &str) -> String {
    crate::crypto::hex(&crate::crypto::sha256(s.as_bytes()))
}

#[test]
fn vectors_are_self_consistent() {
    let all = vectors();
    for v in all["vectors"].as_array().unwrap() {
        let string = v["string"].as_str().unwrap();
        assert_eq!(v["sha256"], sha_hex(string), "{}", v["format"]);
        assert!(string.starts_with(&format!("{}\n", v["format"].as_str().unwrap())), "{}", v["format"]);
        if let Some(key) = v["mac_key"].as_str() {
            assert_eq!(v["mac"], mac_b64(&STANDARD.decode(key).unwrap(), string), "{}", v["format"]);
        }
        if let Some(key) = v["mac_key_text"].as_str() {
            assert_eq!(v["mac"], mac_b64(key.as_bytes(), string), "{}", v["format"]);
            assert_eq!(key, challenge::normalize_code(v["pairing_code"].as_str().unwrap()));
        }
    }
}

#[test]
fn challenge_produces_the_signed_formats() {
    let all = vectors();
    let v = vector(&all, "deskway-answer-v1");
    assert_eq!(challenge::answer_v1(s(v, "host"), s(v, "session"), s(v, "offer"), s(v, "answer")), v["string"].as_str().unwrap());

    let v = vector(&all, "deskway-endorse-v1");
    let c = challenge::endorse_v1(s(v, "cred"), v["inputs"]["alg"].as_i64().unwrap(), s(v, "key"), s(v, "name"), v["inputs"]["ts"].as_i64().unwrap());
    assert_eq!(sha_hex(&c), v["sha256"].as_str().unwrap(), "endorse-v1");

    let v = vector(&all, "deskway-remove-v1");
    assert_eq!(sha_hex(&challenge::remove_v1(s(v, "cred"), v["inputs"]["ts"].as_i64().unwrap())), v["sha256"].as_str().unwrap(), "remove-v1");

    let v = vector(&all, "deskway-rename-v1");
    assert_eq!(sha_hex(&challenge::rename_v1(s(v, "cred"), s(v, "name"), v["inputs"]["ts"].as_i64().unwrap())), v["sha256"].as_str().unwrap(), "rename-v1");

    let v = vector(&all, "deskway-policy-v1");
    assert_eq!(sha_hex(&challenge::policy_v1(v["inputs"]["keys"].as_bool().unwrap(), v["inputs"]["ts"].as_i64().unwrap())), v["sha256"].as_str().unwrap(), "policy-v1");

    let v = vector(&all, "deskway-vouch-v1");
    let list: Vec<(&str, i64, &str)> = v["inputs"]["list"].as_array().unwrap().iter().map(|c| (c["cred"].as_str().unwrap(), c["alg"].as_i64().unwrap(), c["key"].as_str().unwrap())).collect();
    assert_eq!(sha_hex(&challenge::vouch_v1(s(v, "host"), &list)), v["sha256"].as_str().unwrap(), "vouch-v1");

    let v = vector(&all, "deskway-code-v1");
    assert_eq!(challenge::key_code(Role::Browser, s(v, "key")), v["code"].as_str().unwrap(), "code-v1");

    let v = vector(&all, "deskway-endorse-v2");
    let c = challenge::endorse_v2(s(v, "cred"), v["inputs"]["alg"].as_i64().unwrap(), s(v, "key"), s(v, "name"), s(v, "role"), v["inputs"]["ts"].as_i64().unwrap());
    assert_eq!(c, v["string"].as_str().unwrap(), "endorse-v2");

    let v = vector(&all, "deskway-code-v2");
    assert_eq!(challenge::code_v2(s(v, "role"), s(v, "key")), v["string"].as_str().unwrap(), "code-v2");
    assert_eq!(challenge::key_code(Role::Ai, s(v, "key")), v["code"].as_str().unwrap(), "code-v2 digits");
    let v = vector(&all, "deskway-computer-code-v1");
    assert_eq!(challenge::computer_code_v1(s(v, "id")), v["string"].as_str().unwrap(), "computer-code-v1");
    assert_eq!(challenge::computer_code(s(v, "id")), v["code"].as_str().unwrap(), "computer-code-v1 digits");

    let v = vector(&all, "deskway-ai-v1");
    let i = &v["inputs"];
    assert_eq!(challenge::ai_v1(s(v, "host"), i["ai"].as_bool().unwrap(), i["admin"].as_bool().unwrap(), i["ts"].as_i64().unwrap()), v["string"].as_str().unwrap(), "ai-v1");

    let v = vector(&all, "deskway-ai-login-v1");
    assert_eq!(challenge::ai_login_v1(s(v, "name"), v["inputs"]["ts"].as_i64().unwrap()), v["string"].as_str().unwrap(), "ai-login-v1");
}

#[test]
fn challenge_produces_the_nonce_formats() {
    let all = vectors();
    let v = vector(&all, "deskway-connect-v3");
    let f = v["inputs"]["force"].as_bool().unwrap();
    assert_eq!(challenge::connect_v3(s(v, "host"), s(v, "session"), s(v, "nonce"), f, s(v, "sdp")), v["string"].as_str().unwrap());
    let v = vector(&all, "deskway-resume-v3");
    let f = v["inputs"]["force"].as_bool().unwrap();
    let string = challenge::resume_v3(s(v, "host"), s(v, "session"), s(v, "nonce"), f, s(v, "sdp"));
    assert_eq!(string, v["string"].as_str().unwrap());
    assert_eq!(mac_b64(&STANDARD.decode(v["mac_key"].as_str().unwrap()).unwrap(), &string), v["mac"].as_str().unwrap());
    let (a, b) = (challenge::new_nonce(), challenge::new_nonce());
    assert_eq!(a.len(), 22);
    assert_ne!(a, b);
}

#[test]
fn pairing_codes_are_normalized_as_they_are_maced() {
    assert_eq!(challenge::normalize_code("abcd-oil0"), "ABCD0110");
}

#[test]
fn the_tools_are_defined_once_each_with_an_object_schema() {
    let mut names = BTreeSet::new();
    for t in crate::tools::catalog() {
        assert!(names.insert(t.name.as_str()), "{} is defined twice", t.name);
        assert_eq!(t.input["type"], "object", "{}", t.name);
        assert!(!t.description.is_empty(), "{}", t.name);
        assert!(t.input["properties"].get("computer").is_none(), "{}: computer is the deskway command's, not a tool's", t.name);
    }
    assert!(crate::tools::all().iter().all(|t| t.batch >= 1));
}
