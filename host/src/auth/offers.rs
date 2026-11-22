//! Checking that an offer comes from a trusted device, and the resume tokens that let a browser
//! reconnect after a network drop without another passkey prompt.

use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde_json::Value;

use super::webauthn::raw_ok;
use super::{hmac_ok, Trust};
use wire::crypto::{random, sha256};
use wire::challenge::{connect_v3, resume_v3};
use wire::control::Control;
use wire::types::Role;

/// Resume tokens never outlive the passkey check they come from by more than this.
pub(super) const RESUME_FOR: Duration = Duration::from_secs(8 * 3600);

/// A verified offer.
pub struct Verified {
    /// The controller's credential id.
    pub controller: String,
    pub name: String,
    /// What it is: a browser takes the desk seat, an AI client the AI seat.
    pub role: Role,
    /// When the passkey last proved it was present. Resume tokens never outlive this by more
    /// than eight hours, however often they are renewed.
    pub since: Instant,
}

pub(super) struct Resume {
    pub(super) key: [u8; 32],
    pub(super) controller: String,
    pub(super) name: String,
    pub(super) expires: Instant,
    pub(super) since: Instant,
}

impl Trust {
    /// Checks that an offer comes from a trusted device. `nonce` is the single-use nonce this
    /// host issued for the session (the caller has checked it); the proof must cover it, so a
    /// captured offer cannot be replayed and no clock is involved.
    pub fn verify_offer(&mut self, session: &str, nonce: &str, sdp: &str, force: bool, auth: &Value) -> Result<Verified> {
        let t = Instant::now();
        self.resume.retain(|_, r| r.expires > t);
        ensure!(!session.is_empty() && session.len() <= 64, "bad session id");
        if !self.keys_required && !matches!(auth["k"].as_str(), Some("rs" | "dk")) {
            // Device checks are off: the server has checked the account password.
            // An AI client always signs (the server lets no other offer of one through).
            return Ok(Verified { controller: String::new(), name: crate::i18n::t("device.password"), role: Role::Browser, since: t });
        }
        ensure!(!nonce.is_empty() && auth["nonce"].as_str() == Some(nonce), "offer does not cover this host's nonce");
        match auth["k"].as_str() {
            Some("dk") => {
                let cred = auth["cred"].as_str().unwrap_or_default();
                let c = self.trusted.iter().find(|c| c.id == cred).cloned().context("this device is not trusted by this computer")?;
                let challenge = sha256(connect_v3(&self.host_id, session, nonce, force, sdp).as_bytes());
                ensure!(raw_ok(c.alg, &c.key, &challenge, auth["sig"].as_str().unwrap_or_default()), "device signature does not match");
                Ok(Verified { controller: c.id, name: c.name, role: c.role, since: t })
            }
            Some("rs") => {
                let id = auth["id"].as_str().unwrap_or_default();
                let r = self.resume.get(id).context("resume token expired")?;
                let mac = STANDARD.decode(auth["mac"].as_str().unwrap_or_default()).unwrap_or_default();
                ensure!(hmac_ok(&r.key, &resume_v3(&self.host_id, session, nonce, force, sdp), &mac), "bad resume token");
                // Only browsers are given resume tokens.
                Ok(Verified { controller: r.controller.clone(), name: r.name.clone(), role: Role::Browser, since: r.since })
            }
            _ => bail!("offer is not signed"),
        }
    }

    /// A token for reconnecting without another passkey prompt, sent over the encrypted channel.
    pub fn issue_resume(&mut self, v: &Verified) -> Control {
        let id = URL_SAFE_NO_PAD.encode(random::<16>());
        let key = random::<32>();
        self.resume.insert(id.clone(), Resume { key, controller: v.controller.clone(), name: v.name.clone(), expires: v.since + RESUME_FOR, since: v.since });
        Control::Resume { id, k: STANDARD.encode(key) }
    }

    /// A session ended `taken` or `ended`: the controller that held it does not come back by
    /// itself. An empty controller (device checks off) names no device, so nothing is revoked.
    pub fn revoke_resume(&mut self, controller: &str) {
        if controller.is_empty() {
            return;
        }
        self.resume.retain(|_, r| r.controller != controller);
    }
}
