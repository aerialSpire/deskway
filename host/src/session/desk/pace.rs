//! Real time over completeness. When what this computer sends queues up (a relay over TCP, which
//! never drops, behind a slow or proxied link), the picture would fall seconds behind the screen.
//! Instead the encoder waits while the queue drains, and then encodes the screen as it is: the
//! screens in between are skipped, never sent late. Nothing already encoded is dropped, so the
//! next frame is an ordinary one and needs no keyframe.
//!
//! Pure: the session feeds it the queued bytes and its bitrate, and tells the encoder on change.

/// About a quarter of a second of sending is queued before encoding waits; never less than a
/// large frame, which is queued whole the moment it is sent.
fn allowance(bitrate: u32) -> usize {
    (bitrate as usize / 8 / 4).max(128 * 1024)
}

#[derive(Default)]
pub struct Pace {
    behind: bool,
}

impl Pace {
    /// `queued`: bytes waiting to leave this computer; `bitrate`: what the session sends at.
    /// Some(behind) when that changed: encoding waits from a full queue, and resumes once half
    /// of it has gone, so one frame does not flip it back and forth.
    pub fn update(&mut self, queued: usize, bitrate: u32) -> Option<bool> {
        self.update_limit(queued, allowance(bitrate))
    }

    /// Sharp video can wait for the previous frame instead of accumulating frames in the pacer.
    pub fn update_sharp(&mut self, queued: usize, bitrate: u32) -> Option<bool> {
        self.update_limit(queued, (bitrate as usize / 8 / 3).max(4 * 1024))
    }

    fn update_limit(&mut self, queued: usize, limit: usize) -> Option<bool> {
        let behind = if self.behind { queued > limit / 2 } else { queued > limit };
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
    fn encoding_waits_while_the_queue_is_full_and_resumes_when_half_has_gone() {
        let mut p = Pace::default();
        let rate = 4_000_000; // a quarter second is 125 000 bytes; the floor of 128 KiB holds
        assert_eq!(p.update(0, rate), None);
        assert_eq!(p.update(100_000, rate), None, "less than the allowance is normal");
        assert_eq!(p.update(140_000, rate), Some(true));
        assert_eq!(p.update(200_000, rate), None);
        assert_eq!(p.update(80_000, rate), None, "still more than half");
        assert_eq!(p.update(60_000, rate), Some(false));
        assert!(!p.behind());
    }

    #[test]
    fn a_faster_link_may_queue_more_bytes_for_the_same_time() {
        let mut p = Pace::default();
        assert_eq!(p.update(1_000_000, 40_000_000), None, "a quarter second at 40 Mbit/s is 1.25 MB");
        assert_eq!(p.update(1_300_000, 40_000_000), Some(true));
    }
}
