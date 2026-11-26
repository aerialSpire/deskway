//! An app's controls, as the person (docs/architecture.md, "MCP and tools"): a window's
//! accessibility tree (`ui_tree`) and acting on one of its elements (`ui_act`) by what it is
//! rather than where it shows. The system's calls are each platform's (`platform::elements`:
//! UI Automation on Windows, the Accessibility API on a Mac); what an AI client asks and is
//! told is here, the same on both.

use serde::Serialize;
use serde_json::json;

use crate::workspace::{Answer, Code, WorkError};
use crate::platform::elements as native;

/// One element of a window: what it is, what it says, where it is on the screen (screen
/// coordinates), and its children.
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
pub struct Node {
    /// What `ui_act` takes to find it again.
    pub id: String,
    pub role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

pub use crate::workspace::UiAction;

/// Why an element could not be found or acted on.
#[derive(Debug, PartialEq)]
pub enum UiError {
    NoWindow,
    NoElement,
    /// The element does not take that action.
    Unsupported,
    Failed(String),
}

/// How much of a tree is read: how deep, and how many elements at most.
pub struct Budget {
    pub depth: u32,
    left: usize,
    /// Elements were left out (deeper, or past the number).
    pub truncated: bool,
}

impl Budget {
    pub fn new(depth: u32, nodes: usize) -> Budget {
        Budget { depth, left: nodes, truncated: false }
    }

    /// One more element may be read.
    pub fn take(&mut self) -> bool {
        if self.left == 0 {
            self.truncated = true;
            return false;
        }
        self.left -= 1;
        true
    }
}

pub fn tree(window: u64, depth: u32, nodes: usize) -> Answer {
    let mut budget = Budget::new(depth.clamp(1, 30), nodes.clamp(10, 2000));
    let root = native::tree(window, &mut budget).map_err(|e| failed(window, "", e))?;
    Ok(json!({ "window": window, "root": root, "truncated": budget.truncated }))
}

pub fn act(window: u64, element: &str, action: UiAction, value: Option<&str>) -> Answer {
    if action == UiAction::SetValue && value.is_none() {
        return Err(WorkError::new(Code::InvalidArgs, "set_value needs value", "Pass the text to put in it."));
    }
    native::act(window, element, action, value).map_err(|e| failed(window, element, e))?;
    Ok(json!({ "done": action, "element": element }))
}

fn failed(window: u64, element: &str, e: UiError) -> WorkError {
    match e {
        UiError::NoWindow => WorkError::new(Code::NotFound, format!("no window {window}"), "Call windows for the windows open now."),
        UiError::NoElement => WorkError::new(Code::NotFound, format!("no element {element} in window {window}"), "Call ui_tree again: the window has changed."),
        UiError::Unsupported => WorkError::new(Code::InvalidArgs, format!("element {element} does not take that action"), "Use another action its role takes, or click it (screenshot, then click)."),
        UiError::Failed(why) => WorkError::new(Code::Failed, why, "Take a screenshot to see why (a dialog may be in the way)."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_budget_says_when_it_ran_out() {
        let mut b = Budget::new(5, 2);
        assert!(b.take() && b.take() && !b.truncated);
        assert!(!b.take() && b.truncated, "the third is left out, and that is said");
    }

    #[test]
    fn a_value_to_set_is_needed_and_a_missing_window_is_not_found() {
        assert_eq!(act(1, "x", UiAction::SetValue, None).unwrap_err().code, Code::InvalidArgs);
        assert_eq!(tree(u64::MAX - 1, 3, 50).unwrap_err().code, Code::NotFound);
    }

    #[test]
    fn a_node_says_only_what_it_has() {
        let n = Node { id: "1.2".into(), role: "button".into(), name: "OK".into(), ..Node::default() };
        assert_eq!(serde_json::to_value(&n).unwrap(), json!({ "id": "1.2", "role": "button", "name": "OK", "x": 0, "y": 0, "w": 0, "h": 0 }));
    }
}
