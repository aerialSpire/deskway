//! MCP messages (JSON-RPC 2.0, as the client wrote them) on an AI session's "ai" data channel.
//!
//! A data channel message is kept small, so a large reply (a screenshot) never holds back the
//! ones behind it for long: a message longer than [`FRAGMENT`] goes as several frames, in order,
//! on the reliable, ordered channel. Each frame is one byte, 1 if more frames of the same
//! message follow and 0 on its last, then that part of the message's UTF-8 text. A message is at
//! most [`MAX_MESSAGE`] long.

/// The most text one frame carries.
pub const FRAGMENT: usize = 64 * 1024;
/// The longest message either end takes.
pub const MAX_MESSAGE: usize = 1024 * 1024;

/// The frames of one message, in order.
pub fn frames(message: &str) -> Vec<Vec<u8>> {
    let bytes = message.as_bytes();
    let parts: Vec<&[u8]> = if bytes.is_empty() { vec![bytes] } else { bytes.chunks(FRAGMENT).collect() };
    let last = parts.len() - 1;
    parts
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let mut f = Vec::with_capacity(p.len() + 1);
            f.push(u8::from(i != last));
            f.extend_from_slice(p);
            f
        })
        .collect()
}

/// Why frames were not a message.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Longer than `MAX_MESSAGE`: the rest of it is dropped as it comes.
    TooLarge,
    /// Not UTF-8 text, or a frame with no header.
    Malformed,
}

/// Puts frames back together into messages.
#[derive(Default)]
pub struct Joiner {
    buf: Vec<u8>,
    /// The message being received is past the limit: its frames are dropped until its last.
    skipping: bool,
}

impl Joiner {
    /// One frame. A whole message once its last frame is in.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<String>, FrameError> {
        let Some((&more, part)) = frame.split_first() else { return Err(FrameError::Malformed) };
        let more = more != 0;
        if self.skipping || self.buf.len() + part.len() > MAX_MESSAGE {
            self.buf.clear();
            self.skipping = more;
            return Err(FrameError::TooLarge);
        }
        self.buf.extend_from_slice(part);
        if more {
            return Ok(None);
        }
        let whole = std::mem::take(&mut self.buf);
        String::from_utf8(whole).map(Some).map_err(|_| FrameError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_crosses_in_frames_of_at_most_the_fragment_and_comes_back_whole() {
        let mut j = Joiner::default();
        for len in [0, 1, FRAGMENT - 1, FRAGMENT, FRAGMENT + 1, 3 * FRAGMENT + 5, MAX_MESSAGE] {
            let m: String = "é".repeat(len / 2) + &"x".repeat(len % 2);
            let fs = frames(&m);
            assert!(fs.iter().all(|f| f.len() <= FRAGMENT + 1), "{len}");
            assert_eq!(fs.iter().filter(|f| f[0] == 0).count(), 1, "one last frame");
            let mut got = None;
            for f in &fs {
                got = j.push(f).unwrap();
            }
            assert_eq!(got.as_deref(), Some(m.as_str()), "{len}");
        }
    }

    #[test]
    fn too_long_or_malformed_is_refused_and_the_next_message_still_arrives() {
        let mut j = Joiner::default();
        let big = "x".repeat(MAX_MESSAGE + 1);
        let errors: Vec<_> = frames(&big).iter().filter_map(|f| j.push(f).err()).collect();
        assert!(!errors.is_empty() && errors.iter().all(|e| *e == FrameError::TooLarge));
        assert_eq!(j.push(&frames("{}")[0]).unwrap().as_deref(), Some("{}"), "the next one is whole");
        assert_eq!(j.push(&[]), Err(FrameError::Malformed));
        assert_eq!(j.push(&[0, 0xff]), Err(FrameError::Malformed));
        assert_eq!(j.push(&frames("ok")[0]).unwrap().as_deref(), Some("ok"));
    }
}
