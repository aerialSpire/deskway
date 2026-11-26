//! The workspace tools (`run`, `proc_*`, `fs_*`, `clipboard_*`, `windows`, `window_act`,
//! `app_*`, `ui_tree`, `ui_act`): the call, read as the workspace request of the same name, carried out as the
//! person signed in (`workspace`).

use serde_json::Value;

use crate::tools::{Ctx, Output};
use crate::mcp::gate::{Granted, ToolError};
use crate::workspace::procs;
use crate::workspace::{Op, Workspace};

/// A tool call as the workspace request of its name.
fn request(name: &str, args: &Value) -> Result<Op, ToolError> {
    let mut request = args.clone();
    request["op"] = Value::from(name);
    serde_json::from_value(request).map_err(|e| ToolError::new("invalid_args", e.to_string(), "Check the tool's arguments."))
}

pub fn work(g: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    // The gate lets these through only with someone signed in, which is a workspace.
    let Some(user) = ctx.workspace.as_deref() else { return Err(ToolError::new("no_user_session", "nobody is signed in at this computer", "Sign in at its sign-in screen first.")) };
    let op = request(&g.tool().name, args)?;
    let failed = |e: crate::workspace::WorkError| ToolError::new(e.code.as_str(), e.message, e.next);
    // Programs as an administrator, and the administrator's programs by their ids, are the
    // admin workspace's where there is one (Windows); elsewhere this one asks for the rights.
    let admin = ctx.admin.as_deref();
    let ws: &dyn Workspace = match &op {
        Op::Run { elevated: true, .. } | Op::ProcStart { elevated: true, .. } => admin.unwrap_or(user),
        Op::ProcRead { id, .. } | Op::ProcWrite { id, .. } | Op::ProcKill { id } if *id >= procs::ADMIN_FIRST => admin.unwrap_or(user),
        Op::ProcList => {
            let mut all = user.call(Op::ProcList).map_err(failed)?;
            if let (Some(a), Some(list)) = (admin, all.as_array_mut()) {
                list.extend(a.call(Op::ProcList).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default());
            }
            return Ok(Output::Data(all));
        }
        _ => user,
    };
    ws.call(op).map(Output::Data).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers every request with whose it is.
    struct Named(&'static str);
    impl Workspace for Named {
        fn call(&self, op: Op) -> crate::workspace::Answer {
            Ok(if op == Op::ProcList { serde_json::json!([self.0]) } else { serde_json::json!(self.0) })
        }
    }

    /// What is asked as an administrator, and the administrator's programs by their ids, go to
    /// the admin workspace where there is one; the rest to the person's; proc_list is both.
    #[test]
    fn requests_go_to_the_person_or_their_administrator() {
        let admin_id = procs::ADMIN_FIRST + 3;
        let cases = [
            ("run", serde_json::json!({ "command": "x" }), "user", "user"),
            ("run", serde_json::json!({ "command": "x", "elevated": true }), "admin", "user"),
            ("proc_start", serde_json::json!({ "elevated": true }), "admin", "user"),
            ("proc_read", serde_json::json!({ "id": 3 }), "user", "user"),
            ("proc_read", serde_json::json!({ "id": admin_id }), "admin", "user"),
            ("proc_kill", serde_json::json!({ "id": admin_id }), "admin", "user"),
            ("proc_write", serde_json::json!({ "id": admin_id, "text": "x" }), "admin", "user"),
            ("fs_list", serde_json::json!({ "path": "x" }), "user", "user"),
        ];
        for (name, args, with_admin, without) in cases {
            for (admin, want) in [(true, with_admin), (false, without)] {
                let ctx = test_ctx(admin);
                let g = crate::mcp::gate::Granted::for_test(name);
                let Ok(Output::Data(v)) = work(&g, &args, &ctx) else { panic!("{name}") };
                assert_eq!(v, serde_json::json!(want), "{name} {args}, admin workspace {admin}");
            }
        }
        let g = crate::mcp::gate::Granted::for_test("proc_list");
        let Ok(Output::Data(v)) = work(&g, &serde_json::json!({}), &test_ctx(true)) else { panic!() };
        assert_eq!(v, serde_json::json!(["user", "admin"]));
    }

    fn test_ctx(admin: bool) -> Ctx {
        struct Still;
        impl crate::platform::Forward for Still {
            fn forward(&self, _: crate::platform::Call) {}
        }
        Ctx {
            facts: Default::default(),
            platform: Some(std::sync::Arc::new(Still)),
            session: "s1".into(),
            replies: Default::default(),
            shot: Default::default(),
            workspace: Some(std::sync::Arc::new(Named("user"))),
            tunnels: Default::default(),
            audit: std::sync::Arc::new(crate::audit::Audit::new(None)),
            admin: admin.then(|| std::sync::Arc::new(Named("admin")) as std::sync::Arc<dyn Workspace>),
        }
    }

    /// Every workspace tool's least call (its required arguments) reads as the request of its
    /// name, with the defaults filled in.
    #[test]
    fn each_workspace_tool_is_the_request_of_its_name() {
        let ours: Vec<&wire::tools::Tool> = wire::tools::all().iter().filter(|t| crate::tools::desktop::TABLE.iter().any(|(n, f)| *n == t.name && std::ptr::fn_addr_eq(*f, work as crate::tools::Tool))).collect();
        assert_eq!(ours.len(), 22);
        for t in ours {
            let mut args = serde_json::json!({});
            for r in t.input["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let p = &t.input["properties"][r];
                args[r] = if p["type"] == "integer" { 1.into() } else { p["enum"].get(0).cloned().unwrap_or_else(|| "x".into()) };
            }
            assert!(request(&t.name, &args).is_ok(), "{}: {args}", t.name);
        }
        assert_eq!(request("run", &serde_json::json!({ "command": "dir" })).unwrap(), Op::Run { command: "dir".into(), cwd: None, wait_ms: 30_000, elevated: false });
    }
}
