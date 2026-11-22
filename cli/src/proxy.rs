//! The local proxy (docs/architecture.md, "The local proxy"): one per user of this machine, started by the
//! first `deskway mcp` and shared by all of them, so every AI client window here is the same AI
//! device on the same AI seat. It holds the signaling socket, knows the account's computers, and
//! keeps one link per computer in use (`link`), made on the first call and given up when the
//! computer ends it (idle, stopped) or on `release`.
//!
//! Its clients reach it on 127.0.0.1 at the port in `proxy.json`, and prove themselves with the
//! token there (a file only this user reads). One JSON line per request:
//! `{"id", "op": "computers"}`, `{"id", "op": "call", "computer", "request"}` (an MCP
//! `tools/call`), `{"id", "op": "release", "computer"}`, `{"id", "op": "quit"}` (signing out);
//! each answered `{"id", "result"}` or `{"id", "error"}`. With no client and no call for 10
//! minutes it leaves.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use wire::types::Reason;

use crate::keys::DeviceKey;
use crate::link::{self, LinkEvent};
use crate::store::{Account, ProxyPlace};

const IDLE: Duration = Duration::from_secs(10 * 60);

/// What a client asks of the proxy.
enum Request {
    Computers(Sender<Value>),
    Call { computer: String, request: Value, reply: Sender<Value> },
    Release { computer: String, reply: Sender<Value> },
    /// A tunnel to a port the computer allowed, on this machine's port `local`.
    Tunnel { computer: String, port: u16, local: u16, reply: Sender<Value> },
    /// Leave now, giving every computer up.
    Quit(Sender<Value>),
}

/// The account's computers as this AI client sees them; `this_computer`: the one it runs on
/// (its screen shows this AI client's own windows); `missing_tools`: what this AI client offers
/// and the computer does not carry out (its Deskway is older), when it said and lacks any.
fn computers(list: &Value) -> Value {
    Value::Array(
        list.as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                let mut c = json!({ "id": d["id"], "name": d["name"], "os": d["os"], "online": d["online"], "ai": d["ai"]["state"].as_str().unwrap_or("off"), "in_use_by": d["ai"]["name"], "this_computer": d["here"].as_bool().unwrap_or(false) });
                let missing: Vec<&str> = wire::tools::all().iter().map(|t| t.name.as_str()).filter(|t| !carries(d, t)).collect();
                if !missing.is_empty() {
                    c["missing_tools"] = json!(missing);
                }
                c
            })
            .collect(),
    )
}

/// Whether computer `d` carries out tool `name`: as its hello said; an older computer that
/// said nothing is left to answer for itself.
fn carries(d: &Value, name: &str) -> bool {
    d["tools"].as_array().is_none_or(|t| t.iter().any(|x| x == name))
}

/// The computer `want` names, by its id or its name (any case).
fn resolve<'a>(list: &'a Value, want: &str) -> Result<&'a Value, String> {
    let all = list.as_array().map(Vec::as_slice).unwrap_or_default();
    if let Some(d) = all.iter().find(|d| d["id"] == want) {
        return Ok(d);
    }
    let named: Vec<&Value> = all.iter().filter(|d| d["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(want))).collect();
    match named.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no computer {want}: the account has {}", all.iter().filter_map(|d| d["name"].as_str()).collect::<Vec<_>>().join(", "))),
        _ => Err(format!("several computers are called {want}: use its id")),
    }
}

/// The server refused the socket because this device's session has ended.
fn ended(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<tungstenite::Error>(), Some(tungstenite::Error::Http(r)) if r.status() == 401)
}

fn connect(a: &Account) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
    let url = format!("{}/ws/client", a.server.replacen("http", "ws", 1));
    let mut req = url.into_client_request()?;
    req.headers_mut().insert("Cookie", a.cookies().parse().context("cookie header")?);
    // The server takes the socket only from its own origin.
    req.headers_mut().insert("Origin", a.server.parse().context("origin header")?);
    let (mut ws, _) = tungstenite::connect(req).context("the signaling socket")?;
    let timeout = Some(Duration::from_millis(20));
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout)?,
        MaybeTlsStream::NativeTls(s) => s.get_ref().set_read_timeout(timeout)?,
        _ => {}
    }
    ws.send(Message::text(json!({ "type": "hello", "version": wire::PROTOCOL_VERSION, "caps": [] }).to_string()))?;
    // Where this AI client runs, so the computer that is this machine says so (peer::here).
    if let Some(me) = peer::here::machine() {
        ws.send(Message::text(json!({ "type": "locate", "where": [me] }).to_string()))?;
    }
    Ok(ws)
}

struct Hub {
    account: Account,
    key: Arc<DeviceKey>,
    list: Value,
    ice: Value,
    /// Computer id -> its link's session and handle.
    links: HashMap<String, (String, Sender<LinkEvent>)>,
    out_tx: Sender<String>,
    ended_tx: Sender<(String, Option<Reason>)>,
    last_call: Instant,
}

impl Hub {
    fn on_server(&mut self, text: &str) {
        let Ok(m) = serde_json::from_str::<Value>(text) else { return };
        match m["type"].as_str().unwrap_or_default() {
            "devices" => self.list = m.get("devices").cloned().unwrap_or_else(|| json!([])),
            // No relays configured: the list is left out, which is an empty one.
            "ice" => self.ice = m.get("ice").cloned().unwrap_or_else(|| json!([])),
            _ => {
                let session = m["session"].as_str().unwrap_or_default();
                if let Some((_, tx)) = self.links.values().find(|(s, _)| s == session) {
                    let _ = tx.send(LinkEvent::Signal(m));
                }
            }
        }
    }

    fn on_request(&mut self, r: Request) {
        self.last_call = Instant::now();
        match r {
            Request::Computers(reply) => {
                let _ = reply.send(computers(&self.list));
            }
            Request::Quit(_) => unreachable!("quitting is the connection's to do"),
            Request::Release { computer, reply } => {
                let id = resolve(&self.list, &computer).ok().and_then(|d| d["id"].as_str()).map(str::to_string);
                if let Some((_, tx)) = id.and_then(|id| self.links.remove(&id)) {
                    let _ = tx.send(LinkEvent::Leave);
                }
                let _ = reply.send(json!({ "released": computer }));
            }
            Request::Tunnel { computer, port, local, reply } => {
                let id = resolve(&self.list, &computer).ok().and_then(|d| d["id"].as_str()).map(str::to_string);
                match id.and_then(|id| self.links.get(&id)) {
                    Some((_, tx)) => {
                        let _ = tx.send(LinkEvent::Tunnel { port, local, reply });
                    }
                    None => {
                        let _ = reply.send(link::error_result("ended", &format!("no connection to {computer} now"), "Call tunnel again."));
                    }
                }
            }
            Request::Call { computer, request, reply } => {
                let d = match resolve(&self.list, &computer) {
                    Ok(d) => d.clone(),
                    Err(e) => {
                        let _ = reply.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": link::error_result("not_found", &e, "Call computers for their names.") }));
                        return;
                    }
                };
                let id = d["id"].as_str().unwrap_or_default().to_string();
                let tool = request["params"]["name"].as_str().unwrap_or_default();
                if !carries(&d, tool) {
                    let name = d["name"].as_str().unwrap_or(&id);
                    let _ = reply.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": link::error_result("unsupported", &format!("{name} does not have {tool}: its Deskway is older than this AI client"), "A person updates Deskway on that computer; computers lists what it lacks.") }));
                    return;
                }
                if !self.links.contains_key(&id) {
                    if d["online"] != true {
                        let (code, message, next) = link::refusal(Some(Reason::Offline), d["name"].as_str().unwrap_or(&id));
                        let _ = reply.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": link::error_result(code, &message, next) }));
                        return;
                    }
                    self.open(&id, d["name"].as_str().unwrap_or(&id));
                }
                if let Some((_, tx)) = self.links.get(&id) {
                    let _ = tx.send(LinkEvent::Call { request, reply });
                }
            }
        }
    }

    /// A new link to computer `id`, on its own thread.
    fn open(&mut self, id: &str, name: &str) {
        let session = URL_SAFE_NO_PAD.encode(wire::crypto::random::<16>());
        let (tx, rx) = channel();
        let p = link::Params { computer: id.to_string(), name: name.to_string(), session: session.clone(), ice: self.ice.clone(), out: self.out_tx.clone(), key: self.key.clone(), ended: self.ended_tx.clone() };
        let own = tx.clone();
        let started = std::thread::Builder::new().name(format!("link-{id}")).spawn(move || link::run(p, rx, own));
        if started.is_ok() {
            self.links.insert(id.to_string(), (session, tx));
        }
    }

    fn attach_all(&self) {
        for (id, (session, _)) in &self.links {
            let _ = self.out_tx.send(json!({ "type": "attach", "session": session, "device": id }).to_string());
        }
    }
}

/// Serves one client: its token first, then its requests, each answered as soon as it can be.
fn client(stream: TcpStream, token: String, requests: Sender<Request>, clients: Arc<AtomicUsize>) {
    clients.fetch_add(1, Ordering::SeqCst);
    let writer = Arc::new(Mutex::new(stream.try_clone().expect("clone a socket")));
    let mut lines = BufReader::new(stream).lines();
    let authed = lines.next().and_then(Result::ok).and_then(|l| serde_json::from_str::<Value>(&l).ok()).is_some_and(|v| v["token"] == token.as_str());
    if authed {
        for line in lines.map_while(Result::ok) {
            let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
            let (reply, answer) = channel();
            let r = match m["op"].as_str().unwrap_or_default() {
                "computers" => Request::Computers(reply),
                "release" => Request::Release { computer: m["computer"].as_str().unwrap_or_default().to_string(), reply },
                "call" => Request::Call { computer: m["computer"].as_str().unwrap_or_default().to_string(), request: m["request"].clone(), reply },
                "tunnel" => Request::Tunnel { computer: m["computer"].as_str().unwrap_or_default().to_string(), port: m["port"].as_u64().unwrap_or_default() as u16, local: m["local_port"].as_u64().unwrap_or_default() as u16, reply },
                "quit" => Request::Quit(reply),
                _ => continue,
            };
            if requests.send(r).is_err() {
                break;
            }
            let (writer, id) = (writer.clone(), m["id"].clone());
            std::thread::spawn(move || {
                let line = match answer.recv() {
                    Ok(result) => json!({ "id": id, "result": result }),
                    Err(_) => json!({ "id": id, "error": "the proxy is leaving" }),
                };
                let _ = writeln!(writer.lock().unwrap(), "{line}");
            });
        }
    }
    clients.fetch_sub(1, Ordering::SeqCst);
}

/// Runs the proxy until it has had no client and no call for 10 minutes.
pub fn run() -> Result<()> {
    let account = Account::load()?;
    if !account.signed_in() {
        bail!("not signed in: run `deskway login` first");
    }
    let key = Arc::new(DeviceKey::load_or_create()?);
    let listener = TcpListener::bind("127.0.0.1:0").context("listen on this machine")?;
    let token = URL_SAFE_NO_PAD.encode(wire::crypto::random::<24>());
    ProxyPlace { port: listener.local_addr()?.port(), token: token.clone(), version: crate::update::version().to_string() }.save()?;
    let (req_tx, req_rx) = channel::<Request>();
    let clients = Arc::new(AtomicUsize::new(0));
    let c = clients.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let (token, tx, c) = (token.clone(), req_tx.clone(), c.clone());
            std::thread::spawn(move || client(s, token, tx, c));
        }
    });
    let (out_tx, out_rx) = channel::<String>();
    let (ended_tx, ended_rx) = channel();
    let mut hub = Hub { account, key, list: Value::Null, ice: Value::Null, links: HashMap::new(), out_tx, ended_tx, last_call: Instant::now() };
    let mut delay = Duration::from_secs(1);
    loop {
        match connect(&hub.account) {
            Ok(mut ws) => {
                delay = Duration::from_secs(1);
                hub.attach_all();
                if let Err(e) = serve(&mut ws, &mut hub, &req_rx, &out_rx, &ended_rx, &clients) {
                    eprintln!("signaling lost: {e:#}");
                } else {
                    return Ok(());
                }
            }
            // The session has ended (it lasts 30 days): this device signs in again with its key.
            // Refused (removed in My devices, or another server's account), the proxy leaves:
            // only `deskway login` can sign it in again.
            Err(e) if ended(&e) => match crate::account::renew(&mut hub.account) {
                Ok(()) => continue,
                Err(r) => return Err(r),
            },
            Err(e) => eprintln!("cannot reach the server: {e:#}"),
        }
        if clients.load(Ordering::SeqCst) == 0 && hub.links.is_empty() && hub.last_call.elapsed() > IDLE {
            return Ok(());
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

/// One signaling connection's life. Ok when the proxy should leave.
fn serve(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, hub: &mut Hub, requests: &Receiver<Request>, out: &Receiver<String>, ended: &Receiver<(String, Option<Reason>)>, clients: &AtomicUsize) -> Result<()> {
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => hub.on_server(t.as_str()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        // A call waits for the device list and the STUN and TURN servers, which come right
        // after connecting.
        if !hub.list.is_null() && !hub.ice.is_null() {
            while let Ok(r) = requests.try_recv() {
                if let Request::Quit(reply) = r {
                    for (_, (_, tx)) in hub.links.drain() {
                        let _ = tx.send(LinkEvent::Leave);
                    }
                    let _ = reply.send(json!({ "quit": true }));
                    // The links' goodbyes go out before the socket closes.
                    std::thread::sleep(Duration::from_millis(300));
                    while let Ok(m) = out.try_recv() {
                        ws.send(Message::text(m))?;
                    }
                    let _ = ws.close(None);
                    return Ok(());
                }
                hub.on_request(r);
            }
        }
        // No AI client is connected any more (it quit, was closed or crashed): the computers it
        // held are given up now, not after 10 idle minutes, so nobody sees an AI there that is gone.
        if clients.load(Ordering::SeqCst) == 0 && !hub.links.is_empty() {
            for (_, (_, tx)) in hub.links.drain() {
                let _ = tx.send(LinkEvent::Leave);
            }
        }
        while let Ok((session, why)) = ended.try_recv() {
            hub.links.retain(|_, (s, _)| *s != session);
            eprintln!("a link ended: {}", why.map_or("left", |r| r.as_str()));
        }
        while let Ok(m) = out.try_recv() {
            ws.send(Message::text(m))?;
        }
        if clients.load(Ordering::SeqCst) == 0 && hub.links.is_empty() && hub.last_call.elapsed() > IDLE {
            let _ = ws.close(None);
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_computer_is_named_by_its_id_or_its_one_name() {
        let list = json!([{ "id": "H1", "name": "Desk" }, { "id": "H2", "name": "Mac" }, { "id": "H3", "name": "mac" }]);
        assert_eq!(resolve(&list, "H2").unwrap()["id"], "H2");
        assert_eq!(resolve(&list, "desk").unwrap()["id"], "H1", "any case");
        assert!(resolve(&list, "MAC").unwrap_err().contains("several"));
        assert!(resolve(&list, "Laptop").unwrap_err().contains("Desk, Mac, mac"));
    }

    #[test]
    fn the_computers_list_says_what_ai_may_do_on_each() {
        let all: Vec<&str> = wire::tools::all().iter().map(|t| t.name.as_str()).collect();
        let older: Vec<&str> = all.iter().copied().filter(|t| *t != "ui_tree").collect();
        let list = json!([
            { "id": "H1", "name": "Desk", "os": "windows", "online": true, "here": true, "tools": all, "ai": { "state": "on", "name": "Claude", "admin": false, "ts": 1 } },
            { "id": "H2", "name": "Mac", "os": "macos", "online": false },
            { "id": "H3", "name": "Old", "os": "windows", "online": true, "tools": older },
        ]);
        let c = computers(&list);
        assert_eq!((c[0]["ai"].as_str(), c[0]["in_use_by"].as_str(), c[0]["this_computer"].as_bool()), (Some("on"), Some("Claude"), Some(true)));
        assert_eq!((c[1]["ai"].as_str(), c[1]["this_computer"].as_bool()), (Some("off"), Some(false)));
        assert!(c[0].get("missing_tools").is_none() && c[1].get("missing_tools").is_none(), "all there, or not said");
        assert_eq!(c[2]["missing_tools"], json!(["ui_tree"]));
        // (computer, tool) -> carried out, as far as this end knows
        for (d, tool, want) in [(0, "ui_tree", true), (2, "ui_tree", false), (2, "run", true), (1, "ui_tree", true)] {
            assert_eq!(carries(&list[d], tool), want, "{} {tool}", list[d]["name"]);
        }
    }
}
