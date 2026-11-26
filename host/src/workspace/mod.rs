//! What an AI client does as the person signed in at the computer (docs/architecture.md,
//! "Workspace and process lifetime"): run programs and read their output, work with files, read and set the
//! clipboard's text, open and quit apps and move their windows, read and use their controls. One request type ([`Op`]) answered by one function ([`Workspace::call`]),
//! so carrying it to another process (the Windows user worker, `service::worker`) is carrying
//! that one type and its answer. `Op` is also the tools' arguments: its tags are the tool names
//! of `proto/tools.json` and its fields their arguments, so a tool call is read into one
//! directly (`tools::work`).
//!
//! Where it runs: in this process when this process is the person (the macOS app; a Windows host
//! started by hand, `--dev`), else in the user worker the Windows service starts as the person
//! signed in at the console.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(desktop_host)]
pub mod apps;
#[cfg(desktop_host)]
pub mod elements;
#[cfg(desktop_host)]
mod files;
#[cfg(desktop_host)]
mod local;
#[cfg(desktop_host)]
pub mod procs;
#[cfg(windows)]
pub mod remote;

#[cfg(desktop_host)]
pub use local::Local;

/// How long `run` waits by default.
const WAIT_DEFAULT: u64 = 30_000;

fn wait_default() -> u64 {
    WAIT_DEFAULT
}

/// Bytes one read returns at most: a larger file is read in parts.
pub(crate) const READ_MOST: u64 = 256 << 10;

fn read_default() -> u64 {
    READ_MOST
}

fn depth_default() -> u32 {
    12
}

fn nodes_default() -> usize {
    400
}

fn cols_default() -> u16 {
    120
}

fn rows_default() -> u16 {
    30
}

/// What `window_act` does to a window.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Focus,
    Minimize,
    Maximize,
    Restore,
    Close,
    Move,
}

/// What `ui_act` does to an element.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UiAction {
    /// What clicking it does (a button, a link, a menu item).
    Invoke,
    Toggle,
    Expand,
    Collapse,
    /// Chooses it (an item of a list, a tab, a radio button).
    Select,
    Focus,
    SetValue,
}

/// One request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Starts `command` through the system's shell and waits for it, at most `wait_ms`.
    Run {
        command: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "wait_default")]
        wait_ms: u64,
        /// As an administrator (`platform::start_program`).
        #[serde(default)]
        elevated: bool,
    },
    /// Starts `command` (a shell, when there is none) in a terminal of its own.
    ProcStart {
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default = "cols_default")]
        cols: u16,
        #[serde(default = "rows_default")]
        rows: u16,
        #[serde(default)]
        elevated: bool,
    },
    /// Types into a terminal's program: text, then keys by name.
    ProcWrite {
        id: u64,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        keys: Vec<String>,
    },
    /// What a program printed from `from` (bytes into its output) on, waiting at most
    /// `wait_ms` for some.
    ProcRead {
        id: u64,
        #[serde(default)]
        from: u64,
        #[serde(default)]
        wait_ms: u64,
    },
    ProcKill { id: u64 },
    ProcList,
    FsList { path: String },
    FsStat { path: String },
    /// `length` bytes from `offset`; text when they are UTF-8, else base64.
    FsRead {
        path: String,
        #[serde(default)]
        offset: u64,
        #[serde(default = "read_default")]
        length: u64,
    },
    /// Text, or base64 bytes; appended, or replacing the file.
    FsWrite {
        path: String,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        base64: Option<String>,
        #[serde(default)]
        append: bool,
    },
    FsHash { path: String },
    FsMove { from: String, to: String },
    FsDelete {
        path: String,
        #[serde(default)]
        recursive: bool,
    },
    ClipboardGet,
    ClipboardSet { text: String },
    Windows,
    WindowAct {
        id: u64,
        action: WindowAction,
        #[serde(default)]
        x: Option<i32>,
        #[serde(default)]
        y: Option<i32>,
        #[serde(default)]
        w: Option<i32>,
        #[serde(default)]
        h: Option<i32>,
    },
    AppList,
    AppOpen {
        name: String,
        #[serde(default)]
        args: Vec<String>,
    },
    AppQuit {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        force: bool,
    },
    UiTree {
        window: u64,
        #[serde(default = "depth_default")]
        depth: u32,
        #[serde(default = "nodes_default")]
        max_nodes: usize,
    },
    UiAct {
        window: u64,
        element: String,
        action: UiAction,
        #[serde(default)]
        value: Option<String>,
    },
}

/// What kind of failure: the code an AI client sees.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    NotFound,
    Denied,
    Exists,
    NotEmpty,
    InvalidArgs,
    Busy,
    NoUserSession,
    ElevationUnavailable,
    Io,
    Failed,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::NotFound => "not_found",
            Code::Denied => "denied",
            Code::Exists => "exists",
            Code::NotEmpty => "not_empty",
            Code::InvalidArgs => "invalid_args",
            Code::Busy => "busy",
            Code::NoUserSession => "no_user_session",
            Code::ElevationUnavailable => "elevation_unavailable",
            Code::Io => "io",
            Code::Failed => "failed",
        }
    }
}

/// Why a request was not carried out: a code, what happened, and what to do next.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorkError {
    pub code: Code,
    pub message: String,
    pub next: String,
}

impl WorkError {
    pub fn new(code: Code, message: impl Into<String>, next: impl Into<String>) -> WorkError {
        WorkError { code, message: message.into(), next: next.into() }
    }
}

pub type Answer = Result<Value, WorkError>;

/// Carries out requests as the person signed in.
pub trait Workspace: Send + Sync {
    fn call(&self, op: Op) -> Answer;
    /// Someone is signed in to carry requests out (the Windows worker is connected).
    fn available(&self) -> bool {
        true
    }
}
