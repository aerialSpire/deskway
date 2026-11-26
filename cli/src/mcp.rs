//! The MCP tool catalog and dispatch shared by stdio and Streamable HTTP.

use anyhow::Result;
use serde_json::{json, Value};
use wire::tools::ToolSpec;

pub const INSTRUCTIONS: &str = "Each tool acts on one Deskway computer of the account: call `computers` first to see their names, which are online and where AI is allowed, then pass `computer` to the others. A computer allows AI only after a person turns on Allow AI control for it in My devices; errors say what to do next. Its person may be using it at the same time.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Local,
    Remote,
}

impl Access {
    fn allows(self, tool: &ToolSpec) -> bool {
        self == Self::Local || !tool.local_only
    }
}

fn tool_list(access: Access) -> Vec<Value> {
    wire::tools::catalog().filter(|t| access.allows(t)).map(|t| {
        let mut input = t.input.clone();
        if t.name != "computers" {
            input["properties"]["computer"] = json!({ "type": "string", "description": "The computer: its name or id, as `computers` lists it." });
            let mut required = input["required"].as_array().cloned().unwrap_or_default();
            required.insert(0, json!("computer"));
            input["required"] = Value::Array(required);
        }
        if t.name == "tunnel" {
            input["properties"]["local_port"] = json!({ "type": "integer", "description": "The port to listen on here (any free one by default)." });
        }
        json!({ "name": t.name, "description": t.description, "inputSchema": input, "annotations": t.annotations })
    }).collect()
}

pub fn server_info() -> Value {
    json!({ "name": "deskway", "version": env!("CARGO_PKG_VERSION") })
}

pub fn capabilities() -> Value {
    json!({ "tools": { "listChanged": false } })
}

pub fn initialize(id: &Value, version: &str) -> Value {
    reply(id, json!({ "protocolVersion": version, "capabilities": capabilities(), "serverInfo": server_info(), "instructions": INSTRUCTIONS }))
}

pub fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn reply(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn text_result(v: &Value) -> Value {
    json!({ "content": [{ "type": "text", "text": v.to_string() }], "structuredContent": { "result": v }, "isError": false })
}

/// The answer to one message; `None` for a notification.
pub fn handle(m: &Value, access: Access, ask: impl Fn(Value) -> Result<Value>) -> Option<Value> {
    let id = m.get("id")?.clone();
    let method = m["method"].as_str().unwrap_or_default();
    let fail = |e: anyhow::Error| reply(&id, crate::link::error_result("proxy", &format!("{e:#}"), "Check `deskway login`, and proxy.log beside its files."));
    Some(match method {
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(&id, json!({ "tools": tool_list(access) })),
        "tools/call" => {
            let name = m["params"]["name"].as_str().unwrap_or_default();
            if access == Access::Remote {
                match wire::tools::catalog().find(|tool| tool.name == name) {
                    Some(tool) if access.allows(tool) => {}
                    Some(_) => return Some(error(&id, -32602, &format!("{name} is available only through deskway mcp."))),
                    None => return Some(error(&id, -32602, "Unknown tool. Use tools/list to see available tools.")),
                }
            }
            let mut args = m["params"]["arguments"].clone();
            let computer = args["computer"].as_str().unwrap_or_default().to_string();
            match name {
                "computers" => ask(json!({ "op": "computers" })).map(|v| reply(&id, text_result(&v))).unwrap_or_else(fail),
                "release" => ask(json!({ "op": "release", "computer": computer })).map(|v| reply(&id, text_result(&v))).unwrap_or_else(fail),
                _ if computer.is_empty() => reply(&id, crate::link::error_result("invalid_args", "which computer: `computer` is missing", "Call computers, then pass one of their names.")),
                "tunnel" => {
                    // The computer allows the port first (its gate, its audit log); then this end
                    // listens for it.
                    let request = json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "tunnel", "arguments": { "port": args["port"] } } });
                    match ask(json!({ "op": "call", "computer": computer, "request": request })) {
                        Ok(a) if a["result"]["isError"] == true => a,
                        Ok(_) => match ask(json!({ "op": "tunnel", "computer": computer, "port": args["port"], "local_port": args["local_port"] })) {
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
                        let answer = ask(json!({ "op": "call", "computer": computer, "request": request })).map_err(|e| crate::copy::Fail::new("proxy", format!("{e:#}"), "Check `deskway login`."))?;
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
                    ask(json!({ "op": "call", "computer": computer, "request": request })).unwrap_or_else(fail)
                }
            }
        }
        _ => error(&id, -32601, "no such method"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_names_its_computer_and_the_two_of_its_own_are_there() {
        let tools = tool_list(Access::Local);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"computers") && names.contains(&"release") && names.contains(&"computer_info"));
        for t in tools.iter().filter(|t| t["name"] != "computers") {
            assert_eq!(t["inputSchema"]["required"][0], "computer", "{}", t["name"]);
            assert!(t["inputSchema"]["properties"]["computer"].is_object(), "{}", t["name"]);
        }
    }

    #[test]
    fn every_tool_has_a_title_and_a_behavior_hint() {
        for tool in tool_list(Access::Local) {
            let annotations = &tool["annotations"];
            assert!(annotations["title"].as_str().is_some_and(|t| !t.trim().is_empty()), "{}", tool["name"]);
            assert!(annotations["readOnlyHint"].is_boolean() || annotations["destructiveHint"].is_boolean(), "{}", tool["name"]);
            if tool["name"] == "run" {
                assert_eq!(annotations["destructiveHint"], true);
            }
        }
    }
}
