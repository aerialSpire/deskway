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
mod screen;
pub mod tunnel;
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
    pub platform: Arc<dyn Platform>,
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

pub(crate) const TABLE: &[(&str, Tool)] = &[
    ("computer_info", info::computer_info),
    ("audit_read", audit::audit_read),
    ("screenshot", screen::screenshot),
    ("wait_for", screen::wait_for),
    ("click", screen::click),
    ("move", screen::pointer_move),
    ("drag", screen::drag),
    ("scroll", screen::scroll),
    ("key", screen::key),
    ("type", screen::type_text),
    ("run", work::work),
    ("proc_start", work::work),
    ("proc_write", work::work),
    ("proc_read", work::work),
    ("proc_kill", work::work),
    ("proc_list", work::work),
    ("fs_list", work::work),
    ("fs_stat", work::work),
    ("fs_read", work::work),
    ("fs_write", work::work),
    ("fs_hash", work::work),
    ("fs_move", work::work),
    ("fs_delete", work::work),
    ("clipboard_get", work::work),
    ("clipboard_set", work::work),
    ("tunnel", tunnel::tunnel),
    ("windows", work::work),
    ("window_act", work::work),
    ("app_list", work::work),
    ("app_open", work::work),
    ("app_quit", work::work),
    ("ui_tree", work::work),
    ("ui_act", work::work),
];

/// The tools this computer has.
pub fn implemented() -> Vec<&'static str> {
    TABLE.iter().map(|(n, _)| *n).collect()
}

pub fn has(name: &str) -> bool {
    TABLE.iter().any(|(n, _)| *n == name)
}

/// Runs the tool the gate let through.
pub fn call(g: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let f = TABLE.iter().find(|(n, _)| *n == g.tool().name).map(|(_, f)| *f).expect("the gate lets only implemented tools through");
    f(g, args, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool here is defined, and every tool of the batches finished is here.
    #[test]
    fn the_table_has_the_tools_of_the_batches_built_and_only_defined_ones() {
        const FINISHED: u32 = 3;
        let here = implemented();
        for t in wire::tools::all() {
            assert!(t.batch > FINISHED || here.contains(&t.name.as_str()), "{} has no function", t.name);
        }
        for name in here {
            assert!(wire::tools::all().iter().any(|t| t.name == name), "{name} is not in proto/tools.json");
        }
    }
}
