//! Desktop UI: tray icon, host panel, the on-screen "being controlled" indicator, and the
//! Windows installer window.
//!
//! The panel is the same on every computer; what runs the host behind it is its [`Backend`]:
//! on Windows the network service, reached over its pipe (`service`), on macOS the host in this
//! process (`inprocess`). The host's [`Status`] and the backend's own facts (`Backend::extras`)
//! make the page's state; the page's messages are [`PageMessage`]s, and the requests among them
//! that the host carries out are [`PanelRequest`]s.

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
use tray_icon::{Icon, MouseButton, TrayIconBuilder, TrayIconEvent};
use wry::{WebContext, WebView, WebViewBuilder};

use crate::discovery::Browse;
use crate::i18n::{self, t};
use crate::platform::ui as native;
use crate::settings::PanelRequest;
use crate::signal::Status;

mod indicator;
#[cfg(target_os = "macos")]
mod inprocess;
#[cfg(windows)]
mod service;
#[cfg(windows)]
pub mod setup;

#[cfg(target_os = "macos")]
use inprocess::{BackendEvent, InProcess as HostBackend};
#[cfg(windows)]
use service::{BackendEvent, Service as HostBackend};

use indicator::Indicator;

const PANEL_HTML: &str = include_str!("panel.html");
static PANEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Deskway's design (tokens and shared components): the file the browser client loads, built
/// into every window here (docs/design.md).
const DESIGN_CSS: &str = include_str!("../../../web/static/design.css");
const DESIGN_MARK: &str = "<!--design-->";
/// What every page shares (page.js), in place of its marker, with the host's languages.
const PAGE_JS: &str = include_str!("page.js");
const PAGE_MARK: &str = "<!--page-->";
/// The language menu of the windows that have one (lang.js), in place of its marker.
const LANG_JS: &str = include_str!("lang.js");
const LANG_MARK: &str = "<!--lang-menu-->";

/// `html` with the design stylesheet, the shared page code (and, where it asks, the language
/// menu) in place of their markers, made once per page.
pub(crate) fn designed(page: &'static std::sync::OnceLock<String>, html: &str) -> &'static str {
    page.get_or_init(|| {
        let languages: Vec<&str> = i18n::Lang::ALL.iter().map(|l| l.code()).collect();
        html.replacen(DESIGN_MARK, &format!("<style>\n{DESIGN_CSS}</style>"), 1)
            .replacen(PAGE_MARK, &format!("<script>\nconst LANGUAGES = {};\n{PAGE_JS}</script>", json!(languages)), 1)
            .replacen(LANG_MARK, &format!("<script>\n{LANG_JS}</script>"), 1)
    })
}

/// A web view for one of this program's pages, starting in the current language.
pub(crate) fn page_builder<'a>(ctx: &'a mut WebContext, scheme: &str, html: &'static str) -> WebViewBuilder<'a> {
    native::load_page(WebViewBuilder::new_with_web_context(ctx).with_initialization_script(i18n::page_script()), scheme, html)
}

pub enum UiEvent {
    /// The host's status; None while it cannot be reached (the Windows network service).
    Status(Option<Status>),
    /// A message from one of the pages (the panel, the indicator).
    Ipc(String),
    /// The screens changed: the panel stays on them.
    ScreenChanged,
    TrayOpen,
    Menu(MenuId),
    /// Draw the panel again (a setting changed elsewhere).
    #[cfg(windows)]
    Refresh,
    /// The backend's own events.
    Backend(BackendEvent),
    LocalServersChanged,
}

/// What a page sends (`cmd`).
#[derive(Deserialize, Debug, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum PageMessage {
    Ready,
    Lang { lang: String },
    OverlayReady,
    OverlaySize { w: f64, h: f64 },
    TransferReady,
    TransferSize { w: f64, h: f64 },
    Drag,
    DragRegion { rect: Option<[f64; 4]> },
    Close,
    Minimize,
    Disconnect,
    /// Stop AI control here (the indicator's and the panel's Stop AI control).
    StopAi,
    OpenFolder,
    Lock { on: bool },
    Autostart { on: bool },
    PreventSleep { on: bool },
    Pair,
    PairStop,
    Ask { session: String, on: bool },
    Unpair,
    RemoveDevice { id: String },
    StartService,
    FindLocalServers,
    Login { server: String, user: String, password: String },
    Leave,
    ScreenRequest,
    AccessibilityRequest,
    ScreenSettings,
    AccessibilitySettings,
    FullDiskAccessSettings,
    LoginSettings,
    #[cfg(target_os = "macos")]
    ReopenForPermissions,
}

impl PageMessage {
    /// The request the host carries out for this message, if it is one.
    fn request(&self) -> Option<PanelRequest> {
        Some(match self {
            PageMessage::Disconnect => PanelRequest::Disconnect,
            PageMessage::StopAi => PanelRequest::StopAi,
            PageMessage::Lock { on } => PanelRequest::AutoLock(*on),
            PageMessage::Autostart { on } => PanelRequest::AutoStart(*on),
            PageMessage::PreventSleep { on } => PanelRequest::PreventSleep(*on),
            PageMessage::Lang { lang } => PanelRequest::Lang(lang.clone()),
            PageMessage::Pair => PanelRequest::StartPairing,
            PageMessage::PairStop => PanelRequest::StopPairing,
            PageMessage::Ask { session, on } => PanelRequest::AnswerAsk(session.clone(), *on),
            // Removing access needs no administrator: it only takes access away.
            PageMessage::Unpair => PanelRequest::RemoveDevices(None),
            PageMessage::RemoveDevice { id } => PanelRequest::RemoveDevices(Some(vec![id.clone()])),
            _ => return None,
        })
    }
}

/// What runs the host behind the panel.
trait Backend {
    /// Runs or reaches the host; its status arrives as `UiEvent::Status`. True if the panel
    /// opens at once.
    fn start(&mut self) -> bool;
    /// The facts of this computer the panel shows besides the host's status (whether it is
    /// signed in, how it starts, what it may do).
    fn extras(&self) -> Value;
    /// A request for the host. True if the panel should be drawn again now.
    fn request(&mut self, r: PanelRequest) -> bool;
    /// A page message that is the backend's own. True if the panel should be drawn again now.
    fn page(&mut self, m: PageMessage) -> bool;
    /// The host's status arrived, or could not be had (None).
    fn status(&mut self, status: Option<&Status>);
    /// One of the backend's own events. True if the panel goes on (and is drawn again), false
    /// if it quits.
    fn event(&mut self, e: BackendEvent) -> bool;
    /// The panel quits from the tray.
    fn quit(&mut self);
}

/// The pairing link as an SVG QR code for the panel. Medium error correction survives a phone
/// camera at an angle; the quiet zone keeps it readable on any background.
fn qr_svg(url: &str) -> String {
    let Ok(code) = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M) else { return String::new() };
    let w = code.width();
    let colors = code.to_colors();
    let quiet = 4;
    let size = w + quiet * 2;
    let mut path = String::new();
    for y in 0..w {
        for x in 0..w {
            if colors[y * w + x] == qrcode::Color::Dark {
                path.push_str(&format!("M{} {}h1v1h-1z", x + quiet, y + quiet));
            }
        }
    }
    let label = t("ui.qr_aria");
    format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {size} {size}" shape-rendering="crispEdges" role="img" aria-label="{label}"><rect width="{size}" height="{size}" fill="#fff"/><path d="{path}" fill="#16191C"/></svg>"##)
}

/// The panel page's state: the host's status (as it is before any arrives, if none has), with
/// the backend's facts over it.
fn panel_state(status: Option<&Status>, extras: Value) -> Value {
    let none = Status::default();
    let s = status.unwrap_or(&none);
    let mut state = json!({
        "name": crate::shown_name(&s.name), "auto_lock": s.auto_lock,
        "online": s.online, "waiting": s.waiting, "approval_code": s.approval_code, "controller": s.controller, "paired": s.paired,
        "ai_client": s.ai_client, "ai_state": s.ai_state,
        "pair_qr": s.pair_url.as_deref().map(qr_svg), "devices": s.devices, "max_devices": crate::auth::MAX_CONTROLLERS,
        "asks": s.asks, "problem": s.problem, "prevent_sleep": s.prevent_sleep,
        "keep_awake_min_battery": crate::power::MIN_BATTERY_PERCENT,
        "account": s.account, "server": s.server,
    });
    if let (Some(state), Value::Object(extras)) = (state.as_object_mut(), extras) {
        state.extend(extras);
    }
    state
}

fn root_problem(error: Option<&str>) -> Option<String> {
    error.map(|e| crate::i18n::tf("err.root_trust", &[("e", e.to_string())]))
}

struct LocalDiscovery {
    _browse: Option<Browse>,
    results: Receiver<Vec<Value>>,
}

impl LocalDiscovery {
    fn start(proxy: EventLoopProxy<UiEvent>) -> Self {
        let (tx, results) = mpsc::channel();
        let mut found = BTreeMap::new();
        let browse = Browse::start(move |change| {
            if let Some(server) = change.server {
                let result = json!({ "name": server.name, "address": server.address.saved() });
                if found.get(&change.instance) == Some(&result) {
                    return;
                }
                found.insert(change.instance, result);
            } else if found.remove(&change.instance).is_none() {
                return;
            }
            if tx.send(found.values().cloned().collect()).is_ok() {
                let _ = proxy.send_event(UiEvent::LocalServersChanged);
            }
        }).map_err(|e| eprintln!("cannot browse for local servers: {e:#}")).ok();
        Self { _browse: browse, results }
    }
}

struct App {
    backend: HostBackend,
    /// The host's last status; None before the first, or while it cannot be reached.
    status: Option<Status>,
    panel_ready: bool,
    discovery: Option<LocalDiscovery>,
    /// The language the windows show now.
    shown: i18n::Lang,
}

impl App {
    fn state(&self) -> Value {
        panel_state(self.status.as_ref(), self.backend.extras())
    }

    fn discover(&mut self, on: bool, panel: &WebView, proxy: &EventLoopProxy<UiEvent>) {
        if on == self.discovery.is_some() {
            return;
        }
        self.discovery = on.then(|| LocalDiscovery::start(proxy.clone()));
        let _ = panel.evaluate_script(&format!("searchLocalServers({on})"));
    }
}

/// Shows everything in the language now chosen, if the windows show another: the panel, the
/// indicator and the tray menu.
fn follow_language(app: &mut App, panel: &WebView, indicator: &mut Option<Indicator>, open: &MenuItem, quit: &MenuItem) {
    if i18n::current() == app.shown {
        return;
    }
    app.shown = i18n::current();
    let _ = panel.evaluate_script(&format!("setLang({})", i18n::page_data()));
    open.set_text(t("tray.open"));
    quit.set_text(t("tray.quit"));
    if let Some(i) = indicator {
        i.relabel();
    }
}

pub fn run(at_login: bool) -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    native::configure_event_loop(&mut event_loop);
    let proxy = event_loop.create_proxy();

    let window = native::panel_window(WindowBuilder::new())
        .with_title(crate::product::NAME)
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(380.0, native::panel_height(event_loop.primary_monitor())))
        .with_resizable(false)
        .with_maximizable(false)
        .with_visible(false)
        .build(&event_loop)?;
    let screens = proxy.clone();
    let panel_window = native::PanelWindow::new(&window, move || {
        let _ = screens.send_event(UiEvent::ScreenChanged);
    })?;
    let data_dir = crate::config::dir()?.join("webview");
    let mut ctx = WebContext::new(Some(data_dir));
    let ipc = proxy.clone();
    let panel = page_builder(&mut ctx, "deskway-panel", designed(&PANEL, PANEL_HTML))
        .with_ipc_handler(move |r| {
            let _ = ipc.send_event(UiEvent::Ipc(r.body().to_string()));
        })
        .build(&window)
        .with_context(|| t(native::PANEL_ERROR))?;

    let open_item = MenuItem::new(t("tray.open"), true, None);
    let quit_item = MenuItem::new(t("tray.quit"), true, None);
    let menu = Menu::new();
    menu.append(&open_item)?;
    menu.append(&quit_item)?;
    let tray = TrayIconBuilder::new()
        .with_icon(Icon::from_rgba(native::tray_rgba(32), 32, 32)?)
        .with_tooltip(crate::product::NAME)
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon_as_template(native::TRAY_TEMPLATE)
        .build()?;
    let (open_id, quit_id) = (open_item.id().clone(), quit_item.id().clone());
    let p = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, .. } = e {
            let _ = p.send_event(UiEvent::TrayOpen);
        }
    }));
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = p.send_event(UiEvent::Menu(e.id));
    }));

    let mut app = App { backend: HostBackend::new(proxy.clone(), at_login), status: None, panel_ready: false, discovery: None, shown: i18n::current() };
    if app.backend.start() {
        let _ = proxy.send_event(UiEvent::TrayOpen);
    }

    let mut indicator: Option<Indicator> = None;
    let _tray = tray;

    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        let sync_discovery = |app: &mut App, state: &Value| {
            let on = state["enrolled"] == false && window.is_visible() && !window.is_minimized();
            app.discover(on, &panel, &proxy);
        };
        let render = |app: &mut App| {
            if app.panel_ready {
                let state = app.state();
                let _ = panel.evaluate_script(&format!("render({state})"));
                sync_discovery(app, &state);
            }
        };
        // The indicator follows who controls this computer: a person, an AI client, or both.
        let mut show_controller = |app: &App, indicator: &mut Option<Indicator>, target: &tao::event_loop::EventLoopWindowTarget<UiEvent>| {
            let s = app.status.clone().unwrap_or_default();
            if indicator.is_none() && (s.controller.is_some() || s.ai_client.is_some()) {
                match Indicator::new(target, &mut ctx, proxy.clone()) {
                    Ok(i) => *indicator = Some(i),
                    Err(e) => eprintln!("cannot show the on-screen indicator: {e}"),
                }
            }
            if let Some(i) = indicator {
                i.name = s.controller;
                i.ai = s.ai_client;
                i.display = s.display;
                i.note = s.note;
                i.sync();
            }
        };
        match event {
            Event::WindowEvent { event: WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }, window_id, .. }
                if indicator.as_ref().is_some_and(|i| i.window.id() == window_id) =>
            {
                if let Some(i) = &indicator {
                    i.fill();
                }
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, window_id, .. } if window_id == window.id() => {
                // Closing the panel never stops the host: it keeps running in the tray.
                native::hide_panel(&window);
                app.discover(false, &panel, &proxy);
            }
            // Permissions may have changed in System Settings meanwhile.
            Event::WindowEvent { event: WindowEvent::Focused(true), window_id, .. } if window_id == window.id() => render(&mut app),
            Event::WindowEvent { event: WindowEvent::Resized(_) | WindowEvent::Focused(false), window_id, .. }
                if window_id == window.id() && app.panel_ready =>
            {
                let state = app.state();
                sync_discovery(&mut app, &state);
            }
            Event::UserEvent(UiEvent::ScreenChanged) if window.is_visible() => native::fit_panel(&window),
            #[cfg(windows)]
            Event::UserEvent(UiEvent::Refresh) => render(&mut app),
            Event::Reopen { .. } | Event::UserEvent(UiEvent::TrayOpen) => {
                native::show_panel(&window);
                render(&mut app);
            }
            Event::UserEvent(UiEvent::Menu(id)) => {
                if id == open_id {
                    native::show_panel(&window);
                    render(&mut app);
                } else if id == quit_id {
                    app.discover(false, &panel, &proxy);
                    app.backend.quit();
                    // Lets the signaling thread tell the viewer, so it does not keep reconnecting.
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UiEvent::Status(st)) => {
                app.backend.status(st.as_ref());
                if let Some(s) = &st {
                    // The host holds the setting, and its notices use it: this panel follows.
                    if let Some(lang) = s.lang.as_deref().and_then(i18n::Lang::parse) {
                        i18n::set(Some(lang.code()));
                    }
                    // Someone at another device is waiting for an answer here.
                    if s.asks.len() > app.status.as_ref().map_or(0, |was| was.asks.len()) {
                        native::show_panel(&window);
                    }
                }
                app.status = st;
                follow_language(&mut app, &panel, &mut indicator, &open_item, &quit_item);
                show_controller(&app, &mut indicator, target);
                render(&mut app);
            }
            Event::UserEvent(UiEvent::Backend(e)) => {
                if app.backend.event(e) {
                    render(&mut app);
                } else {
                    app.discover(false, &panel, &proxy);
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UiEvent::LocalServersChanged) => {
                if let Some(results) = app.discovery.as_ref().and_then(|d| d.results.try_iter().last()) {
                    let _ = panel.evaluate_script(&format!("showLocalServers({})", json!(results)));
                }
            }
            Event::UserEvent(UiEvent::Ipc(text)) => {
                let Ok(m) = serde_json::from_str::<PageMessage>(&text) else { return };
                match m {
                    PageMessage::Ready => {
                        app.panel_ready = true;
                        app.discovery = None;
                        // A reloaded page may carry the language it was first built with.
                        let _ = panel.evaluate_script(&format!("setLang({})", i18n::page_data()));
                        render(&mut app);
                    }
                    PageMessage::OverlayReady => {
                        if let Some(i) = &mut indicator {
                            i.ready = true;
                            i.sync();
                        }
                    }
                    PageMessage::OverlaySize { w, h } => {
                        if let Some(i) = &mut indicator {
                            i.fit(w, h);
                        }
                    }
                    PageMessage::TransferReady => {
                        if let Some(i) = &mut indicator {
                            i.note_ready = true;
                            i.sync();
                        }
                    }
                    PageMessage::TransferSize { w, h } => {
                        if let Some(i) = &indicator {
                            i.fit_note(w, h);
                        }
                    }
                    PageMessage::Drag => {
                        let _ = window.drag_window();
                    }
                    PageMessage::DragRegion { rect } => panel_window.set_region(rect),
                    PageMessage::Close | PageMessage::Minimize => {
                        if m == PageMessage::Close {
                            native::hide_panel(&window);
                        } else {
                            native::minimize_panel(&window);
                        }
                        app.discover(false, &panel, &proxy);
                    }
                    PageMessage::FindLocalServers => {
                        if app.discovery.take().is_some() {
                            app.discover(true, &panel, &proxy);
                        }
                    }
                    // Only the folder the host named; the page cannot choose another.
                    PageMessage::OpenFolder => {
                        if let Some((_, Some(folder))) = indicator.as_ref().and_then(|i| i.note.clone()) {
                            native::open_folder(&folder);
                        }
                    }
                    m => {
                        let redraw = match m.request() {
                            Some(r) => {
                                // The panel shows the chosen language at once; the host keeps it.
                                if let PanelRequest::Lang(code) = &r {
                                    i18n::set(i18n::Lang::parse(code).map(|l| l.code()));
                                    follow_language(&mut app, &panel, &mut indicator, &open_item, &quit_item);
                                }
                                app.backend.request(r)
                            }
                            None => app.backend.page(m),
                        };
                        if redraw {
                            render(&mut app);
                        }
                    }
                }
            }
            _ => {}
        }
    })
}

/// Shows the on-screen indicator for a made-up controller, until its "disconnect" is clicked:
/// for trying the strip and its border by hand.
pub fn indicator_test() -> Result<()> {
    let mut event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    native::configure_event_loop(&mut event_loop);
    let proxy = event_loop.create_proxy();
    let mut ctx = WebContext::new(None);
    let mut indicator = Indicator::new(&event_loop, &mut ctx, proxy)?;
    indicator.name = Some("Deskway test".into());
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let Event::UserEvent(UiEvent::Ipc(text)) = event else { return };
        match serde_json::from_str::<PageMessage>(&text) {
            Ok(PageMessage::OverlayReady) => {
                indicator.ready = true;
                indicator.sync();
            }
            Ok(PageMessage::OverlaySize { w, h }) => indicator.fit(w, h),
            Ok(PageMessage::Disconnect) => *flow = ControlFlow::Exit,
            _ => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_window_takes_the_design_and_page_code_from_their_one_file() {
        for (name, html, lang) in [("panel", PANEL_HTML, true), ("setup", include_str!("setup.html"), true), ("overlay", include_str!("overlay.html"), false)] {
            assert_eq!(html.matches(DESIGN_MARK).count(), 1, "{name}.html has one design marker");
            assert_eq!(html.matches(PAGE_MARK).count(), 1, "{name}.html has one page marker");
            assert_eq!(html.matches(LANG_MARK).count(), usize::from(lang), "{name}.html takes the language menu exactly where it shows one");
            let page = html.replacen(DESIGN_MARK, DESIGN_CSS, 1).replacen(PAGE_MARK, PAGE_JS, 1).replacen(LANG_MARK, LANG_JS, 1);
            assert!(page.contains("--accent:") && !page.contains(DESIGN_MARK) && !page.contains(PAGE_MARK) && !page.contains(LANG_MARK), "{name}: the tokens, page code and menu are in place");
            assert!(!page.contains("<select"), "{name}: no native select; the language is the shared menu");
            assert!(!html.contains(".innerHTML = `") && !html.contains("esc("), "{name}: text goes in as text, never as markup");
        }
    }

    #[test]
    fn the_panel_state_is_the_status_with_the_backend_facts() {
        let account = Some(crate::config::Account { user: "me".into(), display: "Alex".into() });
        let s = Status { online: true, name: "Desk".into(), server: "https://s.example".into(), account, paired: 2, pair_url: Some("https://s.example/#pair=H1.ABCD".into()), prevent_sleep: true, ..Status::default() };
        let state = panel_state(Some(&s), json!({ "enrolled": true, "autostart": false }));
        assert_eq!((state["account"]["user"].as_str(), state["account"]["display"].as_str(), state["server"].as_str()), (Some("me"), Some("Alex"), Some("https://s.example")), "the account and its server, for the footer");
        assert_eq!((state["name"].as_str(), state["online"].as_bool(), state["paired"].as_u64(), state["enrolled"].as_bool()), (Some("Desk"), Some(true), Some(2), Some(true)));
        assert!(state["pair_qr"].as_str().is_some_and(|svg| svg.starts_with("<svg")), "the pairing link becomes its QR code");
        assert_eq!((state["max_devices"].as_u64(), state["autostart"].as_bool(), state["prevent_sleep"].as_bool()), (Some(crate::auth::MAX_CONTROLLERS as u64), Some(false), Some(true)));
        // Before any status: nothing is claimed, and the backend's facts still show.
        let none = panel_state(None, json!({ "enrolled": false }));
        assert_eq!((none["online"].as_bool(), none["controller"].is_null(), none["pair_qr"].is_null(), none["enrolled"].as_bool()), (Some(false), true, true, Some(false)));
    }

    #[test]
    fn page_messages_name_the_host_requests() {
        let m = |s: &str| serde_json::from_str::<PageMessage>(s).unwrap();
        assert_eq!(m(r#"{"cmd":"ask","session":"s1","on":false}"#).request(), Some(PanelRequest::AnswerAsk("s1".into(), false)));
        assert_eq!(m(r#"{"cmd":"remove-device","id":"d1"}"#).request(), Some(PanelRequest::RemoveDevices(Some(vec!["d1".into()]))));
        assert_eq!(m(r#"{"cmd":"stop-ai"}"#).request(), Some(PanelRequest::StopAi));
        assert_eq!(m(r#"{"cmd":"lock","on":true}"#).request(), Some(PanelRequest::AutoLock(true)));
        assert_eq!(m(r#"{"cmd":"drag-region","rect":null}"#), PageMessage::DragRegion { rect: None });
        assert_eq!(m(r#"{"cmd":"overlay-size","w":200,"h":28}"#).request(), None);
        assert_eq!(m(r#"{"cmd":"full-disk-access-settings"}"#), PageMessage::FullDiskAccessSettings);
    }
}
