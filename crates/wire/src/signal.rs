//! `signaling.host`: the host's WebSocket to the server.

use super::types::{AiState, AiSwitch, AnswerAuth, Display, Holder, IceCandidate, IceServer, LogEntry, OfferAuth, PairAuth, PairedAuth, Reason, View};
use super::wire_enum;

wire_enum! {
    /// Every message between host and server, both directions.
    pub enum Signal (tag "type") {
        "hello" => Hello {
            "version" version: u32,
            "caps" caps: Vec<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            "tools" tools: Option<Vec<String>>,
        },
        "status" => Status {
            "displays" displays: Vec<Display>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "holder" holder: Option<Holder>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "ai_session" ai: Option<Holder>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "ai_state" ai_state: Option<AiState>,
        },
        "thumbs" => Thumbs { "on" on: bool },
        "thumb" => Thumb { "screen" screen: String, "data" data: String },
        "devlog" => DeviceLog {
            "auth" auth: Vec<LogEntry>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "ai" ai: Option<AiSwitch>,
        },
        "account" => Account { "user" user: String, "display" display: String },
        "locate" => Locate { "probe" probe: String, "where" place: Vec<String> },
        "here" => Here { "probe" probe: String },
        "auditreq" => AuditRequest { "probe" probe: String, "query" query: serde_json::Value },
        "audit" => Audit { "probe" probe: String, "page" page: serde_json::Value },
        "challenge" => Challenge { "session" session: String },
        "nonce" => Nonce { "session" session: String, "nonce" nonce: String },
        "offer" => Offer {
            "session" session: String,
            "epoch" epoch: u32,
            "sdp" sdp: String,
            "ice" ice: Vec<IceServer>,
            "view" view: View,
            #[serde(skip_serializing_if = "Option::is_none")]
            "force" force: Option<bool>,
            "user" user: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            "auth" auth: Option<OfferAuth>,
        },
        "answer" => Answer { "session" session: String, "epoch" epoch: u32, "sdp" sdp: String, "auth" auth: AnswerAuth },
        "restart" => Restart { "session" session: String, "epoch" epoch: u32, "sdp" sdp: String },
        "candidate" => Candidate { "session" session: String, "candidate" candidate: IceCandidate },
        "bye" => Bye {
            "session" session: String,
            "reason" reason: Reason,
            #[serde(skip_serializing_if = "Option::is_none")]
            "by" by: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "same" same: Option<bool>,
            #[serde(skip_serializing_if = "Option::is_none")]
            "holder" holder: Option<Holder>,
        },
        "pair" => Pair { "session" session: String, "auth" auth: PairAuth, "user" user: String, "from" from: String },
        "pairask" => PairAsk { "session" session: String, "auth" auth: PairAuth, "user" user: String, "from" from: String },
        "paired" => Paired { "session" session: String, "auth" auth: PairedAuth },
    }
}

impl Signal {
    /// A plain `bye` with only its reason.
    pub fn bye(session: &str, reason: Reason) -> Signal {
        Signal::Bye { session: session.to_string(), reason, by: None, same: None, holder: None }
    }
}
