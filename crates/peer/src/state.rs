//! A session's state machine (docs/session-lifecycle.md, "States" and "Ending"), the same on
//! both ends. Pure: the str0m driver feeds it what the peer connection reports and carries out
//! the actions it returns.
//!
//! `Connecting -> Active <-> Recovering`, and from any of them `Ended(reason)`. Liveness is the
//! peer connection's own: ICE consent freshness and the DTLS/SCTP state. There is no keep-alive.

use std::time::{Duration, Instant};

use str0m::IceConnectionState;
use wire::types::Reason;

/// How long connecting (from the offer) or recovering may take: the consent expiry of RFC
/// 7675. A peer that has had no working path for this long is gone; both ends keep the same
/// deadlines.
pub const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Connecting,
    Active,
    Recovering,
    Ended(Reason),
}

/// Why this end ends a session, and for `taken` who took it.
#[derive(Debug, Clone, PartialEq)]
pub struct End {
    pub reason: Reason,
    /// `taken`: the taker's device name.
    pub by: Option<String>,
    /// `taken`: the taker is the same device as this session's.
    pub same: bool,
}

impl End {
    pub fn plain(reason: Reason) -> End {
        End { reason, by: None, same: false }
    }
}

/// What happened to the peer connection, or to the session from outside.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// The control channel opened: ICE, DTLS and SCTP all work.
    ChannelOpen,
    /// ICE has a working path (again).
    IceConnected,
    /// ICE consent failed, or ICE is checking again after it had connected: no working path
    /// for now.
    IceDisconnected,
    /// DTLS/SCTP are gone.
    TransportClosed,
    /// The other end closed the control channel.
    ChannelClosed,
    /// The other end's `end` on the control channel.
    PeerEnded(Reason),
    /// The other end's `bye` through the server: it ends the session in any state.
    PeerBye(Reason),
    /// This end ends the session.
    LocalEnd(End),
    /// Time passes; deadlines are checked.
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Lift every key and button held: the session left Active (on the host; the end that
    /// controls holds nothing to lift).
    ReleaseInput,
    /// Tell the other end why, on the control channel (while it is open).
    SendEnd(End),
    /// Tell the other end why, through the server: every end this side decides, in any state
    /// (an end just back cannot know whether its channel still carries anything).
    SendBye(End),
    /// Close the peer connection; nothing more happens.
    Close,
}

/// What an ICE state means for the session. Checking again after it had connected is a lost
/// path while other candidates are still tried: recovering, as on disconnected (the machine
/// takes it only from Active; while connecting, checking is how ICE starts).
pub fn ice_signal(st: IceConnectionState) -> Option<Signal> {
    match st {
        IceConnectionState::Connected | IceConnectionState::Completed => Some(Signal::IceConnected),
        IceConnectionState::Disconnected | IceConnectionState::Checking => Some(Signal::IceDisconnected),
        _ => None,
    }
}

pub struct Machine {
    state: State,
    /// When Connecting or Recovering began.
    since: Instant,
}

impl Machine {
    pub fn new(now: Instant) -> Machine {
        Machine { state: State::Connecting, since: now }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn ended(&self) -> bool {
        matches!(self.state, State::Ended(_))
    }

    /// Input reaches the computer only while the session is Active.
    pub fn active(&self) -> bool {
        self.state == State::Active
    }

    /// An ICE restart is taken only on a session that was established.
    pub fn accepts_restart(&self) -> bool {
        matches!(self.state, State::Active | State::Recovering)
    }

    /// When a Tick next matters, if at all.
    pub fn deadline(&self) -> Option<Instant> {
        matches!(self.state, State::Connecting | State::Recovering).then(|| self.since + DEADLINE)
    }

    /// How an end this side decides reaches the other end: always through the server, and also
    /// on the control channel while it is open (`channel`: it has not closed under this end).
    fn tell(&self, end: End, channel: bool, out: &mut Vec<Action>) {
        if channel && matches!(self.state, State::Active | State::Recovering) {
            out.push(Action::SendEnd(end.clone()));
        }
        out.push(Action::SendBye(end));
    }

    pub fn handle(&mut self, s: Signal, now: Instant) -> Vec<Action> {
        if self.ended() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let next = match (&self.state, s) {
            (State::Connecting, Signal::ChannelOpen) => State::Active,
            (State::Active, Signal::IceDisconnected) => {
                self.since = now;
                State::Recovering
            }
            (State::Recovering, Signal::IceConnected) => State::Active,
            (State::Connecting | State::Recovering, Signal::Tick) if now >= self.since + DEADLINE => {
                self.tell(End::plain(Reason::Failed), true, &mut out);
                State::Ended(Reason::Failed)
            }
            // The peer connection or the control channel gone without the other end's `end`.
            (_, Signal::TransportClosed | Signal::ChannelClosed) => {
                self.tell(End::plain(Reason::Failed), false, &mut out);
                State::Ended(Reason::Failed)
            }
            (_, Signal::PeerEnded(r) | Signal::PeerBye(r)) => State::Ended(r),
            (_, Signal::LocalEnd(end)) => {
                let reason = end.reason;
                self.tell(end, true, &mut out);
                State::Ended(reason)
            }
            _ => return out,
        };
        // Held keys and buttons are released on every way out of Active, before anything else.
        if self.active() && next != State::Active {
            out.insert(0, Action::ReleaseInput);
        }
        if matches!(next, State::Ended(_)) {
            out.push(Action::Close);
        }
        self.state = next;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(t0: Instant) -> Machine {
        let mut m = Machine::new(t0);
        assert_eq!(m.handle(Signal::ChannelOpen, t0), vec![]);
        assert_eq!(m.state(), &State::Active);
        m
    }

    fn recovering(t0: Instant) -> Machine {
        let mut m = active(t0);
        m.handle(Signal::IceDisconnected, t0);
        m
    }

    #[test]
    fn connecting_becomes_active_when_the_channel_opens() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        assert_eq!(m.state(), &State::Connecting);
        assert!(!m.active() && !m.accepts_restart());
        // ICE flapping while connecting changes nothing.
        assert!(m.handle(Signal::IceDisconnected, t0).is_empty());
        assert!(m.handle(Signal::IceConnected, t0).is_empty());
        assert_eq!(m.handle(Signal::ChannelOpen, t0), vec![]);
        assert!(m.active() && m.accepts_restart());
        assert_eq!(m.deadline(), None, "an active session has no deadline");
    }

    #[test]
    fn a_blip_is_recovering_and_releases_input() {
        let t0 = Instant::now();
        let mut m = active(t0);
        let t1 = t0 + Duration::from_secs(100);
        assert_eq!(m.handle(Signal::IceDisconnected, t1), vec![Action::ReleaseInput]);
        assert_eq!(m.state(), &State::Recovering);
        assert!(!m.active() && m.accepts_restart());
        assert_eq!(m.deadline(), Some(t1 + DEADLINE), "the deadline counts from the blip");
        assert!(m.handle(Signal::Tick, t1 + DEADLINE - Duration::from_millis(1)).is_empty());
        assert_eq!(m.handle(Signal::IceConnected, t1 + Duration::from_secs(5)), vec![]);
        assert_eq!(m.state(), &State::Active);
    }

    #[test]
    fn no_recovery_by_the_deadline_ends_failed_on_the_channel_and_through_the_server() {
        let t0 = Instant::now();
        let mut m = recovering(t0);
        let failed = End::plain(Reason::Failed);
        assert_eq!(m.handle(Signal::Tick, t0 + DEADLINE), vec![Action::SendEnd(failed.clone()), Action::SendBye(failed), Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Failed));
    }

    #[test]
    fn connecting_that_never_connects_ends_with_bye_failed() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        assert_eq!(m.deadline(), Some(t0 + DEADLINE));
        assert_eq!(m.handle(Signal::Tick, t0 + DEADLINE), vec![Action::SendBye(End::plain(Reason::Failed)), Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Failed));
    }

    #[test]
    fn the_transport_or_the_channel_closing_ends_at_once_told_through_the_server() {
        let t0 = Instant::now();
        let failed = End::plain(Reason::Failed);
        let mut m = active(t0);
        assert_eq!(m.handle(Signal::ChannelClosed, t0), vec![Action::ReleaseInput, Action::SendBye(failed.clone()), Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Failed));
        let mut m = active(t0);
        assert_eq!(m.handle(Signal::TransportClosed, t0), vec![Action::ReleaseInput, Action::SendBye(failed.clone()), Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Failed));
        // From Recovering, input was already released.
        let mut m = recovering(t0);
        assert_eq!(m.handle(Signal::TransportClosed, t0), vec![Action::SendBye(failed), Action::Close]);
    }

    #[test]
    fn the_browser_ends_it_with_its_reason() {
        let t0 = Instant::now();
        let mut m = active(t0);
        assert_eq!(m.handle(Signal::PeerEnded(Reason::Left), t0), vec![Action::ReleaseInput, Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Left));
    }

    #[test]
    fn a_bye_from_the_browser_ends_it_in_any_state() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        assert_eq!(m.handle(Signal::PeerBye(Reason::Left), t0), vec![Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Left));
        let mut m = active(t0);
        assert_eq!(m.handle(Signal::PeerBye(Reason::Left), t0), vec![Action::ReleaseInput, Action::Close]);
        let mut m = recovering(t0);
        assert_eq!(m.handle(Signal::PeerBye(Reason::Failed), t0), vec![Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Failed));
    }

    #[test]
    fn the_computer_ends_it_through_the_server_always_and_on_the_channel_while_open() {
        let t0 = Instant::now();
        let taken = End { reason: Reason::Taken, by: Some("Alex".into()), same: true };
        // Active: on the control channel and through the server.
        let mut m = active(t0);
        assert_eq!(m.handle(Signal::LocalEnd(taken.clone()), t0), vec![Action::ReleaseInput, Action::SendEnd(taken.clone()), Action::SendBye(taken.clone()), Action::Close]);
        assert_eq!(m.state(), &State::Ended(Reason::Taken));
        // Never opened: through the server, with the same reason and who.
        for r in [Reason::Taken, Reason::Ended, Reason::Unpaired, Reason::Failed] {
            let mut m = Machine::new(t0);
            let end = End { reason: r, ..taken.clone() };
            assert_eq!(m.handle(Signal::LocalEnd(end.clone()), t0), vec![Action::SendBye(end), Action::Close]);
        }
        // Recovering: both as well.
        let mut m = recovering(t0);
        let ended = End::plain(Reason::Ended);
        assert_eq!(m.handle(Signal::LocalEnd(ended.clone()), t0), vec![Action::SendEnd(ended.clone()), Action::SendBye(ended), Action::Close]);
    }

    #[test]
    fn an_ended_session_does_nothing_more() {
        let t0 = Instant::now();
        let mut m = active(t0);
        m.handle(Signal::LocalEnd(End::plain(Reason::Ended)), t0);
        for s in [Signal::ChannelClosed, Signal::IceConnected, Signal::Tick, Signal::PeerEnded(Reason::Left), Signal::PeerBye(Reason::Left), Signal::ChannelOpen] {
            assert!(m.handle(s, t0 + DEADLINE * 2).is_empty());
        }
        assert_eq!(m.state(), &State::Ended(Reason::Ended));
        assert!(!m.accepts_restart() && m.deadline().is_none());
    }

    #[test]
    fn checking_after_connected_is_recovering_and_while_connecting_nothing() {
        let t0 = Instant::now();
        let mut m = Machine::new(t0);
        m.handle(ice_signal(IceConnectionState::Checking).unwrap(), t0);
        assert_eq!(m.state(), &State::Connecting, "checking is how ICE starts");
        m.handle(Signal::ChannelOpen, t0);
        assert_eq!(m.handle(ice_signal(IceConnectionState::Checking).unwrap(), t0), vec![Action::ReleaseInput]);
        assert_eq!(m.state(), &State::Recovering);
        assert_eq!(m.deadline(), Some(t0 + DEADLINE), "with its deadline");
        m.handle(ice_signal(IceConnectionState::Connected).unwrap(), t0);
        assert_eq!(m.state(), &State::Active);
    }
}
