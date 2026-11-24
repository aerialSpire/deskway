//! Real time over completeness. When what this computer sends queues up (a relay over TCP, which
//! never drops, behind a slow or proxied link), the picture would fall seconds behind the screen.
//! Instead the encoder waits while the queue drains, and then encodes the screen as it is: the
//! screens in between are skipped, never sent late. Nothing already encoded is dropped, so the
//! next frame is an ordinary one and needs no keyframe.
//!
//! Pure: the session feeds it the queued bytes and its bitrate, and tells the encoder on change.

use std::time::Duration;

/// The TWCC estimate is shared with control, retransmissions and protocol overhead.
pub(super) fn video_budget(estimate: u32, ceiling: u32) -> u32 {
    (u64::from(estimate) * 4 / 5).clamp(1, u64::from(ceiling.max(1))) as u32
}

#[derive(Default)]
pub struct Pace {
    behind: bool,
}

impl Pace {
    // These thresholds gate the next encode. One large access unit may exceed the budget.
    pub fn update(&mut self, queued: usize, bitrate: u32, oldest: Duration) -> Option<bool> {
        let millis = if self.behind { 50 } else { 100 };
        let limit = u64::from(bitrate) * millis / 8000;
        let behind = queued != 0 && (queued as u64 > limit || oldest > Duration::from_millis(millis));
        (behind != self.behind).then(|| {
            self.behind = behind;
            behind
        })
    }

    pub fn behind(&self) -> bool {
        self.behind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_budget_scales_with_bandwidth_and_resumes_at_half() {
        let mut pace = Pace::default();
        let rate = video_budget(200_000, 6_000_000);
        assert_eq!(rate, 160_000);
        assert_eq!(pace.update(2000, rate, Duration::ZERO), None);
        assert_eq!(pace.update(2001, rate, Duration::ZERO), Some(true));
        assert_eq!(pace.update(1001, rate, Duration::ZERO), None);
        assert_eq!(pace.update(1000, rate, Duration::ZERO), Some(false));
        assert_eq!(pace.update(1, rate, Duration::from_millis(101)), Some(true));
        assert_eq!(pace.update(0, rate, Duration::from_secs(1)), Some(false));
        assert_eq!(video_budget(500_000, 200_000), 200_000);
    }
}
