//! Every string the host or a browser signs or MACs (docs/protocol.md; `proto/vectors.json`
//! holds a golden copy of each).
//!
//! The host issues a single-use nonce per offer, so a connect proof is never checked against a
//! clock: the browser's clock may be wrong and a captured offer cannot be replayed.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::crypto::{hex, random, sha256};

fn sdp_hash(sdp: &str) -> String {
    hex(&sha256(sdp.as_bytes()))
}

/// What a device key signs (as SHA-256) for an offer: `force` is covered so the server cannot
/// add it, and the SDP hash covers the browser's DTLS fingerprint.
pub fn connect_v3(host: &str, session: &str, nonce: &str, force: bool, sdp: &str) -> String {
    format!("deskway-connect-v3\n{host}\n{session}\n{nonce}\n{}\n{}", force as u8, sdp_hash(sdp))
}

/// What a resume token MACs (HMAC-SHA256 with the token key) for an offer.
pub fn resume_v3(host: &str, session: &str, nonce: &str, force: bool, sdp: &str) -> String {
    format!("deskway-resume-v3\n{host}\n{session}\n{nonce}\n{}\n{}", force as u8, sdp_hash(sdp))
}

/// What the host signs in its answer: both descriptions, so neither side can be swapped.
pub fn answer_v1(host: &str, session: &str, offer: &str, answer: &str) -> String {
    format!("deskway-answer-v1\n{host}\n{session}\n{}\n{}", sdp_hash(offer), sdp_hash(answer))
}

/// What a device signs (as SHA-256) to let another device in: every computer that trusts the
/// signer then trusts the new one.
pub fn endorse_v1(cred: &str, alg: i64, key: &str, name: &str, ts: i64) -> String {
    format!("deskway-endorse-v1\n{cred}\n{alg}\n{key}\n{name}\n{ts}")
}

/// The same for an AI client's device: its role is signed, so neither the server nor anyone
/// else can pass it off as a browser, or a browser as one. Browsers keep `endorse_v1`, which
/// computers from before roles also check.
pub fn endorse_v2(cred: &str, alg: i64, key: &str, name: &str, role: &str, ts: i64) -> String {
    format!("deskway-endorse-v2\n{cred}\n{alg}\n{key}\n{name}\n{role}\n{ts}")
}

/// What a trusted browser signs (as SHA-256) to let AI clients control one computer, and with
/// administrator rights or not.
pub fn ai_v1(host: &str, ai: bool, admin: bool, ts: i64) -> String {
    format!("deskway-ai-v1\n{host}\n{}\n{}\n{ts}", ai as u8, admin as u8)
}

/// What an approved AI client signs (as SHA-256) to sign in again without the account password:
/// the account name it signs in as, and the time.
pub fn ai_login_v1(name: &str, ts: i64) -> String {
    format!("deskway-ai-login-v1\n{name}\n{ts}")
}

/// What a device signs (as SHA-256) to remove another.
pub fn remove_v1(cred: &str, ts: i64) -> String {
    format!("deskway-remove-v1\n{cred}\n{ts}")
}

/// What a device signs (as SHA-256) to give a device of the account (itself or another) a new
/// name. The newest one a trusted device signed counts.
pub fn rename_v1(cred: &str, name: &str, ts: i64) -> String {
    format!("deskway-rename-v1\n{cred}\n{name}\n{ts}")
}

/// What a trusted device signs (as SHA-256) to turn device checks off (account password only)
/// or on again.
pub fn policy_v1(keys: bool, ts: i64) -> String {
    format!("deskway-policy-v1\n{}\n{ts}", keys as u8)
}

/// What a device signs (as SHA-256) when it is added at a computer: "trust these devices of mine
/// as well". `list` holds (credential id, algorithm, key) in any order; the string has them
/// sorted by id.
pub fn vouch_v1(host: &str, list: &[(&str, i64, &str)]) -> String {
    let mut sorted = list.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut text = format!("deskway-vouch-v1\n{host}\n");
    for (id, alg, key) in sorted {
        text.push_str(&format!("{id}\n{alg}\n{key}\n"));
    }
    text
}

/// What a device's six-digit code is made from (the first four bytes of its SHA-256).
pub fn code_v1(key: &str) -> String {
    format!("deskway-code-v1\n{key}")
}

/// The same for an AI client's device, with its role: a server that called it a browser (or
/// the other way round) would show different digits on the two sides.
pub fn code_v2(role: &str, key: &str) -> String {
    format!("deskway-code-v2\n{role}\n{key}")
}

/// What a computer's six digits are made from while it waits for approval: its id, which the
/// server gave it. The computer shows them and so does the approval, so a person approves the
/// computer in front of them and not another one by the same name.
pub fn computer_code_v1(id: &str) -> String {
    format!("deskway-computer-code-v1\n{id}")
}

/// Six digits made from a device's key (and an AI client's role). The device and whoever
/// approves it compute them on their own, so a person can see they are looking at the same
/// request.
pub fn key_code(role: crate::types::Role, key: &str) -> String {
    six_digits(&match role {
        crate::types::Role::Browser => code_v1(key),
        crate::types::Role::Ai => code_v2("ai", key),
    })
}

/// The six digits of a computer waiting for approval (`computer_code_v1`).
pub fn computer_code(id: &str) -> String {
    six_digits(&computer_code_v1(id))
}

/// The first four bytes of the text's SHA-256, big-endian, modulo 10^6, zero-padded.
fn six_digits(text: &str) -> String {
    let h = sha256(text.as_bytes());
    format!("{:06}", u32::from_be_bytes(h[..4].try_into().unwrap()) % 1_000_000)
}

/// A pairing code as it is MACed: upper case, no separators, and the letters people confuse
/// with digits mapped to them.
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

/// What a browser MACs with the pairing code to be added.
pub fn pair_v1(host: &str, cred: &str, alg: i64, key: &str, name: &str) -> String {
    format!("deskway-pair-v1\n{host}\n{cred}\n{alg}\n{key}\n{name}")
}

/// What the host MACs with the pairing code in its reply, so the browser can pin its key.
pub fn paired_v1(host: &str, cred: &str, host_key: &str) -> String {
    format!("deskway-paired-v1\n{host}\n{cred}\n{host_key}")
}

/// A fresh nonce: 16 random bytes, base64url without padding.
pub fn new_nonce() -> String {
    URL_SAFE_NO_PAD.encode(random::<16>())
}
