//! The on-screen "being controlled" strip and its border. Both are excluded from capture, so
//! the remote viewer never sees them. Where the platform gives a transfer note a window of its
//! own (`platform::ui::NOTE_WINDOW`, macOS), the note stays in the stream; otherwise it is part
//! of the strip.

use anyhow::Result;
use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use wry::{WebContext, WebView};

use super::{page_builder, UiEvent};
use crate::display::DisplayInfo;
use crate::i18n::{self, t};
use crate::platform::ui::{self as native, Border};

const OVERLAY_HTML: &str = include_str!("overlay.html");
static OVERLAY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub struct Indicator {
    pub window: Window,
    view: WebView,
    border: Border,
    pub ready: bool,
    /// The person controlling this computer (their device's name).
    pub name: Option<String>,
    /// The AI client using it (its device's name), beside or without a person.
    pub ai: Option<String>,
    pub display: Option<DisplayInfo>,
    pub note: Option<(String, Option<String>)>,
    /// The transfer note's own window, where it has one.
    note_window: Option<(Window, WebView)>,
    pub note_ready: bool,
    /// The strip's height, which the note window sits under.
    height: f64,
}

impl Indicator {
    pub fn new(target: &EventLoopWindowTarget<UiEvent>, ctx: &mut WebContext, proxy: EventLoopProxy<UiEvent>) -> Result<Indicator> {
        let build_window = |title: &str| -> Result<Window> {
            let window = native::indicator_builder(WindowBuilder::new())
                .with_title(title)
                .with_decorations(false)
                .with_always_on_top(true)
                .with_focused(false)
                .with_visible(false)
                .with_resizable(false)
                .with_inner_size(LogicalSize::new(240.0, 28.0))
                .build(target)?;
            native::prepare_indicator(&window);
            Ok(window)
        };
        let ipc = |proxy: EventLoopProxy<UiEvent>| {
            move |r: wry::http::Request<String>| {
                let _ = proxy.send_event(UiEvent::Ipc(r.body().to_string()));
            }
        };
        let window = build_window(&t("ui.indicator_title"))?;
        let view = page_builder(ctx, "deskway-indicator", super::designed(&OVERLAY, OVERLAY_HTML)).with_ipc_handler(ipc(proxy.clone())).build(&window)?;
        let note_window = if native::NOTE_WINDOW {
            // The transfer notice is ordinary UI and stays visible in the remote stream.
            let window = build_window(&t("ui.transfer_title"))?;
            let view = page_builder(ctx, "deskway-transfer", super::designed(&OVERLAY, OVERLAY_HTML))
                .with_initialization_script("window.deskwayTransfer = true;")
                .with_ipc_handler(ipc(proxy))
                .build(&window)?;
            Some((window, view))
        } else {
            None
        };
        let border = Border::new()?;
        border.exclude_from_capture(&window);
        Ok(Indicator { window, view, border, ready: false, name: None, ai: None, display: None, note: None, note_window, note_ready: false, height: 28.0 })
    }

    /// Someone or something controls this computer: the strip shows.
    fn shown(&self) -> bool {
        self.name.is_some() || self.ai.is_some()
    }

    pub fn sync(&mut self) {
        let note = self.note.as_ref().map(|(text, folder)| json!({ "text": text, "folder": folder.is_some() }));
        if self.shown() && self.ready {
            // Shown once the page reports its size, in `fit`.
            let in_strip = if self.note_window.is_none() { format!("setNote({}); ", json!(note)) } else { String::new() };
            let _ = self.view.evaluate_script(&format!("{in_strip}setState({})", json!({ "person": self.name, "ai": self.ai })));
        } else if !self.shown() {
            self.window.set_visible(false);
        }
        // The border marks the display a person sees; an AI client is shown by the strip alone.
        if self.name.is_none() {
            self.border.show(false);
        }
        if let Some((window, view)) = &self.note_window {
            match (&self.name, note, self.note_ready) {
                (Some(_), Some(note), true) => {
                    let _ = view.evaluate_script(&format!("setNote({note})"));
                }
                _ => window.set_visible(false),
            }
        }
    }

    /// Shows the strip in the language now chosen; it measures itself again.
    pub fn relabel(&mut self) {
        let script = format!("setLang({})", i18n::page_data());
        let _ = self.view.evaluate_script(&script);
        self.window.set_title(&t("ui.indicator_title"));
        if let Some((window, view)) = &self.note_window {
            let _ = view.evaluate_script(&script);
            window.set_title(&t("ui.transfer_title"));
        }
    }

    /// Keeps the page exactly the size of the window. Windows resizes the window itself when it
    /// moves to a display with other scaling; the page must follow or a white area shows.
    pub fn fill(&self) {
        let size = self.window.inner_size();
        let _ = self.view.set_bounds(wry::Rect {
            position: wry::dpi::PhysicalPosition::new(0, 0).into(),
            size: wry::dpi::PhysicalSize::new(size.width, size.height).into(),
        });
    }

    /// Sizes the window to the strip, centers it against the top edge, and shows it.
    pub fn fit(&mut self, w: f64, h: f64) {
        if !self.shown() {
            return;
        }
        self.height = h;
        native::place_indicator(&self.window, w, h, self.display.as_ref(), 0.0);
        self.fill();
        if self.name.is_some() {
            if let Some(display) = &self.display {
                self.border.place(display);
            }
            self.border.show(true);
        }
        self.border.exclude_from_capture(&self.window);
    }

    /// Places the note window under the strip.
    pub fn fit_note(&self, w: f64, h: f64) {
        if let (Some((window, _)), Some(_), Some(_)) = (&self.note_window, &self.name, &self.note) {
            native::place_indicator(window, w, h, self.display.as_ref(), self.height + 4.0);
        }
    }
}
