//! STUN binding requests for discovering a public address and measuring relay latency.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::net::resolve_ipv4;

const MAGIC: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Parses "stun:host:port" (the query string, if any, is ignored).
pub fn resolve(url: &str) -> Option<SocketAddr> {
    let rest = url.strip_prefix("stun:")?;
    let rest = rest.split('?').next()?;
    let target = if rest.contains(':') { rest.to_string() } else { format!("{rest}:3478") };
    resolve_ipv4(target)
}

fn request(id: u64) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[1] = 0x01; // binding request, length 0
    m[4..8].copy_from_slice(&MAGIC);
    m[8..16].copy_from_slice(&id.to_be_bytes());
    m
}

fn parse(resp: &[u8], id: u64) -> Option<SocketAddr> {
    if resp.len() < 20 || resp[0] != 0x01 || resp[1] != 0x01 || resp[4..8] != MAGIC || resp[8..16] != id.to_be_bytes() {
        return None;
    }
    let mut p = 20;
    while p + 4 <= resp.len() {
        let typ = u16::from_be_bytes([resp[p], resp[p + 1]]);
        let len = u16::from_be_bytes([resp[p + 2], resp[p + 3]]) as usize;
        let val = resp.get(p + 4..p + 4 + len)?;
        if typ == XOR_MAPPED_ADDRESS && len >= 8 && val[1] == 1 {
            let port = u16::from_be_bytes([val[2], val[3]]) ^ 0x2112;
            let ip = Ipv4Addr::new(val[4] ^ MAGIC[0], val[5] ^ MAGIC[1], val[6] ^ MAGIC[2], val[7] ^ MAGIC[3]);
            return Some(SocketAddr::new(IpAddr::V4(ip), port));
        }
        p += 4 + len.div_ceil(4) * 4;
    }
    None
}

/// Asking one STUN server, from one local socket, which address it sees. Requests go out a
/// few times, 200 ms apart, and the answer arrives through the socket's reader like any other
/// packet, so nothing waits for it: the session goes on and offers the address when it comes.
pub struct Probe {
    pub local: SocketAddr,
    server: SocketAddr,
    id: u64,
    sent: u32,
    next: Instant,
    answered: bool,
}

const TRIES: u32 = 3;
const GAP: Duration = Duration::from_millis(200);

impl Probe {
    pub fn new(local: SocketAddr, server: SocketAddr, now: Instant) -> Self {
        Probe { local, server, id: u64::from_be_bytes(wire::crypto::random()), sent: 0, next: now, answered: false }
    }

    /// When it next wants to send, while it still may.
    pub fn due(&self) -> Option<Instant> {
        (!self.answered && self.sent < TRIES).then_some(self.next)
    }

    /// The request to send now and where to, if one is due.
    pub fn poll(&mut self, now: Instant) -> Option<([u8; 20], SocketAddr)> {
        let due = self.due()?;
        if now < due {
            return None;
        }
        self.sent += 1;
        self.next = now + GAP;
        Some((request(self.id), self.server))
    }

    /// The address the server sees, when `data` (from `source`, to `destination`) answers this
    /// probe; the first answer only.
    pub fn answer(&mut self, source: SocketAddr, destination: SocketAddr, data: &[u8]) -> Option<SocketAddr> {
        if self.answered || source != self.server || destination != self.local {
            return None;
        }
        let addr = parse(data, self.id)?;
        self.answered = true;
        Some(addr)
    }
}

#[derive(Default)]
struct RoundTrip {
    pending: Option<(u64, Instant)>,
    best: Option<Duration>,
}

impl RoundTrip {
    fn sample(&mut self, id: u64, now: Instant) -> [u8; 20] {
        self.pending = Some((id, now));
        request(id)
    }

    fn answer(&mut self, data: &[u8], now: Instant) -> bool {
        let Some((id, sent)) = self.pending else { return false };
        if parse(data, id).is_none() {
            return false;
        }
        let elapsed = now - sent;
        self.best = Some(self.best.map_or(elapsed, |best| best.min(elapsed)));
        self.pending = None;
        true
    }
}

/// Samples all relay addresses in parallel on one socket. Run off the signaling thread.
pub fn measure(servers: impl IntoIterator<Item = SocketAddr>) -> HashMap<SocketAddr, Duration> {
    let mut probes: HashMap<_, RoundTrip> = servers.into_iter().filter(SocketAddr::is_ipv4).map(|addr| (addr, RoundTrip::default())).collect();
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else { return HashMap::new() };
    let mut data = [0; 1024];
    for _ in 0..TRIES {
        let mut pending = 0;
        for (addr, probe) in &mut probes {
            let id = u64::from_be_bytes(wire::crypto::random());
            if socket.send_to(&probe.sample(id, Instant::now()), addr).is_ok() {
                pending += 1;
            } else {
                probe.pending = None;
            }
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while pending > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || socket.set_read_timeout(Some(left)).is_err() {
                break;
            }
            let (len, source) = match socket.recv_from(&mut data) {
                Ok(packet) => packet,
                Err(e) if matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::ConnectionRefused | ErrorKind::Interrupted) => continue,
                Err(_) => break,
            };
            if probes.get_mut(&source).is_some_and(|p| p.answer(&data[..len], Instant::now())) {
                pending -= 1;
            }
        }
    }
    probes.into_iter().filter_map(|(addr, probe)| Some((addr, probe.best?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xor_mapped_address() {
        let r = answer_for(42);
        assert_eq!(parse(&r, 42), Some("203.0.113.9:5000".parse().unwrap()));
        assert_eq!(parse(&r, 43), None, "a response for another transaction is ignored");
    }

    fn answer_for(id: u64) -> Vec<u8> {
        let mut r = vec![0x01, 0x01, 0, 12];
        r.extend_from_slice(&MAGIC);
        r.extend_from_slice(&id.to_be_bytes());
        r.extend_from_slice(&[0, 0, 0, 0]);
        r.extend_from_slice(&[0x00, 0x20, 0, 8, 0, 1]);
        r.extend_from_slice(&(5000u16 ^ 0x2112).to_be_bytes());
        r.extend_from_slice(&[203 ^ MAGIC[0], 0 ^ MAGIC[1], 113 ^ MAGIC[2], 9 ^ MAGIC[3]]);
        r
    }

    #[test]
    fn a_probe_sends_a_few_times_apart_and_takes_the_first_answer() {
        let (local, server): (SocketAddr, SocketAddr) = ("192.168.1.2:5000".parse().unwrap(), "192.0.2.1:3478".parse().unwrap());
        let t0 = Instant::now();
        let mut p = Probe::new(local, server, t0);
        let (req, to) = p.poll(t0).expect("the first request goes at once");
        assert_eq!(to, server);
        assert!(p.poll(t0).is_none(), "not again before the gap");
        assert!(p.poll(t0 + GAP).is_some() && p.poll(t0 + GAP * 2).is_some());
        assert!(p.poll(t0 + GAP * 3).is_none() && p.due().is_none(), "it gives up after {TRIES} tries");
        let id = u64::from_be_bytes(req[8..16].try_into().unwrap());
        let other: SocketAddr = "192.0.2.9:3478".parse().unwrap();
        assert_eq!(p.answer(other, local, &answer_for(id)), None, "only its server answers it");
        assert_eq!(p.answer(server, local, &answer_for(id + 1)), None, "only for its transaction");
        assert_eq!(p.answer(server, local, &answer_for(id)), Some("203.0.113.9:5000".parse().unwrap()), "a late answer still counts");
        assert_eq!(p.answer(server, local, &answer_for(id)), None, "the first answer only");
    }

    #[test]
    fn round_trips_keep_the_fastest_answer_and_leave_out_unanswered_probes() {
        let t0 = Instant::now();
        let mut answered = RoundTrip::default();
        let mut unanswered = RoundTrip::default();
        for (i, ms) in [80, 15, 40].into_iter().enumerate() {
            let now = t0 + Duration::from_secs(i as u64);
            let id = i as u64;
            answered.sample(id, now);
            unanswered.sample(id, now);
            assert!(!answered.answer(&answer_for(id ^ 1), now));
            let reply = answer_for(id);
            assert!(answered.answer(&reply, now + Duration::from_millis(ms)));
            assert!(!answered.answer(&reply, now + Duration::from_millis(ms + 1)));
        }
        assert_eq!(answered.best, Some(Duration::from_millis(15)));
        assert_eq!(unanswered.best, None);
    }

    #[test]
    fn resolves_ip_urls() {
        assert_eq!(resolve("stun:192.0.2.1:3478"), Some("192.0.2.1:3478".parse().unwrap()));
        assert_eq!(resolve("turn:192.0.2.1:3478"), None);
    }
}
