//! `computer_info`: what this computer is and what it allows now.

use serde_json::{json, Value};

use super::{Ctx, Output};
use crate::mcp::gate::{Granted, ToolError};
use crate::platform::Desk;

pub fn computer_info(_: &Granted, _: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let f = ctx.facts();
    Ok(Output::Data(json!({
        "name": f.name,
        "os": f.os,
        "deskway": f.version,
        "displays": f.displays,
        "desk": f.desk.map(Desk::as_str),
        "locked": f.desk == Some(Desk::Winlogon),
        "signed_in": f.user_session,
        "view_only": f.view_only,
        "person_controlling": f.person,
        "admin": f.admin.unwrap_or(false),
    })))
}
