//! The installer window: Deskway-Setup.exe, and uninstalling from Settings → Apps. Same look as
//! the panel. It installs, updates and uninstalls only: the computer is signed in and out in
//! the panel. The work that needs an administrator runs in an elevated copy of this program
//! (`install --quiet` / `uninstall --quiet`), which reports back through a small result file.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::WebContext;

use crate::config::Config;
use crate::i18n::{t, tf};

const SETUP_HTML: &str = include_str!("setup.html");
static SETUP: std::sync::OnceLock<String> = std::sync::OnceLock::new();

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Install,
    Uninstall,
}

enum Msg {
    Ipc(String),
    /// Progress: which step is running now (index into the step list).
    Step(usize),
    Finished(std::result::Result<bool, String>),
}

struct State {
    mode: Mode,
    page: &'static str,
    error: String,
    /// Catalog keys, so the list follows a change of language.
    steps: Vec<&'static str>,
    now: usize,
    purged: bool,
}

impl State {
    fn view(&self) -> Value {
        let (cfg, enrolled) = match Config::load().ok().or_else(machine_config) {
            Some(c) => (Some(c), true),
            None => (None, false),
        };
        let steps: Vec<Value> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, l)| json!({ "label": t(l), "state": if i < self.now { "done" } else if i == self.now { "now" } else { "todo" } }))
            .collect();
        json!({
            "mode": if self.mode == Mode::Install { "install" } else { "uninstall" },
            "page": self.page, "error": self.error, "steps": steps, "purged": self.purged,
            "enrolled": enrolled,
            "name": crate::shown_name(cfg.as_ref().map(|c| c.name.as_str()).unwrap_or_default()),
            "server": cfg.as_ref().map(|c| c.server.trim_start_matches("https://").to_string()).unwrap_or_default(),
        })
    }
}

/// The service's copy of the settings, readable by administrators only: shown when it exists
/// and the person's own copy does not.
fn machine_config() -> Option<Config> {
    Config::load_from(&crate::service::machine_dir()).ok()
}

fn result_file() -> PathBuf {
    std::env::temp_dir().join(format!("deskway-setup-{}.txt", std::process::id()))
}

/// Runs an elevated step and waits for it. Ok(()) when it reported success.
fn elevated(args: &str, proxy: &EventLoopProxy<Msg>, waiting_step: usize, working_step: usize) -> std::result::Result<(), String> {
    let file = result_file();
    let _ = std::fs::remove_file(&file);
    let _ = proxy.send_event(Msg::Step(waiting_step));
    // `--keep`: this window stays open while the elevated step closes whatever holds the files.
    let child = crate::service::install::elevate_start(&format!("{args} --quiet --keep {} --result \"{}\"", std::process::id(), file.display())).map_err(|e| {
        if format!("{e:#}").contains("1223") || format!("{e:#}").to_lowercase().contains("cancel") {
            t("setup.declined")
        } else {
            tf("setup.no_admin", &[("e", format!("{e:#}"))])
        }
    })?;
    let _ = proxy.send_event(Msg::Step(working_step));
    let code = crate::service::install::wait_exit(child);
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    if code == 0 && text.trim() == "ok" {
        Ok(())
    } else if text.trim().is_empty() {
        Err(tf("setup.crashed", &[("code", code.to_string())]))
    } else {
        Err(text.trim().to_string())
    }
}

pub fn run(mode: Mode) -> Result<()> {
    crate::platform::platform_init();
    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let title = |mode: Mode| t(if mode == Mode::Install { "setup.title_install" } else { "setup.title_uninstall" });
    let window = crate::platform::ui::panel_window(WindowBuilder::new())
        .with_title(title(mode))
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(460.0, 470.0))
        .with_resizable(false)
        .with_maximizable(false)
        .build(&event_loop)?;
    let mut ctx = WebContext::new(Some(std::env::temp_dir().join("deskway-setup-webview")));
    let ipc = proxy.clone();
    let view = super::page_builder(&mut ctx, "deskway-setup", super::designed(&SETUP, SETUP_HTML))
        .with_ipc_handler(move |r| {
            let _ = ipc.send_event(Msg::Ipc(r.body().to_string()));
        })
        .build(&window)
        .with_context(|| t("err.webview"))?;

    let installed = crate::service::net::installed();
    let mut st = State {
        mode,
        page: match (mode, installed) {
            (Mode::Uninstall, _) => "uninstall",
            (Mode::Install, true) => "installed",
            (Mode::Install, false) => "welcome",
        },
        error: String::new(),
        steps: Vec::new(),
        now: 0,
        purged: false,
    };
    let mut ready = false;

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let render = |st: &State| {
            let _ = view.evaluate_script(&format!("render({})", st.view()));
        };
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if st.page != "working" {
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(Msg::Step(i)) => {
                st.now = i;
                render(&st);
            }
            Event::UserEvent(Msg::Finished(r)) => {
                match r {
                    Ok(purged) => {
                        st.purged = purged;
                        st.page = "done";
                    }
                    Err(e) => {
                        st.error = e;
                        st.page = "failed";
                    }
                }
                render(&st);
            }
            Event::UserEvent(Msg::Ipc(text)) => {
                let Ok(m) = serde_json::from_str::<Value>(&text) else { return };
                match m["cmd"].as_str().unwrap_or_default() {
                    "ready" => {
                        ready = true;
                        render(&st);
                    }
                    "drag" => {
                        let _ = window.drag_window();
                    }
                    "close" => {
                        if st.page != "working" {
                            *flow = ControlFlow::Exit;
                        }
                    }
                    // Chosen here before anything is installed; saved with the settings below.
                    "lang" => {
                        if let Some(lang) = m["lang"].as_str().and_then(crate::i18n::Lang::parse) {
                            crate::i18n::set(Some(lang.code()));
                            window.set_title(&title(mode));
                            let _ = view.evaluate_script(&format!("setLang({})", crate::i18n::page_data()));
                            render(&st);
                        }
                    }
                    "show-uninstall" => {
                        st.mode = Mode::Uninstall;
                        st.page = "uninstall";
                        render(&st);
                    }
                    "retry" => {
                        st.error.clear();
                        st.page = if st.mode == Mode::Uninstall { "uninstall" } else if crate::service::net::installed() { "installed" } else { "welcome" };
                        render(&st);
                    }
                    "install" => {
                        st.steps = vec!["setup.step_confirm", "setup.step_install"];
                        st.now = 0;
                        st.page = "working";
                        st.error.clear();
                        render(&st);
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            // The language picked in this window becomes the host's.
                            let lang = crate::i18n::current().code();
                            let r = elevated(&format!("install --set-lang {lang}"), &p, 0, 1);
                            let _ = p.send_event(Msg::Finished(r.map(|_| false)));
                        });
                    }
                    "uninstall" => {
                        let purge = m["purge"].as_bool().unwrap_or(false);
                        st.steps = vec!["setup.step_confirm", "setup.step_remove"];
                        st.now = 0;
                        st.page = "working";
                        render(&st);
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            let r = elevated(if purge { "uninstall --purge" } else { "uninstall" }, &p, 0, 1);
                            let _ = p.send_event(Msg::Finished(r.map(|_| purge)));
                        });
                    }
                    "finish" => {
                        if st.mode == Mode::Install {
                            let _ = std::process::Command::new(crate::service::install::program_dir().join("deskway-host.exe")).spawn();
                        }
                        *flow = ControlFlow::Exit;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        let _ = ready;
    })
}
