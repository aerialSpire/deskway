//! The Host's on-demand browse of Deskway local services on the LAN.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use deskway_server_connection::Address;
use mdns_sd::{ServiceDaemon, ServiceEvent};

const SERVICE: &str = "_deskway._tcp.local.";
const VERSION: &str = "1";
const STOP_POLL: Duration = Duration::from_millis(200);
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);

pub(crate) struct Server {
    pub name: String,
    pub address: Address,
}

fn from_records<'a>(
    host: &str,
    port: u16,
    txt: impl Fn(&str) -> Option<&'a str>,
) -> Option<Server> {
    if txt("v")? != VERSION || port == 0 {
        return None;
    }
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    let label = host.strip_suffix(".local")?;
    if label.is_empty()
        || label.len() > 63
        || label.starts_with('-')
        || label.ends_with('-')
        || !label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return None;
    }
    let name = txt("name")?;
    if name.is_empty() || name != name.trim() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return None;
    }
    let address = Address::parse(&format!("https://{host}:{port}#ca={}", txt("fp")?)).ok()?;
    Some(Server { name: name.into(), address })
}

pub(crate) enum Change {
    Update { instance: String, server: Option<Server> },
    Reset,
}

/// Dropping the consumer disconnects the stop channel. The worker owns daemon shutdown.
pub(crate) struct Browse {
    _stop: Sender<()>,
}

impl Browse {
    pub fn start(changed: impl FnMut(Change) + Send + 'static) -> std::io::Result<Self> {
        let (stop, stopped) = mpsc::channel();
        thread::Builder::new().name("local-discovery".into()).spawn(move || run(stopped, changed))?;
        Ok(Self { _stop: stop })
    }
}

fn run(stop: Receiver<()>, mut changed: impl FnMut(Change)) {
    let mut delay = Duration::from_secs(1);
    while stop.try_recv() == Err(TryRecvError::Empty) {
        let started = Instant::now();
        let Err(error) = browse_once(&stop, &mut changed) else { return };
        if stop.try_recv() != Err(TryRecvError::Empty) {
            return;
        }
        // A full event queue closes the subscription and can lose removals. Start with a
        // fresh cache after a delay so a burst cannot keep restarting the daemon.
        eprintln!("local discovery: {error}; restarting browse");
        changed(Change::Reset);
        if started.elapsed() >= MAX_RESTART_DELAY {
            delay = Duration::from_secs(1);
        }
        if stop.recv_timeout(delay) != Err(RecvTimeoutError::Timeout) {
            return;
        }
        delay = (delay * 2).min(MAX_RESTART_DELAY);
    }
}

fn browse_once(stop: &Receiver<()>, changed: &mut impl FnMut(Change)) -> Result<()> {
    let daemon = ServiceDaemon::new()?;
    let result = match daemon.browse(SERVICE) {
        Ok(events) => receive(&events, stop, changed),
        Err(error) => Err(error.into()),
    };
    if let Ok(stopped) = daemon.shutdown() {
        let _ = stopped.recv();
    }
    result
}

fn receive(
    events: &mdns_sd::Receiver<ServiceEvent>,
    stop: &Receiver<()>,
    changed: &mut impl FnMut(Change),
) -> Result<()> {
    while stop.try_recv() == Err(TryRecvError::Empty) {
        if events.is_disconnected() {
            return Err(anyhow!("mDNS subscription disconnected"));
        }
        match events.recv_timeout(STOP_POLL) {
            Ok(ServiceEvent::ServiceResolved(info)) => changed(Change::Update {
                instance: info.get_fullname().to_ascii_lowercase(),
                server: from_records(info.get_hostname(), info.get_port(), |key| info.get_property_val_str(key)),
            }),
            Ok(ServiceEvent::ServiceRemoved(_, instance)) => changed(Change::Update {
                instance: instance.to_ascii_lowercase(),
                server: None,
            }),
            Ok(ServiceEvent::SearchStopped(_)) => return Err(anyhow!("mDNS subscription stopped")),
            Err(mdns_sd::RecvTimeoutError::Disconnected) => return Err(anyhow!("mDNS subscription disconnected")),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn maps_srv_and_txt_to_a_pinned_server() {
        let contract: Value = serde_json::from_str(include_str!("../../proto/local-discovery.json")).unwrap();
        let records = &contract["records"];
        let srv = &records["srv"];
        let txt = &records["txt"];
        let host = srv["host"].as_str().unwrap();
        let port = srv["port"].as_u64().unwrap().try_into().unwrap();
        let fingerprint = txt["fp"].as_str().unwrap();
        let expected = contract["address"].as_str().unwrap();
        assert_eq!(contract["service"], SERVICE);
        let fields = contract["txt"]["fields"].as_object().unwrap();
        assert_eq!(fields.len(), 4);
        for (key, optional) in [("v", false), ("fp", false), ("name", false), ("guide", true)] {
            assert_eq!(fields[key]["type"], "string");
            assert_eq!(fields[key]["optional"], optional);
        }
        assert_eq!(fields["v"]["enum"], json!([VERSION]));
        let min = fields["name"]["minLength"].as_u64().unwrap() as usize;
        let max = fields["name"]["maxLength"].as_u64().unwrap() as usize;

        for (host, port, change, want) in [
            (host, port, None, Some(expected)),
            ("SEAN-ASUS.LOCAL.", port, Some(("fp", Some(fingerprint.to_ascii_uppercase()))), Some(expected)),
            (host, port, Some(("guide", None)), Some(expected)),
            (host, port, Some(("v", Some("2".into()))), None),
            (host, port, Some(("v", None)), None),
            (host, port, Some(("fp", Some("invalid".into()))), None),
            (host, port, Some(("fp", None)), None),
            (host, port, Some(("name", None)), None),
            (host, port, Some(("name", Some(" ".into()))), None),
            (host, port, Some(("name", Some("客".repeat(min - 1)))), None),
            (host, port, Some(("name", Some("客".repeat(min)))), Some(expected)),
            (host, port, Some(("name", Some("客".repeat(max)))), Some(expected)),
            (host, port, Some(("name", Some("客".repeat(max + 1)))), None),
            (host, port, Some(("name", Some(" Hub".into()))), None),
            (host, port, Some(("name", Some("Hub\u{a0}".into()))), None),
            (host, port, Some(("name", Some("H\u{85}ub".into()))), None),
            (host, 0, None, None),
            ("hub.example.", port, None, None),
            ("hub@other.local.", port, None, None),
        ] {
            let property = |key: &str| {
                if let Some((changed_key, value)) = &change {
                    if *changed_key == key {
                        return value.as_deref();
                    }
                }
                txt[key].as_str()
            };
            let server = from_records(host, port, property);
            assert_eq!(server.as_ref().map(|s| s.address.saved()).as_deref(), want, "{host}:{port} {change:?}");
            if let Some(server) = server {
                assert_eq!(server.name, property("name").unwrap());
            }
        }
    }

    #[test]
    fn a_stopped_subscription_is_reported_to_the_worker() {
        let daemon = ServiceDaemon::new_with_port(0).unwrap();
        let events = daemon.browse(SERVICE).unwrap();
        daemon.stop_browse(SERVICE).unwrap();
        let (_stop, stopped) = mpsc::channel();
        let result = receive(&events, &stopped, &mut |_| {});
        let _ = daemon.shutdown().unwrap().recv();
        assert!(result.is_err());
    }
}
