//! Devices asking the person at this computer to let them in: shown in the panel with a code
//! both sides compute, until the person answers or the ask expires.

use std::time::{Duration, Instant};

use anyhow::{ensure, Result};
use serde::Serialize;
use serde_json::Value;

use super::{Controller, DevicesFull, Trust};
use wire::challenge::key_code;
use wire::signal::Signal;
use wire::types::{PairedAuth, Reason, Role};

const ASK_FOR: Duration = Duration::from_secs(5 * 60);

/// A device asking the person at this computer to let it in.
#[derive(Clone, Debug, Serialize)]
pub struct Ask {
    pub session: String,
    pub name: String,
    pub from: String,
    pub code: String,
    #[serde(skip)]
    device: Controller,
    #[serde(skip)]
    vouch: Vec<Controller>,
    #[serde(skip)]
    at: Instant,
}

impl Trust {
    /// A device asks to be let in; the panel shows it until the person answers.
    pub fn ask(&mut self, session: &str, m: &Value, from: &str) -> Result<Ask> {
        self.asks.retain(|a| a.at.elapsed() < ASK_FOR);
        ensure!(self.asks.len() < 4, "too many requests waiting");
        self.room_for(m["cred"].as_str().unwrap_or_default())?;
        let device = Controller::from_message(m, "device.default");
        let vouch = self.vouched(&device, &m["vouch"])?;
        let a = Ask { session: session.into(), name: device.name.clone(), from: from.chars().take(40).collect(), code: key_code(Role::Browser, &device.key), device, vouch, at: Instant::now() };
        self.asks.push(a.clone());
        Ok(a)
    }

    pub fn asks(&self) -> Vec<Ask> {
        self.asks.iter().filter(|a| a.at.elapsed() < ASK_FOR).cloned().collect()
    }

    /// The person's answer. Some(reply) for the browser if the request was still open.
    pub fn answer(&mut self, session: &str, allow: bool) -> Result<Option<Signal>> {
        let Some(i) = self.asks.iter().position(|a| a.session == session) else { return Ok(None) };
        let a = self.asks.remove(i);
        let bye = |reason: Reason| -> Result<Option<Signal>> { Ok(Some(Signal::bye(session, reason))) };
        if !allow {
            return bye(Reason::Denied);
        }
        if let Err(e) = self.add_roots(a.device, a.vouch) {
            if e.is::<DevicesFull>() {
                return bye(Reason::Full);
            }
            return Err(e);
        }
        Ok(Some(Signal::Paired { session: session.into(), auth: PairedAuth { hk: self.host_public(), mac: None } }))
    }
}
