//! Minimal TURN client (RFC 8656), for when a direct path cannot be found. It reaches the
//! relay over UDP, or TCP on networks that block UDP. The relayed traffic is still end-to-end
//! encrypted by WebRTC; the relay only forwards it.

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use serde_json::Value;
use sha1::Sha1;

use crate::{NetEvent, Stop};

const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

const ALLOCATE: u16 = 0x0003;
const ALLOCATE_OK: u16 = 0x0103;
const ALLOCATE_ERR: u16 = 0x0113;
const REFRESH: u16 = 0x0004;
const REFRESH_ERR: u16 = 0x0114;
const CREATE_PERMISSION: u16 = 0x0008;
const CREATE_PERMISSION_ERR: u16 = 0x0118;
const SEND_INDICATION: u16 = 0x0016;
const DATA_INDICATION: u16 = 0x0017;

const A_USERNAME: u16 = 0x0006;
const A_INTEGRITY: u16 = 0x0008;
const A_ERROR: u16 = 0x0009;
const A_LIFETIME: u16 = 0x000D;
const A_PEER: u16 = 0x0012;
const A_DATA: u16 = 0x0013;
const A_REALM: u16 = 0x0014;
const A_NONCE: u16 = 0x0015;
const A_RELAYED: u16 = 0x0016;
const A_TRANSPORT: u16 = 0x0019;

const REFRESH_EVERY: Duration = Duration::from_secs(240);
const PERMISSION_EVERY: Duration = Duration::from_secs(240);

#[derive(Clone, Debug, PartialEq)]
pub struct TurnServer {
    pub addr: SocketAddr,
    pub tcp: bool,
    pub username: String,
    pub credential: String,
}

/// Reads the TURN servers out of the offer's RTCIceServer list.
pub fn servers(ice: &Value) -> Vec<TurnServer> {
    let mut out = Vec::new();
    for s in ice.as_array().into_iter().flatten() {
        let (Some(user), Some(cred)) = (s["username"].as_str(), s["credential"].as_str()) else { continue };
        for url in s["urls"].as_array().into_iter().flatten().filter_map(|u| u.as_str()) {
            let Some(rest) = url.strip_prefix("turn:") else { continue };
            let (target, query) = rest.split_once('?').unwrap_or((rest, ""));
            let target = if target.contains(':') { target.to_string() } else { format!("{target}:3478") };
            let Some(addr) = target.to_socket_addrs().ok().and_then(|mut a| a.find(|x| x.is_ipv4())) else { continue };
            out.push(TurnServer { addr, tcp: query.contains("transport=tcp"), username: user.to_string(), credential: cred.to_string() });
        }
    }
    out
}

#[derive(Clone)]
struct Auth {
    username: String,
    realm: String,
    nonce: String,
    key: [u8; 16],
}

fn long_term_key(username: &str, realm: &str, credential: &str) -> [u8; 16] {
    Md5::digest(format!("{username}:{realm}:{credential}")).into()
}

fn transaction_id() -> [u8; 12] {
    // Not a secret: it only has to differ between requests, and message integrity covers it.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u32);
    let mut id = [0u8; 12];
    id[..8].copy_from_slice(&n.to_be_bytes());
    id[8..].copy_from_slice(&t.to_be_bytes());
    id
}

fn put_attr(m: &mut Vec<u8>, typ: u16, val: &[u8]) {
    m.extend_from_slice(&typ.to_be_bytes());
    m.extend_from_slice(&(val.len() as u16).to_be_bytes());
    m.extend_from_slice(val);
    m.resize(m.len() + (4 - val.len() % 4) % 4, 0);
}

fn set_length(m: &mut [u8], len: usize) {
    m[2..4].copy_from_slice(&(len as u16).to_be_bytes());
}

fn build(typ: u16, id: &[u8; 12], attrs: &[(u16, &[u8])], auth: Option<&Auth>) -> Vec<u8> {
    let mut m = Vec::with_capacity(128);
    m.extend_from_slice(&typ.to_be_bytes());
    m.extend_from_slice(&[0, 0]);
    m.extend_from_slice(&MAGIC);
    m.extend_from_slice(id);
    for (t, v) in attrs {
        put_attr(&mut m, *t, v);
    }
    if let Some(a) = auth {
        put_attr(&mut m, A_USERNAME, a.username.as_bytes());
        put_attr(&mut m, A_REALM, a.realm.as_bytes());
        put_attr(&mut m, A_NONCE, a.nonce.as_bytes());
        // The length field already counts the integrity attribute when the hash is taken.
        let len = m.len() - 20 + 24;
        set_length(&mut m, len);
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&a.key).expect("any key length works");
        mac.update(&m);
        let tag = mac.finalize().into_bytes();
        put_attr(&mut m, A_INTEGRITY, &tag);
    } else {
        let len = m.len() - 20;
        set_length(&mut m, len);
    }
    m
}

fn xor_addr(a: SocketAddr) -> Option<[u8; 8]> {
    let IpAddr::V4(ip) = a.ip() else { return None };
    let (p, o) = ((a.port() ^ 0x2112).to_be_bytes(), ip.octets());
    Some([0, 1, p[0], p[1], o[0] ^ MAGIC[0], o[1] ^ MAGIC[1], o[2] ^ MAGIC[2], o[3] ^ MAGIC[3]])
}

fn parse_xor_addr(v: &[u8]) -> Option<SocketAddr> {
    if v.len() < 8 || v[1] != 1 {
        return None;
    }
    let port = u16::from_be_bytes([v[2], v[3]]) ^ 0x2112;
    Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(v[4] ^ MAGIC[0], v[5] ^ MAGIC[1], v[6] ^ MAGIC[2], v[7] ^ MAGIC[3])), port))
}

struct Message<'a> {
    typ: u16,
    id: &'a [u8],
    attrs: Vec<(u16, &'a [u8])>,
}

impl<'a> Message<'a> {
    fn parse(b: &'a [u8]) -> Option<Message<'a>> {
        if b.len() < 20 || b[4..8] != MAGIC || b[0] & 0xC0 != 0 {
            return None;
        }
        let len = u16::from_be_bytes([b[2], b[3]]) as usize;
        let body = b.get(20..20 + len)?;
        let mut attrs = Vec::new();
        let mut p = 0;
        while p + 4 <= body.len() {
            let t = u16::from_be_bytes([body[p], body[p + 1]]);
            let l = u16::from_be_bytes([body[p + 2], body[p + 3]]) as usize;
            attrs.push((t, body.get(p + 4..p + 4 + l)?));
            p += 4 + l.div_ceil(4) * 4;
        }
        Some(Message { typ: u16::from_be_bytes([b[0], b[1]]), id: &b[8..20], attrs })
    }

    fn attr(&self, t: u16) -> Option<&'a [u8]> {
        self.attrs.iter().find(|(k, _)| *k == t).map(|(_, v)| *v)
    }

    fn error_code(&self) -> Option<u16> {
        let v = self.attr(A_ERROR)?;
        (v.len() >= 4).then(|| v[2] as u16 * 100 + v[3] as u16)
    }

    fn text(&self, t: u16) -> Option<String> {
        self.attr(t).map(|v| String::from_utf8_lossy(v).into_owned())
    }
}

/// The system's send buffer of a TCP relay: small, so a slow link shows as bytes queued here,
/// where the session sees them (`queued`) and the encoder waits (session::pace), instead of
/// seconds of video hidden in the system's buffer.
const TCP_SEND_BUFFER: usize = 64 * 1024;
/// A last bound on what a TCP relay queues: beyond it packets are dropped, as a congested link
/// would. The encoder waiting keeps the queue far below it.
const TCP_QUEUE_MAX: usize = 4 * 1024 * 1024;

/// Writes never block the session. Over TCP a thread writes the queue out, and the bytes it
/// still holds are counted.
enum Writer {
    Udp(Arc<UdpSocket>),
    Tcp { queue: Sender<Vec<u8>>, queued: Arc<AtomicUsize> },
}

impl Writer {
    fn send(&self, m: Vec<u8>) {
        match self {
            Writer::Udp(s) => {
                let _ = s.send(&m);
            }
            Writer::Tcp { queue, queued } => {
                let n = m.len();
                if queued.load(Ordering::Relaxed) + n > TCP_QUEUE_MAX {
                    return;
                }
                queued.fetch_add(n, Ordering::Relaxed);
                if queue.send(m).is_err() {
                    queued.fetch_sub(n, Ordering::Relaxed);
                }
            }
        }
    }

    /// Bytes handed to this relay and not yet given to the system (none over UDP).
    fn queued(&self) -> usize {
        match self {
            Writer::Udp(_) => 0,
            Writer::Tcp { queued, .. } => queued.load(Ordering::Relaxed),
        }
    }
}

enum Reader {
    Udp(Arc<UdpSocket>, Vec<u8>),
    Tcp(TcpStream, Vec<u8>),
}

impl Reader {
    /// Returns one complete message, or None when nothing arrived before the read timeout.
    fn next(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        match self {
            Reader::Udp(s, buf) => match s.recv(buf) {
                Ok(n) => Ok(Some(buf[..n].to_vec())),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset) => Ok(None),
                Err(e) => Err(e),
            },
            Reader::Tcp(s, pending) => loop {
                if pending.len() >= 4 {
                    let total = 20 + u16::from_be_bytes([pending[2], pending[3]]) as usize;
                    let total = total + (4 - total % 4) % 4;
                    if pending.len() >= total {
                        let rest = pending.split_off(total);
                        return Ok(Some(std::mem::replace(pending, rest)));
                    }
                }
                let mut chunk = [0u8; 8192];
                match s.read(&mut chunk) {
                    Ok(0) => return Err(ErrorKind::UnexpectedEof.into()),
                    Ok(n) => pending.extend_from_slice(&chunk[..n]),
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(None),
                    Err(e) => return Err(e),
                }
            },
        }
    }
}

fn open(server: &TurnServer) -> Result<(Writer, Reader, SocketAddr)> {
    let timeout = Some(Duration::from_millis(200));
    if server.tcp {
        let stream = TcpStream::connect_timeout(&server.addr, Duration::from_secs(2)).context("connect")?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(timeout)?;
        socket2::SockRef::from(&stream).set_send_buffer_size(TCP_SEND_BUFFER)?;
        let local = stream.local_addr()?;
        let mut writer = stream.try_clone()?;
        let (tx, rx) = channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let left = queued.clone();
        std::thread::Builder::new().name("turn-write".into()).spawn(move || {
            while let Ok(m) = rx.recv() {
                let n = m.len();
                let written = writer.write_all(&m);
                left.fetch_sub(n, Ordering::Relaxed);
                if written.is_err() {
                    break;
                }
            }
        })?;
        Ok((Writer::Tcp { queue: tx, queued }, Reader::Tcp(stream, Vec::new()), local))
    } else {
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(server.addr)?;
        socket.set_read_timeout(timeout)?;
        let local = socket.local_addr()?;
        let socket = Arc::new(socket);
        Ok((Writer::Udp(socket.clone()), Reader::Udp(socket, vec![0u8; 2048]), local))
    }
}

/// Sends a request until its answer arrives, skipping anything else.
fn exchange(w: &Writer, r: &mut Reader, make: impl Fn(&[u8; 12]) -> Vec<u8>) -> Result<Vec<u8>> {
    for _ in 0..6 {
        let id = transaction_id();
        w.send(make(&id));
        let until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < until {
            if let Some(raw) = r.next()? {
                if Message::parse(&raw).is_some_and(|m| m.id == id) {
                    return Ok(raw);
                }
            }
        }
    }
    bail!("no answer")
}

/// A live allocation on a relay.
pub struct Relay {
    pub addr: SocketAddr,
    pub local: SocketAddr,
    /// The relay server and how it is reached: one allocation per server at a time.
    pub server: SocketAddr,
    pub tcp: bool,
    writer: Writer,
    auth: Arc<Mutex<Auth>>,
    permissions: Arc<Mutex<HashMap<IpAddr, Instant>>>,
    stop: Stop,
}

impl Relay {
    /// Allocates a relayed address and starts forwarding what arrives on it to the connection's
    /// owner, as `NetEvent::Udp`.
    pub fn start<E: From<NetEvent> + Send + 'static>(server: &TurnServer, owner: Sender<E>, stop: Stop) -> Result<Relay> {
        let (writer, mut reader, local) = open(server)?;
        let transport = [17u8, 0, 0, 0]; // relay over UDP, whatever reaches the server
        let raw = exchange(&writer, &mut reader, |id| build(ALLOCATE, id, &[(A_TRANSPORT, &transport)], None))?;
        let first = Message::parse(&raw).context("bad response")?;
        let (realm, nonce) = (first.text(A_REALM).context("no realm")?, first.text(A_NONCE).context("no nonce")?);
        let auth = Auth {
            key: long_term_key(&server.username, &realm, &server.credential),
            username: server.username.clone(),
            realm,
            nonce,
        };
        let raw = exchange(&writer, &mut reader, |id| build(ALLOCATE, id, &[(A_TRANSPORT, &transport)], Some(&auth)))?;
        let reply = Message::parse(&raw).context("bad response")?;
        match reply.typ {
            ALLOCATE_OK => {}
            ALLOCATE_ERR => return Err(anyhow!("allocate refused: {:?}", reply.error_code())),
            t => return Err(anyhow!("unexpected reply {t:#x}")),
        }
        let addr = reply.attr(A_RELAYED).and_then(parse_xor_addr).context("no relayed address")?;

        let auth = Arc::new(Mutex::new(auth));
        let permissions = Arc::new(Mutex::new(HashMap::new()));
        let (write2, auth2, perms2, reader_stop) = (clone_writer(&writer), auth.clone(), permissions.clone(), stop.clone());
        std::thread::Builder::new().name("turn-read".into()).spawn(move || {
            run_reader(reader, write2, auth2, perms2, addr, owner, reader_stop);
        })?;
        Ok(Relay { addr, local, server: server.addr, tcp: server.tcp, writer, auth, permissions, stop })
    }

    /// Gives the allocation up: the reader stops and releases the relay's port.
    pub fn close(&self) {
        self.stop.close();
    }

    /// Bytes waiting to leave through this relay (the host paces its video by it).
    pub fn queued(&self) -> usize {
        self.writer.queued()
    }

    /// Sends one packet to a peer through the relay.
    pub fn send(&self, peer: SocketAddr, data: &[u8]) {
        let Some(xor) = xor_addr(peer) else { return };
        let due = {
            let mut p = self.permissions.lock().unwrap();
            let due = p.get(&peer.ip()).is_none_or(|t| t.elapsed() > PERMISSION_EVERY);
            if due {
                p.insert(peer.ip(), Instant::now());
            }
            due
        };
        if due {
            let auth = self.auth.lock().unwrap().clone();
            self.writer.send(build(CREATE_PERMISSION, &transaction_id(), &[(A_PEER, &xor)], Some(&auth)));
        }
        self.writer.send(build(SEND_INDICATION, &transaction_id(), &[(A_PEER, &xor), (A_DATA, data)], None));
    }
}

fn clone_writer(w: &Writer) -> Writer {
    match w {
        Writer::Udp(s) => Writer::Udp(s.clone()),
        Writer::Tcp { queue, queued } => Writer::Tcp { queue: queue.clone(), queued: queued.clone() },
    }
}

fn run_reader<E: From<NetEvent>>(
    mut reader: Reader,
    writer: Writer,
    auth: Arc<Mutex<Auth>>,
    permissions: Arc<Mutex<HashMap<IpAddr, Instant>>>,
    relay: SocketAddr,
    owner: Sender<E>,
    stop: Stop,
) {
    let mut last_refresh = Instant::now();
    let refresh = |lifetime: u32| {
        let a = auth.lock().unwrap().clone();
        let life = lifetime.to_be_bytes();
        writer.send(build(REFRESH, &transaction_id(), &[(A_LIFETIME, &life)], Some(&a)));
    };
    while !stop.stopped() {
        if last_refresh.elapsed() >= REFRESH_EVERY {
            last_refresh = Instant::now();
            refresh(600);
        }
        let raw = match reader.next() {
            Ok(Some(raw)) => raw,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("relay connection lost: {e}");
                return;
            }
        };
        let Some(m) = Message::parse(&raw) else { continue };
        match m.typ {
            DATA_INDICATION => {
                let (Some(peer), Some(data)) = (m.attr(A_PEER).and_then(parse_xor_addr), m.attr(A_DATA)) else { continue };
                if owner.send(NetEvent::Udp { data: data.to_vec(), source: peer, destination: relay, slot: None }.into()).is_err() {
                    return;
                }
            }
            REFRESH_ERR | CREATE_PERMISSION_ERR if matches!(m.error_code(), Some(401 | 438)) => {
                // The nonce went stale: take the new one and let the request happen again.
                if let Some(n) = m.text(A_NONCE) {
                    auth.lock().unwrap().nonce = n;
                }
                if m.typ == REFRESH_ERR {
                    refresh(600);
                } else {
                    permissions.lock().unwrap().clear();
                }
            }
            _ => {}
        }
    }
    refresh(0); // releases the relay port right away instead of when it times out
    std::thread::sleep(Duration::from_millis(50));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TCP relay the network does not keep up with: what waits is counted here (the system's
    /// buffer is small), never beyond the last bound, and goes once the other end reads.
    #[test]
    fn a_tcp_relay_counts_what_waits_and_bounds_it() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = TurnServer { addr: listener.local_addr().unwrap(), tcp: true, username: String::new(), credential: String::new() };
        let (writer, _reader, _) = open(&server).unwrap();
        let (peer, _) = listener.accept().unwrap();
        socket2::SockRef::from(&peer).set_recv_buffer_size(64 * 1024).unwrap();
        for _ in 0..64 {
            writer.send(vec![0u8; 100_000]);
        }
        std::thread::sleep(Duration::from_millis(200));
        let waiting = writer.queued();
        assert!(waiting > 2_000_000, "a peer that reads nothing leaves most of it waiting here: {waiting}");
        assert!(waiting <= TCP_QUEUE_MAX, "never more than the bound: {waiting}");
        let mut peer = peer;
        let reader = std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            while peer.read(&mut buf).is_ok_and(|n| n > 0) {}
        });
        let until = Instant::now() + Duration::from_secs(10);
        while writer.queued() > 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(writer.queued(), 0, "read: nothing waits any more");
        drop(writer);
        drop(reader);
    }

    fn auth() -> Auth {
        Auth { username: "u".into(), realm: "deskway".into(), nonce: "n".into(), key: long_term_key("u", "deskway", "p") }
    }

    #[test]
    fn long_term_key_is_md5_of_the_triple() {
        // md5("u:deskway:p")
        let expected = Md5::digest(b"u:deskway:p");
        assert_eq!(long_term_key("u", "deskway", "p").as_slice(), expected.as_slice());
    }

    #[test]
    fn message_round_trip_with_padding() {
        let m = build(SEND_INDICATION, &[7; 12], &[(A_DATA, b"hello")], None);
        assert_eq!(m.len() % 4, 0, "attributes are padded to four bytes");
        let p = Message::parse(&m).unwrap();
        assert_eq!(p.typ, SEND_INDICATION);
        assert_eq!(p.id, &[7; 12]);
        assert_eq!(p.attr(A_DATA), Some(&b"hello"[..]));
    }

    #[test]
    fn integrity_covers_the_message_with_its_final_length() {
        let a = auth();
        let m = build(ALLOCATE, &[1; 12], &[(A_TRANSPORT, &[17, 0, 0, 0])], Some(&a));
        let p = Message::parse(&m).unwrap();
        assert_eq!(p.text(A_USERNAME).as_deref(), Some("u"));
        let tag = p.attr(A_INTEGRITY).unwrap();
        assert_eq!(tag.len(), 20);
        let covered = &m[..m.len() - 24];
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&a.key).unwrap();
        mac.update(covered);
        assert_eq!(mac.finalize().into_bytes().as_slice(), tag);
        assert_eq!(u16::from_be_bytes([m[2], m[3]]) as usize, m.len() - 20);
    }

    #[test]
    fn xor_address_round_trip() {
        let a: SocketAddr = "203.0.113.9:5000".parse().unwrap();
        assert_eq!(parse_xor_addr(&xor_addr(a).unwrap()), Some(a));
        assert_eq!(xor_addr("[2001:db8::1]:5000".parse().unwrap()), None);
    }

    #[test]
    fn reads_servers_from_the_offer() {
        let ice = serde_json::json!([
            { "urls": ["stun:192.0.2.1:3478"] },
            { "urls": ["stun:192.0.2.1:3478", "turn:192.0.2.1:3478?transport=udp", "turn:192.0.2.1:3478?transport=tcp"], "username": "u", "credential": "p" },
        ]);
        let list = servers(&ice);
        assert_eq!(list.len(), 2);
        assert!(!list[0].tcp);
        assert!(list[1].tcp);
        assert_eq!(list[1].addr, "192.0.2.1:3478".parse().unwrap());
        assert_eq!(list[1].username, "u");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Message::parse(&[0u8; 8]).is_none());
        let mut m = build(SEND_INDICATION, &[0; 12], &[(A_DATA, b"x")], None);
        m[4] = 0;
        assert!(Message::parse(&m).is_none(), "wrong magic cookie");
        let mut m = build(SEND_INDICATION, &[0; 12], &[(A_DATA, b"x")], None);
        m[3] = 200;
        assert!(Message::parse(&m).is_none(), "length past the end");
    }
}
