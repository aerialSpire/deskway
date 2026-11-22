//! What data channels could not take yet. str0m accepts a write only while SCTP has room for it
//! and drops it otherwise (`write` says false), so a large message's frames, or a tunnel's
//! bytes, wait here in order and go as room is made: `flush` after each pass over the
//! connection's output.

use std::collections::{HashMap, VecDeque};

use str0m::channel::ChannelId;
use str0m::Rtc;

/// Binary messages waiting, per channel.
#[derive(Default)]
pub struct Outbox {
    queued: HashMap<ChannelId, VecDeque<Vec<u8>>>,
}

impl Outbox {
    /// Sends `data` on channel `id` after what waits there already.
    pub fn send(&mut self, rtc: &mut Rtc, id: ChannelId, data: Vec<u8>) {
        self.queued.entry(id).or_default().push_back(data);
        self.flush_one(rtc, id);
    }

    /// Sends what waits, while the channels take it.
    pub fn flush(&mut self, rtc: &mut Rtc) {
        let ids: Vec<ChannelId> = self.queued.keys().copied().collect();
        for id in ids {
            self.flush_one(rtc, id);
        }
    }

    fn flush_one(&mut self, rtc: &mut Rtc, id: ChannelId) {
        let Some(q) = self.queued.get_mut(&id) else { return };
        while let Some(front) = q.front() {
            match rtc.channel(id).map(|mut ch| ch.write(true, front)) {
                Some(Ok(true)) => {
                    q.pop_front();
                }
                Some(Ok(false)) => return,
                // The channel is gone, or broken: what waits for it goes too.
                _ => {
                    q.clear();
                    break;
                }
            }
        }
        self.queued.remove(&id);
    }

    /// Bytes waiting for channel `id`.
    pub fn waiting(&self, id: ChannelId) -> usize {
        self.queued.get(&id).map_or(0, |q| q.iter().map(Vec::len).sum())
    }

    /// Channel `id` is closed: what waits for it goes.
    pub fn forget(&mut self, id: ChannelId) {
        self.queued.remove(&id);
    }
}
