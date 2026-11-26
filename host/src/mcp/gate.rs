//! The one checkpoint every tool call passes (docs/architecture.md, "The gate"). A tool runs
//! only with a [`Granted`], which only [`Gate::check`] makes; every call is recorded in the
//! audit log when it starts and when it ends, and a call refused here as one that failed at once.

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;

use super::Facts;
use crate::audit::{short, Audit, Record};
use crate::platform::Desk;
use wire::tools::{Screen, Tool};

/// Why a call was not carried out: a code the AI client can act on, what happened, and what to
/// do next.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
    pub next: String,
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>, next: impl Into<String>) -> ToolError {
        ToolError { code, message: message.into(), next: next.into() }
    }
}

/// Leave to run one tool. Its fields are private: only the gate makes one.
pub struct Granted {
    tool: &'static Tool,
    started: Instant,
}

impl Granted {
    pub fn tool(&self) -> &'static Tool {
        self.tool
    }

    /// The leave to call `name`, for tests of what tools do with it.
    #[cfg(test)]
    pub fn for_test(name: &str) -> Granted {
        Granted { tool: wire::tools::all().iter().find(|t| t.name == name).expect("a defined tool"), started: Instant::now() }
    }
}

/// One AI session's gate: who it is, for the audit log.
pub struct Gate {
    pub audit: Arc<Audit>,
    pub client: String,
    pub device: String,
    pub session: String,
}

impl Gate {
    /// In order: the tool exists, this computer has it, its arguments fit, someone is signed in
    /// if it needs that, and it does not act on a secure desktop (UAC and the like: an AI
    /// client never answers those; the input itself is held back where it is injected too).
    pub fn check(&self, name: &str, args: &Value, facts: &Facts, implemented: impl Fn(&str) -> bool) -> Result<Granted, ToolError> {
        self.record(name, "start", what_was_asked(name, args), None);
        let allowed = (|| {
            let tool = wire::tools::get(name).ok_or_else(|| ToolError::new("not_found", format!("no tool {name}"), "List the tools again."))?;
            if !implemented(name) {
                return Err(ToolError::new("unsupported", crate::i18n::tf("mcp.unsupported_tool", &[("tool", name.to_string())]), crate::i18n::t("mcp.unsupported_next")));
            }
            rules(tool, args, facts).map(|()| tool)
        })();
        match allowed {
            Ok(tool) => Ok(Granted { tool, started: Instant::now() }),
            Err(e) => {
                // Refused: a call that failed at once, why in the record.
                self.record(name, e.code, short(&e.message, 200), Some(0));
                Err(e)
            }
        }
    }

    /// The call is over: recorded with what came of it.
    pub fn done(&self, g: &Granted, outcome: &Result<String, ToolError>) {
        let (what, detail) = match outcome {
            Ok(summary) => ("ok", short(summary, 200)),
            Err(e) => (e.code, short(&e.message, 200)),
        };
        self.record(&g.tool.name, what, detail, Some(g.started.elapsed().as_millis() as u64));
    }

    fn record(&self, tool: &str, what: &str, detail: String, ms: Option<u64>) {
        self.audit.write(&Record { at: crate::auth::unix_now(), client: &self.client, device: &self.device, session: &self.session, tool, what, detail, ms });
    }
}

/// A call's arguments as recorded: what it was asked to do, never the content it carried.
/// File contents, the clipboard's text and what is typed (a password, perhaps) are recorded
/// by their size; any other long value too.
fn what_was_asked(tool: &str, args: &Value) -> String {
    let mut a = args.clone();
    if let Some(fields) = a.as_object_mut() {
        for (key, v) in fields.iter_mut() {
            let Some(s) = v.as_str() else { continue };
            let content = key == "base64" || (key == "text" && matches!(tool, "fs_write" | "clipboard_set" | "type"));
            if key == "base64" {
                *v = Value::from(format!("<{} bytes>", s.len() / 4 * 3));
            } else if content || s.chars().count() > 120 {
                *v = Value::from(format!("<{} characters>", s.chars().count()));
            }
        }
    }
    short(&a.to_string(), 200)
}

/// What the computer's state and the tool's own definition allow.
fn rules(tool: &Tool, args: &Value, facts: &Facts) -> Result<(), ToolError> {
    check_args(&tool.input, args).map_err(|e| ToolError::new("invalid_args", e, "Call it again with the arguments its schema describes."))?;
    if args["elevated"] == true {
        match facts.admin {
            None => return Err(ToolError::new("elevation_unavailable", crate::i18n::t("mcp.elevation_unavailable"), crate::i18n::t("mcp.elevation_next"))),
            Some(false) => return Err(ToolError::new("elevation_off", "AI clients may not use administrator rights on this computer", "A person turns on Allow administrator rights for it in My devices.")),
            Some(true) => {}
        }
    }
    if tool.needs_user && !facts.user_session {
        return Err(ToolError::new("no_user_session", "nobody is signed in at this computer", "Sign in at its sign-in screen first (screenshot, then type)."));
    }
    if tool.screen == Screen::Act && facts.view_only {
        return Err(ToolError::new("view_only", "this computer may be watched, not controlled", "Its owner runs it without --view-only for that."));
    }
    if tool.screen == Screen::Act && facts.desk == Some(Desk::Other) {
        return Err(ToolError::new("secure_desktop", "a secure prompt (such as UAC) is on the screen", "A person at the computer answers it; try again after."));
    }
    Ok(())
}

/// Whether `args` fit `schema`, as far as the tools' schemas go: an object, its required
/// properties there, no others when it says so, and each of the JSON type it names.
fn check_args(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(obj) = args.as_object() else { return Err("the arguments are not an object".into()) };
    let props = schema["properties"].as_object();
    for r in schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !obj.contains_key(r) {
            return Err(format!("{r} is missing"));
        }
    }
    for (k, v) in obj {
        let Some(p) = props.and_then(|p| p.get(k)) else {
            if schema["additionalProperties"] == false {
                return Err(format!("no argument {k}"));
            }
            continue;
        };
        let ok = match p["type"].as_str() {
            Some("string") => v.is_string(),
            Some("integer") => v.is_i64() || v.is_u64(),
            Some("number") => v.is_number(),
            Some("boolean") => v.is_boolean(),
            Some("array") => v.is_array(),
            Some("object") => v.is_object(),
            _ => true,
        };
        if !ok {
            return Err(format!("{k} should be {}", p["type"].as_str().unwrap_or("something else")));
        }
        if let Some(allowed) = p["enum"].as_array() {
            if !allowed.contains(v) {
                return Err(format!("{k} is not one of {}", Value::Array(allowed.clone())));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn arguments_fit_the_schema_or_say_why_not() {
        let schema = json!({ "type": "object", "properties": { "cmd": { "type": "string" }, "ms": { "type": "integer" }, "how": { "type": "string", "enum": ["a", "b"] } }, "required": ["cmd"], "additionalProperties": false });
        for (args, ok) in [
            (json!({ "cmd": "dir" }), true),
            (json!({ "cmd": "dir", "ms": 5, "how": "b" }), true),
            (json!({}), false),
            (json!({ "cmd": 1 }), false),
            (json!({ "cmd": "x", "ms": 1.5 }), false),
            (json!({ "cmd": "x", "how": "c" }), false),
            (json!({ "cmd": "x", "other": 1 }), false),
            (json!("cmd"), false),
        ] {
            assert_eq!(check_args(&schema, &args).is_ok(), ok, "{args}");
        }
    }

    /// Every rule of the gate, in its order (the first that fails is the answer).
    #[test]
    fn the_gate_lets_a_call_through_only_when_every_rule_holds() {
        let dir = std::env::temp_dir().join(format!("deskway-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let gate = Gate { audit: Arc::new(Audit::new(Some(dir.clone()))), client: "Claude".into(), device: "c1".into(), session: "s1".into() };
        let facts = |user: bool, desk: Option<Desk>| Facts { user_session: user, desk, admin: Some(false), ..Facts::default() };
        let all = |_: &str| true;
        let none = |_: &str| false;
        // Tools made up here: the rules read only their flags and schema.
        let tool = |screen, needs_user| Tool { name: "t".into(), batch: 1, screen, needs_user, description: "x".into(), input: json!({ "type": "object" }) };
        // (tool, signed in, desktop, the refusal)
        let cases = [
            (tool(Screen::Act, false), true, Some(Desk::Default), None),
            (tool(Screen::Act, false), true, Some(Desk::Winlogon), None),
            (tool(Screen::Act, false), false, None, None),
            (tool(Screen::Act, false), true, Some(Desk::Other), Some("secure_desktop")),
            (tool(Screen::Read, false), true, Some(Desk::Other), None),
            (tool(Screen::None, true), true, Some(Desk::Other), None),
            (tool(Screen::None, true), false, Some(Desk::Winlogon), Some("no_user_session")),
            (tool(Screen::Act, true), false, Some(Desk::Other), Some("no_user_session")),
        ];
        for (t, signed_in, desk, refusal) in cases {
            assert_eq!(rules(&t, &json!({}), &facts(signed_in, desk)).err().map(|e| e.code), refusal, "{:?} needs_user {} on {desk:?}, signed in {signed_in}", t.screen, t.needs_user);
        }
        let watched = Facts { view_only: true, user_session: true, ..Facts::default() };
        assert_eq!(rules(&tool(Screen::Act, false), &json!({}), &watched).err().map(|e| e.code), Some("view_only"), "a view-only computer takes no input");
        assert!(rules(&tool(Screen::Read, false), &json!({}), &watched).is_ok(), "but may be looked at");
        // Administrator rights only where they are allowed.
        let elevated = json!({ "elevated": true });
        for (admin, refusal) in [(None, Some("elevation_unavailable")), (Some(false), Some("elevation_off")), (Some(true), None)] {
            assert_eq!(rules(&tool(Screen::None, false), &elevated, &Facts { admin, ..Facts::default() }).err().map(|e| e.code), refusal);
        }
        assert!(rules(&tool(Screen::None, false), &json!({ "elevated": false }), &Facts::default()).is_ok());
        // And through the gate, with a real tool.
        let code = |name: &str, args: Value, f: Facts, imp: &dyn Fn(&str) -> bool| gate.check(name, &args, &f, imp).err().map(|e| e.code);
        assert_eq!(code("no_such_tool", json!({}), facts(true, None), &all), Some("not_found"));
        assert_eq!(code("computer_info", json!({}), facts(true, None), &none), Some("unsupported"));
        assert_eq!(code("computer_info", json!({ "x": 1 }), facts(true, None), &all), Some("invalid_args"));
        assert_eq!(code("computer_info", json!({}), facts(false, Some(Desk::Other)), &all), None, "it needs nobody signed in and acts on nothing");
        let g = gate.check("computer_info", &json!({}), &facts(true, None), all).unwrap();
        gate.done(&g, &Ok("Desk".into()));
        let log = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
        let whats: Vec<String> = log.lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["what"].as_str().unwrap().to_string()).collect();
        assert_eq!(whats.iter().rev().take(2).collect::<Vec<_>>(), ["ok", "start"], "each call that passes is recorded at both ends");
        // A call refused here is recorded too, as one that failed at once with its reason.
        let _ = gate.check("run", &json!({ "command": "x", "elevated": true }), &facts(true, None), all);
        let calls = gate.audit.calls(&crate::audit::Query { failed: true, limit: 50, ..crate::audit::Query::default() }).calls;
        assert!(calls.iter().any(|c| c.tool == "run" && c.result == "elevation_off" && c.ms == Some(0) && c.args.contains("elevated")), "{calls:?}");
    }

    /// (tool, arguments) -> what is recorded of them: the content a call carries, by its size.
    #[test]
    fn what_a_call_carries_is_recorded_by_its_size() {
        let long = "x".repeat(150);
        for (tool, args, want) in [
            ("fs_write", json!({ "path": "a.txt", "base64": "AAAAAAAA" }), r#"{"base64":"<6 bytes>","path":"a.txt"}"#.to_string()),
            ("fs_write", json!({ "path": "a.txt", "text": "secret" }), r#"{"path":"a.txt","text":"<6 characters>"}"#.into()),
            ("clipboard_set", json!({ "text": "密码" }), r#"{"text":"<2 characters>"}"#.into()),
            ("type", json!({ "text": "hunter2" }), r#"{"text":"<7 characters>"}"#.into()),
            ("proc_write", json!({ "id": 3, "text": "npm test" }), r#"{"id":3,"text":"npm test"}"#.into()),
            ("run", json!({ "command": long }), r#"{"command":"<150 characters>"}"#.into()),
            ("run", json!({ "command": "dir" }), r#"{"command":"dir"}"#.into()),
        ] {
            assert_eq!(what_was_asked(tool, &args), want, "{tool} {args}");
        }
    }
}
