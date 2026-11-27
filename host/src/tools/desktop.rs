//! Tools carried out in the signed-in user's desktop workspace.

use serde_json::Value;

use crate::mcp::gate::{Granted, ToolError};
use crate::workspace::DesktopOp;
use super::{Ctx, Output};

pub fn work(g: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let user = ctx.user_workspace()?;
    let op: DesktopOp = super::work::request(&g.tool().name, args.clone())?;
    user.desktop(op).map(Output::Data).map_err(super::work::failed)
}
