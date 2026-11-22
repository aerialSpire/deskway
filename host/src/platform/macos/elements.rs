//! Accessibility controls rooted at the same window object reported by `windows`.
//! An element's path, such as "2.0.5", is valid while that window's tree stays the same.

use std::time::{Duration, Instant};

use objc2_core_foundation::{CFBoolean, CFString};

use crate::workspace::elements::{Budget, Node, UiAction, UiError};

use super::accessibility::{Element, INVALID_ELEMENT, UNSUPPORTED_ACTION, UNSUPPORTED_ATTRIBUTE};
use super::windows::{with_window, WindowError};

fn window_error(error: WindowError) -> UiError {
    match error {
        WindowError::Missing => UiError::NoWindow,
        WindowError::Unavailable => {
            UiError::Failed("macOS did not expose Accessibility controls for this window".into())
        }
        WindowError::Access(code) => UiError::Failed(format!(
            "macOS could not verify the window (Accessibility error {code})"
        )),
    }
}

pub fn tree(window: u64, budget: &mut Budget) -> Result<Node, UiError> {
    with_window(window, |element| node(element, String::new(), 0, budget)).map_err(window_error)
}

fn node(element: &Element, id: String, depth: u32, budget: &mut Budget) -> Node {
    let role = element.string("AXRole").unwrap_or_default();
    let name = ["AXTitle", "AXDescription"]
        .iter()
        .filter_map(|attr| element.string(attr))
        .find(|s| !s.is_empty())
        .unwrap_or_default();
    let value = if element.string("AXSubrole").as_deref() == Some("AXSecureTextField") {
        None
    } else {
        element.string("AXValue").filter(|s| !s.is_empty())
    };
    let rect = element.bounds().unwrap_or_default();
    let mut children = Vec::new();
    let elements = element.elements("AXChildren").unwrap_or_default();
    if depth >= budget.depth {
        budget.truncated |= !elements.is_empty();
    } else {
        for (index, child) in elements.iter().enumerate() {
            if !budget.take() {
                break;
            }
            let path = if id.is_empty() {
                index.to_string()
            } else {
                format!("{id}.{index}")
            };
            children.push(node(child, path, depth + 1, budget));
        }
    }
    Node {
        id,
        role: role.strip_prefix("AX").unwrap_or(&role).to_lowercase(),
        name,
        value,
        x: rect.origin.x as i32,
        y: rect.origin.y as i32,
        w: rect.size.width as i32,
        h: rect.size.height as i32,
        disabled: element.boolean("AXEnabled") == Some(false),
        focused: element.boolean("AXFocused") == Some(true),
        children,
    }
}

fn accessibility_error(code: i32) -> UiError {
    match code {
        INVALID_ELEMENT => UiError::NoElement,
        UNSUPPORTED_ACTION | UNSUPPORTED_ATTRIBUTE => UiError::Unsupported,
        _ => UiError::Failed(format!(
            "macOS rejected the control action (Accessibility error {code})"
        )),
    }
}

pub fn act(
    window: u64,
    element: &str,
    action: UiAction,
    value: Option<&str>,
) -> Result<(), UiError> {
    with_window(window, |root| {
        let mut target = root.clone();
        for index in element.split('.').filter(|_| !element.is_empty()) {
            let index = index.parse::<usize>().map_err(|_| UiError::NoElement)?;
            target = target.elements("AXChildren").map_err(accessibility_error)?.into_iter().nth(index).ok_or(UiError::NoElement)?;
        }
        let yes = CFBoolean::new(true);
        match action {
            UiAction::Invoke | UiAction::Toggle => target.perform("AXPress"),
            UiAction::Expand => target.set("AXExpanded", yes),
            UiAction::Collapse => target.set("AXExpanded", CFBoolean::new(false)),
            UiAction::Select => target.set("AXSelected", yes),
            UiAction::Focus => target.set("AXFocused", yes),
            UiAction::SetValue => {
                let text = value.ok_or(UiError::Unsupported)?;
                if target.string("AXSubrole").as_deref() == Some("AXSecureTextField") {
                    return Err(UiError::Unsupported);
                }
                target.set("AXValue", &CFString::from_str(text)).map_err(accessibility_error)?;
                let until = Instant::now() + Duration::from_millis(250);
                loop {
                    if target.string("AXValue").as_deref() == Some(text) {
                        return Ok(());
                    }
                    if Instant::now() >= until {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                return Err(UiError::Failed("macOS accepted the assignment but the control did not report the requested value".into()));
            }
        }.map_err(accessibility_error)
    }).map_err(window_error)?
}
