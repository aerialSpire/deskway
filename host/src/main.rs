//! Deskway host: runs on the computer being controlled.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]
// A host with no desktop still compiles the desktop interfaces the shared code is written
// against (`Platform`, `Workspace`); it never calls them. Unused code is caught on the desktop builds.
#![cfg_attr(not(desktop_host), allow(dead_code))]

mod audit;
mod auth;
#[cfg(desktop_host)]
mod clipboard;
mod config;
mod display;
mod i18n;
#[cfg(desktop_host)]
mod icon;
#[cfg(desktop_host)]
mod input;
mod mcp;
mod platform;
#[cfg(desktop_host)]
mod power;
#[cfg(desktop_host)]
mod product;
#[cfg(windows)]
mod service;
mod session;
mod settings;
mod signal;
mod tools;
#[cfg(desktop_host)]
mod transfer;
#[cfg(desktop_host)]
mod ui;
#[cfg(desktop_host)]
mod video;
mod vitals;
mod wake;
mod workspace;

use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, IsTerminal, Write};
use deskway_server_connection::Address;

use i18n::{t, tf};
#[cfg(windows)]
use service::{install, net};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn read_password() -> Result<String> {
    if let Ok(p) = std::env::var("DESKWAY_PASSWORD") {
        return Ok(p);
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    let mut read = || {
        let mut stdout = std::io::stdout().lock();
        write!(stdout, "{}", t("cli.password_prompt"))?;
        stdout.flush()?;
        stdin.lock().read_line(&mut line)
    };
    if stdin.is_terminal() {
        platform::without_echo(read)?;
        println!();
    } else {
        read()?;
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// The name to show for this computer. An empty name in the config means none was set: then
/// the computer's own name, or "this computer" in the language shown now (never stored, so it
/// follows a change of language).
pub fn shown_name(name: &str) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    platform::computer_name().unwrap_or_else(|| t("device.this_computer"))
}

/// Signs this computer in. `name` is the one chosen, or "" for none: the server is told the name
/// shown for it. `back` is the computer signed out here (`Config::kept`): when the same account
/// signs in at the same server, the server answers with its id and no token, and `Config::settle`
/// gives it back as it was. Otherwise the answer is a new computer, waiting for approval. Nothing
/// is saved here: the caller settles it where the computer keeps its sign-in.
pub fn enroll_with(server: &str, user: &str, password: &str, name: &str, back: Option<&config::Kept>) -> Result<config::Config> {
    let address = Address::parse(server).map_err(server_error)?;
    let server = address.saved();
    let server_root = address.fetch_root().map_err(server_error)?;
    let connection = address.connection(server_root.as_deref()).map_err(server_error)?;
    let back = back.filter(|k| k.server == server).map(|k| k.id.as_str());
    let res = connection.http(ureq::builder())?
        .post(&connection.url("/api/host/enroll"))
        .send_json(serde_json::json!({ "user": user, "password": password, "name": shown_name(name), "os": std::env::consts::OS, "back": back, "shows_code": true }));
    let body: serde_json::Value = match res {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(401, _)) => bail!(t("err.wrong_password")),
        // The account has as many computers as the server allows.
        Err(ureq::Error::Status(403, _)) => bail!(t("err.computer_limit")),
        Err(ureq::Error::Status(429, _)) => bail!(t("err.too_many")),
        Err(e) => return Err(server_error(e)),
    };
    Ok(config::Config {
        server,
        server_root,
        id: body["id"].as_str().with_context(|| t("err.bad_response"))?.into(),
        // Given back, the computer keeps the token it has.
        token: body["token"].as_str().unwrap_or_default().into(),
        name: name.to_string(),
        auto_lock: false,
        clip_share: false,
        lang: None,
        prevent_sleep: cfg!(target_os = "macos"),
        account: Some(config::Account { user: user.trim().to_string(), display: String::new() }),
        #[cfg(target_os = "macos")]
        login_item_setup_pending: true,
    })
}

fn server_error(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!(tf("err.cant_reach_server", &[("e", format!("{e:#}"))]))
}

/// Withdraws a sign-in that was never put to use (the service's elevated step was declined):
/// the server forgets the computer it just added. Signing out never does this; only removing
/// the computer from the account does.
#[cfg(desktop_host)]
pub fn withdraw(cfg: &config::Config) -> Result<()> {
    let server = cfg.connection()?;
    server.http(ureq::builder())?
        .post(&server.url(&format!("/api/host/leave?id={}", cfg.id)))
        .set("Authorization", &format!("Bearer {}", cfg.token))
        .call()?;
    Ok(())
}

fn enroll_command(args: &[String]) -> Result<()> {
    let need = |f: &str| tf("cli.need_flag", &[("flag", f.to_string())]);
    let server = flag(args, "--server").with_context(|| need("--server"))?;
    let user = flag(args, "--user").with_context(|| need("--user"))?;
    let name = flag(args, "--name").unwrap_or_default();
    #[cfg(target_os = "linux")]
    let _lock = platform::lock()?;
    let cfg = config::Config::settle(enroll_with(&server, &user, &read_password()?, &name, config::Config::kept().as_ref())?)?;
    #[cfg(target_os = "macos")]
    if let Err(e) = update_browser_trust(&cfg, true) {
        eprintln!("{}", tf("err.root_trust", &[("e", format!("{e:#}"))]));
    }
    // A host with no window says how to start it and where its approval code appears.
    let done = if cfg!(desktop_host) { "cli.enrolled" } else { "cli.enrolled_linux" };
    println!("{}", tf(done, &[("name", shown_name(&cfg.name))]));
    Ok(())
}

#[cfg(target_os = "macos")]
fn update_browser_trust(cfg: &config::Config, install: bool) -> Result<()> {
    let address = Address::parse(&cfg.server)?;
    if let Some(fingerprint) = address.fingerprint() {
        platform::update_server_root(&config::root_path(&config::dir()?), fingerprint, install)?;
    }
    Ok(())
}

fn main() -> Result<()> {
    #[cfg(desktop_host)]
    platform::attach_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(target_os = "macos")]
    if matches!(args.first().map(String::as_str), None | Some("--at-login")) && platform::ui::redirect_to_installed()? {
        return Ok(());
    }
    // An elevated copy is told the language of the window that started it; it may not be able
    // to read that person's settings. Services read theirs once they know where they are, and
    // never look in their own profile.
    match flag(&args, "--lang") {
        Some(code) => i18n::set(Some(&code)),
        None if !matches!(args.first().map(String::as_str), Some("service" | "net" | "agent")) => i18n::from_config(),
        None => {}
    }
    // The same program, named DeskwayHost-Setup-windows-x64.exe and double-clicked, is the installer. On
    // Windows it is also what opens until Deskway is installed: Deskway runs as its services.
    #[cfg(windows)]
    if args.is_empty() && (!net::installed() || std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase())).is_some_and(|s| s.contains("setup"))) {
        return ui::setup::run(ui::setup::Mode::Install);
    }
    match args.first().map(String::as_str) {
        Some("enroll") => enroll_command(&args),
        #[cfg(target_os = "linux")]
        Some("stop-ai") => platform::stop_ai(),
        // Opens a ten-minute window for pairing one passkey, and prints its code. With the
        // service installed this needs an administrator.
        #[cfg(desktop_host)]
        Some("pair") => admin_command(&args, |dialog| {
            let code = auth::Trust::start_pairing(&config::dir()?)?;
            let text = tf("cli.pair_code", &[("code", code.to_string())]);
            if dialog { platform::message(&t("cli.pair_title"), &text) } else { println!("{text}") }
            Ok(())
        }),
        // Lists the paired passkeys, or removes one (`unpair ID`) or all (`unpair --all`).
        #[cfg(desktop_host)]
        Some("passkeys") => admin_command(&args, |dialog| {
            let cfg = config::Config::load()?;
            let t = auth::Trust::open(config::dir()?, &cfg.id, &cfg.server)?;
            let text = t.controllers().iter().map(|c| format!("{}  {}", c.id, c.name)).collect::<Vec<_>>().join("\n");
            if dialog { platform::message(product::NAME, &text) } else if !text.is_empty() { println!("{text}") }
            Ok(())
        }),
        #[cfg(desktop_host)]
        Some("unpair") => admin_command(&args, |dialog| {
            let cfg = config::Config::load()?;
            let mut t = auth::Trust::open(config::dir()?, &cfg.id, &cfg.server)?;
            t.cancel_pairing();
            let text = if args.iter().any(|a| a == "--all") {
                i18n::tn("cli.unpaired_all", t.remove_all()?, &[])
            } else {
                let id = args.get(1).filter(|a| !a.starts_with("--")).with_context(|| i18n::t("cli.need_passkey_id"))?;
                anyhow::ensure!(t.remove(id)?, i18n::t("cli.no_such_passkey"));
                i18n::t("cli.unpaired")
            };
            if dialog { platform::message(product::NAME, &text) } else { println!("{text}") }
            Ok(())
        }),
        #[cfg(windows)]
        Some("install") => {
            let keep = flag(&args, "--keep").and_then(|k| k.parse().ok());
            let r = install::install(keep, flag(&args, "--set-lang"));
            write_result(&args, &r);
            report(r, &args, &t("cli.installed"))
        }
        // The panel signs this computer in or out through these, elevated.
        #[cfg(windows)]
        Some("sign-in") => {
            let from = flag(&args, "--from").with_context(|| tf("cli.need_flag", &[("flag", "--from".to_string())]))?;
            let r = install::sign_in(std::path::Path::new(&from));
            write_result(&args, &r);
            r
        }
        #[cfg(windows)]
        Some("sign-out") => {
            let r = install::sign_out();
            write_result(&args, &r);
            r
        }
        // Settings → Apps runs this without elevation: show the uninstall window.
        #[cfg(windows)]
        Some("uninstall") if !install::is_elevated() && !args.iter().any(|a| a == "--quiet") => ui::setup::run(ui::setup::Mode::Uninstall),
        #[cfg(windows)]
        Some("uninstall") if args.iter().any(|a| a == "--result") => {
            let r = install::uninstall(args.iter().any(|a| a == "--purge"), flag(&args, "--keep").and_then(|k| k.parse().ok()));
            write_result(&args, &r);
            r
        }
        #[cfg(windows)]
        Some("uninstall") => report(install::uninstall(args.iter().any(|a| a == "--purge"), None), &args, &t("cli.uninstalled")),
        // Opened by hand while automatic start is off: start it, and start it with Windows again.
        #[cfg(windows)]
        Some("start") => {
            if !install::is_elevated() {
                let code = install::elevate("start")?;
                anyhow::ensure!(code == 0, t("err.not_done"));
                return Ok(());
            }
            let r = install::start_services();
            if let Err(e) = &r {
                platform::message(product::NAME, &tf("err.start_failed", &[("e", format!("{e:#}"))]));
            }
            r
        }
        // Started by the service manager only.
        #[cfg(windows)]
        Some("service") => service::run_agent_service(),
        #[cfg(windows)]
        Some("net") if args.iter().any(|a| a == "--dev") => net::run(true, args.iter().any(|a| a == "--view-only")),
        #[cfg(windows)]
        Some("net") => service::run_net_service(),
        #[cfg(windows)]
        Some("agent") => service::agent::run(args.iter().any(|a| a == "--dev")),
        // Started by the `Deskway` service as the person signed in, with its key on stdin.
        #[cfg(windows)]
        Some("worker") => service::worker::run(if args.iter().any(|a| a == "--admin") { service::worker::Kind::Admin } else { service::worker::Kind::User }),
        // Shows the on-screen indicator for a made-up controller.
        #[cfg(desktop_host)]
        Some("indicator-test") => {
            platform::platform_init();
            ui::indicator_test()
        }
        #[cfg(target_os = "macos")]
        Some("refresh-login-item") => platform::ui::refresh_login_item(),
        // Counts the border's pixels in real captures, with and without capture exclusion.
        #[cfg(windows)]
        Some("border-test") => {
            platform::platform_init();
            platform::ui::border_capture_test()
        }
        #[cfg(desktop_host)]
        None | Some("--at-login") => {
            let at_login = args.iter().any(|a| a == "--at-login");
            // Automatic start turned off: nothing at all runs at sign-in.
            #[cfg(windows)]
            if at_login && (!net::installed() || install::autostart() == Some(false)) {
                return Ok(());
            }
            platform::platform_init();
            if !platform::first_instance() {
                return Ok(());
            }
            ui::run(at_login)
        }
        #[cfg(target_os = "linux")]
        None => platform::run(),
        Some(other) => bail!(tf("err.unknown_command", &[("cmd", other.to_string())])),
    }
}

/// Runs `f` against the machine-wide data when the service is installed (elevating first if
/// needed), or against this user's data otherwise. `f` gets whether to answer in a dialog.
#[cfg(desktop_host)]
fn admin_command(args: &[String], f: impl FnOnce(bool) -> Result<()>) -> Result<()> {
    let dialog = args.iter().any(|a| a == "--dialog");
    // An explicit DESKWAY_HOME (tests, a second copy) means that copy's data, not the service's.
    #[cfg(windows)]
    if net::installed() && std::env::var_os("DESKWAY_HOME").is_none() {
        if !install::is_elevated() {
            let code = install::elevate(&format!("{} --dialog", args[0]))?;
            anyhow::ensure!(code == 0, t("err.not_done"));
            return Ok(());
        }
        service::harden::privileged();
        config::use_dir(service::machine_dir());
    }
    let r = f(dialog);
    if let (Err(e), true) = (&r, dialog) {
        platform::message(product::NAME, &format!("{e:#}"));
    }
    r
}

/// The installer window reads how an elevated step went from this file: "ok", or the reason.
#[cfg(windows)]
fn write_result(args: &[String], r: &Result<()>) {
    if let Some(path) = flag(args, "--result") {
        let text = match r {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("{e:#}"),
        };
        // The path comes from the unelevated window: never write through whatever is there.
        let _ = std::fs::remove_file(&path);
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            let _ = std::io::Write::write_all(&mut f, text.as_bytes());
        }
    }
}

#[cfg(windows)]
fn report(r: Result<()>, args: &[String], ok: &str) -> Result<()> {
    if args.iter().any(|a| a == "--dialog") {
        match &r {
            Ok(()) => platform::message(product::NAME, ok),
            Err(e) => platform::message(product::NAME, &format!("{e:#}")),
        }
    } else if r.is_ok() {
        println!("{ok}");
    }
    r
}
