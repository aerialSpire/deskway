//! The tools an AI client can call on a computer, as `proto/tools.json` defines them: the one
//! definition, read here at build time by both ends.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

const TOOLS: &str = include_str!("../../../proto/tools.json");

/// Whether a tool looks at or changes what the screen shows.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Screen {
    None,
    Read,
    Act,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Tool {
    pub name: String,
    pub batch: u32,
    pub screen: Screen,
    /// It works only with someone signed in at the computer.
    pub needs_user: bool,
    pub description: String,
    /// Its arguments, as a JSON Schema object.
    pub input: Value,
}

#[derive(Deserialize)]
struct Definition {
    version: u32,
    tools: Vec<Tool>,
}

fn definition() -> &'static Definition {
    static D: OnceLock<Definition> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(TOOLS).expect("proto/tools.json parses"))
}

/// The definition's version, which a computer announces with the tools it has.
pub fn version() -> u32 {
    definition().version
}

pub fn all() -> &'static [Tool] {
    &definition().tools
}

pub fn get(name: &str) -> Option<&'static Tool> {
    all().iter().find(|t| t.name == name)
}
