//! The workspace across a pipe (Windows): the network service, which is not the person, asks
//! the user worker, which is (`service::worker`). Requests carry an id and are answered in any
//! order: a `run` that waits does not hold up a file read behind it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Answer, Code, Op, WorkError, Workspace};
use crate::service::ipc::Pipe;

#[derive(Serialize, Deserialize)]
struct Request {
    id: u64,
    op: Op,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    id: u64,
    answer: Answer,
}

/// The network service's side: the worker connected now, if any, and the requests it holds.
#[derive(Default)]
pub struct Remote {
    worker: Mutex<Option<Arc<Pipe>>>,
    waiting: Mutex<HashMap<u64, Sender<Answer>>>,
    next: AtomicU64,
    /// The administrator's worker: absent, it is no administrator, not nobody signed in.
    admin: bool,
}

impl Remote {
    pub fn new(admin: bool) -> Remote {
        Remote { admin, ..Default::default() }
    }

    /// Why a request finds no worker.
    fn absent(&self) -> WorkError {
        if self.admin {
            WorkError::new(Code::ElevationUnavailable, "there are no administrator rights to use here now", "The person signed in must be an administrator; check computer_info, or run without elevated.")
        } else {
            WorkError::new(Code::NoUserSession, "nobody is signed in at this computer", "Sign in at its sign-in screen first (screenshot, then type).")
        }
    }

    /// Serves one connected worker until it leaves. The requests it still held fail: their
    /// programs run on, and proc_list finds them when it is back.
    pub fn attach(&self, pipe: Arc<Pipe>) -> Result<()> {
        *self.worker.lock().unwrap() = Some(pipe.clone());
        let served = loop {
            match pipe.recv_json::<Reply>() {
                Ok(r) => {
                    if let Some(tx) = self.waiting.lock().unwrap().remove(&r.id) {
                        let _ = tx.send(r.answer);
                    }
                }
                Err(e) => break Err(e),
            }
        };
        *self.worker.lock().unwrap() = None;
        for (_, tx) in self.waiting.lock().unwrap().drain() {
            let _ = tx.send(Err(WorkError::new(Code::Failed, "the person's session ended before it answered", "Check computer_info, then proc_list.")));
        }
        served
    }
}

impl Workspace for Remote {
    fn available(&self) -> bool {
        self.worker.lock().unwrap().is_some()
    }

    fn call(&self, op: Op) -> Answer {
        let pipe = self.worker.lock().unwrap().clone().ok_or_else(|| self.absent())?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.waiting.lock().unwrap().insert(id, tx);
        if pipe.send_json(&Request { id, op }).is_err() {
            self.waiting.lock().unwrap().remove(&id);
            return Err(self.absent());
        }
        rx.recv().unwrap_or_else(|_| Err(self.absent()))
    }
}

/// The worker's side: answers every request, each on a thread of its own, until the pipe closes.
pub fn serve(pipe: Arc<Pipe>, ws: Arc<dyn Workspace>) -> Result<()> {
    loop {
        let Request { id, op } = pipe.recv_json()?;
        let (pipe, ws) = (pipe.clone(), ws.clone());
        std::thread::spawn(move || {
            let answer = ws.call(op);
            let _ = pipe.send_json(&Reply { id, answer });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ipc;
    use serde_json::json;

    /// Answers each request with what it was, after a while for `run`.
    struct Echo;
    impl Workspace for Echo {
        fn call(&self, op: Op) -> Answer {
            match op {
                Op::Run { wait_ms, .. } => {
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                    Ok(json!("ran"))
                }
                Op::FsStat { path } => Err(WorkError::new(Code::NotFound, path, "")),
                other => Ok(serde_json::to_value(other).unwrap()),
            }
        }
    }

    #[test]
    fn requests_cross_the_pipe_and_a_slow_one_holds_up_nothing() {
        let name = format!(r"\\.\pipe\deskway-test-ws-{}", std::process::id());
        let sddl = format!("D:P(A;;GA;;;{})", ipc::own_sid().unwrap());
        let server = Arc::new(Pipe::create(&name, &sddl).unwrap());
        let accepting = {
            let s = server.clone();
            std::thread::spawn(move || s.accept())
        };
        let client = Arc::new(Pipe::connect(&name).unwrap());
        accepting.join().unwrap().unwrap();
        std::thread::spawn(move || serve(client, Arc::new(Echo)));

        let remote = Arc::new(Remote::new(false));
        assert!(!remote.available());
        assert_eq!(remote.call(Op::ProcList).unwrap_err().code, Code::NoUserSession, "no worker, no session");
        assert_eq!(Remote::new(true).call(Op::ProcList).unwrap_err().code, Code::ElevationUnavailable, "no admin worker, no administrator");
        let attached = {
            let (r, s) = (remote.clone(), server.clone());
            std::thread::spawn(move || r.attach(s))
        };
        while !remote.available() {
            std::thread::yield_now();
        }
        let slow = {
            let r = remote.clone();
            std::thread::spawn(move || r.call(Op::Run { command: "x".into(), cwd: None, wait_ms: 400, elevated: false }))
        };
        let t = std::time::Instant::now();
        assert_eq!(remote.call(Op::ProcList).unwrap(), json!({ "op": "proc_list" }));
        assert!(t.elapsed() < std::time::Duration::from_millis(300), "answered while run waits");
        assert_eq!(remote.call(Op::FsStat { path: "p".into() }).unwrap_err().code, Code::NotFound, "errors cross too");
        assert_eq!(slow.join().unwrap().unwrap(), json!("ran"));
        server.disconnect();
        assert!(attached.join().unwrap().is_err());
        assert!(!remote.available(), "gone with the pipe");
    }
}
