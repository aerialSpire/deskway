//! An AI client's workspace runs with the local user's rights. Requests use the tool names
//! and arguments from proto/tools.json. The Windows user worker carries the same requests
//! over its pipe; Linux and macOS run them in-process.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(desktop_host)]
pub mod apps;
#[cfg(desktop_host)]
pub mod elements;
#[cfg(desktop_host)]
mod desktop;
mod files;
mod local;
pub mod procs;
#[cfg(windows)]
pub mod remote;

pub use local::Local;
#[cfg(desktop_host)]
pub use desktop::{DesktopOp, UiAction, WindowAction};

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

/// A local filesystem operation and its resolved path for the host's journal.
pub type BrowseFiles = fn(wire::browse::Browse, &dyn Fn() -> bool) -> (PathBuf, wire::browse::Browse);

pub use file_transfer::{Conflict, Download, FileTransfers, Upload};
mod file_transfer;

#[cfg(target_os = "linux")]
pub(crate) use files::transfer::sweep_home;

/// Carries out requests with the workspace user's rights.
pub trait Workspace: Send + Sync {
    fn call(&self, op: Op) -> Answer;

    fn browse_files(&self) -> Option<BrowseFiles> {
        None
    }

    fn file_transfers(&self) -> Option<FileTransfers> {
        None
    }

    #[cfg(desktop_host)]
    fn desktop(&self, op: DesktopOp) -> Answer;

    /// The workspace can carry requests out (the Windows worker is connected).
    fn available(&self) -> bool {
        true
    }
}
