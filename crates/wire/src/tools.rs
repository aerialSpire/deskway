//! The tool catalog in `proto/tools.json`, read by the host and the AI client.

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
    #[serde(flatten)]
    pub spec: ToolSpec,
    pub batch: u32,
    pub screen: Screen,
    /// It works only with someone signed in at the computer.
    pub needs_user: bool,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// Its arguments, as a JSON Schema object.
    pub input: Value,
    pub annotations: Value,
    /// The client must have access to its own machine to offer this tool.
    #[serde(default)]
    pub local_only: bool,
}

#[derive(Deserialize)]
struct Definition {
    version: u32,
    tools: Vec<Tool>,
    client_tools: Vec<ToolSpec>,
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
    all().iter().find(|t| t.spec.name == name)
}

/// The tools the client understands, including those it carries out itself.
pub fn catalog() -> impl Iterator<Item = &'static ToolSpec> {
    all().iter().map(|t| &t.spec).chain(&definition().client_tools)
}
