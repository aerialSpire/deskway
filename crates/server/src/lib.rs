//! The Host and CLI's connection to the Deskway server named in their sign-in.

use std::io::Read;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use native_tls::{Certificate, TlsConnector};
use tungstenite::{
    client::IntoClientRequest,
    handshake::{client::Request, HandshakeError},
    stream::MaybeTlsStream,
    Connector, WebSocket,
};
use wire::crypto::{hex, sha256};

pub struct Address {
    base: String,
    pin: Option<String>,
}

impl Address {
    pub fn parse(address: &str) -> Result<Self> {
        let (base, pin) = match address.trim().split_once('#') {
            Some((base, fragment)) => {
                let pin = fragment
                    .strip_prefix("ca=")
                    .filter(|p| p.len() == 64 && p.bytes().all(|b| b.is_ascii_hexdigit()))
                    .context("expected #ca= followed by 64 hex digits")?;
                (base, Some(pin.to_ascii_lowercase()))
            }
            None => (address.trim(), None),
        };
        let base = base.trim_end_matches('/');
        let mut base = if base.starts_with("http://") || base.starts_with("https://") {
            base.to_string()
        } else {
            format!("https://{base}")
        };
        if pin.is_some() {
            let url = url::Url::parse(&base).context("invalid local server address")?;
            ensure!(
                url.scheme() == "https"
                    && url.host().is_some()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.username().is_empty()
                    && url.password().is_none(),
                "a pinned server address must be an HTTPS origin"
            );
            base = url.origin().ascii_serialization();
        }
        Ok(Self { base, pin })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn saved(&self) -> String {
        self.pin.as_ref().map_or_else(
            || self.base.clone(),
            |pin| format!("{}#ca={pin}", self.base),
        )
    }

    pub fn is_pinned(&self) -> bool {
        self.pin.is_some()
    }

    pub fn fingerprint(&self) -> Option<&str> {
        self.pin.as_deref()
    }

    /// The only unverified request. It carries no credentials and does not follow redirects.
    /// The root endpoint serves DER even though the saved certificate is PEM.
    pub fn fetch_root(&self) -> Result<Option<String>> {
        if !self.is_pinned() {
            return Ok(None);
        }
        let tls = TlsConnector::builder()
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            .build()?;
        let http = ureq::builder()
            .redirects(0)
            .timeout(Duration::from_secs(30))
            .tls_connector(Arc::new(tls))
            .build();
        let response = http
            .get(&format!("{}/local/ca.crt", self.base))
            .call()
            .context("fetch the local server root")?;
        ensure!(
            response.status() == 200,
            "the local server root request returned {}",
            response.status()
        );
        let mut der = Vec::new();
        const MAX_ROOT: u64 = 64 * 1024;
        response
            .into_reader()
            .take(MAX_ROOT + 1)
            .read_to_end(&mut der)?;
        ensure!(
            der.len() as u64 <= MAX_ROOT,
            "the local server root is too large"
        );
        Ok(Some(self.pin_root(&der)?))
    }

    pub fn pin_root(&self, der: &[u8]) -> Result<String> {
        self.check_pin(der)?;
        Certificate::from_der(der).context("invalid local server root")?;
        Ok(pem_rfc7468::encode_string(
            "CERTIFICATE",
            pem_rfc7468::LineEnding::LF,
            der,
        )?)
    }

    fn check_pin(&self, der: &[u8]) -> Result<()> {
        ensure!(
            self.pin.as_deref() == Some(hex(&sha256(der)).as_str()),
            "local server root fingerprint does not match the address"
        );
        Ok(())
    }

    pub fn connection(&self, root: Option<&str>) -> Result<Connection> {
        let tls = if self.is_pinned() {
            let der = root_der(root.context("the pinned local server root is missing")?)?;
            self.check_pin(&der)?;
            Some(
                TlsConnector::builder()
                    .add_root_certificate(Certificate::from_der(&der)?)
                    .build()?,
            )
        } else {
            None
        };
        Ok(Connection {
            base: self.base.clone(),
            tls,
        })
    }
}

pub fn root_der(pem: &str) -> Result<Vec<u8>> {
    let (label, der) = pem_rfc7468::decode_vec(pem.as_bytes())?;
    ensure!(label == "CERTIFICATE", "expected a PEM certificate");
    Ok(der)
}

#[cfg(test)]
mod test_tls;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_tls::{listen, TestTls};

    #[test]
    fn parses_server_addresses() {
        let pin = "ab".repeat(32);
        for (input, base, saved) in [
            (
                " desk.example.com/ ".to_string(),
                "https://desk.example.com",
                "https://desk.example.com".to_string(),
            ),
            (
                "http://127.0.0.1:8080/".to_string(),
                "http://127.0.0.1:8080",
                "http://127.0.0.1:8080".to_string(),
            ),
            (
                format!(" https://HUB.local:18599/#ca={} ", pin.to_uppercase()),
                "https://hub.local:18599",
                format!("https://hub.local:18599#ca={pin}"),
            ),
            (
                format!("[::1]:18599#ca={pin}"),
                "https://[::1]:18599",
                format!("https://[::1]:18599#ca={pin}"),
            ),
        ] {
            let address = Address::parse(&input).unwrap();
            assert_eq!(address.base(), base);
            assert_eq!(address.saved(), saved);
        }
        for invalid in [
            "https://hub.local#ca=abc".to_string(),
            "https://hub.local#setup=x".to_string(),
            format!("https://hub.local#ca={}", "g".repeat(64)),
            format!("http://hub.local#ca={pin}"),
            format!("https://hub.local/subpath#ca={pin}"),
            format!("https://hub.local/?query#ca={pin}"),
            format!("https://user@hub.local#ca={pin}"),
        ] {
            assert!(Address::parse(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn refuses_a_different_root_on_first_contact() {
        let listener = listen();
        let trusted = TestTls::new();
        let other = TestTls::new();
        let address = Address::parse(&trusted.address(&listener)).unwrap();
        let serve = std::thread::spawn(move || {
            other.reply(&listener, "GET /local/ca.crt", "200 OK", &[], &other.root);
        });
        let error = address.fetch_root().unwrap_err();
        assert!(
            error.to_string().contains("fingerprint does not match"),
            "{error:#}"
        );
        serve.join().unwrap();
    }

    #[test]
    fn trusts_the_pinned_chain_and_rejects_another_chain() {
        let listener = listen();
        let trusted = TestTls::new();
        let address = Address::parse(&trusted.address(&listener)).unwrap();
        let serve = std::thread::spawn(move || {
            trusted.reply(&listener, "GET /local/ca.crt", "200 OK", &[], &trusted.root);
            trusted.reply(&listener, "GET /check", "200 OK", &[], b"trusted");
            trusted.reply(&listener, "GET /ws/client", "401 Unauthorized", &[], b"");
            let other = TestTls::new();
            if let Ok(mut tls) = other.handshake(&listener) {
                // Schannel can report a trust failure after the TLS handshake completes.
                // The client must never send an HTTP request over that connection.
                assert!(!matches!(tls.read(&mut [0]), Ok(1)));
            }
        });
        let root = address.fetch_root().unwrap().unwrap();
        let connection = address.connection(Some(&root)).unwrap();
        let http = connection
            .http(ureq::builder().timeout(Duration::from_secs(5)))
            .unwrap();
        let url = connection.url("/check");
        assert_eq!(
            http.get(&url).call().unwrap().into_string().unwrap(),
            "trusted"
        );
        let error = connection.websocket("/ws/client", |_| Ok(())).unwrap_err();
        assert!(
            matches!(error.downcast_ref::<tungstenite::Error>(), Some(tungstenite::Error::Http(r)) if r.status() == 401)
        );
        assert!(matches!(
            http.get(&url).call(),
            Err(ureq::Error::Transport(_))
        ));
        serve.join().unwrap();
    }
}

pub struct Connection {
    base: String,
    tls: Option<TlsConnector>,
}

impl Connection {
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn http(&self, builder: ureq::AgentBuilder) -> Result<ureq::Agent> {
        let (builder, tls) = match &self.tls {
            Some(tls) => (builder.redirects(0), tls.clone()),
            None => (builder, TlsConnector::new()?),
        };
        Ok(builder.tls_connector(Arc::new(tls)).build())
    }

    pub fn websocket(
        &self,
        path: &str,
        headers: impl FnOnce(&mut Request) -> Result<()>,
    ) -> Result<WebSocket<MaybeTlsStream<TcpStream>>> {
        let mut request =
            format!("{}{path}", self.base.replacen("http", "ws", 1)).into_client_request()?;
        headers(&mut request)?;
        let Some(tls) = &self.tls else {
            return Ok(tungstenite::connect(request)?.0);
        };
        let uri = request.uri();
        let Some(host) = uri.host() else {
            bail!("missing WebSocket hostname")
        };
        let socket =
            TcpStream::connect((host.trim_matches(['[', ']']), uri.port_u16().unwrap_or(443)))?;
        socket.set_nodelay(true)?;
        match tungstenite::client_tls_with_config(
            request,
            socket,
            None,
            Some(Connector::NativeTls(tls.clone())),
        ) {
            Ok((ws, _)) => Ok(ws),
            Err(HandshakeError::Failure(e)) => Err(e.into()),
            Err(HandshakeError::Interrupted(_)) => {
                bail!("the blocking WebSocket handshake was interrupted")
            }
        }
    }
}
