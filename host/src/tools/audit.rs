//! `audit_read`: what AI clients did on this computer, as calls (`crate::audit`), a page at a
//! time and narrowed as asked. Nothing can change or remove a record.

use serde_json::Value;

use super::{Ctx, Output};
use crate::audit::Query;
use crate::mcp::gate::{Granted, ToolError};

/// Calls in one answer: by default, and at most.
const DEFAULT: u64 = 50;
const MOST: u64 = 500;

pub fn audit_read(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let q = Query {
        since: args["since"].as_u64().unwrap_or(0),
        until: args["until"].as_u64(),
        tool: args["tool"].as_str().map(String::from),
        client: args["client"].as_str().map(String::from),
        failed: args["failed"].as_bool().unwrap_or(false),
        offset: args["offset"].as_u64().unwrap_or(0) as usize,
        limit: args["limit"].as_u64().unwrap_or(DEFAULT).clamp(1, MOST) as usize,
    };
    Ok(Output::Data(serde_json::to_value(ctx.audit.calls(&q)).expect("a page serializes")))
}
