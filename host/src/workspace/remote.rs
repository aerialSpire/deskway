//! The workspace across a pipe (Windows): the network service, which is not the person, asks
//! the user worker, which is (`service::worker`). Requests carry an id and are answered in any
//! order: a `run` that waits does not hold up a file read behind it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::workspace::{Answer, Code, DesktopOp, Op, WorkError, Workspace};
use crate::service::ipc::Pipe;
use crate::platform::WorkspaceState;
use crate::signal::Control;

mod browser;

#[derive(Serialize, Deserialize)]
struct Request {
    id: u64,
    op: RequestOp,
}

// Both variants use the original {"op": "<tool>", ...} form on the pipe.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RequestOp {
    Work(Op),
    Desktop(DesktopOp),
    Browser(browser::Operation),
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Answer { id: u64, answer: Answer },
    Workspace(WorkspaceState),
}

struct Link {
    pipe: Arc<Pipe>,
    next: AtomicU64,
    waiting: Mutex<Option<HashMap<u64, Sender<Answer>>>>,
    state: Mutex<WorkspaceState>,
}

impl Link {
    fn send(&self, op: RequestOp, reply: Option<Sender<Answer>>) -> Option<()> {
        let mut guard = self.waiting.lock().unwrap();
        let waiting = guard.as_mut()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if let Some(reply) = reply {
            waiting.insert(id, reply);
        }
        if self.pipe.send_json(&Request { id, op }).is_err() {
            waiting.remove(&id);
            return None;
        }
        Some(())
    }

    fn request(&self, op: RequestOp) -> Option<Answer> {
        let (tx, rx) = channel();
        self.send(op, Some(tx))?;
        rx.recv().ok()
    }
}

/// The network service's side: the worker connected now, if any, and the requests it holds.
#[derive(Default)]
pub struct Remote {
    worker: Mutex<Option<Arc<Link>>>,
    workspace_updates: Option<Sender<Control>>,
    /// The administrator's worker: absent, it is no administrator, not nobody signed in.
    admin: bool,
}

impl Remote {
    pub fn new(admin: bool) -> Remote {
        Remote { admin, ..Default::default() }
    }

    pub fn user(updates: Sender<Control>) -> Remote {
        Remote { workspace_updates: Some(updates), ..Default::default() }
    }

    fn state_changed(&self, state: WorkspaceState) {
        if let Some(updates) = &self.workspace_updates {
            let _ = updates.send(Control::WorkspaceChanged(state));
        }
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
        let link = Arc::new(Link {
            pipe: pipe.clone(), next: AtomicU64::new(0),
            waiting: Mutex::new(Some(HashMap::new())), state: Mutex::new(WorkspaceState::NO_USER),
        });
        *self.worker.lock().unwrap() = Some(link.clone());
        let served = loop {
            match pipe.recv_json::<Reply>() {
                Ok(Reply::Answer { id, answer }) => {
                    if let Some(tx) = link.waiting.lock().unwrap().as_mut().and_then(|pending| pending.remove(&id)) {
                        let _ = tx.send(answer);
                    }
                }
                Ok(Reply::Workspace(state)) => {
                    *link.state.lock().unwrap() = state;
                    self.state_changed(state);
                }
                Err(e) => break Err(e),
            }
        };
        *self.worker.lock().unwrap() = None;
        let pending = link.waiting.lock().unwrap().take().unwrap_or_default();
        self.state_changed(WorkspaceState::NO_USER);
        for (_, tx) in pending {
            let _ = tx.send(Err(WorkError::new(Code::Failed, "the person's session ended before it answered", "Check computer_info, then proc_list.")));
        }
        served
    }

    fn request(&self, op: RequestOp) -> Answer {
        let link = self.worker.lock().unwrap().clone().ok_or_else(|| self.absent())?;
        link.request(op).unwrap_or_else(|| Err(self.absent()))
    }
}

impl Workspace for Remote {
    fn available(&self) -> bool {
        self.worker.lock().unwrap().is_some()
    }

    fn call(&self, op: Op) -> Answer {
        self.request(RequestOp::Work(op))
    }

    fn desktop(&self, op: DesktopOp) -> Answer {
        self.request(RequestOp::Desktop(op))
    }
}

/// The worker's side: answers every request, each on a thread of its own, until the pipe closes.
pub fn serve(pipe: Arc<Pipe>, ws: Arc<dyn Workspace>, user: bool) -> Result<()> {
    let notices = pipe.clone();
    let watch = user.then(|| crate::platform::workspace::WorkspaceWatch::start(move |state| {
        let _ = notices.send_json(&Reply::Workspace(state));
    }).ok()).flatten();
    let browser = watch.as_ref().map(|_| Arc::new(browser::Worker::new(crate::platform::workspace::browser(None))));
    serve_requests(pipe, ws, browser)
}

fn serve_requests(pipe: Arc<Pipe>, ws: Arc<dyn Workspace>, browser: Option<Arc<browser::Worker>>) -> Result<()> {
    let result = loop {
        let Request { id, op } = match pipe.recv_json() {
            Ok(request) => request,
            Err(error) => break Err(error),
        };
        let (pipe, ws, browser) = (pipe.clone(), ws.clone(), browser.clone());
        std::thread::spawn(move || {
            let answer = match op {
                RequestOp::Work(op) => ws.call(op),
                RequestOp::Desktop(op) => ws.desktop(op),
                RequestOp::Browser(op) => browser.as_ref().ok_or_else(|| WorkError::new(Code::Denied, "no browser workspace", ""))
                    .and_then(|worker| worker.call(op)),
            };
            let _ = pipe.send_json(&Reply::Answer { id, answer });
        });
    };
    if let Some(browser) = browser { browser.disconnect(); }
    result
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

        fn desktop(&self, op: DesktopOp) -> Answer {
            Ok(serde_json::to_value(op).unwrap())
        }
    }

    #[test]
    fn workspace_requests_keep_their_pipe_shape() {
        for (op, wire_op) in [
            (RequestOp::Work(Op::FsStat { path: "p".into() }), json!({ "op": "fs_stat", "path": "p" })),
            (RequestOp::Desktop(DesktopOp::ClipboardSet { text: "t".into() }), json!({ "op": "clipboard_set", "text": "t" })),
        ] {
            let wire = json!({ "id": 1, "op": wire_op });
            assert_eq!(serde_json::to_value(Request { id: 1, op }).unwrap(), wire);
            let decoded: Request = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
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
        std::thread::spawn(move || serve(client, Arc::new(Echo), false));

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
