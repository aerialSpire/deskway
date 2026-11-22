//! A short window of reliable lossless updates, released when the browser has applied them.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use str0m::{channel::ChannelId, Rtc};

const MAX_IN_FLIGHT: usize = 24;
const ACK_TIMEOUT: Duration = Duration::from_secs(25);
static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);

#[derive(Default)]
pub(super) struct Screen {
    pub channel: Option<ChannelId>,
    pub supported: bool,
    pub allowed: bool,
    pub epoch: u32,
    request_pending: bool,
    pending: Option<Vec<u8>>,
    resume_video: bool,
    next_seq: u32,
    flight: VecDeque<(u32, usize, Instant)>,
    bytes: usize,
    window: usize,
}

impl Screen {
    pub fn active(&self) -> bool {
        self.epoch != 0
    }

    pub fn start(&mut self, bitrate: u32) {
        self.epoch = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed).max(1);
        // Roughly half a second of video payload. A single tile may exceed this window;
        // allowing it into an empty window keeps photographic tiles moving too.
        self.window = (bitrate as usize / 16).clamp(4 * 1024, 24 * 1024);
        self.pending = None;
        self.request_pending = false;
        self.resume_video = false;
    }

    pub fn stop(&mut self) {
        self.epoch = 0;
        self.pending = None;
        self.request_pending = false;
        self.flight.clear();
        self.bytes = 0;
        self.resume_video = self.channel.is_some();
    }

    pub fn ack(&mut self, data: &[u8]) {
        let Ok(raw) = <[u8; 4]>::try_from(data) else { return };
        let seq = u32::from_le_bytes(raw);
        if self.flight.front().is_some_and(|&(first, _, _)| first == seq) {
            let (_, bytes, _) = self.flight.pop_front().expect("front just matched");
            self.bytes -= bytes;
        }
    }

    pub fn timed_out(&self, now: Instant) -> bool {
        self.flight.front().is_some_and(|&(_, _, sent)| now - sent >= ACK_TIMEOUT)
    }

    pub fn receive(&mut self, packet: Vec<u8>) {
        self.request_pending = false;
        let mut data = self.next_seq.to_le_bytes().to_vec();
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend(packet);
        self.pending = Some(data);
    }

    /// Advances the channel by one update, then takes another capture credit if it fits.
    pub fn pump(&mut self, rtc: &mut Rtc, relay_queued: usize, now: Instant) -> bool {
        let Some(mut ch) = self.channel.and_then(|id| rtc.channel(id)) else { return false };
        if self.resume_video {
            if matches!(ch.write(false, b"video"), Ok(true)) {
                self.resume_video = false;
            }
            return false;
        }
        if !self.active() || relay_queued >= self.window || ch.buffered_amount() >= self.window {
            return false;
        }
        if let Some(packet) = self.pending.as_ref() {
            if self.bytes != 0 && self.bytes + packet.len() > self.window {
                return false;
            }
            if !matches!(ch.write(true, packet), Ok(true)) {
                return false;
            }
            let bytes = packet.len();
            self.pending = None;
            self.flight.push_back((self.next_seq, bytes, now));
            self.bytes += bytes;
            self.next_seq = self.next_seq.wrapping_add(1);
        }
        if self.request_pending || self.flight.len() >= MAX_IN_FLIGHT || self.bytes >= self.window {
            return false;
        }
        self.request_pending = true;
        true
    }
}
