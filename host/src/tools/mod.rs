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
mod desktop;
#[cfg(desktop_host)]
mod screen;
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
    pub(super) fn user_workspace(&self) -> Result<&dyn crate::workspace::Workspace, ToolError> {
        self.workspace.as_deref().ok_or_else(|| ToolError::new("no_user_session", "nobody is signed in at this computer", "Sign in at its sign-in screen first."))
    }

    /// The host's facts, with the workspace user's availability read at call time.
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

pub(crate) enum Group {
    Base,
    Work,
    Desktop,
}

// Keep the hello's order on hosts that offer the full set.
pub(crate) const TOOLS: &[(Group, &str, Tool)] = &[
    (Group::Base, "computer_info", info::computer_info),
    (Group::Base, "audit_read", audit::audit_read),
    #[cfg(desktop_host)]
    (Group::Desktop, "screenshot", screen::screenshot),
    #[cfg(desktop_host)]
    (Group::Desktop, "wait_for", screen::wait_for),
    #[cfg(desktop_host)]
    (Group::Desktop, "click", screen::click),
    #[cfg(desktop_host)]
    (Group::Desktop, "move", screen::pointer_move),
    #[cfg(desktop_host)]
    (Group::Desktop, "drag", screen::drag),
    #[cfg(desktop_host)]
    (Group::Desktop, "scroll", screen::scroll),
    #[cfg(desktop_host)]
    (Group::Desktop, "key", screen::key),
    #[cfg(desktop_host)]
    (Group::Desktop, "type", screen::type_text),
    (Group::Work, "run", work::work),
    (Group::Work, "proc_start", work::work),
    (Group::Work, "proc_write", work::work),
    (Group::Work, "proc_read", work::work),
    (Group::Work, "proc_kill", work::work),
    (Group::Work, "proc_list", work::work),
    (Group::Work, "fs_list", work::work),
    (Group::Work, "fs_stat", work::work),
    (Group::Work, "fs_read", work::work),
    (Group::Work, "fs_write", work::work),
    (Group::Work, "fs_hash", work::work),
    (Group::Work, "fs_move", work::work),
    (Group::Work, "fs_delete", work::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "clipboard_get", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "clipboard_set", desktop::work),
    (Group::Work, "tunnel", tunnel::tunnel),
    #[cfg(desktop_host)]
    (Group::Desktop, "windows", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "window_act", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "app_list", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "app_open", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "app_quit", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "ui_tree", desktop::work),
    #[cfg(desktop_host)]
    (Group::Desktop, "ui_act", desktop::work),
];
