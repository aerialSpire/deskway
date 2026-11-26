//! deskway: lets AI clients (Claude Code and others that speak MCP) use the account's Deskway
//! computers (docs/architecture.md, "AI control").
//!
//!   deskway login --server URL --user NAME [--name NAME]   (no password: a browser approves)
//!   deskway logout [--forget]
//!   deskway mcp                       the MCP server an AI client starts (stdio)
//!   deskway computers                 the account's computers, and where AI is allowed
//!   deskway call COMPUTER TOOL [JSON] one tool call, as an AI client would make it
//!   deskway proxy                     the shared local proxy (started by `mcp` itself)
//!   deskway update                    replaces this program with the server's now (deskway mcp does it by itself)

mod account;
mod copy;
mod keys;
mod link;
mod mcp;
mod os;
mod proxy;
mod store;
mod update;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn this_machine() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    if host.is_empty() { "AI client".into() } else { format!("AI client · {host}") }
}

fn login(args: &[String]) -> Result<()> {
    let server = flag(args, "--server").context("--server URL is needed")?;
    let user = flag(args, "--user").context("--user NAME is needed")?;
    let name = flag(args, "--name").unwrap_or_else(this_machine);
    let a = account::login(&server, &user, &name, |code| {
        eprintln!("On a browser already signed in to this account, a request from \"{name}\" waits with the numbers {} {}.", &code[..3], &code[3..]);
        eprintln!("Approve it there if the numbers match. Waiting…");
    })?;
    eprintln!("Signed in to {} as {}. Allow AI control for each computer it may use, in My devices.", a.server, a.user);
    Ok(())
}

/// Asks as an MCP client would, without one: one request, its answer printed.
fn through_mcp(request: Value) -> Result<()> {
    let answer = mcp::answer(&request)?;
    println!("{}", serde_json::to_string_pretty(&answer["result"])?);
    if answer["result"]["isError"] == true {
        bail!("the call failed");
    }
    Ok(())
}

fn call(args: &[String]) -> Result<()> {
    let mut arguments: Value = match args.get(3) {
        Some(a) => serde_json::from_str(a).context("the arguments are not JSON")?,
        None => json!({}),
    };
    arguments["computer"] = json!(args[1]);
    through_mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": args[2], "arguments": arguments } }))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("login") => login(&args),
        Some("logout") => account::logout(has(&args, "--forget")),
        Some("mcp") => mcp::run(),
        Some("proxy") => proxy::run(),
        Some("update") => update::run(),
        Some("computers") => through_mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "computers", "arguments": {} } })),
        Some("call") if args.len() >= 3 => call(&args),
        _ => {
            eprintln!("{}", include_str!("main.rs").lines().skip(3).take_while(|l| l.starts_with("//!")).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("deskway: {e:#}");
        std::process::exit(1);
    }
}
