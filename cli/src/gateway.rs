//! The JSON-only Streamable HTTP transport for the shared MCP server.

use std::io::{Cursor, Read};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use http::uri::Authority;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response};

use crate::mcp::{self, Access};
use crate::proxy_client::Proxy;

pub const USAGE: &str = "   deskway gateway --listen IP:PORT [--public-host HOST] [--call-timeout-seconds SECONDS] [--max-result-chars CHARS]";

const PATH: &str = "/mcp";
const MODERN: &str = "2026-07-28";
const LEGACY: [&str; 2] = ["2025-11-25", "2025-06-18"];
const VERSIONS: [&str; 3] = [MODERN, LEGACY[0], LEGACY[1]];
const VERSION_HEADER: &str = "MCP-Protocol-Version";
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT: &str = "io.modelcontextprotocol/clientInfo";
const META_CLIENT_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";
const JSON: &str = "application/json";
const SSE: &str = "text/event-stream";
const MIN_RESULT_CHARS: usize = 512;
const MAX_REQUEST_BYTES: u64 = 2 * wire::mcp::MAX_MESSAGE as u64;
const MAX_ACTIVE_REQUESTS: usize = 32;

type HttpResponse = Response<Cursor<Vec<u8>>>;

struct RequestSlot(Arc<AtomicUsize>);

impl Drop for RequestSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Config {
    address: SocketAddr,
    public_host: Option<Authority>,
    limits: Limits,
}

#[derive(Clone, Copy)]
struct Limits {
    call: Duration,
    result: usize,
}

impl Config {
    fn parse(args: &[String]) -> Result<Self> {
        const LISTEN_ERROR: &str = "--listen requires a loopback IP and port, such as 127.0.0.1:8765 or [::1]:8765.";
        let mut address = None;
        let mut public_host = None;
        let mut limits = Limits { call: Duration::from_secs(240), result: 150_000 };
        let mut pairs = args.chunks_exact(2);
        for pair in &mut pairs {
            match pair[0].as_str() {
                "--listen" => {
                    let socket: SocketAddr = pair[1].parse().ok().context(LISTEN_ERROR)?;
                    if !socket.ip().is_loopback() {
                        bail!("The gateway has no authorization of its own. Listen on a loopback address only.");
                    }
                    address = Some(socket);
                }
                "--public-host" => {
                    public_host = Some(authority(&pair[1]).context("--public-host requires a host name or IP with an optional port.")?);
                }
                "--call-timeout-seconds" => {
                    limits.call = pair[1].parse().ok().filter(|&n| n > 0).map(Duration::from_secs)
                        .filter(|&d| Instant::now().checked_add(d).is_some())
                        .context("--call-timeout-seconds requires a positive number of seconds.")?;
                }
                "--max-result-chars" => {
                    limits.result = pair[1].parse().ok().filter(|&n| n >= MIN_RESULT_CHARS)
                        .with_context(|| format!("--max-result-chars requires at least {MIN_RESULT_CHARS} characters so errors can fit."))?;
                }
                _ => bail!("{USAGE}"),
            }
        }
        if !pairs.remainder().is_empty() {
            bail!("{USAGE}");
        }
        Ok(Self { address: address.context(LISTEN_ERROR)?, public_host, limits })
    }
}

fn authority(value: &str) -> Option<Authority> {
    let parsed: Authority = value.parse().ok()?;
    // An authority in Host or Origin may contain a port, but never user info.
    (!value.contains('@') && (parsed.host() == value || parsed.port_u16().is_some())).then_some(parsed)
}

fn same_authority(lhs: &Authority, rhs: &Authority, default_port: u16) -> bool {
    lhs.host().eq_ignore_ascii_case(rhs.host())
        && lhs.port_u16().unwrap_or(default_port) == rhs.port_u16().unwrap_or(default_port)
}

fn headers_named<'a>(headers: &'a [Header], name: &'static str) -> impl Iterator<Item = &'a str> {
    headers.iter().filter(move |h| h.field.equiv(name)).map(|h| h.value.as_str())
}

fn single_header<'a>(headers: &'a [Header], name: &'static str) -> Result<Option<&'a str>, ()> {
    let mut values = headers_named(headers, name);
    let first = values.next();
    if values.next().is_some() { Err(()) } else { Ok(first) }
}

fn allowed_origin(headers: &[Header], address: SocketAddr, public_host: Option<&Authority>) -> bool {
    let Some(host) = single_header(headers, "Host").ok().flatten().and_then(authority) else { return false };
    let ip = host.host().trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>();
    let local = (host.host().eq_ignore_ascii_case("localhost") || ip == Ok(address.ip()))
        && host.port_u16().unwrap_or(80) == address.port();
    let public = public_host.is_some_and(|allowed| same_authority(&host, allowed, 443));
    match single_header(headers, "Origin") {
        Ok(None) => local || public,
        Ok(Some(origin)) => {
            let Some((scheme, origin_host)) = origin.split_once("://") else { return false };
            let Some(origin_host) = authority(origin_host) else { return false };
            match scheme {
                "http" => local && same_authority(&origin_host, &host, 80),
                "https" => public && same_authority(&origin_host, &host, 443),
                _ => false,
            }
        }
        Err(()) => false,
    }
}

fn accepts(headers: &[Header], media: &str) -> bool {
    headers_named(headers, "Accept").flat_map(|v| v.split(',')).any(|range| {
        let mut parts = range.split(';');
        parts.next().is_some_and(|kind| kind.trim().eq_ignore_ascii_case(media))
            && parts.filter_map(|p| p.trim().split_once('=')).all(|(key, value)| {
                !key.eq_ignore_ascii_case("q") || value.parse::<f32>().is_ok_and(|q| q > 0.0 && q <= 1.0)
            })
    })
}

fn header(name: &'static str, value: &'static str) -> Header {
    Header::from_bytes(name, value).expect("static HTTP header")
}

fn empty(status: u16) -> HttpResponse {
    Response::from_data(Vec::new()).with_status_code(status)
}

fn json_response(status: u16, body: &Value) -> HttpResponse {
    Response::from_string(body.to_string()).with_status_code(status).with_header(header("Content-Type", JSON))
}

fn reject(id: &Value, status: u16, code: i32, message: &str) -> HttpResponse {
    json_response(status, &mcp::error(id, code, message))
}

fn preflight(method: &Method, path: &str, headers: &[Header], config: &Config) -> Option<HttpResponse> {
    if !allowed_origin(headers, config.address, config.public_host.as_ref()) {
        return Some(empty(403));
    }
    if path != PATH {
        return Some(empty(404));
    }
    if *method != Method::Post {
        return Some(empty(405).with_header(header("Allow", "POST")));
    }
    let media = single_header(headers, "Content-Type").ok().flatten().and_then(|v| v.split(';').next());
    if !media.is_some_and(|v| v.trim().eq_ignore_ascii_case(JSON)) {
        return Some(empty(415));
    }
    if !accepts(headers, JSON) || !accepts(headers, SSE) {
        return Some(empty(406));
    }
    None
}

fn header_name(value: &str) -> Option<String> {
    if let Some(encoded) = value.strip_prefix("=?base64?").and_then(|v| v.strip_suffix("?=")) {
        return String::from_utf8(STANDARD.decode(encoded).ok()?).ok();
    }
    value.bytes().all(|b| b.is_ascii_graphic() || b == b' ' || b == b'\t').then(|| value.to_owned())
}

fn modern_headers_match(headers: &[Header], request: &Value, method: &str) -> bool {
    if single_header(headers, "Mcp-Method") != Ok(Some(method)) {
        return false;
    }
    let key = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" => "uri",
        _ => return true,
    };
    let expected = request["params"][key].as_str();
    let actual = single_header(headers, "Mcp-Name").ok().flatten().and_then(header_name);
    expected.is_some() && actual.as_deref() == expected
}

fn complete(result: &mut Value) {
    result["resultType"] = json!("complete");
    result["_meta"][META_SERVER] = mcp::server_info();
}

fn complete_discovery(result: &mut Value) {
    complete(result);
    result["ttlMs"] = json!(0);
    result["cacheScope"] = json!("public");
}

impl Limits {
    fn tool_error(self, id: &Value, timeout: bool, modern: bool) -> Value {
        let mut result = if timeout {
            crate::link::error_result(
                "timeout",
                &format!("The tool call exceeded the gateway limit of {} seconds.", self.call.as_secs()),
                "Check whether the call took effect before trying again.",
            )
        } else {
            crate::link::error_result(
                "too_large",
                &format!("The tool result exceeded the gateway limit of {} characters.", self.result),
                "Check the outcome before retrying. Ask for less or raise the limit.",
            )
        };
        if modern {
            complete(&mut result);
        }
        mcp::reply(id, result)
    }

    fn call(
        self,
        request: Value,
        modern: bool,
        ask: impl Fn(Value, Instant) -> Result<Value> + Send + 'static,
    ) -> Value {
        let id = request["id"].clone();
        let deadline = Instant::now() + self.call;
        let (tx, rx) = mpsc::channel();
        // The caller's deadline also covers reconnecting or waiting to write to the proxy.
        std::thread::spawn(move || {
            let answer = mcp::handle(&request, Access::Remote, |m| ask(m, deadline)).expect("request has an id");
            let _ = tx.send(answer);
        });
        let Ok(mut answer) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
            return self.tool_error(&id, true, modern);
        };
        let oversized = if let Some(result) = answer.get_mut("result") {
            if modern {
                complete(result);
            }
            result.to_string().chars().count() > self.result
        } else {
            false
        };
        if Instant::now() >= deadline {
            return self.tool_error(&id, true, modern);
        }
        if oversized {
            return self.tool_error(&id, false, modern);
        }
        answer
    }
}

fn rpc(
    headers: &[Header],
    body: &[u8],
    limits: Limits,
    ask: impl Fn(Value, Instant) -> Result<Value> + Send + 'static,
) -> HttpResponse {
    let Ok(request) = serde_json::from_slice::<Value>(body) else {
        return reject(&Value::Null, 400, -32700, "Parse error");
    };
    let invalid_request = |id: &Value| reject(id, 400, -32600, "Invalid request");
    let Some(method) = request["method"].as_str().filter(|_| request["jsonrpc"] == "2.0") else {
        return invalid_request(&Value::Null);
    };
    let id = &request["id"];
    let notification = request.get("id").is_none();
    if !notification && !(id.is_string() || id.is_i64() || id.is_u64()) {
        return invalid_request(&Value::Null);
    }
    let invalid_params = || reject(id, 400, -32602, "Invalid params");
    if request.get("params").is_some_and(|p| !p.is_object()) {
        return invalid_params();
    }
    let mismatch = || reject(id, 400, -32020, "Missing or mismatched MCP header");
    let Ok(version) = single_header(headers, VERSION_HEADER) else { return mismatch() };
    let meta = &request["params"]["_meta"];
    if meta.get(META_VERSION).is_some_and(|body| version.is_none() || version != body.as_str()) {
        return mismatch();
    }
    if let Some(version) = version.filter(|v| !VERSIONS.contains(v)) {
        let mut answer = mcp::error(id, -32022, "Unsupported protocol version");
        answer["error"]["data"] = json!({ "supported": VERSIONS, "requested": version });
        return json_response(400, &answer);
    }
    let modern = version == Some(MODERN);
    if !modern && method == "initialize" && !notification {
        let params = &request["params"];
        let Some(asked) = params["protocolVersion"].as_str() else {
            return invalid_params();
        };
        if !params["capabilities"].is_object() || !implementation(&params["clientInfo"]) {
            return invalid_params();
        }
        let chosen = if LEGACY.contains(&asked) { asked } else { LEGACY[0] };
        return json_response(200, &mcp::initialize(id, chosen));
    }
    if version.is_none() {
        return mismatch();
    }
    if notification {
        return if !modern && matches!(method, "notifications/initialized" | "notifications/cancelled") {
            empty(202)
        } else {
            invalid_request(id)
        };
    }
    if modern {
        if !meta[META_VERSION].is_string() || !meta[META_CLIENT_CAPS].is_object()
            || meta.get(META_CLIENT).is_some_and(|info| !implementation(info)) {
            return invalid_params();
        }
        if !modern_headers_match(headers, &request, method) {
            return mismatch();
        }
    }
    let answer = match method {
        "tools/call" => {
            let params = &request["params"];
            if !params["name"].is_string() || params.get("arguments").is_some_and(|a| !a.is_object()) {
                return invalid_params();
            }
            limits.call(request, modern, ask)
        }
        "ping" if !modern => mcp::handle(&request, Access::Remote, |_| unreachable!()).expect("request has an id"),
        "tools/list" => {
            let mut answer = mcp::handle(&request, Access::Remote, |_| unreachable!()).expect("request has an id");
            if modern {
                complete_discovery(&mut answer["result"]);
            }
            answer
        }
        "server/discover" if modern => {
            let mut result = json!({ "supportedVersions": VERSIONS, "capabilities": mcp::capabilities(), "instructions": mcp::INSTRUCTIONS });
            complete_discovery(&mut result);
            mcp::reply(id, result)
        }
        _ => return reject(id, if modern { 404 } else { 200 }, -32601, "Method not found"),
    };
    json_response(200, &answer)
}

fn implementation(info: &Value) -> bool {
    info["name"].is_string() && info["version"].is_string()
}

fn serve(mut req: Request, config: &Config, proxy: Arc<Proxy>, slot: Arc<RequestSlot>) {
    if let Some(response) = preflight(req.method(), req.url(), req.headers(), config) {
        let _ = req.respond(response);
        return;
    }
    let mut body = Vec::new();
    let response = match req.as_reader().take(MAX_REQUEST_BYTES + 1).read_to_end(&mut body) {
        Err(_) => empty(400),
        Ok(_) if body.len() as u64 > MAX_REQUEST_BYTES => empty(413),
        Ok(_) => {
            // The proxy worker can outlive a timed-out HTTP response.
            let call_slot = slot.clone();
            rpc(req.headers(), &body, config.limits, move |m, until| {
                let _slot = &call_slot;
                proxy.ask_until(m, Some(until))
            })
        }
    };
    let _ = req.respond(response);
}

pub fn run(args: &[String]) -> Result<()> {
    const BIND_ERROR: &str = "Cannot start the gateway listener.";
    let mut config = Config::parse(args)?;
    let listener = TcpListener::bind(config.address).context(BIND_ERROR)?;
    config.address = listener.local_addr()?;
    let proxy = Proxy::connect()?;
    let server = tiny_http::Server::from_listener(listener, None).map_err(|e| anyhow::anyhow!("{e}")).context(BIND_ERROR)?;
    eprintln!("Gateway listening at http://{}/mcp", config.address);
    let config = Arc::new(config);
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        let request = server.recv().context("The gateway listener failed.")?;
        if headers_named(request.headers(), "Content-Length").any(|value| {
            match value.parse::<u64>() {
                Ok(length) => length > MAX_REQUEST_BYTES,
                Err(e) => *e.kind() == std::num::IntErrorKind::PosOverflow,
            }
        }) {
            let _ = request.respond(empty(413));
            continue;
        }
        if active.load(Ordering::Relaxed) >= MAX_ACTIVE_REQUESTS {
            let _ = request.respond(reject(&Value::Null, 503, -32000, "Gateway busy"));
            continue;
        }
        active.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(RequestSlot(active.clone()));
        let (config, proxy) = (config.clone(), proxy.clone());
        std::thread::spawn(move || serve(request, &config, proxy, slot));
    }
}

#[cfg(test)]
mod tests;
