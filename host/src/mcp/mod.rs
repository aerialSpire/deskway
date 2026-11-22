//! The MCP side of an AI session (docs/architecture.md, "MCP and tools"): JSON-RPC 2.0
//! requests from the AI client's deskway command, answered off the session thread so a slow tool
//! never holds the connection up. The deskway command answers MCP's own setup (initialize, the
//! tool list for the client) itself; a computer answers:
//!
//! - `tools/list`: the tools this computer has, by name, with the definition's version;
//! - `tools/call`: one call, through the gate (`gate`) to its tool (`crate::tools`);
//! - `ping`.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, RwLock};

use serde::Serialize;
use serde_json::{json, Value};

use crate::display::DisplayInfo;
use crate::platform::Desk;
use crate::session::SessionEvent;
use crate::tools::{self, Ctx, Output};

pub mod gate;

use gate::{Gate, ToolError};

/// What the AI side knows of this computer. The host keeps it (one copy, updated whenever its
/// status changes); the gate and the tools read it.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Facts {
    /// The computer's name, as its account sees it.
    pub name: String,
    pub os: String,
    /// Deskway's version on it.
    pub version: String,
    pub displays: Vec<DisplayInfo>,
    /// The input desktop: the person's, the sign-in or lock screen, or a secure prompt.
    pub desk: Option<Desk>,
    /// A person controlling this computer now (their device's name), beside the AI client.
    pub person: Option<String>,
    /// AI clients may use administrator rights here.
    pub admin: bool,
    /// Someone is signed in at the computer: the session's workspace says, when read
    /// (`tools::Ctx::facts`).
    pub user_session: bool,
    /// The computer may be watched, not controlled (`deskway-host net --view-only`): no input.
    pub view_only: bool,
}

pub type SharedFacts = Arc<RwLock<Facts>>;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const NO_METHOD: i64 = -32601;

fn reply(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error(id: &Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// A tool's result as MCP carries it: its data as text (and structured), an image as an image;
/// a refusal as an error result, with its code and what to do next. Structured content must be
/// an object: a list (windows, app_list, proc_list) goes under `result`, as deskway's own tools'.
fn call_result(r: &Result<Output, ToolError>) -> Value {
    match r {
        Ok(Output::Data(v)) => {
            let structured = if v.is_object() { v.clone() } else { json!({ "result": v }) };
            json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": structured, "isError": false })
        }
        Ok(Output::Image { jpeg, about }) => {
            use base64::Engine;
            let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
            json!({ "content": [{ "type": "image", "data": data, "mimeType": "image/jpeg" }, { "type": "text", "text": about.to_string() }], "structuredContent": about, "isError": false })
        }
        Err(e) => json!({ "content": [{ "type": "text", "text": format!("{}: {} Next: {}", e.code, e.message, e.next) }], "structuredContent": { "error": e.code, "message": e.message, "next": e.next }, "isError": true }),
    }
}

/// The answer to a call, if it fits one message (`wire::mcp::MAX_MESSAGE`); else what to ask
/// instead, so the AI client is not left waiting for an answer that cannot come.
fn fitting(id: &Value, result: &Result<Output, ToolError>) -> String {
    let text = reply(id, call_result(result));
    if text.len() <= wire::mcp::MAX_MESSAGE {
        return text;
    }
    let e = ToolError::new("too_large", format!("the answer is {} bytes; a message is at most {}", text.len(), wire::mcp::MAX_MESSAGE), "Ask for less at once: a smaller length, max_width or folder.");
    reply(id, call_result(&Err(e)))
}

/// One-line summary of a result, for the audit log.
fn summary(o: &Output) -> String {
    match o {
        Output::Data(v) => v.to_string(),
        Output::Image { jpeg, about } => format!("{} bytes JPEG {about}", jpeg.len()),
    }
}

/// Answers one message. `None` for a notification, or for a call answered later from its own
/// thread (`spawn` sends that answer to `out`).
fn handle(text: &str, gate: &Arc<Gate>, ctx: &Arc<Ctx>, out: &Sender<SessionEvent>) -> Option<String> {
    let Ok(m) = serde_json::from_str::<Value>(text) else { return Some(error(&Value::Null, PARSE_ERROR, "not JSON")) };
    let id = m.get("id").cloned();
    let method = m["method"].as_str();
    let (Some(id), Some(method)) = (id, method) else {
        // A notification (no id) needs no answer; anything else is not a request.
        return m.get("id").map(|id| error(id, INVALID_REQUEST, "not a request"));
    };
    match method {
        "ping" => Some(reply(&id, json!({}))),
        "tools/list" => Some(reply(&id, json!({ "version": wire::tools::version(), "tools": tools::implemented() }))),
        "tools/call" => {
            let (name, args) = (m["params"]["name"].as_str().unwrap_or_default().to_string(), m["params"].get("arguments").cloned().unwrap_or(json!({})));
            let (gate, ctx, out) = (gate.clone(), ctx.clone(), out.clone());
            let spawned = std::thread::Builder::new().name("tool".into()).spawn(move || {
                let facts = ctx.facts();
                let result = gate.check(&name, &args, &facts, tools::has).and_then(|g| {
                    let r = tools::call(&g, &args, &ctx);
                    gate.done(&g, &r.as_ref().map(summary).map_err(Clone::clone));
                    r
                });
                let _ = out.send(SessionEvent::Mcp(fitting(&id, &result)));
            });
            spawned.err().map(|_| error(&m["id"], -32000, "could not start the call"))
        }
        _ => Some(error(&id, NO_METHOD, "no such method")),
    }
}

/// Serves an AI session's requests: each message given to the returned sender is answered
/// through `out` (`SessionEvent::Mcp`), in its own time.
pub fn spawn(gate: Gate, ctx: Ctx, out: Sender<SessionEvent>) -> Sender<String> {
    let (tx, rx) = channel::<String>();
    let (gate, ctx) = (Arc::new(gate), Arc::new(ctx));
    std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            for text in rx {
                if let Some(answer) = handle(&text, &gate, &ctx, &out) {
                    let _ = out.send(SessionEvent::Mcp(answer));
                }
            }
        })
        .expect("spawn mcp thread");
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::Audit;
    use std::time::Duration;

    fn server() -> (Sender<String>, std::sync::mpsc::Receiver<SessionEvent>) {
        let gate = Gate { audit: Arc::new(Audit::new(None)), client: "Claude".into(), device: "c1".into(), session: "s1".into() };
        let facts = Facts { name: "Desk".into(), os: "windows".into(), ..Facts::default() };
        struct Nothing;
        impl crate::platform::Forward for Nothing {
            fn forward(&self, _: crate::platform::Call) {}
        }
        let ctx = Ctx { facts: Arc::new(RwLock::new(facts)), platform: Arc::new(Nothing), session: "s1".into(), replies: Arc::default(), shot: Default::default(), workspace: None, tunnels: Default::default(), admin: None, audit: Arc::new(Audit::new(None)) };
        let (out, rx) = channel();
        (spawn(gate, ctx, out), rx)
    }

    fn answer(rx: &std::sync::mpsc::Receiver<SessionEvent>) -> Value {
        match rx.recv_timeout(Duration::from_secs(10)).expect("an answer") {
            SessionEvent::Mcp(t) => serde_json::from_str(&t).unwrap(),
            _ => panic!("not an MCP answer"),
        }
    }

    /// MCP clients take structured content only as an object: a list is put under `result`.
    #[test]
    fn structured_content_is_always_an_object() {
        for (data, want) in [(json!({ "a": 1 }), json!({ "a": 1 })), (json!([{ "id": 1 }]), json!({ "result": [{ "id": 1 }] })), (json!("x"), json!({ "result": "x" }))] {
            let r = call_result(&Ok(Output::Data(data.clone())));
            assert_eq!((&r["structuredContent"], r["content"][0]["text"].as_str()), (&want, Some(data.to_string().as_str())));
        }
    }

    /// An answer that cannot fit one message is a too_large error, never none.
    #[test]
    fn an_answer_too_large_to_send_says_so() {
        for (n, error) in [(1000, None), (wire::mcp::MAX_MESSAGE, Some("too_large"))] {
            let a: Value = serde_json::from_str(&fitting(&json!(7), &Ok(Output::Data(json!("x".repeat(n)))))).unwrap();
            assert_eq!((a["id"].as_i64(), a["result"]["structuredContent"]["error"].as_str()), (Some(7), error), "{n} bytes");
        }
    }

    #[test]
    fn requests_are_answered_and_calls_go_through_the_gate() {
        let (tx, rx) = server();
        // (request, what the answer holds)
        let cases: [(&str, fn(&Value) -> bool); 6] = [
            (r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, |a| a["result"] == json!({})),
            (r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, |a| a["result"]["tools"].as_array().is_some_and(|t| t.contains(&json!("computer_info")))),
            (r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"computer_info","arguments":{}}}"#, |a| a["result"]["structuredContent"]["name"] == "Desk" && a["result"]["isError"] == false),
            (r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nothing"}}"#, |a| a["result"]["isError"] == true && a["result"]["structuredContent"]["error"] == "not_found"),
            (r#"{"jsonrpc":"2.0","id":5,"method":"resources/list"}"#, |a| a["error"]["code"] == NO_METHOD),
            (r#"not json"#, |a| a["error"]["code"] == PARSE_ERROR),
        ];
        for (req, ok) in cases {
            tx.send(req.into()).unwrap();
            let a = answer(&rx);
            assert!(ok(&a), "{req} -> {a}");
        }
        // A notification gets no answer.
        tx.send(r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#.into()).unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
