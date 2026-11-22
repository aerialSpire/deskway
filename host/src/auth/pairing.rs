//! The pairing window: opened at this computer, it shows a code (as a QR code) that a browser
//! proves it knows to be added.

use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::webauthn::public_key;
use super::{b64u, hmac, hmac_ok, unix_now, Controller, Trust, MAX_CONTROLLERS};
use wire::crypto::random;
use wire::challenge::{normalize_code, pair_v1, paired_v1, vouch_v1};
use wire::types::{PairedAuth, Role};

const PAIR_FOR: u64 = 10 * 60;
pub(super) const PAIR_TRIES: u32 = 5;
/// Crockford base32: no I, L, O or U, so a code read aloud or typed is hard to get wrong.
const CODE_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A pairing code with 80 random bits: too many to guess, even offline from a captured message.
pub(super) fn new_pairing_code() -> String {
    let bytes = random::<10>();
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = String::new();
    for b in bytes {
        bits = (bits << 8) | b as u32;
        n += 8;
        while n >= 5 {
            n -= 5;
            out.push(CODE_ALPHABET[((bits >> n) & 31) as usize] as char);
        }
    }
    out.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("-")
}

/// An open pairing window, created on this computer by its owner.
#[derive(Serialize, Deserialize, Clone)]
pub struct Pairing {
    pub code: String,
    pub expires: u64,
    pub tries: u32,
}

impl Pairing {
    pub fn new() -> Pairing {
        Pairing { code: new_pairing_code(), expires: unix_now() + PAIR_FOR, tries: PAIR_TRIES }
    }
}

impl Trust {
    fn pairing_path(&self) -> PathBuf {
        self.dir.join("pairing.json")
    }

    /// Opens a pairing window and returns its code. Run on this computer, never from the network.
    pub fn start_pairing(dir: &Path) -> Result<String> {
        let p = Pairing::new();
        crate::config::write_private(&dir.join("pairing.json"), &serde_json::to_vec(&p)?)?;
        Ok(p.code)
    }

    pub fn cancel_pairing(&self) {
        let _ = std::fs::remove_file(self.pairing_path());
    }

    /// The pairing link the panel shows as a QR code, while a window is open.
    pub fn pairing_url(&self) -> Option<String> {
        let p = self.read_pairing()?;
        Some(format!("{}/#pair={}.{}", self.origin, self.host_id, normalize_code(&p.code)))
    }

    fn read_pairing(&self) -> Option<Pairing> {
        let p: Pairing = serde_json::from_slice(&std::fs::read(self.pairing_path()).ok()?).ok()?;
        if p.expires < unix_now() || p.tries == 0 {
            self.cancel_pairing();
            return None;
        }
        Some(p)
    }

    /// Adds a browser's passkey if it proves it knows the pairing code shown on this computer.
    /// Returns the reply that lets the browser pin this host's key.
    pub fn pair(&mut self, m: &Value) -> Result<PairedAuth> {
        let Some(mut p) = self.read_pairing() else { bail!("no pairing window is open") };
        let device = Controller::from_message(m, "device.browser");
        let mac = STANDARD.decode(m["mac"].as_str().unwrap_or_default()).unwrap_or_default();
        let code = normalize_code(&p.code);
        if !hmac_ok(code.as_bytes(), &pair_v1(&self.host_id, &device.id, device.alg, &device.key, &device.name), &mac) {
            p.tries = p.tries.saturating_sub(1);
            if p.tries == 0 {
                self.cancel_pairing();
            } else {
                crate::config::write_private(&self.pairing_path(), &serde_json::to_vec(&p)?)?;
            }
            bail!("wrong pairing code");
        }
        self.room_for(&device.id)?;
        // One pairing per code.
        self.cancel_pairing();
        let vouch = self.vouched(&device, &m["vouch"])?;
        let cred = device.id.clone();
        self.add_roots(device, vouch)?;
        let hk = self.host_public();
        let mac = hmac(code.as_bytes(), &paired_v1(&self.host_id, &cred, &hk));
        Ok(PairedAuth { hk, mac: Some(STANDARD.encode(mac)) })
    }

    /// The other devices a new device vouches for, checked against its own signature. AI
    /// clients among them are left out: they come in only through a browser's endorsement.
    pub(super) fn vouched(&self, device: &Controller, v: &Value) -> Result<Vec<Controller>> {
        ensure!(!device.id.is_empty() && device.id.len() <= 1024 && b64u(&device.id).is_ok(), "bad credential id");
        ensure!(device.role == Role::Browser, "an AI client is added through a browser's approval, not at the computer");
        public_key(device.alg, &STANDARD.decode(&device.key).context("bad key")?)?;
        let Some(items) = v["list"].as_array() else { return Ok(Vec::new()) };
        let list: Vec<Controller> = items.iter().take(MAX_CONTROLLERS).map(|i| Controller::from_message(i, "device.default")).filter(|c| !c.id.is_empty() && c.usable()).collect();
        let signer = std::slice::from_ref(device);
        let signed: Vec<(&str, i64, &str)> = list.iter().map(|c| (c.id.as_str(), c.alg, c.key.as_str())).collect();
        let challenge = wire::crypto::sha256(vouch_v1(&self.host_id, &signed).as_bytes());
        ensure!(self.signed_by(signer, &json!({ "by": device.id, "ad": v["ad"], "cd": v["cd"], "sig": v["sig"] }), &challenge), "the device's list of its other devices is not signed");
        Ok(list.into_iter().filter(|c| c.role == Role::Browser).collect())
    }
}
