//! Whether AI clients may control this computer (docs/architecture.md, "The AI switch").
//!
//! A browser of the account turns it on or off for this computer, signing `deskway-ai-v1`; the
//! server only stores the latest and passes it on with the device log. This computer takes a
//! switch only from a browser it trusts and only if it is newer than every switch it has seen,
//! and keeps it, so a server that sends an older one, or none, cannot turn AI back on.
//!
//! "Stop", pressed here, ends AI control at once and holds until a browser signs a switch
//! newer than the one it stopped: only a person can open it again, from their devices.

use std::{io, path::Path};

use anyhow::Result;
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
    /// The newest switch, refused because no device trusted here signed it: taken as soon as
    /// its signer is trusted (added here, or endorsed in the log), and reported meanwhile so
    /// the browser can say why the switch did not take. Not kept: the server sends it again.
    #[serde(skip)]
    refused: Option<Value>,
}

impl Ai {
    pub(super) fn load(dir: &Path) -> Ai {
        Self::read(dir).unwrap_or_default()
    }

    fn read(dir: &Path) -> Result<Ai> {
        match std::fs::read(dir.join("ai.json")) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Ai::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, dir: &Path) -> Result<()> {
        crate::config::write_private(&dir.join("ai.json"), &serde_json::to_vec(self)?)
    }

    fn state(&self) -> AiState {
        match self.switch {
            Some(s) if s.ai && self.stopped.is_some_and(|t| t >= s.ts) => AiState::Stopped,
            Some(s) if s.ai => AiState::On { admin: s.admin },
            _ => AiState::Off,
        }
    }

    fn stop(&mut self) -> bool {
        if let (AiState::On { .. }, Some(s)) = (self.state(), self.switch) {
            self.stopped = Some(s.ts);
            return true;
        }
        false
    }
}

/// Read the saved switch without mistaking a damaged or unreadable file for a switch that is off.
#[cfg(any(not(desktop_host), test))]
pub fn saved_ai_state(dir: &Path) -> Result<AiState> {
    Ok(Ai::read(dir)?.state())
}

/// Stop at the computer while holding exclusive access to the state directory.
#[cfg(any(not(desktop_host), test))]
pub fn stop_saved_ai(dir: &Path) -> Result<bool> {
    let mut ai = Ai::read(dir)?;
    if ai.stop() {
        ai.save(dir)?;
        return Ok(true);
    }
    Ok(false)
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

    /// The time of the newest switch refused because no device trusted here signed it.
    pub fn ai_refused(&self) -> Option<i64> {
        self.ai.refused.as_ref().and_then(|v| v["ts"].as_i64())
    }

    /// Trust changed: a switch refused before may count now.
    pub(super) fn retry_switch(&mut self) {
        if let Some(v) = self.ai.refused.take() {
            self.take_switch(&v);
        }
    }

    /// Stop, pressed here: AI control ends until a newer switch. Nothing to do when it is not on.
    pub fn stop_ai(&mut self) {
        if self.ai.stop() {
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
            self.ai.refused = Some(v.clone());
            return;
        }
        self.ai.refused = None;
        self.ai.switch = Some(Switch { ts, ai, admin });
        self.save_ai();
    }

    fn save_ai(&self) {
        if let Err(e) = self.ai.save(&self.dir) {
            eprintln!("could not keep the AI switch: {e:#}");
        }
    }
}
