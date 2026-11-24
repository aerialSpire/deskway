//! The account's signed device log: which devices this computer trusts beyond those added here.
//! Entries count only when signed by a device this computer already trusts: the server can hide
//! entries, but cannot add any.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value;

use super::webauthn::{check_assertion, raw_ok, AssertionCheck};
use super::{b64u, device_name, Controller, Trust, MAX_CONTROLLERS};
use wire::crypto::sha256;
use wire::challenge::{endorse_v1, endorse_v2, policy_v1, remove_v1, rename_v1};
use wire::types::Role;

impl Trust {
    /// Takes the account's device log from the server and works out who is trusted; then this
    /// computer's AI switch (`ai`, null if never set), which only those can sign.
    pub fn set_log(&mut self, entries: &Value, ai: &Value) {
        self.log = entries.as_array().cloned().unwrap_or_default();
        self.adopt();
        self.recompute();
        self.take_switch(ai);
    }

    /// A computer that has never trusted any device takes the account's device list as it is,
    /// once. It was just signed in by someone at it with the account password, so that is the
    /// moment to trust the server's word, like SSH trusting a host key the first time. From then
    /// on only signed changes count. Never again after that, even if every device is removed.
    /// AI clients are not adopted: they reach a computer only through a browser's signature.
    fn adopt(&mut self) {
        let marker = self.dir.join("adopted");
        if !self.controllers.is_empty() || marker.exists() {
            return;
        }
        let mut list: Vec<Controller> = Vec::new();
        for e in &self.log {
            let cred = e["cred"].as_str().unwrap_or_default().to_string();
            match e["op"].as_str() {
                Some("add") => {
                    let c = Controller { added: 0, ..Controller::from_message(e, "device.default") };
                    if c.role == Role::Browser && c.usable() && !cred.is_empty() && !list.iter().any(|x| x.id == cred) {
                        list.push(c);
                    }
                }
                Some("remove") => list.retain(|c| c.id != cred),
                _ => {}
            }
        }
        if list.is_empty() || list.len() > MAX_CONTROLLERS {
            return;
        }
        for c in &mut list {
            c.used = self.controllers.iter().find(|x| x.id == c.id).map_or(0, |x| x.used);
        }
        self.controllers = list;
        if self.save().is_ok() {
            let _ = crate::config::write_private(&marker, b"1");
        }
    }

    /// Whether `e` (`by`, `sig`, and for a passkey `ad` and `cd`) is signed over `challenge`
    /// by one of the browsers of `set`. An AI client signs for nothing and no one: whoever got
    /// hold of one could otherwise let others in, or keep them in after it is removed.
    pub(super) fn signed_by(&self, set: &[Controller], e: &Value, challenge: &[u8]) -> bool {
        let by = e["by"].as_str().unwrap_or_default();
        let Some(signer) = set.iter().find(|c| c.id == by && c.role == Role::Browser) else { return false };
        if e["ad"].as_str().unwrap_or_default().is_empty() {
            return raw_ok(signer.alg, &signer.key, challenge, e["sig"].as_str().unwrap_or_default());
        }
        let (Ok(ad), Ok(cd), Ok(sig), Ok(key)) = (
            b64u(e["ad"].as_str().unwrap_or_default()),
            b64u(e["cd"].as_str().unwrap_or_default()),
            b64u(e["sig"].as_str().unwrap_or_default()),
            STANDARD.decode(&signer.key),
        ) else {
            return false;
        };
        check_assertion(&AssertionCheck { rp_id: &self.rp_id, origin: &self.origin, challenge, alg: signer.alg, key: &key, auth_data: &ad, client_data: &cd, sig: &sig }).is_ok()
    }

    /// Who is trusted: the devices added here (less those removed here), then the log's
    /// signed additions, removals, renames and device-check policy, in order.
    pub(super) fn recompute(&mut self) {
        let mut set: Vec<Controller> = self.controllers.iter().filter(|c| !self.blocked.contains(&c.id)).cloned().collect();
        let mut removed_roots = Vec::new();
        let mut keys = true;
        for e in self.log.clone() {
            let cred = e["cred"].as_str().unwrap_or_default().to_string();
            let ts = e["ts"].as_i64().unwrap_or_default();
            match e["op"].as_str() {
                Some("add") => {
                    let alg = e["alg"].as_i64().unwrap_or_default();
                    let key = e["key"].as_str().unwrap_or_default().to_string();
                    let name = device_name(&e["name"], "device.default");
                    // An AI client's role is signed (endorse-v2): neither can pass for the other.
                    let role = if e["role"] == "ai" { Role::Ai } else { Role::Browser };
                    let c = Controller { id: cred.clone(), alg, key, name, added: ts.max(0) as u64, sign_count: 0, used: 0, role };
                    let signed = match role {
                        Role::Browser => endorse_v1(&cred, alg, &c.key, &c.name, ts),
                        Role::Ai => endorse_v2(&cred, alg, &c.key, &c.name, "ai", ts),
                    };
                    let challenge = sha256(signed.as_bytes());
                    if c.usable() && !self.blocked.contains(&cred) && !set.iter().any(|x| x.id == cred) && self.signed_by(&set, &e, &challenge) {
                        set.push(c);
                    }
                }
                Some("policy") => {
                    let on = e["keys"].as_bool().unwrap_or(true);
                    if self.signed_by(&set, &e, &sha256(policy_v1(on, ts).as_bytes())) {
                        keys = on;
                    }
                }
                // A trusted device names a trusted device (itself or another): shown from then on.
                Some("rename") => {
                    let raw = e["name"].as_str().unwrap_or_default();
                    let signed = !raw.is_empty() && self.signed_by(&set, &e, &sha256(rename_v1(&cred, raw, ts).as_bytes()));
                    if let Some(c) = set.iter_mut().find(|c| c.id == cred).filter(|_| signed) {
                        c.name = device_name(&e["name"], "device.default");
                    }
                }
                Some("remove") => {
                    // A removal covers the device as it was then. Added here again later, by
                    // the person at this computer, it stays.
                    let re_added = self.controllers.iter().any(|c| c.id == cred && c.added as i64 > ts);
                    if !re_added && set.iter().any(|c| c.id == cred) && self.signed_by(&set, &e, &sha256(remove_v1(&cred, ts).as_bytes())) {
                        set.retain(|c| c.id != cred);
                        removed_roots.push(cred);
                    }
                }
                _ => {}
            }
        }
        // A signed removal also forgets a device added here, for good.
        if removed_roots.iter().any(|r| self.controllers.iter().any(|c| &c.id == r)) {
            self.controllers.retain(|c| !removed_roots.contains(&c.id));
            let _ = self.save();
        }
        self.resume.retain(|_, r| set.iter().any(|c| c.id == r.controller));
        self.trusted = set;
        self.keys_required = keys;
        self.retry_switch();
    }
}
