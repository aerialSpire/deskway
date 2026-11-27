//! One connection from this AI client to one computer (docs/session-lifecycle.md, the end that
//! starts a session; docs/architecture.md, "The channel"). It asks for a nonce, offers data
//! channels only ("control" and "ai"), signs the offer with the device key, checks the
//! computer's signed answer against the host key pinned for it, and then carries MCP requests
//! in frames until the session ends. The network paths and the state machine are the ones the
//! computer uses (`peer`); restarting ICE is this end's, as it is the page's. A tunnel listens
//! on this machine's loopback and carries each connection on a channel of its own
//! (`peer::pipe`) to the port the computer allowed; it lasts as long as the link.

use std::collections::{HashMap, VecDeque};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey;
use peer::net::Net;
use peer::outbox::Outbox;
use peer::pipe::{PipeEvent, Pipes};
use peer::state::{ice_signal, Action, End, Machine, Signal};
use peer::NetEvent;
use serde_json::{json, Value};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId};
use str0m::{Candidate, Event, Input, Output, Rtc};
use wire::control::Control;
use wire::mcp::{frames, Joiner};
use wire::types::Reason;

use crate::keys::DeviceKey;

/// What a link hears.
pub enum LinkEvent {
    Net(NetEvent),
    /// A signaling message for this session, from the server.
    Signal(Value),
    /// An MCP request (JSON-RPC text without its id) and where its answer goes.
    Call { request: Value, reply: Sender<Value>, deadline: Option<Instant> },
    /// Give the computer up (release, or the proxy quitting).
    Leave,
    /// Listen on this machine's port `local` (any, when 0) for the computer's port `port`,
    /// which it allowed; the answer says where.
    Tunnel { port: u16, local: u16, reply: Sender<Value> },
    /// A connection to a tunnel's local port, for the computer's port `port`.
    Accepted { port: u16, socket: TcpStream },
    /// A tunnel's connection gave bytes, or closed.
    Pipe(PipeEvent),
}

impl From<NetEvent> for LinkEvent {
    fn from(e: NetEvent) -> LinkEvent {
        LinkEvent::Net(e)
    }
}

impl From<PipeEvent> for LinkEvent {
    fn from(e: PipeEvent) -> LinkEvent {
        LinkEvent::Pipe(e)
    }
}

/// What starts a link.
pub struct Params {
    /// The computer's id, which signaling and the pinned key go by.
    pub computer: String,
    /// Its name, which messages say.
    pub name: String,
    pub session: String,
    pub ice: Value,
    /// Signaling messages to send.
    pub out: Sender<String>,
    pub key: Arc<DeviceKey>,
    /// Told when the link is over, with why (`None`: this end left).
    pub ended: Sender<(String, Option<Reason>)>,
}

fn send(out: &Sender<String>, m: Value) {
    let _ = out.send(m.to_string());
}

/// Why calls on a link that ended get no answer, in words for the AI client.
pub fn refusal(reason: Option<Reason>, computer: &str) -> (&'static str, String, &'static str) {
    match reason {
        Some(Reason::AiOff) => ("ai_off", format!("AI control is not allowed on {computer} now: never turned on, turned off, or stopped there"), "A person turns on Allow AI control for it in My devices."),
        Some(Reason::Busy) => ("busy", format!("another AI client is using {computer}"), "Wait, or ask the person to stop the other AI client."),
        Some(Reason::Unpaired) => ("unpaired", format!("{computer} does not know this AI client"), "A browser of the account approves this AI client again (deskway login)."),
        Some(Reason::Auth) => ("auth", format!("{computer} did not accept this AI client's signature"), "Sign in again: deskway login."),
        Some(Reason::Offline) | Some(Reason::Gone) => ("offline", format!("{computer} is offline"), "Try again once it is online."),
        Some(Reason::Idle) => ("idle", format!("the seat on {computer} was given up after 10 minutes without a tool call. Programs started there keep running"), "Call any tool to connect again. Use proc_list to find programs and proc_read for their output and exit code."),
        Some(Reason::Forbidden) => ("forbidden", format!("{computer} refused this AI client"), "Check that the account owns it and AI is allowed on it."),
        Some(Reason::Permissions) => ("permissions", format!("{computer} cannot take a session now (its permissions)"), "A person at the computer grants Deskway what it asks for."),
        Some(Reason::Ended) | Some(Reason::Taken) => ("ended", format!("the session on {computer} was ended there"), "Call the tool again to connect again."),
        Some(r) => ("failed", format!("the connection to {computer} ended ({})", r.as_str()), "Try again; check this machine's network."),
        None => ("left", "the connection was given up here".into(), "Call the tool again."),
    }
}

/// An MCP error result, as a tool's would be.
pub fn error_result(code: &str, message: &str, next: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("{code}: {message} Next: {next}") }], "structuredContent": { "error": code, "message": message, "next": next }, "isError": true })
}

fn call_refusal(code: &str, message: &str, next: &str, sent: bool) -> Value {
    if sent {
        error_result(
            code,
            &format!("{message}. The call may have been carried out on the computer."),
            &format!("Programs it started keep running. Check proc_list (mine) before repeating the call. {next}"),
        )
    } else {
        error_result(code, message, next)
    }
}

/// The computer's signature over both descriptions holds, with its key `hk`.
fn answer_signed(host: &str, session: &str, offer: &str, answer: &str, hk: &str, sig: &str) -> bool {
    let (Ok(der), Ok(sig)) = (STANDARD.decode(hk), STANDARD.decode(sig)) else { return false };
    let (Ok(key), Ok(sig)) = (p256::ecdsa::VerifyingKey::from_public_key_der(&der), p256::ecdsa::Signature::from_slice(&sig)) else { return false };
    key.verify(wire::challenge::answer_v1(host, session, offer, answer).as_bytes(), &sig).is_ok()
}

struct Link {
    p: Params,
    machine: Machine,
    epoch: u32,
    /// The offer of the negotiation under way, and its description (to check the answer's
    /// signature).
    pending: Option<(SdpPendingOffer, String)>,
    control: ChannelId,
    ai: ChannelId,
    joiner: Joiner,
    /// Requests waiting for the session to be Active, and those sent, by the id given here.
    queued: VecDeque<(Value, Sender<Value>, Option<Instant>)>,
    /// The "ai" channel is open: requests can go (it opens after, or before, "control").
    ai_open: bool,
    waiting: HashMap<u64, (Value, Sender<Value>)>,
    /// Requests' frames the "ai" channel has not taken yet.
    outbox: Outbox,
    /// This link's own queue, where tunnels' connections arrive.
    own: Sender<LinkEvent>,
    /// Tunnels: their listeners' stops, connections whose channel is opening, and those joined.
    listeners: Vec<Arc<AtomicBool>>,
    opening: HashMap<ChannelId, TcpStream>,
    pipes: Pipes,
    next_id: u64,
    restarted: Option<Instant>,
    started: Instant,
}

impl Link {
    fn log(&self, what: std::fmt::Arguments) {
        eprintln!("[{} {:>5} ms] {what}", self.p.computer, self.started.elapsed().as_millis());
    }

    /// Feeds the state machine and carries out what it decides. True once over.
    fn signal(&mut self, rtc: &mut Rtc, s: Signal) -> bool {
        for a in self.machine.handle(s, Instant::now()) {
            match a {
                Action::SendEnd(End { reason, .. }) => {
                    let text = serde_json::to_vec(&Control::End { reason, by: None, same: None }).expect("end serializes");
                    if let Some(mut ch) = rtc.channel(self.control) {
                        let _ = ch.write(false, &text);
                    }
                }
                Action::SendBye(End { reason, .. }) => send(&self.p.out, json!({ "type": "bye", "session": self.p.session, "reason": reason })),
                Action::ReleaseInput | Action::Close => {}
            }
        }
        self.flush(rtc);
        self.machine.ended()
    }

    fn ready(&self) -> bool {
        self.ai_open && self.machine.active()
    }

    /// Requests held until the link was ready go now.
    fn flush(&mut self, rtc: &mut Rtc) {
        while self.ready() {
            let Some((req, reply, deadline)) = self.queued.pop_front() else { break };
            self.call(rtc, req, reply, deadline);
        }
    }

    fn call(&mut self, rtc: &mut Rtc, mut req: Value, reply: Sender<Value>, deadline: Option<Instant>) {
        self.next_id += 1;
        let id = self.next_id;
        let theirs = req.get("id").cloned().unwrap_or(Value::Null);
        req["id"] = json!(id);
        req["jsonrpc"] = json!("2.0");
        let text = req.to_string();
        if text.len() > wire::mcp::MAX_MESSAGE {
            let _ = reply.send(json!({ "jsonrpc": "2.0", "id": theirs, "result": error_result("too_large", &format!("the request is {} bytes; a message is at most {}", text.len(), wire::mcp::MAX_MESSAGE), "Send less at once (fs_write a file in parts, with append).") }));
            return;
        }
        let frames = frames(&text);
        if deadline.is_some_and(|until| Instant::now() >= until) {
            return;
        }
        for f in frames {
            self.outbox.send(rtc, self.ai, f);
        }
        self.waiting.insert(id, (theirs, reply));
    }

    /// An answer from the computer: back to whoever asked, with their id.
    fn answered(&mut self, text: &str) {
        let Ok(mut v) = serde_json::from_str::<Value>(text) else { return };
        let Some(id) = v["id"].as_u64() else { return };
        if let Some((theirs, reply)) = self.waiting.remove(&id) {
            v["id"] = theirs;
            let _ = reply.send(v);
        }
    }

    /// Listens here for a tunnel to the computer's `port`, on a thread that hands each
    /// connection to the link until the link ends.
    fn listen(&mut self, port: u16, local: u16, reply: Sender<Value>) {
        let answer = match TcpListener::bind(("127.0.0.1", local)).and_then(|l| l.set_nonblocking(true).map(|()| l)) {
            Ok(listener) => {
                let at = listener.local_addr().map_or(local, |a| a.port());
                let stop = Arc::new(AtomicBool::new(false));
                let (tx, stopped) = (self.own.clone(), stop.clone());
                std::thread::spawn(move || {
                    while !stopped.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok((socket, _)) => {
                                let _ = socket.set_nonblocking(false);
                                if tx.send(LinkEvent::Accepted { port, socket }).is_err() {
                                    break;
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                            Err(_) => break,
                        }
                    }
                });
                self.listeners.push(stop);
                self.log(format_args!("tunnel: 127.0.0.1:{at} here to port {port} there"));
                json!({ "port": port, "local_port": at, "connect_to": format!("127.0.0.1:{at}") })
            }
            Err(e) => error_result("local", &format!("cannot listen on port {local} here: {e}"), "Pick another local_port, or leave it out."),
        };
        let _ = reply.send(answer);
    }

    /// A connection to a tunnel: a channel of its own, joined once it opens.
    fn accepted(&mut self, rtc: &mut Rtc, port: u16, socket: TcpStream) {
        if self.machine.active() {
            let id = rtc.direct_api().create_data_channel(ChannelConfig { label: peer::pipe::label(port), ..Default::default() });
            self.opening.insert(id, socket);
        }
    }

    /// The link is over: every request it still holds gets why as its answer.
    fn refuse_all(&mut self, reason: Option<Reason>) {
        let (code, message, next) = refusal(reason, &self.p.name);
        let queued = call_refusal(code, &message, next, false);
        for (request, reply, _) in self.queued.drain(..) {
            let _ = reply.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": queued }));
        }
        let waiting = call_refusal(code, &message, next, true);
        for (_, (id, reply)) in self.waiting.drain() {
            let _ = reply.send(json!({ "jsonrpc": "2.0", "id": id, "result": waiting }));
        }
    }

    fn offer_candidate(&self, rtc: &mut Rtc, c: Candidate) -> bool {
        let Some(c) = rtc.add_local_candidate(c) else {
            return false;
        };
        let sdp = c.to_sdp_string();
        send(&self.p.out, json!({ "type": "candidate", "session": self.p.session, "candidate": { "candidate": sdp, "sdpMLineIndex": 0 } }));
        true
    }

    /// The path is lost: a new negotiation on the same connection, after finding this machine's
    /// addresses again.
    fn restart(&mut self, rtc: &mut Rtc, net: &mut Net<LinkEvent>) -> Result<()> {
        let fresh = net.rebind(rtc, Instant::now())?;
        for c in fresh {
            self.offer_candidate(rtc, c);
        }
        let mut api = rtc.sdp_api();
        api.ice_restart(true);
        let Some((offer, pending)) = api.apply() else { return Ok(()) };
        self.epoch += 1;
        let sdp = offer.to_sdp_string();
        self.pending = Some((pending, sdp.clone()));
        self.restarted = Some(Instant::now());
        self.log(format_args!("ICE restart {}", self.epoch));
        send(&self.p.out, json!({ "type": "restart", "session": self.p.session, "epoch": self.epoch, "sdp": sdp }));
        Ok(())
    }

    /// A signaling message for this session. True once over.
    fn on_signal(&mut self, rtc: &mut Rtc, m: Value) -> Result<bool> {
        match m["type"].as_str().unwrap_or_default() {
            "answer" if m["epoch"].as_u64() == Some(self.epoch as u64) => {
                let Some((pending, offer)) = self.pending.take() else { return Ok(false) };
                let (sdp, hk, sig) = (m["sdp"].as_str().unwrap_or_default(), m["auth"]["hk"].as_str().unwrap_or_default(), m["auth"]["sig"].as_str().unwrap_or_default());
                let pinned = crate::store::host_key(&self.p.computer)?;
                if pinned.as_deref().is_some_and(|k| k != hk) || !answer_signed(&self.p.computer, &self.p.session, &offer, sdp, hk, sig) {
                    // The computer's identity is not confirmed: someone may be posing as it.
                    self.log(format_args!("the answer is not the computer's: stopped"));
                    self.signal(rtc, Signal::LocalEnd(End::plain(Reason::Auth)));
                    return Ok(true);
                }
                if pinned.is_none() {
                    crate::store::pin_host_key(&self.p.computer, hk)?;
                }
                rtc.sdp_api().accept_answer(pending, SdpAnswer::from_sdp_string(sdp)?).context("accept the answer")?;
            }
            "candidate" => {
                if let Some(c) = m["candidate"]["candidate"].as_str().and_then(|s| Candidate::from_sdp_string(s).ok()) {
                    rtc.add_remote_candidate(c);
                }
            }
            "bye" => {
                let reason = serde_json::from_value(m["reason"].clone()).unwrap_or(Reason::Ended);
                self.log(format_args!("the computer ended it: {}", reason.as_str()));
                return Ok(self.signal(rtc, Signal::PeerBye(reason)));
            }
            _ => {}
        }
        Ok(false)
    }
}

/// Runs a link until it ends; every request still waiting gets the reason as its answer.
pub fn run(p: Params, rx: Receiver<LinkEvent>, tx: Sender<LinkEvent>) {
    let (computer, name, session, ended) = (p.computer.clone(), p.name.clone(), p.session.clone(), p.ended.clone());
    let reason = match serve(p, &rx, tx) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{computer}] link failed: {e:#}");
            Some(Reason::Failed)
        }
    };
    let (code, message, next) = refusal(reason, &name);
    // Requests that came meanwhile, or never reached the computer, get the same answer.
    for e in rx.try_iter() {
        match e {
            LinkEvent::Call { request, reply, .. } => {
                let _ = reply.send(json!({ "jsonrpc": "2.0", "id": request["id"], "result": error_result(code, &message, next) }));
            }
            LinkEvent::Tunnel { reply, .. } => {
                let _ = reply.send(error_result(code, &message, next));
            }
            _ => {}
        }
    }
    let _ = ended.send((session, reason));
}

/// Waits for this session's nonce: a challenge, answered through the server.
fn nonce(p: &Params, rx: &Receiver<LinkEvent>, early: &mut Vec<LinkEvent>) -> Result<std::result::Result<String, Reason>> {
    send(&p.out, json!({ "type": "challenge", "session": p.session, "device": p.computer }));
    let until = Instant::now() + peer::state::DEADLINE;
    loop {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(LinkEvent::Signal(m)) if m["type"] == "nonce" => return Ok(Ok(m["nonce"].as_str().unwrap_or_default().to_string())),
            Ok(LinkEvent::Signal(m)) if m["type"] == "bye" => return Ok(Err(serde_json::from_value(m["reason"].clone()).unwrap_or(Reason::Failed))),
            Ok(LinkEvent::Leave) => return Ok(Err(Reason::Left)),
            Ok(e) => early.push(e),
            Err(RecvTimeoutError::Timeout) => return Ok(Err(Reason::Failed)),
            Err(RecvTimeoutError::Disconnected) => bail!("the proxy is gone"),
        }
    }
}

fn serve(p: Params, rx: &Receiver<LinkEvent>, tx: Sender<LinkEvent>) -> Result<Option<Reason>> {
    let mut early = Vec::new();
    let nonce = match nonce(&p, rx, &mut early)? {
        Ok(n) => n,
        Err(Reason::Left) => return Ok(None),
        Err(r) => return Ok(Some(r)),
    };
    let own = tx.clone();
    let mut rtc = Rtc::builder().build(Instant::now());
    let stun: Vec<String> = p.ice.as_array().into_iter().flatten().flat_map(|s| s["urls"].as_array().into_iter().flatten()).filter_map(Value::as_str).filter(|u| u.starts_with("stun:")).map(String::from).collect();
    let mut net = Net::start(&mut rtc, stun, peer::turn::servers(&p.ice), tx)?;
    let mut api = rtc.sdp_api();
    let control = api.add_channel("control".into());
    let ai = api.add_channel("ai".into());
    let (offer, pending) = api.apply().context("an offer")?;
    let sdp = offer.to_sdp_string();
    let challenge = wire::crypto::sha256(wire::challenge::connect_v3(&p.computer, &p.session, &nonce, false, &sdp).as_bytes());
    let auth = json!({ "k": "dk", "nonce": nonce, "cred": p.key.cred, "sig": p.key.sign(&challenge) });
    send(&p.out, json!({ "type": "offer", "session": p.session, "device": p.computer, "epoch": 0, "sdp": sdp, "auth": auth }));
    let mut l = Link { p, machine: Machine::new(Instant::now()), epoch: 0, pending: Some((pending, sdp)), control, ai, joiner: Joiner::default(), queued: VecDeque::new(), ai_open: false, waiting: HashMap::new(), outbox: Outbox::default(), own, listeners: Vec::new(), opening: HashMap::new(), pipes: Pipes::default(), next_id: 0, restarted: None, started: Instant::now() };
    for e in early {
        if let LinkEvent::Call { request, reply, deadline } = e {
            l.queued.push_back((request, reply, deadline));
        }
    }
    let over = drive(&mut l, &mut rtc, &mut net, rx);
    // Tunnels end with the link.
    for stop in &l.listeners {
        stop.store(true, Ordering::Relaxed);
    }
    l.pipes.close_all();
    net.close(&mut rtc);
    l.refuse_all(over.as_ref().map_or(Some(Reason::Failed), |r| *r));
    over
}

fn drive(l: &mut Link, rtc: &mut Rtc, net: &mut Net<LinkEvent>, rx: &Receiver<LinkEvent>) -> Result<Option<Reason>> {
    let reason = |l: &Link| match l.machine.state() {
        peer::state::State::Ended(Reason::Left) => None,
        peer::state::State::Ended(r) => Some(*r),
        _ => Some(Reason::Failed),
    };
    loop {
        let deadline = loop {
            match rtc.poll_output().context("poll rtc")? {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => net.transmit(&t),
                Output::Event(e) => {
                    let over = match e {
                        Event::IceConnectionStateChange(st) => match ice_signal(st) {
                            Some(s) => l.signal(rtc, s),
                            None => false,
                        },
                        Event::ChannelOpen(id, _) if id == l.control => {
                            let hello = serde_json::to_vec(&Control::Hello { version: wire::PROTOCOL_VERSION, caps: Vec::new() }).expect("hello serializes");
                            if let Some(mut ch) = rtc.channel(l.control) {
                                let _ = ch.write(false, &hello);
                            }
                            l.signal(rtc, Signal::ChannelOpen)
                        }
                        Event::ChannelOpen(id, _) if id == l.ai => {
                            l.ai_open = true;
                            l.flush(rtc);
                            false
                        }
                        Event::ChannelOpen(id, _) if l.opening.contains_key(&id) => {
                            let socket = l.opening.remove(&id).expect("an opening tunnel");
                            if l.pipes.add(id, socket, l.own.clone()).is_err() {
                                rtc.direct_api().close_data_channel(id);
                            }
                            false
                        }
                        Event::ChannelClose(id) if id == l.control => l.signal(rtc, Signal::ChannelClosed),
                        Event::ChannelClose(id) => {
                            l.opening.remove(&id);
                            l.pipes.channel_closed(id);
                            false
                        }
                        Event::ChannelData(d) if l.pipes.has(d.id) => {
                            l.pipes.from_channel(d.id, &d.data);
                            false
                        }
                        Event::ChannelData(d) if d.id == l.control => match serde_json::from_slice::<Control>(&d.data) {
                            Ok(Control::End { reason, .. }) => l.signal(rtc, Signal::PeerEnded(reason)),
                            _ => false,
                        },
                        Event::ChannelData(d) if d.id == l.ai => {
                            if let Ok(Some(text)) = l.joiner.push(&d.data) {
                                l.answered(&text);
                            }
                            false
                        }
                        _ => false,
                    };
                    if over {
                        return Ok(reason(l));
                    }
                }
            }
            l.outbox.flush(rtc);
            l.pipes.drained(rtc);
            if !rtc.is_alive() {
                l.signal(rtc, Signal::TransportClosed);
                return Ok(reason(l));
            }
        };
        // Recovering: this end restarts ICE, again every 10 s while it lasts.
        if matches!(l.machine.state(), peer::state::State::Recovering) && l.restarted.is_none_or(|t| t.elapsed() > Duration::from_secs(10)) {
            l.restart(rtc, net)?;
        }
        let next = net.poll(Instant::now()).into_iter().chain(l.machine.deadline()).fold(deadline, Instant::min);
        let wait = next.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
        match rx.recv_timeout(wait) {
            Ok(LinkEvent::Net(e)) => {
                let l2: &Link = l;
                net.handle(rtc, e, |rtc, c| l2.offer_candidate(rtc, c))?;
            }
            Ok(LinkEvent::Signal(m)) => {
                if l.on_signal(rtc, m)? {
                    return Ok(reason(l));
                }
            }
            Ok(LinkEvent::Call { request, reply, deadline }) => {
                l.queued.push_back((request, reply, deadline));
                l.flush(rtc);
            }
            Ok(LinkEvent::Tunnel { port, local, reply }) => l.listen(port, local, reply),
            Ok(LinkEvent::Accepted { port, socket }) => l.accepted(rtc, port, socket),
            Ok(LinkEvent::Pipe(e)) => l.pipes.to_channel(rtc, e),
            Ok(LinkEvent::Leave) | Err(RecvTimeoutError::Disconnected) => {
                l.signal(rtc, Signal::LocalEnd(End::plain(Reason::Left)));
                return Ok(None);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if l.signal(rtc, Signal::Tick) {
            return Ok(reason(l));
        }
        rtc.handle_input(Input::Timeout(Instant::now())).context("rtc timeout")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_way_a_link_ends_is_told_to_the_ai_client_with_what_to_do() {
        for (r, want) in [
            (Some(Reason::AiOff), "ai_off"),
            (Some(Reason::Busy), "busy"),
            (Some(Reason::Unpaired), "unpaired"),
            (Some(Reason::Offline), "offline"),
            (Some(Reason::Idle), "idle"),
            (Some(Reason::Failed), "failed"),
            (None, "left"),
        ] {
            let (code, message, next) = refusal(r, "Desk");
            let v = error_result(code, &message, next);
            assert_eq!(code, want);
            assert_eq!(v["isError"], true);
            assert!(!next.is_empty() && v["content"][0]["text"].as_str().unwrap().starts_with(code), "{r:?}");
        }
        let (_, message, next) = refusal(Some(Reason::Idle), "Desk");
        assert_eq!(message, "the seat on Desk was given up after 10 minutes without a tool call. Programs started there keep running");
        assert_eq!(next, "Call any tool to connect again. Use proc_list to find programs and proc_read for their output and exit code.");
    }

    #[test]
    fn only_unanswered_calls_that_were_sent_may_have_run() {
        let (code, message, next) = refusal(Some(Reason::Ended), "Desk");
        for (sent, message_suffix, next_prefix) in [
            (false, "", ""),
            (true, ". The call may have been carried out on the computer.",
                "Programs it started keep running. Check proc_list (mine) before repeating the call. "),
        ] {
            let result = call_refusal(code, &message, next, sent);
            assert_eq!(result["structuredContent"], json!({
                "error": code, "message": format!("{message}{message_suffix}"), "next": format!("{next_prefix}{next}")
            }));
        }
    }

    #[test]
    fn only_the_computers_key_over_both_descriptions_is_its_answer() {
        use p256::ecdsa::signature::Signer;
        use p256::pkcs8::EncodePublicKey;
        let key = p256::ecdsa::SigningKey::from_slice(&[3u8; 32]).unwrap();
        let hk = STANDARD.encode(key.verifying_key().to_public_key_der().unwrap().as_bytes());
        let sig: p256::ecdsa::Signature = key.sign(wire::challenge::answer_v1("H1", "s1", "offer", "answer").as_bytes());
        let sig = STANDARD.encode(sig.to_bytes());
        assert!(answer_signed("H1", "s1", "offer", "answer", &hk, &sig));
        assert!(!answer_signed("H1", "s1", "offer", "other answer", &hk, &sig), "a changed description");
        assert!(!answer_signed("H2", "s1", "offer", "answer", &hk, &sig), "another computer");
        let other = p256::ecdsa::SigningKey::from_slice(&[4u8; 32]).unwrap();
        let other_hk = STANDARD.encode(other.verifying_key().to_public_key_der().unwrap().as_bytes());
        assert!(!answer_signed("H1", "s1", "offer", "answer", &other_hk, &sig), "another key");
    }
}
