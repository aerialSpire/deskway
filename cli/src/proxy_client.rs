//! The MCP process's persistent connection to its profile's local proxy.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::store::ProxyPlace;

/// The proxy, connected again when its connection is lost (it stopped, or was replaced).
pub struct Proxy {
    conn: Mutex<Option<Arc<Conn>>>,
    next: Mutex<u64>,
}

/// One connection to the proxy: requests out, each answer to whoever waits for its id.
struct Conn {
    writer: Mutex<TcpStream>,
    waiting: Mutex<HashMap<u64, Sender<Value>>>,
    closed: AtomicBool,
}

/// Tells a running proxy to leave (signing out); nothing if none runs.
pub fn stop_proxy() {
    let Ok(place) = ProxyPlace::load() else { return };
    if let Ok(mut s) = TcpStream::connect(("127.0.0.1", place.port)) {
        let _ = writeln!(s, "{}", json!({ "token": place.token }));
        let _ = writeln!(s, "{}", json!({ "id": 1, "op": "quit" }));
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = BufReader::new(s).lines().next();
    }
}

impl Conn {
    /// Connects to the proxy, starting it if none runs. `newest`: this process was started
    /// from the current program file, so it replaces a proxy of another version.
    fn open(newest: bool) -> Result<Arc<Conn>> {
        let own = crate::update::version();
        let want = newest && ProxyPlace::load().is_ok_and(|p| p.version != own);
        if want {
            stop_proxy();
        }
        let try_once = || -> Result<TcpStream> {
            let place = ProxyPlace::load()?;
            if want && place.version != own {
                bail!("the proxy of another version is still there");
            }
            let mut s = TcpStream::connect(("127.0.0.1", place.port))?;
            writeln!(s, "{}", json!({ "token": place.token }))?;
            Ok(s)
        };
        let stream = match try_once() {
            Ok(s) => s,
            Err(_) => {
                crate::os::start_proxy()?;
                let until = Instant::now() + Duration::from_secs(10);
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    match try_once() {
                        Ok(s) => break s,
                        Err(e) if Instant::now() > until => return Err(e).context("the proxy did not start (see proxy.log)"),
                        Err(_) => {}
                    }
                }
            }
        };
        let c = Arc::new(Conn { writer: Mutex::new(stream.try_clone()?), waiting: Mutex::new(HashMap::new()), closed: AtomicBool::new(false) });
        let reader = c.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                let Ok(m) = serde_json::from_str::<Value>(&line) else { continue };
                let Some(id) = m["id"].as_u64() else { continue };
                if let Some(tx) = reader.waiting.lock().unwrap().remove(&id) {
                    let _ = tx.send(m);
                }
            }
            // The proxy is gone: whoever waits hears it, and the next call connects again.
            reader.closed.store(true, Ordering::SeqCst);
            reader.waiting.lock().unwrap().clear();
        });
        Ok(c)
    }
}

impl Proxy {
    pub fn connect() -> Result<Arc<Proxy>> {
        Ok(Arc::new(Proxy { conn: Mutex::new(Some(Conn::open(true)?)), next: Mutex::new(0) }))
    }

    /// Reconnect to the running proxy, which a newer process may have started.
    fn conn(&self) -> Result<Arc<Conn>> {
        let mut c = self.conn.lock().unwrap();
        if let Some(open) = c.as_ref().filter(|c| !c.closed.load(Ordering::SeqCst)) {
            return Ok(open.clone());
        }
        let open = Conn::open(false)?;
        *c = Some(open.clone());
        Ok(open)
    }

    /// One request, answered. A call under way when the proxy goes is not made again (it may
    /// have been carried out): it fails, and the next one connects again.
    pub fn ask(&self, m: Value) -> Result<Value> {
        self.ask_until(m, None)
    }

    pub fn ask_until(&self, mut m: Value, deadline: Option<Instant>) -> Result<Value> {
        let conn = self.conn()?;
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        m["id"] = json!(id);
        let mut writer = conn.writer.lock().unwrap();
        if let Some(until) = deadline {
            // Instants are process-local. Send an absolute deadline, so time in transit counts.
            let now = SystemTime::now();
            let remaining = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())
                .context("the proxy request expired before sending")?;
            m["deadline"] = json!(now.checked_add(remaining).context("the proxy deadline is out of range")?);
        }
        let (tx, rx) = channel();
        conn.waiting.lock().unwrap().insert(id, tx);
        if conn.closed.load(Ordering::SeqCst) {
            conn.waiting.lock().unwrap().remove(&id);
            bail!("the proxy stopped");
        }
        if let Err(e) = writeln!(writer, "{m}") {
            conn.waiting.lock().unwrap().remove(&id);
            conn.closed.store(true, Ordering::SeqCst);
            return Err(e).context("the proxy");
        }
        drop(writer);
        let answer = match deadline {
            None => rx.recv().context("the proxy stopped")?,
            Some(until) => match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(answer) => answer,
                Err(e) => {
                    conn.waiting.lock().unwrap().remove(&id);
                    return Err(e).context("waiting for the proxy");
                }
            },
        };
        if let Some(e) = answer["error"].as_str() {
            bail!("{e}");
        }
        Ok(answer["result"].clone())
    }
}
