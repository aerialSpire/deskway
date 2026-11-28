//! The local terminal window. Each stream has its own renderer and connection.

use std::borrow::Cow;
use std::sync::{mpsc, Arc, OnceLock};

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::json;
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use wry::{http::Response, WebContext, WebView, WebViewBuilder};

use crate::i18n::{self, tf};
use crate::session::terminal::local::{Connection, Frame};
use crate::signal::Status;

use super::{designed, native, Backend, UiEvent};

const SCHEME: &str = "deskway-terminal";
const URL: &str = "deskway-terminal://localhost/index.html";
static PAGE: OnceLock<String> = OnceLock::new();
const IPC_LIMIT: usize = wire::terminal::FRAME.div_ceil(3) * 4 + 256;

fn asset(path: &str) -> Response<Cow<'static, [u8]>> {
    let (kind, body): (&str, &str) = match path {
        "/index.html" => ("text/html", designed(&PAGE, include_str!("terminal.html"))),
        "/terminal.js" => ("text/javascript", include_str!("terminal.js")),
        "/vendor/xterm.css" => ("text/css", include_str!("../../../web/static/vendor/xterm.css")),
        "/vendor/xterm.mjs" => ("text/javascript", include_str!("../../../web/static/vendor/xterm.mjs")),
        "/vendor/addon-fit.mjs" => ("text/javascript", include_str!("../../../web/static/vendor/addon-fit.mjs")),
        "/js/proto.js" => ("text/javascript", include_str!("../../../web/static/js/proto.js")),
        "/js/session/credit.js" => ("text/javascript", include_str!("../../../web/static/js/session/credit.js")),
        "/js/session/terminal-channel.js" => ("text/javascript", include_str!("../../../web/static/js/session/terminal-channel.js")),
        _ => return Response::builder().status(404).body(Cow::Borrowed(&[][..])).unwrap(),
    };
    Response::builder()
        .header("Content-Type", format!("{kind}; charset=utf-8"))
        .header("Access-Control-Allow-Origin", "*")
        .body(Cow::Borrowed(body.as_bytes())).unwrap()
}

pub enum Event {
    Ipc(String),
    // Acknowledgement bounds the queued UI work even when the window is busy.
    Output(u64, Frame, mpsc::SyncSender<()>),
    End(u64),
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
enum Request {
    Ready,
    Connect { id: u64 },
    Disconnect { id: u64 },
    Send { id: u64, binary: bool, data: String },
}

pub(super) struct TerminalWindow {
    view: WebView,
    pub window: Window,
    pub generation: u64,
    proxy: EventLoopProxy<UiEvent>,
    ready: bool,
    stream: Option<(u64, Arc<Connection>)>,
}

impl TerminalWindow {
    pub fn new(target: &EventLoopWindowTarget<UiEvent>, ctx: &mut WebContext, proxy: EventLoopProxy<UiEvent>, generation: u64) -> Result<Self> {
        let window = native::panel_window(WindowBuilder::new())
            .with_visible(false)
            .with_inner_size(LogicalSize::new(800.0, 500.0))
            .with_min_inner_size(LogicalSize::new(360.0, 240.0))
            .build(target)?;
        let ipc = proxy.clone();
        let builder = WebViewBuilder::new_with_web_context(ctx)
            .with_initialization_script(i18n::page_script())
            .with_custom_protocol(SCHEME.into(), |_, request| asset(request.uri().path()))
            .with_navigation_handler(|url| url == URL || url == "http://deskway-terminal.localhost/index.html")
            .with_url(URL)
            .with_ipc_handler(move |message| {
                if message.body().len() <= IPC_LIMIT {
                    let _ = ipc.send_event(UiEvent::Terminal(generation, Event::Ipc(message.body().clone())));
                }
            });
        #[cfg(target_os = "macos")]
        let builder = builder.with_accept_first_mouse(true);
        let view = builder.build(&window)?;
        Ok(Self { view, window, generation, proxy, ready: false, stream: None })
    }

    pub fn focus(&self) {
        #[cfg(target_os = "macos")]
        native::activate();
        self.window.set_minimized(false);
        self.window.set_visible(true);
        self.window.set_focus();
    }

    fn title(&self, status: Option<&Status>) {
        let name = status.map_or("", |s| s.name.as_str());
        self.window.set_title(&tf("terminal.window_title", &[("name", crate::shown_name(name))]));
    }

    pub fn update(&mut self, status: Option<&Status>) {
        self.title(status);
        let terminals = status.and_then(|s| s.terminals.as_ref());
        if terminals.is_none() {
            self.disconnect();
        }
        if self.ready {
            let locked = status.is_some_and(|s| s.terminal_locked);
            self.script(&format!("setTerminalStatus({}, {locked})", json!(terminals)));
        }
    }

    pub fn relabel(&self, status: Option<&Status>) {
        self.title(status);
        if self.ready {
            self.script(&format!("setLang({})", i18n::page_data()));
        }
    }

    fn script(&self, script: &str) {
        let _ = self.view.evaluate_script(script);
    }

    fn disconnect(&mut self) {
        if let Some((_, stream)) = self.stream.take() {
            stream.cancel();
        }
    }

    fn stream_is(&self, id: u64) -> bool {
        self.stream.as_ref().is_some_and(|(active, _)| *active == id)
    }

    fn connect(&mut self, id: u64, backend: &impl Backend) {
        self.disconnect();
        match backend.terminal() {
            Ok(connection) => {
                let connection = Arc::new(connection);
                self.stream = Some((id, connection.clone()));
                let proxy = self.proxy.clone();
                let generation = self.generation;
                std::thread::spawn(move || {
                    while let Some(frame) = connection.receive() {
                        let (ack, consumed) = mpsc::sync_channel(1);
                        if proxy.send_event(UiEvent::Terminal(generation, Event::Output(id, frame, ack))).is_err()
                            || consumed.recv().is_err() {
                            connection.cancel();
                            return;
                        }
                    }
                    let _ = proxy.send_event(UiEvent::Terminal(generation, Event::End(id)));
                });
                self.script(&format!("terminalConnected({id})"));
            }
            Err(e) => {
                eprintln!("{}: {e:#}", i18n::t("terminal.open_failed"));
                self.script(&format!("terminalEnded({id})"));
            }
        }
    }

    pub fn event(&mut self, event: Event, status: Option<&Status>, backend: &impl Backend) {
        match event {
            Event::Ipc(text) => {
                let Ok(message) = serde_json::from_str(&text) else { return };
                match message {
                    Request::Ready => {
                        self.disconnect();
                        self.ready = true;
                        self.relabel(status);
                        self.update(status);
                    }
                    Request::Connect { id } if self.ready && status.is_some_and(|s| s.terminals.is_some()) => {
                        self.connect(id, backend);
                    }
                    Request::Connect { id } => self.script(&format!("terminalEnded({id})")),
                    Request::Disconnect { id } if self.stream_is(id) => self.disconnect(),
                    Request::Send { id, binary, data } if self.stream_is(id) => {
                        let result = if binary { STANDARD.decode(data).ok() } else { Some(data.into_bytes()) };
                        let stream = &self.stream.as_ref().unwrap().1;
                        if result.is_none_or(|bytes| stream.send(Frame { binary, bytes }).is_err()) {
                            self.disconnect();
                            self.script(&format!("terminalEnded({id})"));
                        }
                    }
                    _ => {}
                }
            }
            Event::Output(id, frame, ack) => {
                if self.stream_is(id) {
                    let data = if frame.binary { STANDARD.encode(frame.bytes) }
                        else { String::from_utf8_lossy(&frame.bytes).into_owned() };
                    self.script(&format!("terminalMessage({id}, {}, {})", frame.binary, json!(data)));
                }
                let _ = ack.send(());
            }
            Event::End(id) if self.stream_is(id) => {
                self.disconnect();
                self.script(&format!("terminalEnded({id})"));
            }
            _ => {}
        }
    }
}

impl Drop for TerminalWindow {
    fn drop(&mut self) {
        self.disconnect();
    }
}
