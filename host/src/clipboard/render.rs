//! Delayed rendering, the waiting part: the OS asks for data behind a placeholder and blocks
//! the pasting program until it gets it, so the platform's clipboard thread waits here for
//! the browser's answer, up to a deadline.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::Data;
use wire::types::FormatKind;

/// The browser's answer to a render request of generation `gen`. None: nothing (cancelled,
/// failed or replaced); the paste gets nothing.
#[derive(Debug)]
pub struct Answer {
    pub gen: u64,
    pub data: Option<Data>,
}

pub struct RenderWait {
    rx: Receiver<Answer>,
}

impl RenderWait {
    /// The waiter and where answers are sent.
    pub fn new() -> (Sender<Answer>, RenderWait) {
        let (tx, rx) = channel();
        (tx, RenderWait { rx })
    }

    /// Waits for `kind` of generation `gen` until `deadline` has passed. Answers left over
    /// from earlier pastes or for another format are dropped; nothing comes back on timeout.
    pub fn wait(&self, gen: u64, kind: FormatKind, deadline: Duration) -> Option<Data> {
        let end = Instant::now() + deadline;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Answer { gen: g, data }) if g == gen => match data {
                    Some(d) if d.kind() == kind => return Some(d),
                    Some(_) => continue,
                    None => return None,
                },
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_its_own_generation_and_format() {
        let (tx, w) = RenderWait::new();
        tx.send(Answer { gen: 3, data: Some(Data::Text("old".into())) }).unwrap();
        tx.send(Answer { gen: 4, data: Some(Data::Html("<i>".into())) }).unwrap();
        tx.send(Answer { gen: 4, data: Some(Data::Text("new".into())) }).unwrap();
        assert_eq!(w.wait(4, FormatKind::Text, Duration::from_secs(5)), Some(Data::Text("new".into())));
    }

    #[test]
    fn a_cancelled_or_late_answer_gives_nothing() {
        let (tx, w) = RenderWait::new();
        tx.send(Answer { gen: 5, data: None }).unwrap();
        assert_eq!(w.wait(5, FormatKind::Image, Duration::from_secs(5)), None);
        let started = Instant::now();
        assert_eq!(w.wait(6, FormatKind::Text, Duration::from_millis(50)), None, "deadline");
        assert!(started.elapsed() >= Duration::from_millis(50));
        // An answer arriving after its paste gave up does not answer the next one.
        tx.send(Answer { gen: 6, data: Some(Data::Text("late".into())) }).unwrap();
        tx.send(Answer { gen: 7, data: Some(Data::Text("right".into())) }).unwrap();
        assert_eq!(w.wait(7, FormatKind::Text, Duration::from_secs(5)), Some(Data::Text("right".into())));
    }
}
