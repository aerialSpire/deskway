//! Whether AI clients may control this computer (docs/architecture.md, "The AI switch").
//!
//! A browser of the account turns it on or off for this computer, signing `deskway-ai-v1`; the
//! server only stores the latest and passes it on with the device log. This computer takes a
//! switch only from a browser it trusts and only if it is newer than every switch it has seen,
//! and keeps it, so a server that sends an older one, or none, cannot turn AI back on.
//!
//! "Stop", pressed here, ends AI control at once and holds until a browser signs a switch
//! newer than the one it stopped: only a person can open it again, from their devices.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Trust;
use wire::challenge::ai_v1;
use wire::crypto::sha256;

/// What AI clients may do on this computer now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiState {
    /// Never allowed, or turned off.
    Off,
    /// Allowed; `admin`: with administrator rights.
    On { admin: bool },
    /// Allowed, but stopped here: until a newer switch.
    Stopped,
}

/// The switch this computer took last.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct Switch {
    ts: i64,
    ai: bool,
    admin: bool,
}

/// What this computer keeps (ai.json, beside the trusted devices).
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
pub(super) struct Ai {
    switch: Option<Switch>,
    /// Stop was pressed while the switch of this time was in force.
    stopped: Option<i64>,
}

impl Ai {
    pub(super) fn load(dir: &Path) -> Ai {
        std::fs::read(dir.join("ai.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn state(&self) -> AiState {
        match self.switch {
            Some(s) if s.ai && self.stopped.is_some_and(|t| t >= s.ts) => AiState::Stopped,
            Some(s) if s.ai => AiState::On { admin: s.admin },
            _ => AiState::Off,
        }
    }
}

/// Whether AI clients may use administrator rights here now, as the switch kept in `dir` says:
/// for the Windows service, which starts the admin worker only then.
#[cfg(any(windows, test))]
pub fn admin_allowed(dir: &Path) -> bool {
    Ai::load(dir).state() == AiState::On { admin: true }
}

#[cfg(test)]
#[test]
fn administrator_rights_are_allowed_only_while_on_with_them_and_not_stopped() {
    let dir = std::env::temp_dir().join(format!("deskway-admin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (kept, want) in [
        (None, false),
        (Some(r#"{"switch":{"ts":5,"ai":true,"admin":true},"stopped":null}"#), true),
        (Some(r#"{"switch":{"ts":5,"ai":true,"admin":false},"stopped":null}"#), false),
        (Some(r#"{"switch":{"ts":5,"ai":true,"admin":true},"stopped":5}"#), false),
        (Some(r#"{"switch":{"ts":5,"ai":false,"admin":true},"stopped":null}"#), false),
    ] {
        let _ = std::fs::remove_file(dir.join("ai.json"));
        if let Some(k) = kept {
            std::fs::write(dir.join("ai.json"), k).unwrap();
        }
        assert_eq!(admin_allowed(&dir), want, "{kept:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

impl Trust {
    pub fn ai_state(&self) -> AiState {
        self.ai.state()
    }

    /// Stop, pressed here: AI control ends until a newer switch. Nothing to do when it is not on.
    pub fn stop_ai(&mut self) {
        if let (AiState::On { .. }, Some(s)) = (self.ai.state(), self.ai.switch) {
            self.ai.stopped = Some(s.ts);
            self.save_ai();
        }
    }

    /// The server's copy of this computer's latest switch: taken if it is for this computer,
    /// signed by a browser trusted now, and newer than the one this computer has.
    pub(super) fn take_switch(&mut self, v: &Value) {
        let (Some(ai), Some(admin), Some(ts)) = (v["ai"].as_bool(), v["admin"].as_bool(), v["ts"].as_i64()) else { return };
        if v["host"] != self.host_id.as_str() || self.ai.switch.is_some_and(|s| s.ts >= ts) {
            return;
        }
        if !self.signed_by(&self.trusted, v, &sha256(ai_v1(&self.host_id, ai, admin, ts).as_bytes())) {
            return;
        }
        self.ai.switch = Some(Switch { ts, ai, admin });
        self.save_ai();
    }

    fn save_ai(&self) {
        let written = serde_json::to_vec(&self.ai).map_err(anyhow::Error::from).and_then(|b| crate::config::write_private(&self.dir.join("ai.json"), &b));
        if let Err(e) = written {
            eprintln!("could not keep the AI switch: {e:#}");
        }
    }
}
