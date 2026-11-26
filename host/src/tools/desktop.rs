//! Tool implementations installed by the desktop host.
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};

use crate::mcp::gate::{Granted, ToolError};
use super::{screen, tunnel, work, Ctx, Output, Table};

pub const TABLE: Table = &[
    ("screenshot", screen::screenshot),
    ("wait_for", screen::wait_for),
    ("click", screen::click),
    ("move", screen::pointer_move),
    ("drag", screen::drag),
    ("scroll", screen::scroll),
    ("key", screen::key),
    ("type", screen::type_text),
    ("run", work::work),
    ("proc_start", work::work),
    ("proc_write", work::work),
    ("proc_read", work::work),
    ("proc_kill", work::work),
    ("proc_list", work::work),
    ("fs_list", work::work),
    ("fs_stat", work::work),
    ("fs_read", work::work),
    ("fs_write", work::work),
    ("fs_hash", work::work),
    ("fs_move", work::work),
    ("fs_delete", work::work),
    ("clipboard_get", work::work),
    ("clipboard_set", work::work),
    ("tunnel", tunnel),
    ("windows", work::work),
    ("window_act", work::work),
    ("app_list", work::work),
    ("app_open", work::work),
    ("app_quit", work::work),
    ("ui_tree", work::work),
    ("ui_act", work::work),
];

pub fn tunnel(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let port = args["port"].as_u64().filter(|p| (1..=65535).contains(p)).ok_or_else(|| ToolError::new("invalid_args", "port is 1 to 65535", "Pass the port a program listens on at the computer."))? as u16;
    if TcpStream::connect_timeout(&tunnel::place(port), Duration::from_secs(2)).is_err() {
        return Err(ToolError::new("not_found", format!("nothing listens on port {port} of the computer's loopback"), "Start the program first (run), then try again."));
    }
    ctx.tunnels.lock().unwrap().insert(port);
    Ok(Output::Data(json!({ "port": port })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_desktop_has_the_tools_of_the_batches_built_and_only_defined_ones() {
        const FINISHED: u32 = 3;
        let features = crate::platform::Features { desktop: Some(crate::platform::DESKTOP), ..Default::default() };
        let names: Vec<_> = features.tools().collect();
        for t in wire::tools::all() {
            assert!(t.batch > FINISHED || names.contains(&t.name.as_str()), "{} has no function", t.name);
        }
        for name in names {
            assert!(wire::tools::get(name).is_some(), "{name} is not in proto/tools.json");
        }
    }
}
