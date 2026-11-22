//! The AI role: an AI client's deskway command on the AI seat (docs/architecture.md, "AI control"). No
//! media: a "control" channel (`Control`: hello and end, as on the desk seat's control channel)
//! and an "ai" channel with the client's MCP requests in frames (`wire::mcp`). The requests are
//! answered off this thread (`crate::mcp`); this role only carries them, and ends the session
//! when the client has called nothing for `idle` (`idle`, the one place that decides it). A
//! channel named `tunnel:<port>` is a connection to that port of this computer's loopback, for
//! a port the client's `tunnel` call allowed (`tools::tunnel`); its bytes count as use.

use std::net::TcpStream;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// How long an AI session may go without a tool call before its seat is given up. The client's
/// deskway connects again on its next call, without asking anyone.
pub const IDLE: Duration = Duration::from_secs(10 * 60);

pub struct Ai {
    id: String,
    platform: Arc<dyn Platform>,
    ai: Option<ChannelId>,
    joiner: Joiner,
    /// Where requests go to be answered (`crate::mcp::spawn`).
    requests: Sender<String>,
    last_call: Instant,
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
    pub fn new(session: String, platform: Arc<dyn Platform>, requests: Sender<String>, idle: Duration, allowed: Allowed, own: Sender<SessionEvent>) -> Ai {
        Ai { id: session, platform, ai: None, joiner: Joiner::default(), requests, last_call: Instant::now(), idle, allowed, pipes: Pipes::default(), own, outbox: Outbox::default() }
    }

    /// A tunnel's channel: joined to a new connection to its port, if the client may reach it.
    fn tunnel(&mut self, cx: &mut Cx, id: ChannelId, port: u16) {
        let joined = self.allowed.lock().unwrap().contains(&port) && TcpStream::connect_timeout(&tunnel::place(port), Duration::from_secs(2)).ok().is_some_and(|s| self.pipes.add(id, s, self.own.clone()).is_ok());
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
        Box::new(move || platform.release_all(&id))
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
            self.last_call = Instant::now();
            return false;
        }
        if self.ai != Some(d.id) || !cx.active() {
            return false;
        }
        match self.joiner.push(&d.data) {
            Ok(Some(text)) => {
                if text.contains("\"tools/call\"") {
                    self.last_call = Instant::now();
                }
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
    }

    fn pipe(&mut self, cx: &mut Cx, e: PipeEvent) {
        if matches!(e, PipeEvent::Data(..)) {
            self.last_call = Instant::now();
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
        let due = self.last_call + self.idle;
        if cx.active() && Instant::now() >= due {
            cx.log(format_args!("no tool call for {} s: the AI seat is given up", self.idle.as_secs()));
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
    use crate::platform::{Call, Forward};

    /// A computer that does nothing it is asked.
    struct Still;
    impl Forward for Still {
        fn forward(&self, _: Call) {}
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
            let ai = Ai::new("s1".into(), Arc::new(Still), requests, idle, Allowed::default(), std::sync::mpsc::channel().0);
            assert_eq!(super::super::tests::ended_by(ai, &["control", "ai"], Duration::from_secs(4)).as_deref(), want, "idle {idle:?}");
        }
    }
}
