//! End-to-end authentication between controllers (browsers) and this host.
//!
//! The server only carries messages. It cannot start a session by itself: every offer must be
//! signed by a passkey that was paired with this host in person, or carry a short-lived resume
//! token that the host handed out over an already authenticated connection. The host signs its
//! answer, so the browser knows it reached this host and not an impostor.
//!
//! Each signature covers the WebRTC offer and answer, which contain the DTLS fingerprints, so a
//! server that rewrote either one would break the signature instead of silently sitting in the
//! middle.
//!
//! [`Trust`] is the state on disk and in memory; its parts live in the submodules:
//! - `pairing`: the pairing window and its code (QR code at this computer),
//! - `asks`: devices asking the person at this computer to let them in,
//! - `log`: the account's signed device log and who it makes trusted,
//! - `ai`: whether AI clients may control this computer (a browser's signed switch, and the
//!   stop pressed here),
//! - `offers`: checking an offer, and resume tokens,
//! - `webauthn`: the signature checks underneath.
//!
//! Every signed string comes from `wire::challenge`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{ensure, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use hmac::{Hmac, Mac};
use p256::ecdsa::signature::Signer;
use p256::pkcs8::EncodePublicKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use wire::crypto::random;
use wire::challenge::answer_v1;
use wire::types::{AnswerAuth, Role};

pub mod ai;
pub mod asks;
mod log;
pub mod offers;
pub mod pairing;
mod webauthn;

#[cfg(test)]
mod tests;

#[cfg(windows)]
pub use ai::admin_allowed;
pub use ai::AiState;
use asks::Ask;

use offers::Resume;
use webauthn::public_key;

/// The most devices that may control one computer (the panel says so when it is reached).
pub const MAX_CONTROLLERS: usize = 50;

/// Adding a device was refused because this computer already has `MAX_CONTROLLERS`.
#[derive(Debug)]
pub struct DevicesFull;

impl std::fmt::Display for DevicesFull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "this computer already has the most devices it takes ({MAX_CONTROLLERS})")
    }
}

impl std::error::Error for DevicesFull {}

/// Unix seconds now: when a device was added or used, and the time a session holds a seat since.
pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn hmac(key: &[u8], msg: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn hmac_ok(key: &[u8], msg: &str, tag: &[u8]) -> bool {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    m.update(msg.as_bytes());
    m.verify_slice(tag).is_ok()
}

fn b64u(s: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).context("bad base64")
}

/// A device name from a message: at most 60 characters, `fallback` (a catalog key) if none.
fn device_name(v: &Value, fallback: &str) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| crate::i18n::t(fallback)).chars().take(60).collect()
}

/// A device allowed to control this host: a person's browser, or an AI client's `deskway`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Controller {
    /// Credential id, base64url.
    pub id: String,
    pub alg: i64,
    /// SubjectPublicKeyInfo, base64.
    pub key: String,
    pub name: String,
    pub added: u64,
    #[serde(default)]
    pub sign_count: u32,
    /// When it last controlled this computer (Unix seconds); 0: not yet.
    #[serde(default)]
    pub used: u64,
    /// What it is. Only a browser signs for others, and only an AI client takes the AI seat.
    #[serde(default)]
    pub role: Role,
}

impl Controller {
    /// A device described by a message (`cred`, `alg`, `key`, `name`, and `role` when an AI
    /// client), added now.
    fn from_message(m: &Value, fallback_name: &str) -> Controller {
        Controller {
            id: m["cred"].as_str().unwrap_or_default().into(),
            alg: m["alg"].as_i64().unwrap_or_default(),
            key: m["key"].as_str().unwrap_or_default().into(),
            name: device_name(&m["name"], fallback_name),
            added: unix_now(),
            sign_count: 0,
            used: 0,
            role: if m["role"] == "ai" { Role::Ai } else { Role::Browser },
        }
    }

    /// Its key decodes and is of an accepted algorithm.
    fn usable(&self) -> bool {
        STANDARD.decode(&self.key).ok().is_some_and(|der| public_key(self.alg, &der).is_ok())
    }
}

/// Where the trust state lives. In service mode the directory is readable only by SYSTEM,
/// administrators and the network service.
pub struct Trust {
    dir: PathBuf,
    host_id: String,
    rp_id: String,
    origin: String,
    host_key: p256::ecdsa::SigningKey,
    /// Devices added at this computer (or vouched for by one that was).
    controllers: Vec<Controller>,
    /// Those plus every device the account's signed log adds through them.
    trusted: Vec<Controller>,
    log: Vec<Value>,
    asks: Vec<Ask>,
    /// Devices removed here, by the person at this computer. The account's log cannot bring
    /// them back; adding them here again (QR code, or allowing a request) can.
    blocked: Vec<String>,
    /// False when a trusted device turned device checks off: the account password is enough.
    keys_required: bool,
    resume: HashMap<String, Resume>,
    /// The latest AI switch for this computer, and the stop pressed here.
    ai: ai::Ai,
}

/// The parts of the server URL a passkey is bound to.
pub fn origin_of(server: &str) -> Result<(String, String)> {
    let address = deskway_server_connection::Address::parse(server)?;
    let server = address.base();
    let (scheme, rest) = server.split_once("://").context("server URL needs a scheme")?;
    let authority = rest.split('/').next().unwrap_or_default().to_ascii_lowercase();
    ensure!(!authority.is_empty() && !authority.contains('@'), "bad server URL");
    let host = if authority.starts_with('[') {
        authority.split(']').next().unwrap_or_default().to_string() + "]"
    } else {
        authority.split(':').next().unwrap_or_default().to_string()
    };
    Ok((host, format!("{}://{authority}", scheme.to_ascii_lowercase())))
}

impl Trust {
    pub fn open(dir: PathBuf, host_id: &str, server: &str) -> Result<Trust> {
        let (rp_id, origin) = origin_of(server)?;
        let key_path = dir.join("host-key");
        let host_key = match std::fs::read(&key_path) {
            Ok(b) => p256::ecdsa::SigningKey::from_slice(&b).context("host key is damaged")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let k = p256::ecdsa::SigningKey::from_slice(&random::<32>()).context("generate host key")?;
                crate::config::write_private(&key_path, &k.to_bytes())?;
                k
            }
            Err(e) => return Err(e).context("read host key"),
        };
        let controllers = match std::fs::read(dir.join("controllers.json")) {
            Ok(b) => serde_json::from_slice(&b).context("controllers.json is damaged")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("read controllers"),
        };
        let blocked: Vec<String> = std::fs::read(dir.join("blocked.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let trusted = controllers.clone();
        let ai = ai::Ai::load(&dir);
        Ok(Trust { dir, host_id: host_id.into(), rp_id, origin, host_key, controllers, trusted, log: Vec::new(), asks: Vec::new(), blocked, keys_required: true, resume: HashMap::new(), ai })
    }

    /// Every device allowed to control this computer.
    pub fn controllers(&self) -> &[Controller] {
        &self.trusted
    }

    /// Reloads the list, which an administrator may have changed from the command line.
    pub fn reload(&mut self) {
        if let Ok(b) = std::fs::read(self.dir.join("controllers.json")) {
            if let Ok(list) = serde_json::from_slice::<Vec<Controller>>(&b) {
                // A removed controller also loses its resume tokens at once.
                self.controllers = list;
                self.recompute();
            }
        }
    }

    fn save(&self) -> Result<()> {
        crate::config::write_private(&self.dir.join("controllers.json"), &serde_json::to_vec_pretty(&self.controllers)?)
    }

    fn save_blocked(&self) -> Result<()> {
        crate::config::write_private(&self.dir.join("blocked.json"), &serde_json::to_vec(&self.blocked)?)
    }

    /// The host's public key, SubjectPublicKeyInfo in base64, as the browser pins it.
    pub fn host_public(&self) -> String {
        let der = self.host_key.verifying_key().to_public_key_der().expect("encode host key");
        STANDARD.encode(der.as_bytes())
    }

    /// Signs (offer, answer) pairs of one session on the session's own thread: the first
    /// answer and those to ICE restarts.
    pub fn answer_signer(&self, session: &str) -> Box<dyn Fn(&str, &str) -> AnswerAuth + Send> {
        let (key, hk, host, session) = (self.host_key.clone(), self.host_public(), self.host_id.clone(), session.to_string());
        Box::new(move |offer, answer| {
            let sig: p256::ecdsa::Signature = key.sign(answer_v1(&host, &session, offer, answer).as_bytes());
            AnswerAuth { sig: STANDARD.encode(sig.to_bytes()), hk: hk.clone() }
        })
    }

    /// Whether one more device (`id`, unless it is here already) fits.
    fn room_for(&self, id: &str) -> Result<()> {
        if self.controllers.len() >= MAX_CONTROLLERS && !self.controllers.iter().any(|c| c.id == id) {
            return Err(DevicesFull.into());
        }
        Ok(())
    }

    /// A device just controlled this computer: when, for the panel's list.
    pub fn mark_used(&mut self, id: &str) -> Result<()> {
        let Some(c) = self.controllers.iter_mut().find(|c| c.id == id) else { return Ok(()) };
        c.used = unix_now();
        self.save()
    }

    /// Adds a device here, with the devices it vouches for (pairing, or an answered ask).
    fn add_roots(&mut self, device: Controller, vouch: Vec<Controller>) -> Result<()> {
        self.room_for(&device.id)?;
        // Added here on purpose: whatever was removed here before no longer applies to it.
        self.blocked.retain(|b| b != &device.id);
        self.save_blocked()?;
        for c in std::iter::once(device).chain(vouch.into_iter().filter(|v| !self.blocked.contains(&v.id))) {
            self.controllers.retain(|x| x.id != c.id);
            if self.controllers.len() >= MAX_CONTROLLERS {
                // The device itself fits (room_for); devices it vouches for beyond the limit wait.
                break;
            }
            self.controllers.push(c);
        }
        self.save()?;
        self.recompute();
        Ok(())
    }

    /// Removes devices here, at this computer: they stop working at once, and the account's
    /// device log cannot bring them back. Returns how many were removed.
    pub fn remove_here(&mut self, ids: &[String]) -> Result<usize> {
        let mut n = 0;
        for id in ids {
            if self.trusted.iter().any(|c| &c.id == id) || self.controllers.iter().any(|c| &c.id == id) {
                n += 1;
            }
            if !self.blocked.contains(id) {
                self.blocked.push(id.clone());
            }
        }
        self.controllers.retain(|c| !ids.contains(&c.id));
        self.resume.retain(|_, r| !ids.contains(&r.controller));
        self.save()?;
        self.save_blocked()?;
        self.recompute();
        Ok(n)
    }

    /// Removes every device that can control this computer.
    pub fn remove_all(&mut self) -> Result<usize> {
        let mut ids: Vec<String> = self.trusted.iter().chain(self.controllers.iter()).map(|c| c.id.clone()).collect();
        ids.sort();
        ids.dedup();
        self.remove_here(&ids)
    }

    pub fn remove(&mut self, id: &str) -> Result<bool> {
        Ok(self.remove_here(&[id.to_string()])? > 0)
    }
}
