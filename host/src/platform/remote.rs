//! The platform of the Windows agent, seen from the network service: every call goes over the
//! agent pipe as a [`Call`], every event comes back as a [`PlatformEvent`]. Nothing here knows
//! any one capability.
//!
//! The agent can restart (a new console session, a crash) while the service keeps running.
//! Calls that set state are kept, the latest per slot, and replayed in their original order
//! to each agent that connects.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;

use super::{Call, Forward, Platform, PlatformEvent};
use crate::service::ipc::Pipe;

pub struct Remote {
    shared: Mutex<Shared>,
    events: Sender<PlatformEvent>,
}

#[derive(Default)]
struct Shared {
    /// The connected agent, if any.
    link: Option<Arc<Pipe>>,
    next: u64,
    kept: BTreeMap<&'static str, (u64, Call)>,
}

impl Remote {
    /// Events from whichever agent is connected go to `events`.
    pub fn new(events: Sender<PlatformEvent>) -> Arc<Remote> {
        Arc::new(Remote { shared: Mutex::new(Shared::default()), events })
    }

    /// Serves one authenticated agent connection until it ends.
    pub fn attach(&self, link: Arc<Pipe>) -> Result<()> {
        let replayed = {
            // Held while replaying: a call made meanwhile goes out after the replay, not before.
            let mut s = self.shared.lock().unwrap();
            s.link = Some(link.clone());
            let mut kept: Vec<&(u64, Call)> = s.kept.values().collect();
            kept.sort_by_key(|(n, _)| *n);
            kept.into_iter().try_for_each(|(_, c)| link.send(c))
        };
        let result = replayed.and_then(|()| loop {
            match link.recv::<PlatformEvent>() {
                Ok(e) => {
                    let _ = self.events.send(e);
                }
                Err(e) => break Err(e),
            }
        });
        self.shared.lock().unwrap().link = None;
        result
    }
}

impl Forward for Remote {
    fn forward(&self, call: Call) {
        let link = {
            let mut s = self.shared.lock().unwrap();
            if let Some(slot) = call.slot() {
                s.next += 1;
                let n = s.next;
                s.kept.insert(slot, (n, call.clone()));
            }
            s.link.clone()
        };
        // Written outside the lock: a slow write never holds anyone else up.
        if let Some(l) = link {
            let _ = l.send(&call);
        }
    }
}

/// The agent's side: carries out the calls arriving on `link` on `platform`, and sends its
/// `events` back, until the connection ends.
pub fn serve(link: &Pipe, platform: &dyn Platform, events: Receiver<PlatformEvent>) -> Result<()> {
    let done = &AtomicBool::new(false);
    std::thread::scope(|scope| {
        // One writer, so events never interleave.
        scope.spawn(move || {
            while !done.load(Ordering::Relaxed) {
                match events.recv_timeout(Duration::from_millis(200)) {
                    Ok(e) => {
                        if link.send(&e).is_err() {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        let result = loop {
            match link.recv::<Call>() {
                Ok(call) => call.apply(platform),
                Err(e) => break Err(e),
            }
        };
        done.store(true, Ordering::Relaxed);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Bytes;
    use crate::service::ipc;
    use std::sync::mpsc::channel;

    /// Records calls, and answers a key press with an event, as an agent's platform would.
    struct Agent {
        calls: Mutex<Vec<Call>>,
        events: Sender<PlatformEvent>,
    }
    impl Forward for Agent {
        fn forward(&self, call: Call) {
            if let Call::Key { code, .. } = &call {
                let _ = self.events.send(PlatformEvent::FileMessage(code.clone()));
            }
            self.calls.lock().unwrap().push(call);
        }
    }

    fn pair(name: &str) -> (Arc<Pipe>, Arc<Pipe>) {
        let sddl = format!("D:P(A;;GA;;;{})", ipc::own_sid().unwrap());
        let server = Pipe::create(name, &sddl).unwrap();
        let name = name.to_string();
        let client = std::thread::spawn(move || Pipe::connect(&name).unwrap());
        server.accept().unwrap();
        (Arc::new(server), Arc::new(client.join().unwrap()))
    }

    fn wait_for(agent: &Agent, n: usize) -> Vec<Call> {
        for _ in 0..200 {
            if agent.calls.lock().unwrap().len() >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        agent.calls.lock().unwrap().clone()
    }

    #[test]
    fn calls_and_events_round_trip_and_state_is_replayed_to_a_new_agent() {
        let (events_tx, events) = channel();
        let remote = Remote::new(events_tx);
        // Before any agent: kept, and a key press is simply lost.
        remote.auto_lock(true);
        remote.video_start("s1".into(), Default::default(), None);
        remote.choose_display("s1", "B".into());
        // A takeover: the new session starts before the old one's stop arrives. Both replay, in
        // order, and the agent's `Local` keeps video for the newer one.
        remote.video_start("s2".into(), Default::default(), Some("A".into()));
        remote.video_stop("s1");
        remote.key("s2", "KeyQ".into(), true);
        remote.auto_lock(false);

        let (agent_events_tx, agent_events) = channel();
        let agent = Arc::new(Agent { calls: Mutex::new(Vec::new()), events: agent_events_tx });
        let (net_end, agent_end) = pair(&format!(r"\\.\pipe\deskway-test-remote-{}", std::process::id()));
        let keep = net_end.clone();
        let r = remote.clone();
        let attached = std::thread::spawn(move || r.attach(net_end));
        let a = agent.clone();
        let served = std::thread::spawn(move || serve(&agent_end, &*a, agent_events));

        // The latest of each, in the order they were made.
        let replayed = wait_for(&agent, 4);
        assert_eq!(
            replayed,
            vec![Call::ChooseDisplay { session: "s1".into(), id: "B".into() }, Call::VideoStart { session: "s2".into(), view: Default::default(), display: Some("A".into()) }, Call::VideoStop { session: "s1".into() }, Call::AutoLock { on: false }]
        );

        remote.key("s2", "KeyA".into(), true);
        remote.file_data("s2", Bytes(vec![9; 40_000]));
        let calls = wait_for(&agent, 6);
        assert_eq!(calls[4], Call::Key { session: "s2".into(), code: "KeyA".into(), down: true });
        assert_eq!(calls[5], Call::FileData { session: "s2".into(), data: Bytes(vec![9; 40_000]) });
        assert_eq!(events.recv_timeout(Duration::from_secs(5)).unwrap(), PlatformEvent::FileMessage("KeyA".into()));

        // The connection ends: both sides finish, and later calls wait for the next agent.
        keep.disconnect();
        assert!(attached.join().unwrap().is_err());
        assert!(served.join().unwrap().is_err());
        remote.key("s2", "KeyB".into(), true);
        assert_eq!(agent.calls.lock().unwrap().len(), 6);
    }
}
