//! One remote session: a str0m WebRTC peer driven from a single thread. The peer connection is
//! the session's source of truth; its state machine (`peer::state`) decides when it is Active,
//! Recovering or over (docs/session-lifecycle.md).
//!
//! The driver here is what every session has, whatever controls the computer: the peer
//! connection and its network paths (`peer::net`), the negotiation (one answer per epoch, ICE
//! restarts on the same connection), the state machine and its deadlines, the greeting on the
//! control channel and the end. What the session is for is its [`Role`]: `desk`, a person's
//! browser with the picture, input, clipboard and files; `ai`, an AI client's MCP requests.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use peer::net::Net;
use peer::state::{ice_signal, Action, End, Machine, Signal};
use peer::turn::TurnServer;
use peer::pipe::PipeEvent;
use peer::NetEvent;
use serde::Serialize;
use str0m::bwe::Bitrate;
use str0m::change::SdpOffer;
use str0m::channel::{ChannelData, ChannelId};
use str0m::crypto::{from_feature_flags, CryptoProvider};
use str0m::{Candidate, Event, Input, Output, Rtc};

use crate::platform::PlatformEvent;
use wire::control::Control;
use wire::signal::Signal as Wire;
use wire::types::{AnswerAuth, IceCandidate, Reason};

pub mod ai;
pub mod desk;

pub enum SessionEvent {
    /// From the network paths (`peer::net`).
    Net(NetEvent),
    /// From the computer, for this session.
    Platform(PlatformEvent),
    RemoteCandidate(String),
    /// An offer for this session again (epoch 0) or an ICE restart (a higher epoch).
    Negotiate { sdp: String, epoch: u32 },
    /// The other end's signaling `bye`, with its reason.
    PeerBye(Reason),
    /// This computer ends the session, telling the other end why.
    End(End),
    /// The shared clipboard was switched (at the computer, or as this browser asked).
    ClipShare(bool),
    /// An answer to an AI client's MCP request (`crate::mcp`), to send it.
    Mcp(String),
    /// A tunnel's connection gave bytes, or closed (`peer::pipe`).
    Pipe(PipeEvent),
}

impl From<NetEvent> for SessionEvent {
    fn from(e: NetEvent) -> SessionEvent {
        SessionEvent::Net(e)
    }
}

impl From<PipeEvent> for SessionEvent {
    fn from(e: PipeEvent) -> SessionEvent {
        SessionEvent::Pipe(e)
    }
}

/// What the host's registry hears from a session thread: the only source of who controls the
/// computer.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionUpdate {
    /// The session reached Active for the first time: it controls the computer now.
    Established(String),
    /// The session is over, however it ended.
    Ended(String),
}

/// Signs an answer to an offer (offer SDP, answer SDP), so the other end knows it reached this host.
pub type AnswerSigner = Box<dyn Fn(&str, &str) -> AnswerAuth + Send>;

/// What every session is started with, whatever its role.
pub struct Params {
    pub session: String,
    pub sdp: String,
    pub stun: Vec<String>,
    pub turn: Vec<TurnServer>,
    pub out: Sender<String>,
    /// Told when the session is established and when it is over.
    pub updates: Sender<SessionUpdate>,
    pub sign: AnswerSigner,
    /// Sent once the control channel opens, in order: hello, then the resume token.
    pub greeting: Vec<Control>,
}

/// What a session is for. The driver calls it on the session thread with the peer connection
/// and the session's own state at hand (`Cx`); the role says and hears everything on its data
/// channels, and asks the driver to end the session through `Cx::signal`.
pub trait Role: Send + 'static {
    /// The bitrate to start bandwidth estimation at, for a role that sends media; `None`: none.
    fn bandwidth(&self) -> Option<u32> {
        None
    }
    /// What lifts every key and button this session holds, when it leaves Active.
    fn releaser(&self) -> Box<dyn Fn() + Send>;
    /// Whether an ICE restart's offer has the shape this role's sessions take.
    fn accepts(&self, _sdp: &str) -> bool {
        true
    }
    /// A data channel opened. True for the role's control channel: the driver then sends the
    /// greeting on it, `control_open` follows, and the session can become Active.
    fn channel_open(&mut self, cx: &mut Cx, id: ChannelId, label: &str) -> bool;
    /// What the role says first on its control channel, after the greeting.
    fn control_open(&mut self, _cx: &mut Cx) {}
    /// Data on any channel, the control channel's `end` included. True once the session is over.
    fn channel_data(&mut self, cx: &mut Cx, d: ChannelData) -> bool;
    /// A str0m event the driver does not handle itself (media, bandwidth, buffers).
    fn rtc_event(&mut self, _cx: &mut Cx, _e: Event) {}
    /// An event from the computer for this session.
    fn platform(&mut self, _cx: &mut Cx, _e: PlatformEvent) {}
    /// The shared clipboard was switched.
    fn clip_share(&mut self, _cx: &mut Cx, _on: bool) {}
    /// An answer to an MCP request, to send.
    fn mcp_reply(&mut self, _cx: &mut Cx, _text: String) {}
    /// A tunnel's connection gave bytes, or closed.
    fn pipe(&mut self, _cx: &mut Cx, _e: PipeEvent) {}
    /// After each pass over str0m's output.
    fn after_output(&mut self, _cx: &mut Cx) {}
    /// Before the loop waits, with the bytes waiting to leave: when the role wants to look again.
    fn before_wait(&mut self, _cx: &mut Cx, _queued: usize) -> Option<Instant> {
        None
    }
    /// After each event or timeout.
    fn after_event(&mut self, _cx: &mut Cx) {}
}

/// What a role works with on each call: the peer connection, and the session's state.
pub struct Cx<'a> {
    pub rtc: &'a mut Rtc,
    peer: &'a mut Peer,
}

impl Cx<'_> {
    /// Feeds the state machine. True once the session is over.
    pub fn signal(&mut self, s: Signal) -> bool {
        self.peer.signal(self.rtc, s)
    }

    /// Input and clipboard count only while the session is Active.
    pub fn active(&self) -> bool {
        self.peer.machine.active()
    }

    /// The role's control channel, once open.
    pub fn control(&self) -> Option<ChannelId> {
        self.peer.control
    }

    /// When the session began: media timestamps count from it.
    pub fn started(&self) -> Instant {
        self.peer.started
    }

    pub fn log(&self, what: std::fmt::Arguments) {
        self.peer.log(what);
    }
}

/// Sends one text message on a channel, if it is open.
pub fn send(rtc: &mut Rtc, channel: Option<ChannelId>, text: &str) {
    if let Some(mut ch) = channel.and_then(|id| rtc.channel(id)) {
        if let Err(e) = ch.write(false, text.as_bytes()) {
            eprintln!("data channel write failed: {e}");
        }
    }
}

/// Sends one message, serialized.
pub fn send_message(rtc: &mut Rtc, channel: Option<ChannelId>, m: &impl Serialize) {
    send(rtc, channel, &serde_json::to_string(m).expect("channel message serializes"));
}

/// Starts a session on its own thread and returns the handle that feeds it. `role` makes what
/// the session is for, given that same handle (a role whose work goes on elsewhere answers
/// through it).
pub fn spawn<R: Role>(p: Params, role: impl FnOnce(Sender<SessionEvent>) -> R) -> Sender<SessionEvent> {
    let (tx, rx) = channel();
    let self_tx = tx.clone();
    let role = role(tx.clone());
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let (updates, session) = (p.updates.clone(), p.session.clone());
            if let Err(e) = run(p, role, rx, self_tx) {
                eprintln!("session ended with error: {e:#}");
            }
            let _ = updates.send(SessionUpdate::Ended(session));
        })
        .expect("spawn session thread");
    tx
}

fn signal_text(m: &Wire) -> String {
    serde_json::to_string(m).expect("signal serializes")
}

/// The DTLS fingerprints an SDP declares, normalized and sorted.
fn fingerprints(sdp: &str) -> Vec<String> {
    let mut out: Vec<String> = sdp.lines().filter_map(|l| l.trim().strip_prefix("a=fingerprint:")).map(|f| f.trim().to_ascii_lowercase()).collect();
    out.sort();
    out.dedup();
    out
}

/// An ICE restart offer is taken only from the same connection: its DTLS fingerprint equals
/// the one the session was authenticated with, so no new proof is needed.
fn same_connection(original: &str, restart: &str) -> bool {
    let (a, b) = (fingerprints(original), fingerprints(restart));
    !a.is_empty() && a == b
}

/// What the loop keeps about the peer connection besides str0m itself.
struct Peer {
    session: String,
    out: Sender<String>,
    updates: Sender<SessionUpdate>,
    /// Established was reported (once, on the first Active).
    established: bool,
    machine: Machine,
    /// The role's control channel: the session's `end` goes there, and its closing ends it.
    control: Option<ChannelId>,
    release: Box<dyn Fn() + Send>,
    started: Instant,
}

impl Peer {
    fn log(&self, what: std::fmt::Arguments) {
        eprintln!("[{:>5} ms] {what}", self.started.elapsed().as_millis());
    }

    /// Feeds the state machine and carries out what it decides. True once the session is over.
    fn signal(&mut self, rtc: &mut Rtc, s: Signal) -> bool {
        let before = format!("{:?}", self.machine.state());
        for a in self.machine.handle(s, Instant::now()) {
            match a {
                Action::ReleaseInput => (self.release)(),
                Action::SendEnd(End { reason, by, same }) => send_message(rtc, self.control, &Control::End { reason, by, same: same.then_some(true) }),
                Action::SendBye(End { reason, by, same }) => {
                    let _ = self.out.send(signal_text(&Wire::Bye { session: self.session.clone(), reason, by, same: same.then_some(true), holder: None }));
                }
                // The caller sends what is queued (an end message) and then closes.
                Action::Close => {}
            }
        }
        let after = format!("{:?}", self.machine.state());
        if after != before {
            self.log(format_args!("session {before} -> {after}"));
        }
        if self.machine.active() && !self.established {
            self.established = true;
            let _ = self.updates.send(SessionUpdate::Established(self.session.clone()));
        }
        self.machine.ended()
    }
}

/// The answers this session gave, one per epoch: the latest is kept, to be sent again for a
/// repeated offer of its epoch (the first one may not have reached the other end).
struct Answered {
    epoch: u32,
    text: String,
}

/// What an offer or restart of some epoch is, against the answers given.
#[derive(Debug, PartialEq)]
enum Negotiation {
    /// The epoch already answered: the same answer again.
    Repeat,
    /// A later epoch: an ICE restart to answer.
    New,
    /// An earlier epoch: superseded, ignored.
    Stale,
}

impl Answered {
    fn meet(&self, epoch: u32) -> Negotiation {
        match epoch.cmp(&self.epoch) {
            std::cmp::Ordering::Equal => Negotiation::Repeat,
            std::cmp::Ordering::Greater => Negotiation::New,
            std::cmp::Ordering::Less => Negotiation::Stale,
        }
    }
}

/// Everything the session loop keeps between events, besides str0m and the network paths.
struct Session<R> {
    id: String,
    /// The offer this session was started from; restarts must belong to it.
    offer: String,
    answered: Answered,
    out: Sender<String>,
    sign: AnswerSigner,
    greeting: Vec<Control>,
    peer: Peer,
    role: R,
}

/// A `Cx` over the session's peer and `rtc`, beside a borrow of its role.
macro_rules! cx {
    ($s:expr, $rtc:expr) => {
        &mut Cx { rtc: $rtc, peer: &mut $s.peer }
    };
}

impl<R: Role> Session<R> {
    /// Signs and sends the answer to `offer` for `epoch`, and keeps it for a repeat.
    fn send_answer(&mut self, offer: &str, answer: String, epoch: u32) {
        let auth = (self.sign)(offer, &answer);
        let text = signal_text(&Wire::Answer { session: self.id.clone(), epoch, sdp: answer, auth });
        let _ = self.out.send(text.clone());
        self.answered = Answered { epoch, text };
    }

    /// Adds a local candidate found after the answer and tells the other end about it.
    fn offer_candidate(&self, rtc: &mut Rtc, c: Candidate) -> bool {
        let sdp = c.to_sdp_string();
        if rtc.add_local_candidate(c).is_none() {
            return false;
        }
        let candidate = IceCandidate { candidate: sdp, sdp_mid: None, sdp_m_line_index: Some(0), username_fragment: None };
        let _ = self.out.send(signal_text(&Wire::Candidate { session: self.id.clone(), candidate }));
        true
    }

    /// One str0m event. True once the session is over.
    fn on_rtc(&mut self, rtc: &mut Rtc, e: Event) -> bool {
        match e {
            Event::IceConnectionStateChange(st) => {
                self.peer.log(format_args!("ice {st:?}"));
                match ice_signal(st) {
                    Some(s) => self.peer.signal(rtc, s),
                    None => false,
                }
            }
            Event::Connected => {
                self.peer.log(format_args!("dtls connected"));
                self.role.rtc_event(cx!(self, rtc), Event::Connected);
                false
            }
            Event::ChannelOpen(id, label) => {
                self.peer.log(format_args!("channel {label} open"));
                if self.role.channel_open(cx!(self, rtc), id, &label) {
                    self.peer.control = Some(id);
                    for g in &self.greeting {
                        send_message(rtc, self.peer.control, g);
                    }
                    self.role.control_open(cx!(self, rtc));
                    return self.peer.signal(rtc, Signal::ChannelOpen);
                }
                false
            }
            // A closed control channel ends the session at once.
            Event::ChannelClose(id) if self.peer.control == Some(id) => self.peer.signal(rtc, Signal::ChannelClosed),
            Event::ChannelData(d) => self.role.channel_data(cx!(self, rtc), d),
            e => {
                self.role.rtc_event(cx!(self, rtc), e);
                false
            }
        }
    }

    /// An offer or ICE restart of `epoch`: each epoch is answered once (the same answer again
    /// for a repeat), and an older one is not this negotiation's.
    fn negotiate(&mut self, rtc: &mut Rtc, net: &mut Net<SessionEvent>, offer: String, epoch: u32) -> Result<()> {
        match self.answered.meet(epoch) {
            Negotiation::Repeat => {
                let _ = self.out.send(self.answered.text.clone());
                Ok(())
            }
            Negotiation::Stale => Ok(()),
            Negotiation::New => self.restart(rtc, net, offer, epoch),
        }
    }

    /// An ICE restart on the same peer connection: new ICE credentials, the same DTLS. This
    /// computer's addresses are found again first, as its network may have changed too.
    fn restart(&mut self, rtc: &mut Rtc, net: &mut Net<SessionEvent>, offer: String, epoch: u32) -> Result<()> {
        if !self.peer.machine.accepts_restart() || !same_connection(&self.offer, &offer) || !self.role.accepts(&offer) {
            self.peer.log(format_args!("ICE restart refused"));
            return Ok(());
        }
        let fresh = net.rebind(rtc, Instant::now())?;
        match SdpOffer::from_sdp_string(&offer).map_err(anyhow::Error::from).and_then(|o| rtc.sdp_api().accept_offer(o).map_err(anyhow::Error::from)) {
            Ok(a) => {
                self.peer.log(format_args!("ICE restart {epoch}"));
                self.send_answer(&offer, a.to_sdp_string(), epoch);
                for c in fresh {
                    self.offer_candidate(rtc, c);
                }
            }
            Err(e) => self.peer.log(format_args!("ICE restart failed: {e}")),
        }
        Ok(())
    }

    /// One event from the session's queue. True once the session is over.
    fn on_event(&mut self, rtc: &mut Rtc, net: &mut Net<SessionEvent>, ev: SessionEvent) -> Result<bool> {
        match ev {
            SessionEvent::Net(e) => net.handle(rtc, e, |rtc, c| self.offer_candidate(rtc, c))?,
            SessionEvent::Platform(e) => self.role.platform(cx!(self, rtc), e),
            SessionEvent::ClipShare(on) => self.role.clip_share(cx!(self, rtc), on),
            SessionEvent::Mcp(text) => self.role.mcp_reply(cx!(self, rtc), text),
            SessionEvent::Pipe(e) => self.role.pipe(cx!(self, rtc), e),
            SessionEvent::RemoteCandidate(s) => {
                // Browsers may send mDNS candidates, which are skipped: the other end still
                // reaches this host's candidates and is learned as peer-reflexive.
                if let Ok(c) = Candidate::from_sdp_string(s.strip_prefix("candidate:").map(|r| format!("candidate:{r}")).as_deref().unwrap_or(&s)) {
                    rtc.add_remote_candidate(c);
                }
            }
            SessionEvent::Negotiate { sdp, epoch } => self.negotiate(rtc, net, sdp, epoch)?,
            SessionEvent::PeerBye(reason) => return Ok(self.peer.signal(rtc, Signal::PeerBye(reason))),
            SessionEvent::End(end) => {
                self.peer.signal(rtc, Signal::LocalEnd(end));
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// The session thread: set up, then run until it ends. Whatever goes wrong ends it `failed`,
/// telling the other end: before the session exists (no usable interface, an offer that cannot
/// be parsed or accepted, a thread that cannot start) with a signaling bye, afterwards through
/// the state machine, which knows how the other end can hear it.
fn run<R: Role>(p: Params, role: R, rx: Receiver<SessionEvent>, tx: Sender<SessionEvent>) -> Result<()> {
    let (id, out) = (p.session.clone(), p.out.clone());
    let (mut s, mut rtc, mut net) = match start(p, role, &tx) {
        Ok(started) => started,
        Err(e) => {
            let _ = out.send(signal_text(&Wire::bye(&id, Reason::Failed)));
            return Err(e);
        }
    };
    let served = serve(&mut s, &mut rtc, &mut net, &rx);
    if served.is_err() {
        s.peer.signal(&mut rtc, Signal::LocalEnd(End::plain(Reason::Failed)));
    }
    net.close(&mut rtc);
    served
}

/// Accepts the offer and answers it (epoch 0), with the host candidates only: the address a
/// STUN server sees and the relays follow as they are found (trickle ICE), so nothing slow
/// delays the answer.
fn start<R: Role>(p: Params, role: R, tx: &Sender<SessionEvent>) -> Result<(Session<R>, Rtc, Net<SessionEvent>)> {
    let Params { session, sdp, stun, turn, out, updates, sign, greeting } = p;
    let provider = CryptoProvider::get_default().cloned().unwrap_or_else(from_feature_flags);
    // str0m's builder panics when its provider cannot generate a certificate. Use the same
    // provider here so a certificate failure takes the session's usual startup error path.
    let cert = provider.dtls_provider.generate_certificate().context("generate DTLS certificate")?;
    let mut builder = Rtc::builder().set_crypto_provider(Arc::new(provider)).set_dtls_cert(cert);
    if let Some(b) = role.bandwidth() {
        builder = builder.enable_bwe(Some(Bitrate::bps(b as u64)));
    }
    let mut rtc = builder.build(Instant::now());
    if let Some(b) = role.bandwidth() {
        rtc.bwe().set_bitrate(Bitrate::bps(0), Bitrate::bps(b as u64));
    }
    let net = Net::start(&mut rtc, stun, turn, tx.clone())?;

    let release = role.releaser();
    let peer = Peer { session: session.clone(), out: out.clone(), updates, established: false, machine: Machine::new(Instant::now()), control: None, release, started: Instant::now() };
    let mut s = Session { id: session, offer: sdp, answered: Answered { epoch: 0, text: String::new() }, out, sign, greeting, peer, role };
    let offer = SdpOffer::from_sdp_string(&s.offer).context("parse offer")?;
    let answer = rtc.sdp_api().accept_offer(offer).context("accept offer")?.to_sdp_string();
    let first = s.offer.clone();
    s.send_answer(&first, answer, 0);
    Ok((s, rtc, net))
}

/// The session loop: str0m's output, the session's events and its deadlines, until it ends.
fn serve<R: Role>(s: &mut Session<R>, rtc: &mut Rtc, net: &mut Net<SessionEvent>, rx: &Receiver<SessionEvent>) -> Result<()> {
    loop {
        let deadline = loop {
            match rtc.poll_output().context("poll rtc")? {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => net.transmit(&t),
                Output::Event(e) => {
                    if s.on_rtc(rtc, e) {
                        return Ok(());
                    }
                }
            }
            s.role.after_output(cx!(s, rtc));
            if !rtc.is_alive() {
                s.peer.signal(rtc, Signal::TransportClosed);
                return Ok(());
            }
        };

        let look = s.role.before_wait(cx!(s, rtc), net.queued());
        let probe = net.poll(Instant::now());
        let next = probe.into_iter().chain(s.peer.machine.deadline()).chain(look).fold(deadline, Instant::min);
        let wait = next.saturating_duration_since(Instant::now()).min(Duration::from_millis(500));
        match rx.recv_timeout(wait) {
            Ok(ev) => {
                if s.on_event(rtc, net, ev)? {
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        s.role.after_event(cx!(s, rtc));
        if s.peer.signal(rtc, Signal::Tick) {
            return Ok(());
        }
        rtc.handle_input(Input::Timeout(Instant::now())).context("rtc timeout")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A role that does nothing: what the driver does on its own.
    struct Nothing;
    impl Role for Nothing {
        fn releaser(&self) -> Box<dyn Fn() + Send> {
            Box::new(|| {})
        }
        fn channel_open(&mut self, _: &mut Cx, _: ChannelId, _: &str) -> bool {
            true
        }
        fn channel_data(&mut self, _: &mut Cx, _: ChannelData) -> bool {
            false
        }
    }

    #[test]
    fn each_epoch_is_answered_once_and_older_ones_are_not_this_negotiation() {
        let a = Answered { epoch: 2, text: "answer 2".into() };
        assert_eq!(a.meet(2), Negotiation::Repeat, "the same epoch again: the same answer again");
        assert_eq!(a.meet(3), Negotiation::New, "a later restart is answered");
        assert_eq!(a.meet(1), Negotiation::Stale, "a superseded one is ignored");
        assert_eq!(Answered { epoch: 0, text: String::new() }.meet(0), Negotiation::Repeat, "a repeated offer");
    }

    #[test]
    fn answers_carry_their_epoch_on_the_wire() {
        let w = Wire::Answer { session: "s1".into(), epoch: 4, sdp: "v=0".into(), auth: AnswerAuth { sig: "s".into(), hk: "k".into() } };
        assert_eq!(serde_json::to_value(&w).unwrap()["epoch"], 4);
    }

    #[test]
    fn a_restart_must_keep_the_fingerprint() {
        let first = "v=0\r\na=ice-ufrag:aaaa\r\na=fingerprint:sha-256 AB:CD\r\nm=application\r\n";
        let restart = "v=0\r\na=ice-ufrag:bbbb\r\na=fingerprint:sha-256 ab:cd\r\nm=application\r\n";
        let other = "v=0\r\na=ice-ufrag:bbbb\r\na=fingerprint:sha-256 EF:01\r\n";
        assert!(same_connection(first, restart));
        assert!(!same_connection(first, other));
        assert!(!same_connection("v=0", "v=0"), "an SDP without a fingerprint proves nothing");
    }

    #[test]
    fn a_session_that_cannot_start_says_failed_through_the_server() {
        let (out, sent) = channel();
        let (updates, heard) = channel();
        let p = Params {
            session: "s1".into(),
            sdp: "not an offer".into(),
            stun: Vec::new(),
            turn: Vec::new(),
            out,
            updates,
            sign: Box::new(|_, _| AnswerAuth { sig: String::new(), hk: String::new() }),
            greeting: Vec::new(),
        };
        let tx = spawn(p, |_| Nothing);
        let bye: serde_json::Value = serde_json::from_str(&sent.recv_timeout(Duration::from_secs(10)).expect("told the other end")).unwrap();
        assert_eq!(bye, serde_json::json!({ "type": "bye", "session": "s1", "reason": "failed" }));
        assert_eq!(heard.recv_timeout(Duration::from_secs(10)).unwrap(), SessionUpdate::Ended("s1".into()), "the registry hears it is over");
        drop(tx);
    }

    /// A session of `role` met by a real offerer on this computer with data channels
    /// `labels`: why the session ended it through the server, within `within`; None when it
    /// did not.
    pub(super) fn ended_by<R: Role>(role: R, labels: &[&str], within: Duration) -> Option<String> {
        use str0m::change::SdpAnswer;
        let (out, sent) = channel();
        let (updates, _heard) = channel();
        let (net_tx, net_rx) = channel::<NetEvent>();
        let mut rtc = Rtc::builder().build(Instant::now());
        let mut net = Net::start(&mut rtc, Vec::new(), Vec::new(), net_tx).unwrap();
        let mut api = rtc.sdp_api();
        for l in labels {
            api.add_channel(l.to_string());
        }
        let (offer, pending) = api.apply().unwrap();
        let p = Params {
            session: "s1".into(),
            sdp: offer.to_sdp_string(),
            stun: Vec::new(),
            turn: Vec::new(),
            out,
            updates,
            sign: Box::new(|_, _| AnswerAuth { sig: String::new(), hk: String::new() }),
            greeting: Vec::new(),
        };
        let _session = spawn(p, |_| role);
        let mut pending = Some(pending);
        let end = Instant::now() + within;
        while Instant::now() < end {
            for m in sent.try_iter() {
                let m: serde_json::Value = serde_json::from_str(&m).unwrap();
                match m["type"].as_str() {
                    Some("answer") => {
                        if let Some(p) = pending.take() {
                            rtc.sdp_api().accept_answer(p, SdpAnswer::from_sdp_string(m["sdp"].as_str().unwrap()).unwrap()).unwrap();
                        }
                    }
                    Some("bye") => return m["reason"].as_str().map(String::from),
                    _ => {}
                }
            }
            let next = loop {
                match rtc.poll_output().unwrap() {
                    Output::Timeout(t) => break t,
                    Output::Transmit(t) => net.transmit(&t),
                    Output::Event(_) => {}
                }
            };
            match net_rx.recv_timeout(next.saturating_duration_since(Instant::now()).min(Duration::from_millis(20))) {
                Ok(ev) => net.handle(&mut rtc, ev, |_, _| false).unwrap(),
                Err(_) => rtc.handle_input(Input::Timeout(Instant::now())).unwrap(),
            }
        }
        None
    }
}
