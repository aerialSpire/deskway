//! The capabilities, seats and AI tools offered by this host.
use std::sync::mpsc::Sender;
use std::sync::Arc;

use wire::types::View;

use super::Platform;
use crate::session::{self, SessionEvent};
use crate::tools::{self, Table, Tool};

/// What a desk session is started with, besides what every session has (`session::Params`).
pub struct DeskParams {
    pub session: String,
    /// The browser's offer: the picture's track and level come from it.
    pub offer: String,
    /// The viewer constraints the offer carried: the picture starts at that size.
    pub view: Option<View>,
    pub platform: Arc<dyn Platform>,
    /// The browser may watch but not control.
    pub view_only: bool,
    /// The shared clipboard is on (the computer's setting).
    pub clip_share: bool,
    /// Where the browser's request to switch it goes: the host saves it and answers with
    /// `SessionEvent::ClipShare`.
    pub share: Sender<bool>,
}

#[derive(Clone, Copy)]
pub struct DesktopFeatures {
    pub caps: fn(bool) -> Vec<String>,
    pub spawn: fn(session::Params, DeskParams) -> Sender<SessionEvent>,
    pub ai_awake: fn(bool),
    pub tools: Table,
}

#[derive(Clone, Copy, Default)]
pub struct Features {
    pub desktop: Option<DesktopFeatures>,
    pub view_only: bool,
}

impl Features {
    pub fn desk_caps(self) -> Vec<String> {
        self.desktop.map_or_else(Vec::new, |desk| (desk.caps)(self.view_only))
    }

    pub fn signaling_caps(self) -> Vec<String> {
        let mut caps = self.desk_caps();
        caps.push(wire::caps::RELAY_RTT.into());
        caps
    }

    fn table(self) -> impl Iterator<Item = &'static (&'static str, Tool)> {
        tools::BASE.iter().chain(self.desktop.into_iter().flat_map(|desktop| desktop.tools))
    }

    pub fn tools(self) -> impl Iterator<Item = &'static str> {
        self.table().map(|(name, _)| *name)
    }

    pub fn tool(self, name: &str) -> Option<Tool> {
        self.table().find(|(tool, _)| *tool == name).map(|(_, call)| *call)
    }
}
