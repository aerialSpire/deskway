//! `deskway mcp`: the MCP server an AI client starts (stdio, one JSON-RPC message per line). It
//! answers MCP's setup and the tool list itself, from `proto/tools.json` with `computer` added
//! to every tool, plus its own (`computers`, `release`, and `copy_to` and `copy_from`, which
//! this end carries out with the computer's file tools, `copy`); each other tool call goes
//! through the local proxy to the computer it names, whose answer comes back as it is.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::store::ProxyPlace;

/// The MCP version answered when the client asks for none this server knows.
const MCP_VERSION: &str = "2025-06-18";
const VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

const INSTRUCTIONS: &str = "Each tool acts on one Deskway computer of the account: call `computers` first to see their names, which are online and where AI is allowed, then pass `computer` to the others. A computer allows AI only after a person turns on Allow AI control for it in My devices; errors say what to do next. Its person may be using it at the same time.";

/// Every tool as the AI client sees it: the computer's tools, each taking `computer` too.
pub fn tool_list() -> Value {
    let computer = json!({ "type": "string", "description": "The computer: its name or id, as `computers` lists it." });
    let mut tools: Vec<Value> = wire::tools::all()
        .iter()
        .map(|t| {
            let mut input = t.input.clone();
            input["properties"]["computer"] = computer.clone();
            if t.name == "tunnel" {
                input["properties"]["local_port"] = json!({ "type": "integer", "description": "The port to listen on here (any free one by default)." });
            }
            let mut required: Vec<Value> = input["required"].as_array().cloned().unwrap_or_default();
            required.insert(0, json!("computer"));
            input["required"] = Value::Array(required);
            json!({ "name": t.name, "description": t.description, "inputSchema": input })
        })
        .collect();
    tools.push(json!({ "name": "computers", "description": "The computers of the account: name, id, system, whether online, and whether AI may control each (off, on, or stopped at the computer), which AI client uses it now, whether it is the computer you yourself run on (this_computer: its screen shows your own windows, and its input reaches them), and the tools it does not have (missing_tools, when its Deskway is older; calling one says unsupported).", "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false } }));
    tools.push(json!({ "name": "release", "description": "Gives a computer up now. It is given up anyway after 10 idle minutes, or as soon as this AI client closes. Programs started there keep running.", "inputSchema": { "type": "object", "properties": { "computer": computer }, "required": ["computer"], "additionalProperties": false } }));
    let path = |whose: &str| json!({ "type": "string", "description": format!("A file's full path {whose}.") });
    tools.push(json!({ "name": "copy_to", "description": "Copies a file from the machine you run on to the computer (replacing one there), checked by SHA-256 at both ends.", "inputSchema": { "type": "object", "properties": { "computer": computer, "from": path("on the machine you run on"), "to": path("on the computer") }, "required": ["computer", "from", "to"], "additionalProperties": false } }));
    tools.push(json!({ "name": "copy_from", "description": "Copies a file from the computer to the machine you run on (replacing one here), checked by SHA-256 at both ends.", "inputSchema": { "type": "object", "properties": { "computer": computer, "from": path("on the computer"), "to": path("on the machine you run on") }, "required": ["computer", "from", "to"], "additionalProperties": false } }));
    Value::Array(tools)
}

/// The proxy, connected again when its connection is lost (it stopped, or was replaced).
struct Proxy {
    conn: Mutex<Option<Arc<Conn>>>,
    next: Mutex<u64>,
}

/// One connection to the proxy: requests out, each answer to whoever waits for its id.
struct Conn {
    writer: Mutex<TcpStream>,
    waiting: Mutex<HashMap<u64, Sender<Value>>>,
    closed: AtomicBool,
}

/// Tells a running proxy to leave (signing out); nothing if none runs.
pub fn stop_proxy() {
    let Ok(place) = ProxyPlace::load() else { return };
    if let Ok(mut s) = TcpStream::connect(("127.0.0.1", place.port)) {
        let _ = writeln!(s, "{}", json!({ "token": place.token }));
        let _ = writeln!(s, "{}", json!({ "id": 1, "op": "quit" }));
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = BufReader::new(s).lines().next();
    }
}

impl Conn {
    /// Connects to the proxy, starting it if none runs. `newest`: this process was started
    /// from the program file as it is now (`deskway mcp` starting), so a proxy of another
    /// version is an older one: it is told to leave and this version's takes its place.
    fn open(newest: bool) -> Result<Arc<Conn>> {
        let own = crate::update::version();
        let want = newest && ProxyPlace::load().is_ok_and(|p| p.version != own);
        if want {
            stop_proxy();
        }
        let try_once = || -> Result<TcpStream> {
            let place = ProxyPlace::load()?;
            if want && place.version != own {
                bail!("the proxy of another version is still there");
            }
            let mut s = TcpStream::connect(("127.0.0.1", place.port))?;
            writeln!(s, "{}", json!({ "token": place.token }))?;
            Ok(s)
        };
        let stream = match try_once() {
            Ok(s) => s,
            Err(_) => {
                crate::os::start_proxy()?;
                let until = Instant::now() + Duration::from_secs(10);
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    match try_once() {
                        Ok(s) => break s,
                        Err(e) if Instant::now() > until => return Err(e).context("the proxy did not start (see proxy.log)"),
                        Err(_) => {}
                    }
                }
            }
        };
        let c = Arc::new(Conn { writer: Mutex::new(stream.try_clone()?), waiting: Mutex::new(HashMap::new()), closed: AtomicBool::new(false) });
        let reader = c.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
                let Some(id) = m["id"].as_u64() else { continue };
                if let Some(tx) = reader.waiting.lock().unwrap().remove(&id) {
                    let _ = tx.send(m);
                }
            }
            // The proxy is gone: whoever waits hears it, and the next call connects again.
            reader.closed.store(true, Ordering::SeqCst);
            reader.waiting.lock().unwrap().clear();
        });
        Ok(c)
    }
}

impl Proxy {
    fn connect() -> Result<Arc<Proxy>> {
        Ok(Arc::new(Proxy { conn: Mutex::new(Some(Conn::open(true)?)), next: Mutex::new(0) }))
    }

    /// The connection, made again if it was lost. Not as the newest: a proxy another,
    /// newer `deskway mcp` started is kept.
    fn conn(&self) -> Result<Arc<Conn>> {
        let mut c = self.conn.lock().unwrap();
        if let Some(open) = c.as_ref().filter(|c| !c.closed.load(Ordering::SeqCst)) {
            return Ok(open.clone());
        }
        let open = Conn::open(false)?;
        *c = Some(open.clone());
        Ok(open)
    }

    /// One request, answered. A call under way when the proxy goes is not made again (it may
    /// have been carried out): it fails, and the next one connects again.
    fn ask(&self, mut m: Value) -> Result<Value> {
        let conn = self.conn()?;
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let (tx, rx) = channel();
        conn.waiting.lock().unwrap().insert(id, tx);
        if conn.closed.load(Ordering::SeqCst) {
            conn.waiting.lock().unwrap().remove(&id);
            bail!("the proxy stopped");
        }
        m["id"] = json!(id);
        if let Err(e) = writeln!(conn.writer.lock().unwrap(), "{m}") {
            conn.closed.store(true, Ordering::SeqCst);
            return Err(e).context("the proxy");
        }
        let answer = rx.recv().context("the proxy stopped")?;
        if let Some(e) = answer["error"].as_str() {
            bail!("{e}");
        }
        Ok(answer["result"].clone())
    }
}

fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn text_result(v: &Value) -> Value {
    json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": { "result": v }, "isError": false })
}

/// The answer to one message; `None` for a notification.
fn handle(m: &Value, proxy: &Proxy) -> Option<Value> {
    let id = m.get("id")?.clone();
    let method = m["method"].as_str().unwrap_or_default();
    let fail = |e: anyhow::Error| reply(&id, crate::link::error_result("proxy", &format!("{e:#}"), "Check `deskway login`, and proxy.log beside its files."));
    Some(match method {
        "initialize" => {
            let asked = m["params"]["protocolVersion"].as_str().unwrap_or(MCP_VERSION);
            let version = if VERSIONS.contains(&asked) { asked } else { MCP_VERSION };
            reply(&id, json!({ "protocolVersion": version, "capabilities": { "tools": { "listChanged": false } }, "serverInfo": { "name": "deskway", "version": env!("CARGO_PKG_VERSION") }, "instructions": INSTRUCTIONS }))
        }
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(&id, json!({ "tools": tool_list() })),
        "tools/call" => {
            let name = m["params"]["name"].as_str().unwrap_or_default();
            let mut args = m["params"]["arguments"].clone();
            let computer = args["computer"].as_str().unwrap_or_default().to_string();
            match name {
                "computers" => proxy.ask(json!({ "op": "computers" })).map(|v| reply(&id, text_result(&v))).unwrap_or_else(fail),
                "release" => proxy.ask(json!({ "op": "release", "computer": computer })).map(|v| reply(&id, text_result(&v))).unwrap_or_else(fail),
                _ if computer.is_empty() => reply(&id, crate::link::error_result("invalid_args", "which computer: `computer` is missing", "Call computers, then pass one of their names.")),
                "tunnel" => {
                    // The computer allows the port first (its gate, its audit log); then this end
                    // listens for it.
                    let request = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "tunnel", "arguments": { "port": args["port"] } } });
                    match proxy.ask(json!({ "op": "call", "computer": computer, "request": request })) {
                        Ok(a) if a["result"]["isError"] == true => a,
                        Ok(_) => match proxy.ask(json!({ "op": "tunnel", "computer": computer, "port": args["port"], "local_port": args["local_port"] })) {
                            Ok(v) if v["isError"] == true => reply(&id, v),
                            Ok(v) => reply(&id, text_result(&v)),
                            Err(e) => fail(e),
                        },
                        Err(e) => fail(e),
                    }
                }
                "copy_to" | "copy_from" => {
                    // The computer's own tools, called one by one: their data, or their error.
                    let call = |tool: &str, a: Value| -> std::result::Result<Value, crate::copy::Fail> {
                        let request = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": tool, "arguments": a } });
                        let answer = proxy.ask(json!({ "op": "call", "computer": computer, "request": request })).map_err(|e| crate::copy::Fail::new("proxy", format!("{e:#}"), "Check `deskway login`."))?;
                        let r = &answer["result"]["structuredContent"];
                        if answer["result"]["isError"] == true {
                            let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
                            return Err(crate::copy::Fail { code: s("error"), message: s("message"), next: s("next") });
                        }
                        Ok(r.clone())
                    };
                    let (a, b) = (args["from"].as_str().unwrap_or_default(), args["to"].as_str().unwrap_or_default());
                    let done = if name == "copy_to" { crate::copy::to(&call, a, b) } else { crate::copy::from(&call, a, b) };
                    reply(&id, done.map(|v| text_result(&v)).unwrap_or_else(|f| crate::link::error_result(&f.code, &f.message, &f.next)))
                }
                _ => {
                    if let Some(o) = args.as_object_mut() {
                        o.remove("computer");
                    }
                    let request = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": args } });
                    proxy.ask(json!({ "op": "call", "computer": computer, "request": request })).unwrap_or_else(fail)
                }
            }
        }
        _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no such method" } }),
    })
}

/// One request answered as `deskway mcp` would answer it (`deskway call`, `deskway computers`).
pub fn answer(request: &Value) -> Result<Value> {
    let proxy = Proxy::connect()?;
    handle(request, &proxy).context("no answer")
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
            if let Some(a) = handle(&m, &proxy) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_names_its_computer_and_the_two_of_its_own_are_there() {
        let tools = tool_list();
        let names: Vec<&str> = tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"computers") && names.contains(&"release") && names.contains(&"computer_info"));
        for t in tools.as_array().unwrap().iter().filter(|t| t["name"] != "computers") {
            assert_eq!(t["inputSchema"]["required"][0], "computer", "{}", t["name"]);
            assert!(t["inputSchema"]["properties"]["computer"].is_object(), "{}", t["name"]);
        }
    }
}
