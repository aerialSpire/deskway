//! The tools a computer carries out for AI clients, one module per area. Each is declared once in
//! `proto/tools.json`; the table here maps a name to its function, and a test checks the two
//! name the same tools of each batch built so far. A tool is `fn(&Granted, &Value, &Ctx)`: it
//! runs only with the gate's leave, and is only translation between its arguments and the
//! computer (`Platform`, `Workspace`).

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::display::DisplayInfo;
use crate::mcp::gate::{Granted, ToolError};
use crate::mcp::{Facts, SharedFacts};
use crate::platform::{Platform, Replies};

mod audit;
mod info;
pub mod tunnel;

#[cfg(desktop_host)]
pub mod desktop;
#[cfg(desktop_host)]
mod screen;
#[cfg(desktop_host)]
mod work;

/// The latest screenshot of a session: what its pixels are of.
#[derive(Clone, Debug, PartialEq)]
pub struct Shot {
    pub display: DisplayInfo,
    pub w: u32,
    pub h: u32,
}

/// What one AI session's tools work with.
pub struct Ctx {
    pub facts: SharedFacts,
    pub platform: Option<Arc<dyn Platform>>,
    /// The AI session: its input is the AI client's hand (`platform::local`).
    pub session: String,
    /// Answers the platform gives under an id (screenshots).
    pub replies: Arc<Replies>,
    pub shot: Mutex<Option<Shot>>,
    /// The person's workspace, when someone is signed in (the computer's, kept by the host:
    /// its programs outlive sessions).
    pub workspace: Option<Arc<dyn crate::workspace::Workspace>>,
    /// The loopback ports this session's client may reach (`tunnel`), which its session reads.
    pub tunnels: tunnel::Allowed,
    /// The person's workspace as an administrator, where it is a process of its own (the
    /// Windows admin worker); `work` sends it what is asked as an administrator.
    pub admin: Option<Arc<dyn crate::workspace::Workspace>>,
    /// What AI clients did here, read only (`audit_read`).
    pub audit: Arc<crate::audit::Audit>,
}

impl Ctx {
    /// The host's facts, with whether someone is signed in as the workspace says it now.
    pub fn facts(&self) -> Facts {
        let mut f = self.facts.read().unwrap_or_else(|e| e.into_inner()).clone();
        f.user_session = self.workspace.as_ref().is_some_and(|w| w.available());
        f
    }
}

/// What a tool gives back: data, or an image with what it shows.
pub enum Output {
    Data(Value),
    Image { jpeg: Vec<u8>, about: Value },
}

pub(crate) type Tool = fn(&Granted, &Value, &Ctx) -> Result<Output, ToolError>;

pub(crate) type Table = &'static [(&'static str, Tool)];

pub(crate) const BASE: Table = &[
    ("computer_info", info::computer_info),
    ("audit_read", audit::audit_read),
];
