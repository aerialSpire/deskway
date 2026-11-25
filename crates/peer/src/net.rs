//! A connection's network paths: one socket per local address (IPv4 and IPv6), the address a
//! STUN server sees through each, and a TURN relay per server. It keeps what it needs to find
//! them all again when ICE restarts (`rebind`), as either end's network may have changed.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Input, Output, Rtc};

use crate::stun::{self, Probe};
use crate::turn::{Relay, Transport, TurnServer};
use crate::{NetEvent, Stop};

pub(crate) fn resolve_ipv4(endpoint: impl ToSocketAddrs) -> Option<SocketAddr> {
    endpoint.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
}

/// Addresses a host candidate can use: not loopback, and for IPv6 only global or unique-local
/// ones (link-local addresses need a scope the candidate cannot carry).
pub fn usable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_unspecified() && !v4.is_link_local(),
        IpAddr::V6(v6) => !v6.is_loopback() && !v6.is_unspecified() && !v6.is_multicast() && (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// The usable addresses of this computer's interfaces now.
pub fn local_ips() -> Vec<IpAddr> {
    if_addrs::get_if_addrs().map(|list| list.into_iter().map(|i| i.ip()).filter(usable).collect()).unwrap_or_default()
}

/// One local address in use: its socket, its reader's stop, and the candidates it gave (host,
/// and the address a STUN server saw), so they can be withdrawn when the address goes.
struct Bound {
    addr: SocketAddr,
    socket: UdpSocket,
    stop: Stop,
    candidates: Vec<Candidate>,
}

/// One socket per local address, so replies leave the interface they arrived on. Each gives a
/// host candidate; the address a STUN server sees comes later (`stun::Probe`), through the same
/// socket. A reader thread per socket feeds the owner until its stop or the connection's.
fn bind<E: From<NetEvent> + Send + 'static>(ips: &[IpAddr], tx: &Sender<E>, all: &Arc<AtomicBool>) -> Result<Vec<Bound>> {
    let mut out = Vec::new();
    for ip in ips {
        let Ok(socket) = UdpSocket::bind(SocketAddr::new(*ip, 0)) else { continue };
        let local = socket.local_addr()?;
        let candidates = Candidate::host(local, "udp").into_iter().collect();
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let stop = Stop::new(all);
        let (reader, tx, reader_stop) = (socket.try_clone()?, tx.clone(), stop.clone());
        std::thread::Builder::new().name("udp-read".into()).spawn(move || {
            let mut buf = vec![0u8; 2048];
            while !reader_stop.stopped() {
                // Timeouts and Windows' ICMP-driven ConnectionReset are both harmless here.
                if let Ok((n, source)) = reader.recv_from(&mut buf) {
                    if tx.send(NetEvent::Udp { data: buf[..n].to_vec(), source, destination: local }.into()).is_err() {
                        break;
                    }
                }
            }
        })?;
        out.push(Bound { addr: local, socket, stop, candidates });
    }
    Ok(out)
}

/// What an ICE restart changes in the local addresses: (to bind, to close). Addresses still
/// there keep their sockets.
fn rebind_plan(bound: &[IpAddr], now: &[IpAddr]) -> (Vec<IpAddr>, Vec<IpAddr>) {
    let mut add: Vec<IpAddr> = now.iter().filter(|ip| !bound.contains(ip)).copied().collect();
    add.dedup();
    let gone = bound.iter().filter(|ip| !now.contains(ip)).copied().collect();
    (add, gone)
}

/// The network paths of one connection. Every reader thread stops when it is dropped.
pub struct Net<E> {
    sockets: Vec<Bound>,
    relays: Vec<(Relay, Candidate)>,
    probes: Vec<Probe>,
    /// The STUN server, once resolved.
    stun: Option<SocketAddr>,
    /// The relays offered to this connection, as the test switch leaves them.
    turn: Vec<TurnServer>,
    /// Local sockets are used (the relay-only test switch turns them off).
    direct: bool,
    tx: Sender<E>,
    all: Arc<AtomicBool>,
}

impl<E: From<NetEvent> + Send + 'static> Net<E> {
    /// Binds the local sockets and adds their host candidates to `rtc`; then, in the background,
    /// resolves the STUN server and sets up the relays, reporting each path as it is ready.
    /// The first answer or offer can go out while those paths are being found.
    ///
    /// Test switch: "udp" and "tcp" use only that relay. "tls" disables direct paths and
    /// leaves all relay allocations running so the test can block the usual TURN port.
    pub fn start(rtc: &mut Rtc, stun_urls: Vec<String>, turn: Vec<TurnServer>, tx: Sender<E>) -> Result<Net<E>> {
        let forced_relay = std::env::var("DESKWAY_TEST_RELAY").ok();
        let all = Arc::new(AtomicBool::new(false));
        let direct = forced_relay.is_none();
        let sockets = if direct { bind(&local_ips(), &tx, &all)? } else { Vec::new() };
        let turn: Vec<TurnServer> = turn.into_iter().filter(|t| forced_relay.as_deref().is_none_or(|f| f == "tls" || f == t.transport.name())).collect();
        anyhow::ensure!(!sockets.is_empty() || !turn.is_empty(), "no usable network interface");
        for c in sockets.iter().flat_map(|b| b.candidates.iter().cloned()) {
            rtc.add_local_candidate(c);
        }
        let net = Net { sockets, relays: Vec::new(), probes: Vec::new(), stun: None, turn, direct, tx, all };
        if !net.sockets.is_empty() && !stun_urls.is_empty() {
            net.spawn_path("stun-resolve", move |_| {
                stun_urls.iter().find_map(|u| stun::resolve(u)).map(NetEvent::StunServer)
            })?;
        }
        net.spawn_relays()?;
        Ok(net)
    }

    /// Bytes waiting to leave this computer: what the relays still hold (sockets hold nothing).
    pub fn queued(&self) -> usize {
        self.relays.iter().map(|(r, _)| r.queued()).sum()
    }

    /// Sends what str0m wants sent, from the socket or relay it names.
    pub fn transmit(&self, t: &Transmit) {
        if let Some(b) = self.sockets.iter().find(|b| b.addr == t.source) {
            let _ = b.socket.send_to(&t.contents, t.destination);
        } else if let Some((r, _)) = self.relays.iter().find(|(r, _)| r.addr == t.source) {
            r.send(t.destination, &t.contents);
        }
    }

    /// One event from the paths. `offer` adds a candidate found now to `rtc` and tells the other
    /// end about it; it returns false when `rtc` does not take it.
    pub fn handle(&mut self, rtc: &mut Rtc, ev: NetEvent, mut offer: impl FnMut(&mut Rtc, Candidate) -> bool) -> Result<()> {
        match ev {
            NetEvent::Udp { data, source, destination } => {
                if let Some(public) = self.probes.iter_mut().find_map(|p| p.answer(source, destination, &data)) {
                    // The address the STUN server saw, offered when it differs from the socket's.
                    if public != destination {
                        if let Ok(c) = Candidate::server_reflexive(public, destination, "udp") {
                            if offer(rtc, c.clone()) {
                                if let Some(b) = self.sockets.iter_mut().find(|b| b.addr == destination) {
                                    b.candidates.push(c);
                                }
                            }
                        }
                    }
                    return Ok(());
                }
                if let Ok(contents) = data.as_slice().try_into() {
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination, contents });
                    if rtc.accepts(&input) {
                        rtc.handle_input(input).context("rtc input")?;
                    }
                }
            }
            NetEvent::RelayReady(r) => {
                eprintln!("relay ready over {}: {}", r.transport.name(), r.addr);
                match relay_candidate(&r) {
                    Ok(c) if offer(rtc, c.clone()) => self.add_relay(rtc, r, c),
                    _ => r.close(),
                }
            }
            NetEvent::StunServer(server) => {
                self.stun = Some(server);
                self.probe(Instant::now());
            }
        }
        Ok(())
    }

    /// Sends the STUN requests that are due; returns when the next one is.
    pub fn poll(&mut self, now: Instant) -> Option<Instant> {
        for p in &mut self.probes {
            if let (Some((req, server)), Some(b)) = (p.poll(now), self.sockets.iter().find(|b| b.addr == p.local)) {
                let _ = b.socket.send_to(&req, server);
            }
        }
        self.probes.iter().filter_map(Probe::due).min()
    }

    /// ICE restarts: this computer's own network may have changed too. Addresses that are gone
    /// lose their sockets and candidates; new ones are bound; STUN and the relays are asked
    /// again. Returns the new host candidates, to be trickled.
    pub fn rebind(&mut self, rtc: &mut Rtc, now: Instant) -> Result<Vec<Candidate>> {
        let mut fresh = Vec::new();
        if self.direct {
            let bound: Vec<IpAddr> = self.sockets.iter().map(|b| b.addr.ip()).collect();
            let (add, gone) = rebind_plan(&bound, &local_ips());
            let (closed, kept): (Vec<Bound>, Vec<Bound>) = std::mem::take(&mut self.sockets).into_iter().partition(|b| gone.contains(&b.addr.ip()));
            self.sockets = kept;
            for b in closed {
                b.stop.close();
                for c in &b.candidates {
                    rtc.direct_api().invalidate_candidate(c);
                }
            }
            for b in bind(&add, &self.tx, &self.all)? {
                fresh.extend(b.candidates.iter().cloned());
                self.sockets.push(b);
            }
            self.probe(now);
        }
        self.spawn_relays()?;
        Ok(fresh)
    }

    /// Sends whatever str0m still has queued (a last end message), then closes the connection.
    pub fn close(&self, rtc: &mut Rtc) {
        let _ = rtc.handle_input(Input::Timeout(Instant::now()));
        for _ in 0..256 {
            match rtc.poll_output() {
                Ok(Output::Transmit(t)) => self.transmit(&t),
                Ok(Output::Event(_)) => {}
                _ => break,
            }
        }
        rtc.disconnect();
    }

    /// Asks the STUN server, from every socket of its address family, which address it sees.
    fn probe(&mut self, now: Instant) {
        let Some(server) = self.stun else { return };
        self.probes = self.sockets.iter().filter(|b| b.addr.is_ipv4() == server.is_ipv4()).map(|b| Probe::new(b.addr, server, now)).collect();
    }

    /// Resolves and sets up each allocation on its own worker, which reports it when ready.
    fn spawn_relays(&self) -> Result<()> {
        for server in self.turn.clone() {
            let stop = Stop::new(&self.all);
            self.spawn_path("turn-setup", move |tx| match Relay::start(&server, tx, stop) {
                Ok(r) => Some(NetEvent::RelayReady(r)),
                Err(e) => {
                    eprintln!("relay {}:{} ({}) unavailable: {e:#}", server.endpoint.0, server.endpoint.1, server.transport.name());
                    None
                }
            })?;
        }
        Ok(())
    }

    /// DNS and path setup may block. Report their results through the owner's event queue.
    fn spawn_path(&self, name: &str, discover: impl FnOnce(Sender<E>) -> Option<NetEvent> + Send + 'static) -> Result<()> {
        let tx = self.tx.clone();
        std::thread::Builder::new().name(name.into()).spawn(move || {
            if let Some(event) = discover(tx.clone()) {
                let _ = tx.send(event.into());
            }
        })?;
        Ok(())
    }

    /// A relay offered as `c`: it takes the place of an earlier allocation on the same server,
    /// which is given up and withdrawn.
    fn add_relay(&mut self, rtc: &mut Rtc, r: Relay, c: Candidate) {
        if let Some(i) = self.relays.iter().position(|(o, _)| o.server == r.server && o.transport == r.transport) {
            let (old, oc) = self.relays.remove(i);
            old.close();
            rtc.direct_api().invalidate_candidate(&oc);
        }
        self.relays.push((r, c));
    }
}

fn relay_candidate(relay: &Relay) -> Result<Candidate> {
    let candidate = Candidate::relayed(relay.addr, relay.local, Protocol::Udp)?;
    // RFC 8445 priority: relayed type preference 0, then the TURN transport,
    // then 256 minus the component ID. Every allocation relays UDP to its peer.
    let preference = match relay.transport {
        Transport::Udp => 3,
        Transport::Tcp => 2,
        Transport::Tls => 1,
    };
    let priority = (preference << 8) | 255;
    let mut hash = DefaultHasher::new();
    (relay.addr.ip(), &relay.server).hash(&mut hash);
    let foundation = format!("{}{:016x}", relay.transport.name(), hash.finish());
    Ok(Candidate::from_parts(foundation, 1, candidate.proto(), priority, candidate.addr(), candidate.kind(), Some((Ipv4Addr::UNSPECIFIED, 0).into()), None, None))
}

impl<E> Drop for Net<E> {
    fn drop(&mut self) {
        self.all.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_candidates_use_ipv4_and_routable_ipv6() {
        for ok in ["192.168.1.5", "10.0.0.2", "2001:db8::1", "fd00::5"] {
            assert!(usable(&ok.parse().unwrap()), "{ok}");
        }
        for no in ["127.0.0.1", "0.0.0.0", "169.254.1.1", "::1", "::", "fe80::1", "ff02::1"] {
            assert!(!usable(&no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn a_restart_binds_new_addresses_and_closes_vanished_ones() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // Wi-Fi to another network: the old address goes, the new one is bound, the wired stays.
        let (add, gone) = rebind_plan(&[ip("192.168.1.5"), ip("10.0.0.2")], &[ip("10.0.0.2"), ip("172.20.3.4")]);
        assert_eq!((add, gone), (vec![ip("172.20.3.4")], vec![ip("192.168.1.5")]));
        // Nothing changed: nothing to do.
        assert_eq!(rebind_plan(&[ip("10.0.0.2")], &[ip("10.0.0.2")]), (vec![], vec![]));
        // Everything gone (no network now): every socket closes.
        assert_eq!(rebind_plan(&[ip("10.0.0.2")], &[]), (vec![], vec![ip("10.0.0.2")]));
    }
}
