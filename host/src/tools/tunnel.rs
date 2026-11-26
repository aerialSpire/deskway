//! `tunnel`: lets this AI session's client reach one port of this computer's loopback, on data
//! channels named for it (`peer::pipe`, carried by `session::ai`). Only loopback, and only a
//! port something listens on now: the call is how the gate and the audit log see a tunnel.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::mcp::gate::{Granted, ToolError};
use super::{Ctx, Output};

/// The ports an AI session's client may reach: one set, which its tools add to and its session
/// reads when a channel opens.
pub type Allowed = Arc<Mutex<BTreeSet<u16>>>;

/// Where on this computer a tunnel's connections go.
pub fn place(port: u16) -> SocketAddr {
    (Ipv4Addr::LOCALHOST, port).into()
}

pub fn tunnel(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let port = args["port"].as_u64().filter(|p| (1..=65535).contains(p)).ok_or_else(|| ToolError::new("invalid_args", "port is 1 to 65535", "Pass the port a program listens on at the computer."))? as u16;
    if TcpStream::connect_timeout(&place(port), Duration::from_secs(2)).is_err() {
        return Err(ToolError::new("not_found", format!("nothing listens on port {port} of the computer's loopback"), "Start the program first (run), then try again."));
    }
    ctx.tunnels.lock().unwrap().insert(port);
    Ok(Output::Data(json!({ "port": port })))
}
