//! The AI role: an AI client's deskway command on the AI seat (docs/architecture.md, "AI control"). No
//! media: a "control" channel (`Control`: hello and end, as on the desk seat's control channel)
//! and an "ai" channel with the client's MCP requests in frames (`wire::mcp`). The requests are
//! answered off this thread (`crate::mcp`); `before_wait` alone decides when the seat is idle:
//! no tool call is open, and no call, reply or tunnel bytes have passed for `idle`. A
//! channel named `tunnel:<port>` is a connection to that port of this computer's loopback, for
//! a port the client's `tunnel` call allowed (`tools::tunnel`); its bytes count as use.

use std::collections::HashSet;
use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use str0m::channel::{ChannelData, ChannelId};
use str0m::Event;

use super::{Cx, Role, SessionEvent};
use crate::platform::Platform;
use crate::tools::tunnel::{self, Allowed};
use peer::outbox::Outbox;
use peer::pipe::{PipeEvent, Pipes};
use peer::state::{End, Signal};
use wire::control::Control;
use wire::mcp::{frames, Joiner};
use wire::types::Reason;

/// How long an unused AI seat is kept. The client's deskway connects again on its next call.
pub const IDLE: Duration = Duration::from_secs(10 * 60);

struct Use {
    open: HashSet<Value>,
    last: Instant,
}

impl Use {
    fn new(now: Instant) -> Self {
        Self { open: HashSet::new(), last: now }
    }

    fn request(&mut self, text: &str, now: Instant) {
        #[derive(serde::Deserialize)]
        struct Request {
            method: String,
            id: Value,
        }
        let Ok(request) = serde_json::from_str::<Request>(text) else { return };
        if request.method == "tools/call" {
            self.open.insert(request.id);
            self.last = now;
        }
    }

    fn reply(&mut self, id: &Value, now: Instant) {
        if self.open.remove(id) {
            self.last = now;
        }
    }

    fn tunnel(&mut self, now: Instant) {
        self.last = now;
    }

    fn due(&self, idle: Duration) -> Option<Instant> {
        self.open.is_empty().then(|| self.last + idle)
    }
}

pub struct Ai {
    id: String,
    platform: Option<Arc<dyn Platform>>,
    ai: Option<ChannelId>,
    joiner: Joiner,
    /// Where requests go to be answered (`crate::mcp::spawn`).
    requests: Sender<String>,
    usage: Use,
    idle: Duration,
    /// The ports the client may reach, and its connections to them.
    allowed: Allowed,
    pipes: Pipes,
    /// This session's own queue, where its connections' bytes arrive.
    own: Sender<SessionEvent>,
    /// Answers' frames the "ai" channel has not taken yet.
    outbox: Outbox,
}

impl Ai {
    pub fn new(session: String, platform: Option<Arc<dyn Platform>>, requests: Sender<String>, idle: Duration, allowed: Allowed, own: Sender<SessionEvent>) -> Ai {
        Ai { id: session, platform, ai: None, joiner: Joiner::default(), requests, usage: Use::new(Instant::now()), idle, allowed, pipes: Pipes::default(), own, outbox: Outbox::default() }
    }

    /// A tunnel's channel: joined to a new connection to its port, if the client may reach it.
    fn tunnel(&mut self, cx: &mut Cx, id: ChannelId, port: u16) {
        let allowed = self.allowed.lock().unwrap().contains(&port);
        let joined = allowed && TcpStream::connect_timeout(&tunnel::place(port), Duration::from_secs(2)).ok().is_some_and(|s| self.pipes.add(id, s, self.own.clone()).is_ok());
        if !joined {
            cx.log(format_args!("tunnel to port {port}: not allowed or nothing listens"));
            cx.rtc.direct_api().close_data_channel(id);
        }
    }
}

impl Drop for Ai {
    fn drop(&mut self) {
        self.pipes.close_all();
    }
}

impl Role for Ai {
    fn releaser(&self) -> Box<dyn Fn() + Send> {
        let (platform, id) = (self.platform.clone(), self.id.clone());
        Box::new(move || {
            if let Some(platform) = &platform {
                platform.release_all(&id);
            }
        })
    }

    fn accepts(&self, sdp: &str) -> bool {
        !has_media(sdp)
    }

    fn channel_open(&mut self, cx: &mut Cx, id: ChannelId, label: &str) -> bool {
        match label {
            "control" => true,
            "ai" => {
                self.ai = Some(id);
                false
            }
            other => {
                if let Some(port) = peer::pipe::port(other).filter(|_| cx.active()) {
                    self.tunnel(cx, id, port);
                }
                false
            }
        }
    }

    fn channel_data(&mut self, cx: &mut Cx, d: ChannelData) -> bool {
        if cx.control() == Some(d.id) {
            return match serde_json::from_slice::<Control>(&d.data) {
                Ok(Control::End { reason, .. }) => cx.signal(Signal::PeerEnded(reason)),
                _ => false,
            };
        }
        if self.pipes.from_channel(d.id, &d.data) {
            self.usage.tunnel(Instant::now());
            return false;
        }
        if self.ai != Some(d.id) || !cx.active() {
            return false;
        }
        match self.joiner.push(&d.data) {
            Ok(Some(text)) => {
                self.usage.request(&text, Instant::now());
                let _ = self.requests.send(text);
            }
            Ok(None) => {}
            Err(e) => cx.log(format_args!("ai channel: a message dropped ({e:?})")),
        }
        false
    }

    fn mcp_reply(&mut self, cx: &mut Cx, text: String) {
        let Some(ch) = self.ai else { return };
        for f in frames(&text) {
            self.outbox.send(cx.rtc, ch, f);
        }
        #[derive(serde::Deserialize)]
        struct Reply {
            id: Value,
        }
        if let Ok(reply) = serde_json::from_str::<Reply>(&text) {
            self.usage.reply(&reply.id, Instant::now());
        }
    }

    fn pipe(&mut self, cx: &mut Cx, e: PipeEvent) {
        if matches!(e, PipeEvent::Data(..)) {
            self.usage.tunnel(Instant::now());
        }
        self.pipes.to_channel(cx.rtc, e);
    }

    fn rtc_event(&mut self, _cx: &mut Cx, e: Event) {
        if let Event::ChannelClose(id) = e {
            self.pipes.channel_closed(id);
        }
    }

    fn after_output(&mut self, cx: &mut Cx) {
        self.outbox.flush(cx.rtc);
        self.pipes.drained(cx.rtc);
    }

    fn before_wait(&mut self, cx: &mut Cx, _queued: usize) -> Option<Instant> {
        let due = self.usage.due(self.idle)?;
        if cx.active() && Instant::now() >= due {
            cx.log(format_args!("AI seat unused for {} s: given up", self.idle.as_secs()));
            cx.signal(Signal::LocalEnd(End::plain(Reason::Idle)));
            return None;
        }
        Some(due)
    }
}

/// Whether an offer has an audio or video section: an AI session has data channels only.
pub fn has_media(sdp: &str) -> bool {
    sdp.lines().any(|l| l.starts_with("m=audio") || l.starts_with("m=video"))
}

/// The hello on the control channel: the AI client learns the computer's version.
pub fn hello() -> Control {
    Control::Hello { version: wire::PROTOCOL_VERSION, caps: vec![wire::caps::AI.to_string()] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seat_waits_for_all_tool_replies_and_counts_tunnel_bytes() {
        enum Event {
            Request(&'static str),
            Reply(Value),
            Tunnel,
        }
        use Event::*;

        let start = Instant::now();
        let mut usage = Use::new(start);
        assert_eq!(usage.due(IDLE), Some(start + IDLE));
        for (seconds, event, last) in [
            (1, Request(r#"{"id":0,"method":"ping","params":["tools/call"]}"#), Some(0)),
            (2, Request(r#"{"method":"tools/call"}"#), Some(0)),
            (3, Request(r#"{"id":1,"method":"tools/call"}"#), None),
            (4, Request(r#"{"id":"2","method":"tools/call"}"#), None),
            (5, Reply(0.into()), None),
            (605, Reply("2".into()), None),
            (606, Tunnel, None),
            (607, Reply(1.into()), Some(607)),
            (608, Reply(0.into()), Some(607)),
            (609, Tunnel, Some(609)),
        ] {
            let now = start + Duration::from_secs(seconds);
            match event {
                Request(text) => usage.request(text, now),
                Reply(id) => usage.reply(&id, now),
                Tunnel => usage.tunnel(now),
            }
            assert_eq!(usage.due(IDLE), last.map(|s| start + Duration::from_secs(s) + IDLE), "at {seconds} s");
        }
    }

    /// (E3) An AI client that calls nothing for `idle` gives the seat up, told `idle`; one
    /// within it keeps it.
    #[test]
    fn an_ai_session_with_no_tool_call_for_its_idle_time_gives_the_seat_up() {
        if peer::net::local_ips().is_empty() {
            return; // only loopback here: no path to connect on
        }
        for (idle, want) in [(Duration::from_millis(300), Some("idle")), (Duration::from_secs(60), None)] {
            let (requests, _heard) = std::sync::mpsc::channel();
            let ai = Ai::new("s1".into(), None, requests, idle, Allowed::default(), std::sync::mpsc::channel().0);
            assert_eq!(super::super::tests::ended_by(ai, &["control", "ai"], Duration::from_secs(4)).as_deref(), want, "idle {idle:?}");
        }
    }
}
