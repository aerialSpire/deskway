//! The Host's on-demand browse of Deskway local services on the LAN.

use std::thread::{self, JoinHandle};

use anyhow::Result;
use deskway_server_connection::Address;
use mdns_sd::{ServiceDaemon, ServiceEvent};

const SERVICE: &str = "_deskway._tcp.local.";
const VERSION: &str = "1";

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
    if name.trim().is_empty() {
        return None;
    }
    let address = Address::parse(&format!("https://{host}:{port}#ca={}", txt("fp")?)).ok()?;
    Some(Server { name: name.into(), address })
}

pub(crate) struct Change {
    pub instance: String,
    pub server: Option<Server>,
}

/// A fresh DNS-SD cache and subscription for the lifetime of a consumer.
pub(crate) struct Browse {
    daemon: ServiceDaemon,
    worker: Option<JoinHandle<()>>,
}

impl Browse {
    pub fn start(mut changed: impl FnMut(Change) + Send + 'static) -> Result<Self> {
        let mut browse = Self { daemon: ServiceDaemon::new()?, worker: None };
        let events = browse.daemon.browse(SERVICE)?;
        browse.worker = Some(thread::Builder::new().name("local-discovery".into()).spawn(move || {
            while let Ok(event) = events.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => changed(Change {
                        instance: info.get_fullname().to_ascii_lowercase(),
                        server: from_records(info.get_hostname(), info.get_port(), |key| info.get_property_val_str(key)),
                    }),
                    ServiceEvent::ServiceRemoved(_, instance) => changed(Change {
                        instance: instance.to_ascii_lowercase(),
                        server: None,
                    }),
                    ServiceEvent::SearchStopped(_) => break,
                    _ => {}
                }
            }
        })?);
        Ok(browse)
    }
}

impl Drop for Browse {
    fn drop(&mut self) {
        if let Ok(stopped) = self.daemon.shutdown() {
            let _ = stopped.recv();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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

        for (host, port, change, want) in [
            (host, port, None, Some(expected)),
            ("SEAN-ASUS.LOCAL.", port, Some(("fp", Some(fingerprint.to_ascii_uppercase()))), Some(expected)),
            (host, port, Some(("guide", None)), Some(expected)),
            (host, port, Some(("v", Some("2".into()))), None),
            (host, port, Some(("fp", Some("invalid".into()))), None),
            (host, port, Some(("fp", None)), None),
            (host, port, Some(("name", Some(" ".into()))), None),
            (host, 0, None, None),
            ("hub.example.", port, None, None),
            ("hub@other.local.", port, None, None),
        ] {
            let server = from_records(host, port, |key| {
                if let Some((changed_key, value)) = &change {
                    if *changed_key == key {
                        return value.as_deref();
                    }
                }
                txt[key].as_str()
            });
            assert_eq!(server.as_ref().map(|s| s.address.saved()).as_deref(), want, "{host}:{port} {change:?}");
            if let Some(server) = server {
                assert_eq!(server.name, txt["name"].as_str().unwrap());
            }
        }
    }
}
