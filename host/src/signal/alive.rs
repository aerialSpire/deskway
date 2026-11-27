//! Whether the server is still there. A connection whose path broke without a word (a router
//! or the provider dropping its mapping) looks exactly like a quiet one: no frames arrive
//! and nothing written fails until TCP gives up, if anything is written at all. The server
//! pings every 15 seconds, and this side pings too, so silence for longer than `SILENT` means
//! the connection is gone and is made again.

use std::time::{Duration, Instant};

/// How often this side pings: also keeps a NAT mapping on the way warm.
pub const PING_EVERY: Duration = Duration::from_secs(15);
/// How long the server may say nothing (its pings, pongs or messages) before the connection
/// counts as gone: three of its pings missed.
pub const SILENT: Duration = Duration::from_secs(45);
/// How long one write may wait for the network before the connection counts as gone.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the connection needs now.
#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    Nothing,
    Ping,
    Gone,
}

pub struct Alive {
    silence: Instant,
    ping: Instant,
    sequence: u64,
    pending: Option<Instant>,
    rtt: Option<u64>,
}

impl Alive {
    pub fn new(now: Instant) -> Alive {
        Alive {
            silence: now + SILENT, ping: now,
            sequence: 0, pending: None, rtt: None,
        }
    }

    /// Something came from the server.
    pub fn heard(&mut self, now: Instant) {
        self.silence = now + SILENT;
    }

    /// The payload ties the reply to this ping, even if an earlier one arrives late.
    pub fn ping_payload(&mut self, now: Instant) -> [u8; 8] {
        self.sequence = self.sequence.wrapping_add(1);
        self.pending = Some(now);
        self.sequence.to_be_bytes()
    }

    pub fn pong(&mut self, payload: &[u8], now: Instant) {
        if payload == self.sequence.to_be_bytes() {
            if let Some(sent) = self.pending.take() {
                self.rtt = Some(now.duration_since(sent).as_millis() as u64);
            }
        }
    }

    pub fn rtt(&self) -> Option<u64> {
        self.rtt
    }

    pub fn next_deadline(&self) -> Instant {
        self.ping.min(self.silence)
    }

    pub fn due(&mut self, now: Instant) -> Due {
        if now >= self.silence {
            return Due::Gone;
        }
        if now >= self.ping {
            self.ping = now + PING_EVERY;
            return Due::Ping;
        }
        Due::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_matching_pong_updates_the_latest_round_trip() {
        let start = Instant::now();
        let mut alive = Alive::new(start);
        let ping = start;
        assert_eq!(alive.due(ping), Due::Ping);
        let first = alive.ping_payload(ping);
        alive.pong(&[], ping + Duration::from_millis(2));
        assert_eq!(alive.rtt(), None);
        alive.pong(&first, ping + Duration::from_millis(18));
        assert_eq!(alive.rtt(), Some(18));
        alive.pong(&first, ping + Duration::from_millis(100));
        assert_eq!(alive.rtt(), Some(18));

        let ping = ping + PING_EVERY;
        assert_eq!(alive.due(ping), Due::Ping);
        let second = alive.ping_payload(ping);
        alive.pong(&first, ping + Duration::from_millis(3));
        assert_eq!(alive.rtt(), Some(18));
        alive.pong(&second, ping + Duration::from_micros(500));
        assert_eq!(alive.rtt(), Some(0));
    }

    #[test]
    fn next_deadline_tracks_ping_and_silence() {
        let start = Instant::now();
        let mut alive = Alive::new(start);
        assert_eq!(alive.next_deadline(), start);
        assert_eq!(alive.due(start), Due::Ping);
        assert_eq!(alive.next_deadline(), start + PING_EVERY);

        let original_silence = start + SILENT;
        let late_ping = original_silence - PING_EVERY / 2;
        assert_eq!(alive.due(late_ping), Due::Ping);
        assert_eq!(alive.next_deadline(), original_silence);

        alive.heard(late_ping);
        assert_eq!(alive.next_deadline(), late_ping + PING_EVERY);

        let silence = late_ping + SILENT;
        assert_eq!(alive.due(silence - PING_EVERY / 2), Due::Ping);
        assert_eq!(alive.next_deadline(), silence);
        assert_eq!(alive.due(silence - Duration::from_nanos(1)), Due::Nothing);
        assert_eq!(alive.due(silence), Due::Gone);
    }

    #[test]
    fn pings_on_schedule_and_gives_up_only_after_silence() {
        let t0 = Instant::now();
        let s = |secs| t0 + Duration::from_secs(secs);
        let mut a = Alive::new(t0);
        // (seconds since start, heard from the server just before, what is due)
        for (at, heard, want) in [
            (0, false, Due::Ping),
            (1, false, Due::Nothing),
            (15, false, Due::Ping),
            (16, false, Due::Nothing),
            (30, true, Due::Ping),
            (44, false, Due::Nothing),
            (45, false, Due::Ping),
            (60, false, Due::Ping),
            (74, false, Due::Nothing),
            (75, false, Due::Gone),
        ] {
            if heard {
                a.heard(s(at));
            }
            assert_eq!(a.due(s(at)), want, "at {at}s");
        }
    }
}
