//! The line-delimited MCP transport for `deskway mcp`.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::mcp::{self, Access};
use crate::proxy_client::Proxy;

/// The MCP version answered when the client asks for none this server knows.
const MCP_VERSION: &str = "2025-06-18";
const VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

pub fn handle(m: &Value, ask: impl Fn(Value) -> Result<Value>) -> Option<Value> {
    if m["method"] == "initialize" {
        let asked = m["params"]["protocolVersion"].as_str().unwrap_or(MCP_VERSION);
        let version = if VERSIONS.contains(&asked) { asked } else { MCP_VERSION };
        return Some(mcp::initialize(m.get("id")?, version));
    }
    mcp::handle(m, Access::Local, ask)
}

/// One request answered as `deskway mcp` would answer it (`deskway call`, `deskway computers`).
pub fn answer(request: &Value) -> Result<Value> {
    let proxy = Proxy::connect()?;
    handle(request, |m| proxy.ask(m)).context("no answer")
}

/// Serves the AI client on stdin and stdout until it closes them, and every answer is out.
pub fn run() -> Result<()> {
    crate::update::in_background();
    let proxy = Proxy::connect()?;
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let mut answering = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let Ok(m) = serde_json::from_str::<Value>(&line) else {
            let e = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "not JSON" } });
            writeln!(out.lock().unwrap(), "{e}")?;
            continue;
        };
        // Each request on its own: a slow tool on one computer holds up no other.
        let (proxy, out) = (proxy.clone(), out.clone());
        answering.retain(|t: &std::thread::JoinHandle<()>| !t.is_finished());
        answering.push(std::thread::spawn(move || {
            if let Some(a) = handle(&m, |m| proxy.ask(m)) {
                let mut o = out.lock().unwrap();
                let _ = writeln!(o, "{a}");
                let _ = o.flush();
            }
        }));
    }
    for t in answering {
        let _ = t.join();
    }
    Ok(())
}
