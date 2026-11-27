//! A local HTTPS peer for the server connection and CLI sign-in tests.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use wire::crypto::{hex, sha256};

pub fn listen() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").unwrap()
}

pub struct TestTls {
    server: Arc<ServerConfig>,
    pub root: Vec<u8>,
}

impl TestTls {
    pub fn new() -> Self {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name.push(
            DnType::CommonName,
            format!("Deskway test CA {}", hex(&wire::crypto::random::<8>())),
        );
        let issuer = Issuer::new(params.clone(), KeyPair::generate().unwrap());
        let root = params.self_signed(issuer.key()).unwrap().der().to_vec();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        let leaf = params.signed_by(&key, &issuer).unwrap();
        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
        Self {
            server: Arc::new(server),
            root,
        }
    }

    pub fn address(&self, listener: &TcpListener) -> String {
        format!(
            "https://localhost:{}#ca={}",
            listener.local_addr().unwrap().port(),
            hex(&sha256(&self.root))
        )
    }

    pub fn handshake(
        &self,
        listener: &TcpListener,
    ) -> io::Result<StreamOwned<ServerConnection, TcpStream>> {
        let mut socket = listener.accept()?.0;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut tls = ServerConnection::new(self.server.clone()).unwrap();
        tls.complete_io(&mut socket)?;
        Ok(StreamOwned::new(tls, socket))
    }

    pub fn reply(
        &self,
        listener: &TcpListener,
        request: &str,
        status: &str,
        headers: &[&str],
        body: &[u8],
    ) {
        let mut reader = BufReader::new(self.handshake(listener).unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), format!("{request} HTTP/1.1"));
        let mut bytes = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.trim_end().is_empty() {
                break;
            }
            if let Some(len) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                bytes = len.trim().parse().unwrap();
            }
        }
        io::copy(&mut reader.by_ref().take(bytes), &mut io::sink()).unwrap();
        let mut tls = reader.into_inner();
        write!(
            tls,
            "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        )
        .unwrap();
        for header in headers {
            write!(tls, "{header}\r\n").unwrap();
        }
        tls.write_all(b"\r\n").unwrap();
        tls.write_all(body).unwrap();
        tls.flush().unwrap();
    }
}
