//! Whether the server is still there. A connection whose path broke without a word (a router
//! or the provider dropping its mapping) looks exactly like a quiet one: reads only time out,
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
    heard: Instant,
    pinged: Instant,
}

impl Alive {
    pub fn new(now: Instant) -> Alive {
        Alive { heard: now, pinged: now }
    }

    /// Something came from the server.
    pub fn heard(&mut self, now: Instant) {
        self.heard = now;
    }

    pub fn due(&mut self, now: Instant) -> Due {
        if now.saturating_duration_since(self.heard) >= SILENT {
            return Due::Gone;
        }
        if now.saturating_duration_since(self.pinged) >= PING_EVERY {
            self.pinged = now;
            return Due::Ping;
        }
        Due::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pings_on_schedule_and_gives_up_only_after_silence() {
        let t0 = Instant::now();
        let s = |secs| t0 + Duration::from_secs(secs);
        let mut a = Alive::new(t0);
        // (seconds since start, heard from the server just before, what is due)
        for (at, heard, want) in [
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
