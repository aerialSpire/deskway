//! Apps and windows, as the person (docs/architecture.md, "MCP and tools"): what is installed and what
//! is open, opening and quitting apps, and moving windows. The system's calls are each
//! platform's (`platform::apps`); what an AI client asks and is told is here, the same on both.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Answer, Code, WorkError};
use crate::platform::apps as native;

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

/// A window on the person's screen, in the screen's own coordinates (not a screenshot's).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Window {
    pub id: u64,
    pub title: String,
    /// Its program, by file name.
    pub app: String,
    pub pid: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub minimized: bool,
    pub maximized: bool,
    pub focused: bool,
}

/// An app the person can open: its name, and what opens it.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct App {
    pub name: String,
    pub path: String,
}

/// Why something could not be opened.
#[derive(Debug, PartialEq)]
pub enum OpenError {
    NotFound,
    Failed(String),
}

/// Where `move` puts a window: its corner, and its size when given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub x: i32,
    pub y: i32,
    pub size: Option<(i32, i32)>,
}

/// How long `app_quit` waits for the programs it asked to end.
const QUIT_WAIT: Duration = Duration::from_secs(5);

pub fn windows() -> Answer {
    Ok(json!(native::windows()))
}

pub fn window_act(id: u64, action: WindowAction, x: Option<i32>, y: Option<i32>, w: Option<i32>, h: Option<i32>) -> Answer {
    let place = match (action, x, y) {
        (WindowAction::Move, Some(x), Some(y)) => Some(Place { x, y, size: w.zip(h) }),
        (WindowAction::Move, ..) => return Err(WorkError::new(Code::InvalidArgs, "move needs x and y", "Pass where its top left corner goes, in screen coordinates (windows lists them).")),
        _ => None,
    };
    if !native::windows().iter().any(|win| win.id == id) {
        return Err(no_window(id));
    }
    // A focus that did not happen says what to do instead: typing now would reach another window.
    let next = if action == WindowAction::Focus { "Click inside the window to focus it before typing." } else { "Call windows again: it may have closed or moved." };
    native::act(id, action, place).map_err(|e| WorkError::new(Code::Failed, e, next))?;
    Ok(json!({ "done": action }))
}

fn no_window(id: u64) -> WorkError {
    WorkError::new(Code::NotFound, format!("no window {id}"), "Call windows for the windows open now.")
}

pub fn apps() -> Answer {
    Ok(json!(native::apps()))
}

/// Opens an app by its name (as `app_list` gives it, any case), a file or folder (with the app
/// the person opens it with), or a web address.
pub fn open(name: &str, args: &[String]) -> Answer {
    let target = named(&native::apps(), name).map_or_else(|| name.to_string(), |a| a.path.clone());
    match native::open(&target, args) {
        Ok(pid) => Ok(json!({ "opened": target, "pid": pid })),
        Err(OpenError::NotFound) => Err(WorkError::new(Code::NotFound, format!("nothing called {name}"), "Call app_list for the apps' names, or pass a full path.")),
        Err(OpenError::Failed(e)) => Err(WorkError::new(Code::Failed, format!("{name}: {e}"), "Check the name or path.")),
    }
}

/// The app called `name`: exactly that name, any case.
fn named<'a>(apps: &'a [App], name: &str) -> Option<&'a App> {
    apps.iter().find(|a| a.name.eq_ignore_ascii_case(name))
}

/// Ends the programs of an app (by its program's name, as `windows` gives it, with or without
/// its extension) or one program: closing their windows, as the person would, or `force`d.
pub fn quit(name: Option<&str>, pid: Option<u32>, force: bool) -> Answer {
    let pids = match (pid, name) {
        (Some(pid), _) => vec![pid],
        (None, Some(name)) => native::pids_named(name),
        (None, None) => return Err(WorkError::new(Code::InvalidArgs, "which app: pass name or pid", "Call windows for the programs open now.")),
    };
    let pids: Vec<u32> = pids.into_iter().filter(|p| native::alive(*p)).collect();
    if pids.is_empty() {
        return Err(WorkError::new(Code::NotFound, format!("no program {}", pid.map(|p| p.to_string()).or(name.map(String::from)).unwrap_or_default()), "Call windows for the programs open now."));
    }
    let asked = native::quit(&pids, force);
    if !force && asked == 0 {
        return Err(WorkError::new(Code::Failed, "it has no window to close", "Pass force to end it."));
    }
    let end = Instant::now() + QUIT_WAIT;
    while Instant::now() < end && pids.iter().any(|p| native::alive(*p)) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let running: Vec<u32> = pids.iter().copied().filter(|p| native::alive(*p)).collect();
    Ok(json!({ "pids": pids, "still_running": running, "next": if running.is_empty() { "" } else { "It may be asking something (unsaved work): take a screenshot, or pass force." } }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_is_found_by_its_whole_name_in_any_case() {
        let apps = [App { name: "Notepad".into(), path: "n.lnk".into() }, App { name: "Notepad++".into(), path: "p.lnk".into() }];
        for (name, want) in [("notepad", Some("n.lnk")), ("NOTEPAD++", Some("p.lnk")), ("note", None)] {
            assert_eq!(named(&apps, name).map(|a| a.path.as_str()), want, "{name}");
        }
    }

    #[test]
    fn moving_needs_a_place_and_an_unknown_window_is_not_found() {
        assert_eq!(window_act(1, WindowAction::Move, Some(0), None, None, None).unwrap_err().code, Code::InvalidArgs);
        assert_eq!(window_act(u64::MAX, WindowAction::Minimize, None, None, None, None).unwrap_err().code, Code::NotFound);
        assert_eq!(quit(None, None, false).unwrap_err().code, Code::InvalidArgs);
        assert_eq!(quit(Some("no-such-program-deskway"), None, true).unwrap_err().code, Code::NotFound);
    }
}
