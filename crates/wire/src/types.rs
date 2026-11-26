//! Structs and value sets shared by several messages (`types` in `proto/messages.json`).

use std::collections::BTreeMap;

use super::{wire_struct, wire_values};

wire_struct! {
    /// A display attached to the host, in physical pixels of its virtual desktop.
    pub struct Display {
        "id" id: String,
        "x" x: i32,
        "y" y: i32,
        "w" w: i32,
        "h" h: i32,
        "primary" primary: bool,
    }
}

wire_struct! {
    /// Viewer constraints, carried with an offer (the control message `view` has the same fields).
    pub struct View {
        "w" w: u32,
        "h" h: u32,
        "maxW" max_w: u32,
        "maxH" max_h: u32,
        "fps" fps: u32,
    }
}

wire_struct! {
    /// The browser's RTCIceServer dictionary.
    pub struct IceServer {
        "urls" urls: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "username" username: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "credential" credential: Option<String>,
    }
}

wire_struct! {
    /// RTCIceCandidateInit.
    pub struct IceCandidate {
        "candidate" candidate: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "sdpMid" sdp_mid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "sdpMLineIndex" sdp_m_line_index: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "usernameFragment" username_fragment: Option<String>,
    }
}

wire_values! {
    pub enum AuthKind { "dk" => DeviceKey, "rs" => Resume }
}

wire_struct! {
    /// Proof that an offer comes from a trusted device: a device key signature over
    /// `deskway-connect-v3` or a resume token MAC over `deskway-resume-v3`, both with the nonce.
    pub struct OfferAuth {
        "k" k: AuthKind,
        "nonce" nonce: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "cred" cred: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "sig" sig: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "id" id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "mac" mac: Option<String>,
    }
}

wire_values! {
    /// Why a session ended or was refused: the one list, shared by signaling `bye` and the
    /// control channel's `end`.
    pub enum Reason {
        "left" => Left, "taken" => Taken, "ended" => Ended, "busy" => Busy, "failed" => Failed,
        "gone" => Gone, "offline" => Offline, "auth" => Auth, "unpaired" => Unpaired,
        "forbidden" => Forbidden, "permissions" => Permissions, "no_desktop" => NoDesktop,
        "pair" => Pair, "denied" => Denied, "full" => Full,
        "ai_off" => AiOff, "idle" => Idle,
    }
}

wire_struct! {
    /// The session holding this computer (established or still connecting), as the status and
    /// a `busy` refusal report it.
    pub struct Holder {
        "session" session: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "cred" cred: Option<String>,
        "name" name: String,
        "user" user: String,
        "established" established: bool,
        /// Unix seconds: since when it holds the computer.
        #[serde(skip_serializing_if = "Option::is_none")]
        "since" since: Option<u64>,
    }
}

wire_struct! {
    /// The host's signature over offer and answer (`deskway-answer-v1`).
    pub struct AnswerAuth {
        "sig" sig: String,
        "hk" hk: String,
    }
}

wire_struct! {
    pub struct VouchEntry {
        "cred" cred: String,
        "alg" alg: i64,
        "key" key: String,
        "name" name: String,
    }
}

wire_struct! {
    /// A new device vouching for the person's other devices (`deskway-vouch-v1`).
    pub struct Vouch {
        "list" list: Vec<VouchEntry>,
        "sig" sig: String,
    }
}

wire_struct! {
    /// A device asking to be added: `pair` carries the MAC, `pairask` does not.
    pub struct PairAuth {
        "cred" cred: String,
        "alg" alg: i64,
        "key" key: String,
        "name" name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "mac" mac: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "vouch" vouch: Option<Vouch>,
    }
}

wire_struct! {
    /// The host's key after pairing; the MAC only answers `pair`.
    pub struct PairedAuth {
        "hk" hk: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "mac" mac: Option<String>,
    }
}

wire_values! {
    pub enum LogOp { "add" => Add, "remove" => Remove, "policy" => Policy, "rename" => Rename }
}

wire_values! {
    /// What a controlling device is: a person's browser, or an AI client's `deskway`
    /// (docs/architecture.md, "Identity"). It is part of what its approver signs.
    pub enum Role { "browser" => Browser, "ai" => Ai }
}

wire_values! {
    /// What AI clients may do on a computer now, as the computer reports it: never allowed or
    /// off, allowed, or allowed but stopped at the computer until a newer switch.
    pub enum AiState { "off" => Off, "on" => On, "stopped" => Stopped }
}

impl Default for Role {
    /// Every device from before roles existed is a browser.
    fn default() -> Role {
        Role::Browser
    }
}

wire_struct! {
    /// A browser's signed choice for one computer: whether an AI may control it, and whether
    /// with administrator rights. Signed over `deskway-ai-v1`; the computer takes the newest it
    /// has seen, from a browser it trusts.
    pub struct AiSwitch {
        "host" host: String,
        "ai" ai: bool,
        "admin" admin: bool,
        "ts" ts: i64,
        "by" by: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "ad" ad: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "cd" cd: Option<String>,
        "sig" sig: String,
    }
}

wire_struct! {
    /// One entry of the account's signed device log.
    pub struct LogEntry {
        "op" op: LogOp,
        "cred" cred: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "alg" alg: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "key" key: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "name" name: Option<String>,
        "ts" ts: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        "by" by: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "ad" ad: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "cd" cd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "sig" sig: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "keys" keys: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "role" role: Option<Role>,
    }
}

wire_struct! {
    /// Modifier state, as `getModifierState` reports it.
    pub struct Mods {
        "shift" shift: bool,
        "ctrl" ctrl: bool,
        "alt" alt: bool,
        "meta" meta: bool,
    }
}

wire_struct! {
    /// Toggle key state.
    pub struct Locks {
        "caps" caps: bool,
        "num" num: bool,
        "scroll" scroll: bool,
    }
}

wire_struct! {
    /// A file: name and size.
    pub struct Item {
        "n" n: String,
        "s" s: u64,
    }
}

wire_values! {
    /// Clipboard format kinds (RDP CLIPRDR model).
    pub enum FormatKind { "text" => Text, "html" => Html, "image" => Image, "files" => Files }
}

wire_struct! {
    /// One clipboard format.
    pub struct Format {
        "kind" kind: FormatKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        "size" size: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "w" w: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "h" h: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "items" items: Option<Vec<Item>>,
    }
}

wire_values! {
    pub enum TransferKind { "files" => Files, "image" => Image, "text" => Text, "html" => Html }
}

wire_values! {
    /// Why a transfer failed (file channel `err`, IPC `FilesFailed`).
    pub enum TransferError {
        "big" => Big, "locked" => Locked, "io" => Io, "gone" => Gone,
        "unsupported" => Unsupported, "timeout" => Timeout, "size" => Size, "image" => Image,
    }
}

wire_values! {
    /// Why a clipboard request was not answered (`cberr`, IPC `ClipboardFailed`).
    pub enum ClipboardError {
        "cancelled" => Cancelled, "gone" => Gone, "denied" => Denied,
        "big" => Big, "unsupported" => Unsupported, "io" => Io,
    }
}

/// Key labels of a layout: `KeyboardEvent.code` -> label.
pub type Labels = BTreeMap<String, String>;
