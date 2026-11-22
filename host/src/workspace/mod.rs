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

pub mod apps;
pub mod elements;
mod files;
pub mod procs;
#[cfg(windows)]
pub mod remote;

/// How long `run` waits by default, and at most.
const WAIT_DEFAULT: u64 = 30_000;
const WAIT_MOST: u64 = 600_000;

fn wait_default() -> u64 {
    WAIT_DEFAULT
}

fn read_default() -> u64 {
    files::READ_MOST
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
        action: apps::WindowAction,
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
        action: elements::UiAction,
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

    fn io(path: &str, e: std::io::Error) -> WorkError {
        let (code, next) = match e.kind() {
            std::io::ErrorKind::NotFound => (Code::NotFound, "Check the path (fs_list its folder)."),
            std::io::ErrorKind::PermissionDenied => (Code::Denied, "The person signed in may not touch it; an administrator's tool may."),
            std::io::ErrorKind::AlreadyExists => (Code::Exists, "Choose another name, or delete it first."),
            std::io::ErrorKind::DirectoryNotEmpty => (Code::NotEmpty, "Pass recursive to delete it with what is in it."),
            _ => (Code::Io, "Try again, or another path."),
        };
        WorkError::new(code, format!("{path}: {e}"), next)
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

/// The workspace of this process, which is the person's.
#[derive(Default)]
pub struct Local {
    procs: procs::Table,
}

impl Local {
    /// One whose programs are numbered from `first` (the admin worker's, `procs::ADMIN_FIRST`).
    #[cfg(windows)]
    pub fn numbered_from(first: u64) -> Local {
        Local { procs: procs::Table::numbered_from(first) }
    }
}

impl Workspace for Local {
    fn call(&self, op: Op) -> Answer {
        match op {
            Op::Run { command, cwd, wait_ms, elevated } => self.procs.run(&command, cwd.as_deref(), wait_ms.min(WAIT_MOST), elevated),
            Op::ProcStart { command, cwd, cols, rows, elevated } => self.procs.start(command.as_deref(), cwd.as_deref(), cols, rows, elevated),
            Op::ProcWrite { id, text, keys } => self.procs.write(id, text.as_deref(), &keys),
            Op::ProcRead { id, from, wait_ms } => self.procs.read(id, from, wait_ms),
            Op::ProcKill { id } => self.procs.kill(id),
            Op::ProcList => Ok(self.procs.list()),
            Op::FsList { path } => files::list(&path),
            Op::FsStat { path } => files::stat(&path),
            Op::FsRead { path, offset, length } => files::read(&path, offset, length),
            Op::FsWrite { path, text, base64, append } => files::write(&path, text, base64, append),
            Op::FsHash { path } => files::hash(&path),
            Op::FsMove { from, to } => files::rename(&from, &to),
            Op::FsDelete { path, recursive } => files::delete(&path, recursive),
            Op::ClipboardGet => Ok(serde_json::json!({ "text": crate::platform::clipboard_text() })),
            Op::ClipboardSet { text } => {
                if crate::platform::set_clipboard_text(&text) {
                    Ok(serde_json::json!({ "done": "set" }))
                } else {
                    Err(WorkError::new(Code::Busy, "another program holds the clipboard", "Try again in a moment."))
                }
            }
            Op::Windows => apps::windows(),
            Op::WindowAct { id, action, x, y, w, h } => apps::window_act(id, action, x, y, w, h),
            Op::AppList => apps::apps(),
            Op::AppOpen { name, args } => apps::open(&name, &args),
            Op::AppQuit { name, pid, force } => apps::quit(name.as_deref(), pid, force),
            Op::UiTree { window, depth, max_nodes } => elements::tree(window, depth, max_nodes),
            Op::UiAct { window, element, action, value } => elements::act(window, &element, action, value.as_deref()),
        }
    }
}
