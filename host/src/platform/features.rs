//! The capabilities, seats and AI tools offered by this host.
use std::io;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use wire::types::{Reason, View};

use super::Platform;
use super::Pty;
use crate::wake::Wake;
use crate::session::{self, SessionEvent};
use crate::tools::{Group, Tool, TOOLS};

/// What a desk session is started with, besides what every session has (`session::Params`).
pub struct DeskParams {
    pub session: String,
    /// The browser's offer: the picture's track and level come from it.
    pub offer: String,
    /// The viewer constraints the offer carried: the picture starts at that size.
    pub view: Option<View>,
    /// The display requested in the offer.
    pub display: Option<String>,
    pub platform: Arc<dyn Platform>,
    /// The browser may watch but not control.
    pub view_only: bool,
    /// The shared clipboard is on (the computer's setting).
    pub clip_share: bool,
    /// Where the browser's request to switch it goes: the host saves it and answers with
    /// `SessionEvent::ClipShare`.
    pub share: Wake<bool>,
}

#[derive(Clone, Copy)]
pub struct DesktopFeatures {
    pub caps: fn(bool) -> Vec<String>,
    pub spawn: fn(session::Params, DeskParams) -> Sender<SessionEvent>,
    pub ai_awake: fn(bool),
}

pub type OpenPty = Arc<dyn Fn(u16, u16) -> io::Result<Box<dyn Pty>> + Send + Sync>;

/// Identity of the current sign-in and whether it accepts browser workspace access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspaceState {
    pub user: Option<u64>,
    pub locked: bool,
}

impl WorkspaceState {
    pub const READY: Self = Self { user: Some(0), locked: false };
    pub const NO_USER: Self = Self { user: None, locked: false };

    pub fn refusal(self, view_only: bool) -> Option<Reason> {
        if view_only {
            Some(Reason::ViewOnly)
        } else if self.user.is_none() {
            Some(Reason::NoUserSession)
        } else if self.locked {
            Some(Reason::Locked)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
pub enum BrowserSession {
    Desktop(DesktopFeatures),
    Data,
}

#[derive(Clone, Copy, Default)]
pub struct Features {
    pub desktop: Option<DesktopFeatures>,
    pub terminal: bool,
    pub work: bool,
    pub browse: bool,
    pub file_transfers: bool,
    pub view_only: bool,
}

impl Features {
    pub fn browser_session(self, data_only: bool) -> Result<BrowserSession, Reason> {
        if data_only {
            self.terminal.then_some(BrowserSession::Data).ok_or(Reason::NoTerminal)
        } else {
            self.desktop.map(BrowserSession::Desktop).ok_or(Reason::NoDesktop)
        }
    }

    pub fn desk_caps(self) -> Vec<String> {
        self.desktop.map_or_else(Vec::new, |desk| (desk.caps)(self.view_only))
    }

    pub fn signaling_caps(self) -> Vec<String> {
        let mut caps = self.desk_caps();
        caps.extend(self.data_caps());
        caps.push(wire::caps::RELAY_RTT.into());
        caps
    }

    pub fn data_caps(self) -> Vec<String> {
        let mut caps = Vec::new();
        if self.terminal && !self.view_only {
            caps.push(wire::caps::TERMINAL.into());
            caps.push(wire::caps::TERMINAL_KEEP.into());
            if self.browse {
                caps.push(wire::caps::BROWSE.into());
                if self.file_transfers {
                    caps.push(wire::caps::FILE_TRANSFER.into());
                }
            }
        }
        caps
    }

    fn table(self) -> impl Iterator<Item = &'static (Group, &'static str, Tool)> {
        TOOLS.iter().filter(move |(group, _, _)| match group {
            Group::Base => true,
            Group::Work => self.work,
            Group::Desktop => self.desktop.is_some(),
        })
    }

    pub fn tools(self) -> impl Iterator<Item = &'static str> {
        self.table().map(|(_, name, _)| *name)
    }

    pub fn tool(self, name: &str) -> Option<Tool> {
        self.table().find(|(_, tool, _)| *tool == name).map(|(_, _, call)| *call)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(desktop_host)]
    const DESKTOP: Option<DesktopFeatures> = Some(crate::platform::DESKTOP);
    #[cfg(not(desktop_host))]
    const DESKTOP: Option<DesktopFeatures> = None;

    #[test]
    fn the_same_features_select_advertised_and_callable_tools() {
        let base = ["computer_info", "audit_read"];
        let work = [
            "run", "proc_start", "proc_write", "proc_read", "proc_kill", "proc_list",
            "fs_list", "fs_stat", "fs_read", "fs_write", "fs_hash", "fs_move", "fs_delete",
        ];
        let screen = ["screenshot", "wait_for", "click", "move", "drag", "scroll", "key", "type"];
        let clipboard = ["clipboard_get", "clipboard_set"];
        let network = ["tunnel"];
        let apps = ["windows", "window_act", "app_list", "app_open", "app_quit", "ui_tree", "ui_act"];
        let full = [&base[..], &screen, &work, &clipboard, &network, &apps].concat();
        let desktop_host = DESKTOP.map(|desktop| (Features { desktop: Some(desktop), work: true, ..Features::default() }, full.clone()));
        let cases = [
            (Features::default(), base.to_vec()),
            (Features { work: true, ..Features::default() }, [&base[..], &work, &network].concat()),
        ].into_iter().chain(desktop_host);
        for (features, expected) in cases {
            assert_eq!(features.tools().collect::<Vec<_>>(), expected);
            for name in &full {
                assert!(wire::tools::get(name).is_some(), "{name} is not in proto/tools.json");
                assert_eq!(features.tool(name).is_some(), expected.contains(name), "{name}");
            }
        }
        for t in wire::tools::all().iter().filter(|t| t.batch <= 3) {
            assert!(full.contains(&t.spec.name.as_str()), "{} has no function", t.spec.name);
        }
    }
}
