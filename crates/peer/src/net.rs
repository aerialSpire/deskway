//! A connection's network paths: one socket per local address (IPv4 and IPv6), the address a
//! STUN server sees through each, router mappings, and a TURN relay per server. It finds them
//! again when ICE restarts (`rebind`), as either end's network may have changed.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Input, Output, Rtc};

use crate::gateway;
use crate::portmap::Lease;
use crate::stun::{self, Probe};
use crate::turn::{Relay, TurnServer};
use crate::{NetEvent, Stop};

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

/// One local address in use: its socket, discovery, and the candidates it offers.
struct Bound {
    addr: SocketAddr,
    socket: UdpSocket,
    stop: Stop,
    host: Option<Candidate>,
    probe: Option<Probe>,
    lease: Option<Lease>,
    stun_result: Option<SocketAddr>,
    reflexive: Vec<Candidate>,
}

impl Bound {
    fn new(addr: SocketAddr, socket: UdpSocket, stop: Stop) -> Self {
        let host = Candidate::host(addr, "udp").ok();
        Self { addr, socket, stop, host, probe: None, lease: None, stun_result: None, reflexive: Vec::new() }
    }

    fn mapped(&self) -> Option<SocketAddr> {
        self.lease.as_ref().and_then(Lease::endpoint)
    }

    fn probe(&mut self, server: Option<SocketAddr>, now: Instant) {
        self.probe = server.filter(|s| s.is_ipv4() == self.addr.is_ipv4()).map(|s| Probe::new(self.addr, s, now));
    }

    fn release(&mut self, now: Instant) {
        if let Some((packet, gateway)) = self.lease.take().and_then(|l| l.release(now)) {
            let _ = self.socket.send_to(&packet, gateway);
        }
    }

    fn reconcile_public(&mut self, previous_mapping: Option<SocketAddr>, server: Option<SocketAddr>, now: Instant, mut offer: impl FnMut(Candidate) -> bool) -> Vec<Candidate> {
        let mapping = self.mapped();
        // STUN may have seen the mapping itself. Confirm the address again if it goes away.
        if previous_mapping.is_some() && previous_mapping != mapping && self.stun_result == previous_mapping {
            self.stun_result = None;
            self.probe(server, now);
        }
        let endpoints = [self.stun_result, mapping.filter(|a| Some(*a) != self.stun_result)];
        let withdrawn = self.reflexive.extract_if(.., |c| !endpoints.contains(&Some(c.addr()))).collect();
        for public in endpoints.into_iter().flatten() {
            if public != self.addr && !self.reflexive.iter().any(|c| c.addr() == public) {
                if let Ok(c) = Candidate::server_reflexive(public, self.addr, "udp") {
                    if offer(c.clone()) {
                        self.reflexive.push(c);
                    }
                }
            }
        }
        withdrawn
    }
}

const UDP_BACKLOG: usize = 64;

/// A place in the owner's queue reserved for a packet from a local socket. All of a session's
/// socket readers share the same limit, leaving signaling and control events free to enqueue.
pub struct UdpSlot(Arc<AtomicUsize>);

impl UdpSlot {
    fn reserve(pending: &Arc<AtomicUsize>) -> Option<Self> {
        pending.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < UDP_BACKLOG).then_some(n + 1)).ok()?;
        Some(Self(pending.clone()))
    }
}

impl Drop for UdpSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One socket per local address, so replies leave the interface they arrived on. Each gives a
/// host candidate; the address a STUN server sees comes later (`stun::Probe`), through the same
/// socket. A reader thread per socket feeds the owner until its stop or the connection's.
fn bind<E: From<NetEvent> + Send + 'static>(ips: &[IpAddr], tx: &Sender<E>, all: &Arc<AtomicBool>, pending: &Arc<AtomicUsize>) -> Result<Vec<Bound>> {
    let mut out = Vec::new();
    for ip in ips {
        let Ok(socket) = UdpSocket::bind(SocketAddr::new(*ip, 0)) else { continue };
        let local = socket.local_addr()?;
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let stop = Stop::new(all);
        let (reader, tx, reader_stop) = (socket.try_clone()?, tx.clone(), stop.clone());
        let pending = pending.clone();
        std::thread::Builder::new().name("udp-read".into()).spawn(move || {
            let mut buf = vec![0u8; 2048];
            while !reader_stop.stopped() {
                // Timeouts and Windows' ICMP-driven ConnectionReset are both harmless here.
                if let Ok((n, source)) = reader.recv_from(&mut buf) {
                    let Some(slot) = UdpSlot::reserve(&pending) else { continue };
                    if tx.send(NetEvent::Udp { data: buf[..n].to_vec(), source, destination: local, slot: Some(slot) }.into()).is_err() {
                        break;
                    }
                }
            }
        })?;
        out.push(Bound::new(local, socket, stop));
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
    /// Only the latest lookup can change the leases.
    gateway_lookup: u64,
    /// The STUN server, once resolved.
    stun: Option<SocketAddr>,
    /// The relays offered to this connection, as the test switch leaves them.
    turn: Vec<TurnServer>,
    /// Local sockets are used (the relay-only test switch turns them off).
    direct: bool,
    tx: Sender<E>,
    all: Arc<AtomicBool>,
    pending_udp: Arc<AtomicUsize>,
}

impl<E: From<NetEvent> + Send + 'static> Net<E> {
    /// Binds the local sockets and adds their host candidates to `rtc`; then, in the background,
    /// resolves the STUN server and sets up a relay on every TURN server, each reported to `tx`
    /// when ready, so nothing slow delays the first answer or offer (trickle ICE).
    ///
    /// Test switch: `DESKWAY_TEST_RELAY` set to "udp" or "tcp" uses only that relay path,
    /// proving it works on its own.
    pub fn start(rtc: &mut Rtc, stun_urls: Vec<String>, turn: Vec<TurnServer>, tx: Sender<E>) -> Result<Net<E>> {
        let forced_relay = std::env::var("DESKWAY_TEST_RELAY").ok();
        let all = Arc::new(AtomicBool::new(false));
        let pending_udp = Arc::new(AtomicUsize::new(0));
        let direct = forced_relay.is_none();
        let sockets = if direct { bind(&local_ips(), &tx, &all, &pending_udp)? } else { Vec::new() };
        let turn: Vec<TurnServer> = turn.into_iter().filter(|t| forced_relay.as_deref().is_none_or(|f| f == if t.tcp { "tcp" } else { "udp" })).collect();
        anyhow::ensure!(!sockets.is_empty() || !turn.is_empty(), "no usable network interface");
        for c in sockets.iter().filter_map(|b| b.host.clone()) {
            rtc.add_local_candidate(c);
        }
        // Resolving the STUN server's name may wait on DNS: off the owner's thread.
        if !sockets.is_empty() && !stun_urls.is_empty() {
            let tx = tx.clone();
            std::thread::Builder::new().name("stun-resolve".into()).spawn(move || {
                if let Some(server) = stun_urls.iter().find_map(|u| stun::resolve(u)) {
                    let _ = tx.send(NetEvent::StunServer(server).into());
                }
            })?;
        }
        let mut net = Net {
            sockets, relays: Vec::new(),
            gateway_lookup: 0, stun: None, turn, direct, tx, all, pending_udp,
        };
        net.find_gateways()?;
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
    pub fn handle(&mut self, rtc: &mut Rtc, ev: NetEvent, now: Instant, mut offer: impl FnMut(&mut Rtc, Candidate) -> bool) -> Result<()> {
        match ev {
            NetEvent::Udp { data, source, destination, slot: _slot } => {
                if let Some(b) = self.sockets.iter_mut().find(|b| b.addr == destination) {
                    let previous = b.mapped();
                    let handled = if let Some(public) = b.probe.as_mut().and_then(|p| p.answer(source, destination, &data)) {
                        b.stun_result = Some(public);
                        true
                    } else {
                        b.lease.as_mut().and_then(|l| l.answer(source, &data, now)).is_some()
                    };
                    if handled || b.mapped() != previous {
                        let withdrawn = b.reconcile_public(previous, self.stun, now, |c| offer(rtc, c));
                        invalidate(rtc, withdrawn);
                    }
                    if handled {
                        return Ok(());
                    }
                }
                if let Ok(contents) = data.as_slice().try_into() {
                    let input = Input::Receive(now, Receive { proto: Protocol::Udp, source, destination, contents });
                    if rtc.accepts(&input) {
                        rtc.handle_input(input).context("rtc input")?;
                    }
                }
            }
            NetEvent::RelayReady(r) => {
                eprintln!("relay ready over {}: {}", if r.tcp { "tcp" } else { "udp" }, r.addr);
                match Candidate::relayed(r.addr, r.local, "udp") {
                    Ok(c) if offer(rtc, c.clone()) => self.add_relay(rtc, r, c),
                    _ => r.close(),
                }
            }
            NetEvent::StunServer(server) => {
                self.stun = Some(server);
                self.probe(now);
            }
            NetEvent::Gateway { lookup, local, gateway } => {
                invalidate(rtc, self.set_gateway(lookup, local, gateway, now));
            }
        }
        Ok(())
    }

    /// Drives STUN and the router leases. Returns their earliest next deadline.
    pub fn poll(&mut self, rtc: &mut Rtc, now: Instant) -> Option<Instant> {
        let mut next = None;
        for b in &mut self.sockets {
            let previous = b.mapped();
            if let Some((packet, gateway)) = b.lease.as_mut().and_then(|l| l.poll(now)) {
                let _ = b.socket.send_to(&packet, gateway);
            }
            if b.mapped() != previous {
                invalidate(rtc, b.reconcile_public(previous, self.stun, now, |_| false));
            }
            if let Some((packet, server)) = b.probe.as_mut().and_then(|p| p.poll(now)) {
                let _ = b.socket.send_to(&packet, server);
            }
            next = next.into_iter().chain(b.probe.as_ref().and_then(Probe::due)).chain(b.lease.as_ref().and_then(Lease::due)).min();
        }
        next
    }

    /// ICE restarts: this computer's own network may have changed too. Addresses that are gone
    /// lose their sockets and candidates; new ones are bound; STUN and the relays are asked
    /// again, as are default gateways. Returns the new host candidates, to be trickled.
    pub fn rebind(&mut self, rtc: &mut Rtc, now: Instant) -> Result<Vec<Candidate>> {
        let mut fresh = Vec::new();
        if self.direct {
            let bound: Vec<IpAddr> = self.sockets.iter().map(|b| b.addr.ip()).collect();
            let (add, gone) = rebind_plan(&bound, &local_ips());
            self.sockets.retain_mut(|b| {
                if !gone.contains(&b.addr.ip()) {
                    return true;
                }
                b.release(now);
                b.stop.close();
                for c in b.host.iter().chain(&b.reflexive) {
                    rtc.direct_api().invalidate_candidate(c);
                }
                false
            });
            for b in bind(&add, &self.tx, &self.all, &self.pending_udp)? {
                fresh.extend(b.host.iter().cloned());
                self.sockets.push(b);
            }
            self.probe(now);
        }
        self.find_gateways()?;
        self.spawn_relays()?;
        Ok(fresh)
    }

    /// Sends one best-effort release for each lease and flushes str0m before disconnecting.
    pub fn close(&mut self, rtc: &mut Rtc) {
        let now = Instant::now();
        for b in &mut self.sockets {
            b.release(now);
        }
        let _ = rtc.handle_input(Input::Timeout(now));
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
        for b in &mut self.sockets {
            b.probe(self.stun, now);
        }
    }

    fn find_gateways(&mut self) -> Result<()> {
        self.gateway_lookup += 1;
        let lookup = self.gateway_lookup;
        let locals: Vec<_> = self.sockets.iter().filter_map(|b| match b.addr.ip() {
            IpAddr::V4(ip) if ip.is_private() => Some((b.addr, ip)),
            _ => None,
        }).collect();
        if !locals.is_empty() {
            let tx = self.tx.clone();
            std::thread::Builder::new().name("gateway-resolve".into()).spawn(move || {
                for (local, ip) in locals {
                    let gateway = gateway::default_gateway(ip);
                    if tx.send(NetEvent::Gateway { lookup, local, gateway }.into()).is_err() {
                        break;
                    }
                }
            })?;
        }
        Ok(())
    }

    fn set_gateway(&mut self, lookup: u64, local: SocketAddr, gateway: Option<Ipv4Addr>, now: Instant) -> Vec<Candidate> {
        if lookup != self.gateway_lookup {
            return Vec::new();
        }
        let Some(b) = self.sockets.iter_mut().find(|b| b.addr == local) else { return Vec::new() };
        if b.lease.as_ref().is_some_and(|l| Some(l.gateway) == gateway && l.due().is_some()) {
            return Vec::new();
        }
        let previous = b.mapped();
        b.release(now);
        b.lease = gateway.map(|g| Lease::new(local, g, now));
        b.reconcile_public(previous, self.stun, now, |_| false)
    }

    /// Sets up a relay on every TURN server, in the background: each is offered when ready
    /// (`RelayReady`), so a blocked relay never delays a direct connection.
    fn spawn_relays(&self) -> Result<()> {
        for server in self.turn.clone() {
            let (tx, stop) = (self.tx.clone(), Stop::new(&self.all));
            std::thread::Builder::new().name("turn-setup".into()).spawn(move || match Relay::start(&server, tx.clone(), stop) {
                Ok(r) => {
                    let _ = tx.send(NetEvent::RelayReady(r).into());
                }
                Err(e) => eprintln!("relay {} ({}) unavailable: {e:#}", server.addr, if server.tcp { "tcp" } else { "udp" }),
            })?;
        }
        Ok(())
    }

    /// A relay offered as `c`: it takes the place of an earlier allocation on the same server,
    /// which is given up and withdrawn.
    fn add_relay(&mut self, rtc: &mut Rtc, r: Relay, c: Candidate) {
        if let Some(i) = self.relays.iter().position(|(o, _)| o.server == r.server && o.tcp == r.tcp) {
            let (old, oc) = self.relays.remove(i);
            old.close();
            rtc.direct_api().invalidate_candidate(&oc);
        }
        self.relays.push((r, c));
    }
}

fn invalidate(rtc: &mut Rtc, candidates: Vec<Candidate>) {
    for c in &candidates {
        rtc.direct_api().invalidate_candidate(c);
    }
}

impl<E> Drop for Net<E> {
    fn drop(&mut self) {
        self.all.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    use crate::portmap::tests::pcp_response;

    fn bound(addr: SocketAddr, all: &Arc<AtomicBool>) -> Bound {
        Bound::new(addr, UdpSocket::bind("127.0.0.1:0").unwrap(), Stop::new(all))
    }

    fn grant(b: &mut Bound, public: std::net::SocketAddrV4, now: Instant) -> Vec<u8> {
        let lease = b.lease.as_mut().unwrap();
        let (request, gateway) = lease.poll(now).unwrap();
        let reply = pcp_response(request[24..36].try_into().unwrap(), b.addr.port(), public, 60, 0);
        lease.answer(gateway, &reply, now).unwrap();
        request
    }

    #[test]
    fn stun_and_the_lease_share_one_candidate_until_the_lease_is_lost() {
        use str0m::ice::{IceAgent, IceCreds};

        let now = Instant::now();
        let local = "192.168.1.2:5000".parse().unwrap();
        let other = "192.168.1.3:5001".parse().unwrap();
        let public: std::net::SocketAddrV4 = "203.0.113.9:5000".parse().unwrap();
        let server = Some("192.0.2.1:3478".parse().unwrap());
        let all = Arc::new(AtomicBool::new(false));
        for stun_first in [true, false] {
            let provider = str0m::crypto::from_feature_flags();
            let mut ice = IceAgent::with_hmac(IceCreds::new(), provider.sha1_hmac_provider);
            let mut b = bound(local, &all);
            b.lease = Some(Lease::new(local, Ipv4Addr::new(192, 168, 1, 1), now));
            for stun in [stun_first, !stun_first] {
                let previous = b.mapped();
                if stun {
                    b.stun_result = Some(public.into());
                } else {
                    grant(&mut b, public, now);
                }
                assert!(b.reconcile_public(previous, server, now, |c| ice.add_local_candidate(c).is_some()).is_empty());
                assert_eq!(b.reflexive.len(), 1);
            }
            let mut other = bound(other, &all);
            other.stun_result = Some(public.into());
            other.reconcile_public(None, server, now, |c| ice.add_local_candidate(c).is_some());
            assert_eq!(ice.local_candidates().count(), 2);

            let previous = b.mapped();
            b.lease = None;
            let withdrawn = b.reconcile_public(previous, server, now, |_| false);
            assert_eq!(withdrawn.len(), 1);
            assert!(ice.invalidate_candidate(&withdrawn[0]));
            assert!(b.reflexive.is_empty());
            assert_eq!(ice.local_candidates().count(), 1);
            let probe = b.probe.as_mut().unwrap();
            let (req, server_addr) = probe.poll(now).unwrap();

            let data = stun::answer_for(u64::from_be_bytes(req[8..16].try_into().unwrap()));
            b.stun_result = probe.answer(server_addr, local, &data);
            b.reconcile_public(None, server, now, |c| ice.add_local_candidate(c).is_some());
            assert_eq!(b.reflexive.len(), 1);
            assert_eq!(ice.local_candidates().count(), 2);
        }
    }

    #[test]
    fn a_restart_replaces_the_lease_only_with_the_current_gateway_lookup() {
        let now = Instant::now();
        let local = "127.0.0.1:5000".parse().unwrap();
        let one = Ipv4Addr::new(127, 0, 0, 1);
        let two = Ipv4Addr::new(127, 0, 0, 2);
        let all = Arc::new(AtomicBool::new(false));
        let mut net = Net::<NetEvent> {
            sockets: vec![bound(local, &all)], relays: Vec::new(), gateway_lookup: 0,
            stun: None, turn: Vec::new(), direct: false, tx: channel().0,
            all, pending_udp: Arc::new(AtomicUsize::new(0)),
        };
        net.find_gateways().unwrap();
        net.set_gateway(1, local, Some(one), now);
        let endpoint = "203.0.113.9:5000".parse().unwrap();
        let old = grant(&mut net.sockets[0], endpoint, now);
        net.sockets[0].reconcile_public(None, None, now, |_| true);
        net.find_gateways().unwrap();
        assert!(net.set_gateway(2, local, Some(one), now).is_empty());
        assert!(net.sockets[0].lease.as_mut().unwrap().poll(now).is_none());
        net.find_gateways().unwrap();
        let withdrawn = net.set_gateway(3, local, Some(two), now);
        assert_eq!(withdrawn.len(), 1);
        assert_eq!((withdrawn[0].local(), withdrawn[0].addr()), (local, endpoint.into()));
        net.set_gateway(2, local, Some(one), now);
        let (new, destination) = net.sockets[0].lease.as_mut().unwrap().poll(now).unwrap();
        assert_eq!(destination.ip(), two);
        assert_ne!(old[24..36], new[24..36], "a new gateway gets a new nonce");
    }

    #[test]
    fn local_datagrams_have_a_shared_queue_limit() {
        let pending = Arc::new(AtomicUsize::new(0));
        let mut slots: Vec<_> = (0..UDP_BACKLOG).map(|_| UdpSlot::reserve(&pending).unwrap()).collect();
        assert!(UdpSlot::reserve(&pending).is_none());
        drop(slots.pop());
        assert!(UdpSlot::reserve(&pending).is_some());
    }

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
