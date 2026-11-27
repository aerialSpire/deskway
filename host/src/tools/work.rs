//! Command and file tools. Requests run with the workspace user's rights.

use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::auth::ClientIdentity;
use crate::tools::{Ctx, Output};
use crate::mcp::gate::{Granted, ToolError};
use crate::workspace::procs;
use crate::workspace::{Op, Workspace};

/// A tool call as the workspace request of its name.
pub(super) fn request<T: DeserializeOwned>(name: &str, mut request: Value) -> Result<T, ToolError> {
    request["op"] = Value::from(name);
    serde_json::from_value(request).map_err(|e| ToolError::new("invalid_args", e.to_string(), "Check the tool's arguments."))
}

pub(super) fn failed(e: crate::workspace::WorkError) -> ToolError {
    ToolError::new(e.code.as_str(), e.message, e.next)
}

pub fn work(g: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    execute(request(&g.tool().name, args.clone())?, ctx)
}

pub fn start(g: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    execute(start_request(&g.tool().name, args.clone(), &ctx.client)?, ctx)
}

fn start_request(name: &str, mut args: Value, client: &ClientIdentity) -> Result<Op, ToolError> {
    args["starter"] = json!(client);
    request(name, args)
}

fn execute(op: Op, ctx: &Ctx) -> Result<Output, ToolError> {
    let user = ctx.user_workspace()?;
    // Programs as an administrator, and the administrator's programs by their ids, are the
    // admin workspace's where there is one (Windows); elsewhere this one asks for the rights.
    let admin = ctx.admin.as_deref();
    let ws: &dyn Workspace = match &op {
        Op::Run { elevated: true, .. } | Op::ProcStart { elevated: true, .. } => admin.unwrap_or(user),
        Op::ProcRead { id, .. } | Op::ProcWrite { id, .. } | Op::ProcKill { id } if *id >= procs::ADMIN_FIRST => admin.unwrap_or(user),
        Op::ProcList => {
            let mut all = programs(user)?;
            if let Some(admin) = admin {
                all.extend(programs(admin).unwrap_or_default());
            }
            let listed: Vec<_> = all.into_iter().map(|p| ListedProgram {
                info: p.info,
                mine: p.starter.device == ctx.client.device,
                started_by: p.starter.name,
            }).collect();
            return Ok(Output::Data(json!(listed)));
        }
        _ => user,
    };
    ws.call(op).map(Output::Data).map_err(failed)
}

fn programs(ws: &dyn Workspace) -> Result<Vec<procs::Record>, ToolError> {
    serde_json::from_value(ws.call(Op::ProcList).map_err(failed)?)
        .map_err(|e| ToolError::new("failed", format!("cannot read the program list: {e}"), "Call proc_list again."))
}

#[derive(Serialize)]
struct ListedProgram {
    #[serde(flatten)]
    info: procs::Info,
    mine: bool,
    started_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Features;

    /// Answers every request with whose it is.
    struct Named(&'static str, Value);
    impl Workspace for Named {
        fn call(&self, op: Op) -> crate::workspace::Answer {
            Ok(if op == Op::ProcList { self.1.clone() } else { json!(self.0) })
        }

        #[cfg(desktop_host)]
        fn desktop(&self, _: crate::workspace::DesktopOp) -> crate::workspace::Answer {
            Ok(json!(self.0))
        }
    }

    /// What is asked as an administrator, and the administrator's programs by their ids, go to
    /// the admin workspace where there is one; the rest to the person's; proc_list is both.
    #[test]
    fn requests_go_to_the_person_or_their_administrator() {
        let admin_id = procs::ADMIN_FIRST + 3;
        let features = Features { work: true, ..Default::default() };
        let cases = [
            ("run", json!({ "command": "x" }), "user", "user"),
            ("run", json!({ "command": "x", "elevated": true }), "admin", "user"),
            ("proc_start", json!({ "elevated": true }), "admin", "user"),
            ("proc_read", json!({ "id": 3 }), "user", "user"),
            ("proc_read", json!({ "id": admin_id }), "admin", "user"),
            ("proc_kill", json!({ "id": admin_id }), "admin", "user"),
            ("proc_write", json!({ "id": admin_id, "text": "x" }), "admin", "user"),
            ("fs_list", json!({ "path": "x" }), "user", "user"),
        ];
        for (name, args, with_admin, without) in cases {
            for (admin, want) in [(true, with_admin), (false, without)] {
                let ctx = test_ctx(admin);
                let g = crate::mcp::gate::Granted::for_test(name);
                let call = features.tool(name).unwrap();
                let Ok(Output::Data(v)) = call(&g, &args, &ctx) else { panic!("{name}") };
                assert_eq!(v, json!(want), "{name} {args}, admin workspace {admin}");
            }
        }
        let g = crate::mcp::gate::Granted::for_test("proc_list");
        let Ok(Output::Data(v)) = work(&g, &json!({}), &test_ctx(true)) else { panic!() };
        let list = v.as_array().unwrap();
        let expected = [(3, true, "Before rename", json!(10)), (admin_id, false, "Claude", Value::Null)];
        assert_eq!(list.len(), expected.len());
        for (program, (id, mine, name, running_for)) in list.iter().zip(expected) {
            assert_eq!(program["id"], id);
            assert_eq!(program["mine"], mine);
            assert_eq!(program["started_by"], name);
            assert_eq!(program["running_for"], running_for);
            assert!(program.get("starter").is_none());
            assert!(program.get("device").is_none());
        }
    }

    fn test_ctx(admin: bool) -> Ctx {
        struct Still;
        impl crate::platform::Forward for Still {
            fn forward(&self, _: crate::platform::Call) {}
        }
        let user_programs = json!([{
            "id": 3, "command": "x", "pid": 42, "terminal": false, "started": 100,
            "running": true, "exit_code": null, "output_bytes": 3, "running_for": 10,
            "starter": { "device": "c1", "name": "Before rename" }
        }]);
        let admin_programs = json!([{
            "id": procs::ADMIN_FIRST + 3, "command": "x", "pid": 43, "terminal": true, "started": 101,
            "running": false, "exit_code": 7, "output_bytes": 0,
            "starter": { "device": "c2", "name": "Claude" }
        }]);
        Ctx {
            facts: Default::default(),
            platform: Some(std::sync::Arc::new(Still)),
            client: std::sync::Arc::new(ClientIdentity { device: "c1".into(), name: "Claude".into() }),
            session: "s1".into(),
            replies: Default::default(),
            shot: Default::default(),
            workspace: Some(std::sync::Arc::new(Named("user", user_programs))),
            tunnels: Default::default(),
            audit: std::sync::Arc::new(crate::audit::Audit::new(None)),
            admin: admin.then(|| std::sync::Arc::new(Named("admin", admin_programs)) as std::sync::Arc<dyn Workspace>),
        }
    }

    /// Every workspace tool's least call (its required arguments) reads as the request of its
    /// name, with the defaults filled in.
    #[test]
    fn each_workspace_tool_is_the_request_of_its_name() {
        let starter = test_ctx(false).client;
        check_requests(work, request::<Op>);
        check_requests(start, |name, args| start_request(name, args, &starter));
        assert_eq!(start_request("run", json!({ "command": "dir" }), &starter).unwrap(),
            Op::Run { command: "dir".into(), cwd: None, wait_ms: 30_000, elevated: false, starter: (*starter).clone() });
    }

    #[cfg(desktop_host)]
    #[test]
    fn each_desktop_workspace_tool_is_the_request_of_its_name() {
        check_requests(crate::tools::desktop::work, request::<crate::workspace::DesktopOp>);
    }

    fn check_requests<T>(handler: crate::tools::Tool, request: impl Fn(&str, Value) -> Result<T, ToolError>) {
        for (_, name, _) in crate::tools::TOOLS.iter().filter(|(_, _, f)| std::ptr::fn_addr_eq(*f, handler)) {
            let t = &wire::tools::get(name).unwrap().spec;
            let mut args = json!({});
            for r in t.input["required"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let p = &t.input["properties"][r];
                args[r] = if p["type"] == "integer" { 1.into() } else { p["enum"].get(0).cloned().unwrap_or_else(|| "x".into()) };
            }
            assert!(request(&t.name, args.clone()).is_ok(), "{}: {args}", t.name);
        }
    }
}
