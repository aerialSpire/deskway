//! A gateway's UDP mapping of one session socket. The owner supplies time and moves packets.

mod natpmp;
mod pcp;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};

const PORT: u16 = 5351;
const LIFETIME: u32 = 120;
const NAT_PMP_ATTEMPTS: u32 = 9;

#[derive(Clone, Copy)]
enum Protocol {
    Pcp,
    NatPmp,
}

impl Protocol {
    fn interval(self, attempt: u32, renewal: bool) -> Duration {
        match self {
            Self::Pcp => {
                // RFC 6887, section 8.1.1 and section 11.2.1: exponential backoff with jitter and a four-second renewal floor.
                let initial = if renewal && attempt > 0 { 4 } else { 3 };
                let base = f64::from((initial << attempt.min(9)).min(1024));
                let random = f64::from(u32::from_be_bytes(wire::crypto::random())) / f64::from(u32::MAX);
                let seconds = base * (0.9 + 0.2 * random);
                Duration::from_secs_f64(if renewal { seconds.max(4.0) } else { seconds })
            }
            Self::NatPmp => Duration::from_millis(250 << attempt.min(NAT_PMP_ATTEMPTS - 1)),
        }
    }
}

#[derive(Clone, Copy)]
enum Query {
    Pcp,
    NatPmpAddress,
    NatPmpMap(Ipv4Addr),
}

enum State {
    Asking { protocol: Query, attempt: u32, due: Instant },
    Held { protocol: Protocol, external: SocketAddrV4, expires: Instant, renew_due: Instant, attempt: u32 },
    Idle,
}

impl State {
    fn asking(protocol: Query, due: Instant) -> Self {
        Self::Asking { protocol, attempt: 0, due }
    }
}

pub(crate) struct Lease {
    local: SocketAddr,
    pub gateway: Ipv4Addr,
    nonce: [u8; 12],
    state: State,
}

impl Lease {
    pub fn new(local: SocketAddr, gateway: Ipv4Addr, now: Instant) -> Self {
        Self { local, gateway, nonce: wire::crypto::random(), state: State::asking(Query::Pcp, now) }
    }

    pub fn endpoint(&self) -> Option<SocketAddr> {
        match self.state {
            State::Held { external, .. } => Some(external.into()),
            _ => None,
        }
    }

    pub fn due(&self) -> Option<Instant> {
        match self.state {
            State::Asking { due, .. } => Some(due),
            State::Held { expires, renew_due, .. } => Some(expires.min(renew_due)),
            State::Idle => None,
        }
    }

    pub fn poll(&mut self, now: Instant) -> Option<(Vec<u8>, SocketAddr)> {
        self.expire(now);
        let packet = match self.state {
            State::Asking { protocol, attempt, due } if now >= due => {
                let (protocol, attempt) = match protocol {
                    Query::Pcp if attempt == 2 => (Query::NatPmpAddress, 0),
                    Query::NatPmpAddress | Query::NatPmpMap(_) if attempt == NAT_PMP_ATTEMPTS => {
                        self.state = State::Idle;
                        return None;
                    }
                    _ => (protocol, attempt),
                };
                let (mapping_protocol, packet) = match protocol {
                    Query::Pcp => (Protocol::Pcp, self.map_request(Protocol::Pcp, None, LIFETIME)),
                    Query::NatPmpAddress => (Protocol::NatPmp, natpmp::address_request().to_vec()),
                    Query::NatPmpMap(_) => (Protocol::NatPmp, self.map_request(Protocol::NatPmp, None, LIFETIME)),
                };
                let due = now + mapping_protocol.interval(attempt, false);
                self.state = State::Asking { protocol, attempt: attempt + 1, due };
                packet
            }
            State::Held { protocol, external, expires, renew_due, attempt } if now >= renew_due => {
                self.state = State::Held {
                    protocol, external, expires,
                    renew_due: now + protocol.interval(attempt, true),
                    attempt: attempt + 1,
                };
                self.map_request(protocol, Some(external), LIFETIME)
            }
            _ => return None,
        };
        Some((packet, (self.gateway, PORT).into()))
    }

    pub fn answer(&mut self, source: SocketAddr, data: &[u8], now: Instant) -> Option<()> {
        if source != (self.gateway, PORT).into() {
            return None;
        }
        self.expire(now);
        let query = match self.state {
            State::Asking { protocol, attempt: 1.., .. } => protocol,
            State::Held { protocol: Protocol::Pcp, .. } => Query::Pcp,
            State::Held { protocol: Protocol::NatPmp, external, .. } => Query::NatPmpMap(*external.ip()),
            _ => return None,
        };
        match query {
            Query::Pcp => {
                let reply = if natpmp::unsupported_version(data) {
                    pcp::Reply::UnsupportedVersion
                } else {
                    pcp::response(data, self.local.port(), self.nonce)?
                };
                match reply {
                    pcp::Reply::Mapped { external, lifetime } => self.hold(Protocol::Pcp, external, lifetime, now),
                    pcp::Reply::UnsupportedVersion if matches!(self.state, State::Asking { .. }) => {
                        self.state = State::asking(Query::NatPmpAddress, now);
                    }
                    _ => {}
                }
            }
            Query::NatPmpAddress | Query::NatPmpMap(_) => {
                match (query, natpmp::response(data, self.local.port())?) {
                    (Query::NatPmpAddress, natpmp::Reply::Address(Some(ip))) => {
                        self.state = if public(ip) { State::asking(Query::NatPmpMap(ip), now) } else { State::Idle };
                    }
                    (Query::NatPmpMap(ip), natpmp::Reply::Map(Some((port, lifetime)))) => {
                        self.hold(Protocol::NatPmp, (ip, port).into(), lifetime, now);
                    }
                    (Query::NatPmpAddress, natpmp::Reply::Address(None))
                    | (Query::NatPmpMap(_), natpmp::Reply::Map(None)) => {}
                    _ => return None,
                }
            }
        }
        Some(())
    }

    fn hold(&mut self, protocol: Protocol, external: SocketAddr, lifetime: u32, now: Instant) {
        self.state = match external {
            SocketAddr::V4(external) if public(*external.ip()) => {
                let lifetime = Duration::from_secs(lifetime.into());
                State::Held { protocol, external, expires: now + lifetime, renew_due: now + lifetime / 2, attempt: 0 }
            }
            _ => State::Idle,
        };
    }

    fn expire(&mut self, now: Instant) {
        if matches!(self.state, State::Held { expires, .. } if now >= expires) {
            self.state = State::Idle;
        }
    }

    pub fn release(mut self, now: Instant) -> Option<(Vec<u8>, SocketAddr)> {
        self.expire(now);
        let State::Held { protocol, .. } = self.state else { return None };
        Some((self.map_request(protocol, None, 0), (self.gateway, PORT).into()))
    }

    fn map_request(&self, protocol: Protocol, suggested: Option<SocketAddrV4>, lifetime: u32) -> Vec<u8> {
        match protocol {
            Protocol::Pcp => pcp::map_request(self.local, self.nonce, suggested.map(SocketAddr::V4), lifetime).to_vec(),
            Protocol::NatPmp => natpmp::map_request(self.local.port(), suggested.map_or(0, |a| a.port()), lifetime).to_vec(),
        }
    }
}

fn public(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_unspecified()
        && !ip.is_multicast()
        && !ip.is_broadcast()
        && !(a == 100 && (64..=127).contains(&b))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::SocketAddrV4;

    // Run with cargo test -p deskway-peer real_router_lease -- --ignored --nocapture.
    #[test]
    #[ignore = "sends UDP to the local router and may open a temporary mapping"]
    fn real_router_lease() -> anyhow::Result<()> {
        use crate::{gateway, stun};
        use std::io::ErrorKind;
        use std::net::{IpAddr, UdpSocket};
        use std::time::{SystemTime, UNIX_EPOCH};

        let start = Instant::now();
        println!("Unix start {:.3}", SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64());
        let log = |line: &str| println!("+{:.3}s {line}", start.elapsed().as_secs_f64());
        let send = |socket: &UdpSocket, to: SocketAddr, kind: &str, data: &[u8]| {
            let result = socket.send_to(data, to);
            log(&format!("TX {to} {kind} {result:?} {data:02x?}"));
        };
        let receive = |socket: &UdpSocket, buffer: &mut [u8]| match socket.recv_from(buffer) {
            Ok(packet) => Ok(Some(packet)),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) if e.kind() == ErrorKind::ConnectionReset => {
                log(&format!("RX socket error {e:?}"));
                Ok(None)
            }
            Err(e) => Err(e),
        };

        let ips: std::collections::BTreeSet<_> = if_addrs::get_if_addrs()?
            .into_iter()
            .filter_map(|iface| match iface.ip() {
                IpAddr::V4(ip) if ip.is_private() => Some(ip),
                _ => None,
            })
            .collect();
        for ip in ips {
            let gateway = gateway::default_gateway(ip);
            log(&format!("default_gateway({ip}) = {gateway:?}"));
            let socket = match UdpSocket::bind((ip, 0)) {
                Ok(socket) => socket,
                Err(e) => {
                    log(&format!("bind {ip}: {e:?}"));
                    continue;
                }
            };
            let local = socket.local_addr()?;
            log(&format!("bound {local}"));
            let Some(gateway) = gateway else { continue };
            socket.set_read_timeout(Some(Duration::from_millis(50)))?;
            let mut lease = Lease::new(local, gateway, Instant::now());
            let show_state = |lease: &Lease| match lease.state {
                State::Asking { protocol, attempt, .. } => {
                    let query = match protocol {
                        Query::Pcp => "PCP MAP",
                        Query::NatPmpAddress => "NAT-PMP address",
                        Query::NatPmpMap(_) => "NAT-PMP UDP MAP",
                    };
                    log(&format!("state Asking {query}, attempt={attempt}"));
                }
                State::Held { external, expires, renew_due, attempt, .. } => log(&format!(
                    "state Held {external}, expires=+{:.3}s, next renewal=+{:.3}s, attempt={attempt}",
                    (expires - start).as_secs_f64(), (renew_due - start).as_secs_f64(),
                )),
                State::Idle => log("state Idle"),
            };
            show_state(&lease);
            let mut probe: Option<stun::Probe> = None;
            let mut renewal_deadline = None;
            let mut gateway_replied = false;
            let mut buffer = [0; 65536];
            loop {
                let now = Instant::now();
                if renewal_deadline.is_some_and(|deadline| now >= deadline) {
                    log("renewal response timed out");
                    break;
                }
                if let Some((data, to)) = lease.poll(now) {
                    let kind = match data[..2] {
                        [2, 1] => "PCP MAP",
                        [0, 0] => "NAT-PMP address",
                        _ => "NAT-PMP UDP MAP",
                    };
                    if let State::Held { expires, .. } = lease.state {
                        renewal_deadline.get_or_insert(now + Duration::from_secs(2).min((expires - now) / 2));
                        log("poll emitted a renewal");
                    }
                    send(&socket, to, kind, &data);
                    show_state(&lease);
                } else if lease.due().is_none() {
                    show_state(&lease);
                    break;
                }
                if let Some((data, to)) = probe.as_mut().and_then(|p| p.poll(now)) {
                    send(&socket, to, "STUN Binding request", &data);
                }
                let Some((n, from)) = receive(&socket, &mut buffer)? else { continue };
                let data = &buffer[..n];
                let kind = match data {
                    [0, 0, 0, 1, ..] => "NAT-PMP unsupported version",
                    [0, 128, ..] => "NAT-PMP address response",
                    [0, 129, ..] => "NAT-PMP UDP MAP response",
                    [_, 129, ..] => "PCP MAP response",
                    [1, 1, _, _, 0x21, 0x12, 0xa4, 0x42, ..] => "STUN Binding response",
                    _ => "unrecognized reply",
                };
                log(&format!("RX {from} {kind} {data:02x?}"));
                gateway_replied |= from == (gateway, PORT).into();
                if let Some(endpoint) = probe.as_mut().and_then(|p| p.answer(from, local, data)) {
                    log(&format!("STUN endpoint={endpoint}, matches lease={}", lease.endpoint() == Some(endpoint)));
                }
                let previous = lease.endpoint();
                if lease.answer(from, data, Instant::now()).is_none() {
                    continue;
                }
                let pcp = pcp::response(data, local.port(), lease.nonce);
                let pmp = natpmp::response(data, local.port());
                log(&format!("Lease::answer accepted, PCP={pcp:?}, NAT-PMP={pmp:?}"));
                show_state(&lease);
                if previous.is_none() && lease.endpoint().is_some() {
                    let server = stun::resolve("stun:stun.l.google.com:19302");
                    log(&format!("STUN server={server:?}"));
                    probe = server.map(|server| stun::Probe::new(local, server, Instant::now()));
                }
                if renewal_deadline.is_some() && matches!(lease.state, State::Held { attempt: 0, .. }) {
                    log("renewal accepted");
                    break;
                }
            }
            if let Some((data, to)) = lease.release(Instant::now()) {
                let kind = if data[0] == 2 { "PCP MAP delete" } else { "NAT-PMP UDP MAP delete" };
                send(&socket, to, kind, &data);
            } else {
                log("Lease::release returned no datagram");
            }
            if !gateway_replied {
                let target: SocketAddr = "239.255.255.250:1900".parse()?;
                let search = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
                socket2::SockRef::from(&socket).set_multicast_if_v4(&ip)?;
                let result = socket.send_to(search, target);
                log(&format!("TX {target} SSDP IGD v1 M-SEARCH {result:?}"));
                let until = Instant::now() + Duration::from_secs(5);
                let mut responders = std::collections::BTreeSet::new();
                while Instant::now() < until {
                    if let Some((n, from)) = receive(&socket, &mut buffer)? {
                        log(&format!("RX {from} SSDP window: {:?}", String::from_utf8_lossy(&buffer[..n])));
                        if buffer[..n].starts_with(b"HTTP/1.1 200") {
                            responders.insert(from);
                        }
                    }
                }
                log(&format!("SSDP responders={responders:?}"));
            }
        }
        Ok(())
    }

    const PUBLIC: [u8; 4] = [198, 51, 100, 5];

    pub(crate) fn pcp_response(nonce: [u8; 12], internal: u16, external: SocketAddrV4, lifetime: u32, code: u8) -> Vec<u8> {
        let mut data = vec![0; 60];
        data[..4].copy_from_slice(&[2, 129, 0, code]);
        data[4..8].copy_from_slice(&lifetime.to_be_bytes());
        data[24..36].copy_from_slice(&nonce);
        data[36] = 17;
        data[40..42].copy_from_slice(&internal.to_be_bytes());
        if lifetime != 0 && code == 0 {
            data[42..44].copy_from_slice(&external.port().to_be_bytes());
            data[54..56].fill(255);
            data[56..60].copy_from_slice(&external.ip().octets());
        }
        data
    }

    #[derive(Clone, Copy)]
    enum In {
        Nothing,
        Pcp { lifetime: u32, address: [u8; 4], code: u8 },
        Unsupported,
        Address([u8; 4]),
        NatMap(u32),
    }

    impl In {
        fn packet(self, nonce: [u8; 12]) -> Option<Vec<u8>> {
            Some(match self {
                Self::Nothing => return None,
                Self::Pcp { lifetime, address, code } => {
                    pcp_response(nonce, 5000, SocketAddrV4::new(address.into(), 9000), lifetime, code)
                }
                Self::Unsupported => vec![0, 0, 0, 1, 0, 0, 0, 0],
                Self::Address(address) => [&[0, 128, 0, 0, 0, 0, 0, 0][..], &address].concat(),
                Self::NatMap(lifetime) => {
                    [&[0, 129, 0, 0, 0, 0, 0, 0, 0x13, 0x88, 0x23, 0x28][..], &lifetime.to_be_bytes()].concat()
                }
            })
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Out {
        NothingSent,
        PcpMap,
        PcpRenew,
        PcpDelete,
        NatAddress,
        NatMap,
        NatRenew,
        NatDelete,
    }

    impl Out {
        fn read(packet: &[u8]) -> Self {
            match packet {
                [2, 1, _, _, 0, 0, 0, 120, ..] if packet[42..44] == [0, 0] => Self::PcpMap,
                [2, 1, _, _, 0, 0, 0, 120, ..] if packet[42..44] == 9000u16.to_be_bytes() => Self::PcpRenew,
                [2, 1, _, _, 0, 0, 0, 0, ..] => Self::PcpDelete,
                [0, 0] => Self::NatAddress,
                [0, 1, _, _, _, _, 0, 0, 0, 0, 0, 120] => Self::NatMap,
                [0, 1, _, _, _, _, 0x23, 0x28, 0, 0, 0, 120] => Self::NatRenew,
                [0, 1, _, _, _, _, 0, 0, 0, 0, 0, 0] => Self::NatDelete,
                _ => panic!("unexpected request {packet:?}"),
            }
        }
    }

    #[test]
    fn lease_sequences() {
        use Out::*;
        let mapped = Some((Ipv4Addr::from(PUBLIC), 9000).into());
        let pcp = |lifetime, address| In::Pcp { lifetime, address, code: 0 };
        let first = (Some(0), pcp(60, PUBLIC), PcpMap, mapped);
        let quiet = In::Nothing;
        let changed_ip = [198, 51, 100, 6];
        let changed = Some((Ipv4Addr::from(changed_ip), 9000).into());
        let nat_map = (None, In::NatMap(60), NatMap, mapped);

        // Times are offsets in milliseconds. None advances to the scheduled deadline.
        let mut cases = vec![
            ("PCP success and release", vec![first], false, PcpDelete),
            (
                "PCP silent then NAT-PMP success and renewal",
                vec![
                    (Some(0), quiet, PcpMap, None),
                    (None, quiet, PcpMap, None),
                    (None, In::Address(PUBLIC), NatAddress, None),
                    nat_map,
                    (None, quiet, NatRenew, mapped),
                    (None, In::NatMap(60), NatRenew, mapped),
                ],
                false,
                NatDelete,
            ),
            (
                "unsupported PCP version then NAT-PMP",
                vec![
                    (Some(0), In::Unsupported, PcpMap, None),
                    (Some(0), In::Address(PUBLIC), NatAddress, None),
                    nat_map,
                ],
                false,
                NatDelete,
            ),
            (
                "no gateway answers",
                [(Some(0), quiet, PcpMap, None), (None, quiet, PcpMap, None)]
                    .into_iter()
                    .chain(std::iter::repeat_n((None, quiet, NatAddress, None), 9))
                    .chain([(None, quiet, NothingSent, None)])
                    .collect(),
                true,
                NothingSent,
            ),
            (
                "NAT-PMP address received but map unanswered",
                [(Some(0), In::Unsupported, PcpMap, None), (None, In::Address(PUBLIC), NatAddress, None)]
                    .into_iter()
                    .chain(std::iter::repeat_n((None, quiet, NatMap, None), 9))
                    .chain([(None, quiet, NothingSent, None)])
                    .collect(),
                true,
                NothingSent,
            ),
            (
                "renewal at half the grant",
                vec![
                    first,
                    (Some(29_999), quiet, NothingSent, mapped),
                    (Some(30_000), pcp(80, PUBLIC), PcpRenew, mapped),
                    (Some(69_999), quiet, NothingSent, mapped),
                    (Some(70_000), pcp(80, PUBLIC), PcpRenew, mapped),
                ],
                false,
                PcpDelete,
            ),
            (
                "one missed renewal then success",
                vec![
                    first,
                    (Some(30_000), quiet, PcpRenew, mapped),
                    (Some(33_999), quiet, NothingSent, mapped),
                    (Some(34_000), pcp(60, PUBLIC), PcpRenew, mapped),
                ],
                false,
                PcpDelete,
            ),
            (
                "renewal error keeps the original schedule and expiry",
                vec![
                    first,
                    (Some(30_000), In::Pcp { lifetime: 40, address: PUBLIC, code: 2 }, PcpRenew, mapped),
                    (Some(34_000), quiet, PcpRenew, mapped),
                    (Some(59_999), quiet, PcpRenew, mapped),
                    (Some(60_000), quiet, NothingSent, None),
                ],
                true,
                NothingSent,
            ),
            (
                "renewal changes the external endpoint",
                vec![
                    first,
                    (Some(30_000), pcp(20, changed_ip), PcpRenew, changed),
                    (Some(39_999), quiet, NothingSent, changed),
                    (Some(40_000), quiet, PcpRenew, changed),
                    (Some(50_000), quiet, NothingSent, None),
                ],
                true,
                NothingSent,
            ),
            (
                "renewal grants a private address",
                vec![first, (Some(30_000), pcp(60, [10, 1, 1, 1]), PcpRenew, None)],
                true,
                NothingSent,
            ),
            (
                "NAT-PMP returns a private address",
                vec![(Some(0), In::Unsupported, PcpMap, None), (None, In::Address([10, 1, 1, 1]), NatAddress, None)],
                true,
                NothingSent,
            ),
            ("release while asking", vec![(Some(0), quiet, PcpMap, None)], false, NothingSent),
        ];
        for (name, address) in [
            ("private", [10, 1, 1, 1]),
            ("loopback", [127, 0, 0, 1]),
            ("link-local", [169, 254, 1, 1]),
            ("unspecified", [0, 0, 0, 0]),
            ("multicast", [224, 0, 0, 1]),
            ("broadcast", [255, 255, 255, 255]),
            ("shared address space start", [100, 64, 0, 0]),
            ("shared address space end", [100, 127, 255, 255]),
        ] {
            cases.push((name, vec![(Some(0), pcp(60, address), PcpMap, None)], true, NothingSent));
        }

        let start = Instant::now();
        let local = "192.168.1.2:5000".parse().unwrap();
        let router = Ipv4Addr::new(192, 168, 1, 1);
        let gateway = (router, PORT).into();
        for (name, steps, idle, release) in cases {
            let mut lease = Lease::new(local, router, start);
            let nonce = lease.nonce;
            let mut now = start;
            let mut unanswered = 0;
            for (i, (time, input, output, expected)) in steps.into_iter().enumerate() {
                now = time.map_or_else(|| lease.due().expect(name), |ms| start + Duration::from_millis(ms));
                let sent = lease.poll(now).map_or(NothingSent, |(bytes, to)| {
                    assert_eq!(to, gateway);
                    if bytes.starts_with(&[2, 1]) {
                        assert_eq!(bytes[24..36], nonce);
                    }
                    Out::read(&bytes)
                });
                assert_eq!(sent, output, "{name}, step {i}");
                if matches!(sent, NatAddress | NatMap | NatRenew) {
                    assert_eq!(lease.due().unwrap() - now, Duration::from_millis(250 << unanswered), "{name}, step {i}");
                    unanswered += 1;
                }
                if let Some(packet) = input.packet(nonce) {
                    lease.answer(gateway, &packet, now).expect(name);
                    unanswered = 0;
                }
                assert_eq!(lease.endpoint(), expected, "{name}, step {i}");
            }
            assert_eq!(lease.due().is_none(), idle, "{name}");
            let sent = lease.release(now).map_or(NothingSent, |(packet, to)| {
                assert_eq!(to, gateway);
                if release == PcpDelete {
                    assert_eq!(packet[24..36], nonce);
                }
                Out::read(&packet)
            });
            assert_eq!(sent, release, "{name}");
        }
    }
}
