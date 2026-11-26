//! Workspace operations that use the signed-in user's desktop.

use serde::{Deserialize, Serialize};

use super::{apps, elements, Answer, Code, WorkError};

fn depth_default() -> u32 {
    12
}

fn nodes_default() -> usize {
    400
}

/// What `window_act` does to a window.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Focus,
    Minimize,
    Maximize,
    Restore,
    Close,
    Move,
}

/// What `ui_act` does to an element.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    /// What clicking it does (a button, a link, a menu item).
    Invoke,
    Toggle,
    Expand,
    Collapse,
    /// Chooses it (an item of a list, a tab, a radio button).
    Select,
    Focus,
    SetValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesktopOp {
    ClipboardGet,
    ClipboardSet { text: String },
    Windows,
    WindowAct {
        id: u64,
        action: WindowAction,
        #[serde(default)]
        x: Option<i32>,
        #[serde(default)]
        y: Option<i32>,
        #[serde(default)]
        w: Option<i32>,
        #[serde(default)]
        h: Option<i32>,
    },
    AppList,
    AppOpen {
        name: String,
        #[serde(default)]
        args: Vec<String>,
    },
    AppQuit {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        force: bool,
    },
    UiTree {
        window: u64,
        #[serde(default = "depth_default")]
        depth: u32,
        #[serde(default = "nodes_default")]
        max_nodes: usize,
    },
    UiAct {
        window: u64,
        element: String,
        action: UiAction,
        #[serde(default)]
        value: Option<String>,
    },
}

pub(super) fn call(op: DesktopOp) -> Answer {
    match op {
        DesktopOp::ClipboardGet => Ok(serde_json::json!({ "text": crate::platform::clipboard_text() })),
        DesktopOp::ClipboardSet { text } => {
            if crate::platform::set_clipboard_text(&text) {
                Ok(serde_json::json!({ "done": "set" }))
            } else {
                Err(WorkError::new(Code::Busy, "another program holds the clipboard", "Try again in a moment."))
            }
        }
        DesktopOp::Windows => apps::windows(),
        DesktopOp::WindowAct { id, action, x, y, w, h } => apps::window_act(id, action, x, y, w, h),
        DesktopOp::AppList => apps::apps(),
        DesktopOp::AppOpen { name, args } => apps::open(&name, &args),
        DesktopOp::AppQuit { name, pid, force } => apps::quit(name.as_deref(), pid, force),
        DesktopOp::UiTree { window, depth, max_nodes } => elements::tree(window, depth, max_nodes),
        DesktopOp::UiAct { window, element, action, value } => elements::act(window, &element, action, value.as_deref()),
    }
}
