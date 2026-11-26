//! Whether a controller runs on the very computer it would control (docs/session-lifecycle.md,
//! "The computer you are using"): a hint the controller shows before connecting, never a rule;
//! connecting works the same either way.
//!
//! A controller says where it is with locators, and each computer of its account answers
//! whether one of them is itself (`is_here`). A browser cannot know its own addresses, so it
//! gives the mDNS host names WebRTC uses for them (`<uuid>.local`): only the computer they
//! resolve to one of its own addresses on is it. An AI client gives its machine (`machine`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A computer looks at no more locators than this, each no longer than `LONGEST`.
pub const MOST: usize = 8;
pub const LONGEST: usize = 128;

/// Whether `ip` is this computer's own: loopback, or an address of one of its interfaces.
pub fn own(ip: IpAddr) -> bool {
    ip.is_loopback() || if_addrs::get_if_addrs().is_ok_and(|list| list.iter().any(|i| i.ip() == ip))
}

/// Whether one of `locators` is this computer. Name lookups wait for the network: call it off
/// any thread that must not wait.
pub fn any_here(locators: &[String]) -> bool {
    locators.iter().take(MOST).any(|l| is_here(l))
}

/// Whether `locator` is this computer: its machine, or an mDNS name that resolves to one of
/// its addresses. Anything else is not.
pub fn is_here(locator: &str) -> bool {
    if locator.len() > LONGEST {
        return false;
    }
    if locator.starts_with(MACHINE) {
        return machine().as_deref() == Some(locator);
    }
    if !mdns_name(locator) {
        return false;
    }
    // Responders answer one question once a second at most: a name asked again (the page's
    // socket back) is answered from here. A name is a page's own, and stays where it is.
    static ANSWERED: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());
    if let Some((_, here)) = ANSWERED.lock().unwrap().iter().find(|(n, _)| n == locator) {
        return *here;
    }
    let here = resolve(locator).into_iter().any(own);
    let mut answered = ANSWERED.lock().unwrap();
    if answered.len() >= 256 {
        answered.remove(0);
    }
    answered.push((locator.to_string(), here));
    here
}

/// How long an mDNS question waits for its answer. A browser on this computer answers at once.
const WAIT: Duration = Duration::from_secs(1);

/// The addresses `name` has on the local link, asked by mDNS alone (RFC 6762): never through
/// a DNS server, which must not learn a browser's names. Asked and answered on the mDNS port
/// itself, which every responder there shares. A and AAAA go as two queries: Windows answers
/// for the browser only a query of one question.
fn resolve(name: &str) -> Vec<IpAddr> {
    let Ok(socket) = mdns_socket() else { return Vec::new() };
    let group: SocketAddr = (MDNS_V4, 5353).into();
    let socket: UdpSocket = socket.into();
    if [A, AAAA].iter().any(|kind| socket.send_to(&question(name, *kind), group).is_err()) {
        return Vec::new();
    }
    let end = Instant::now() + WAIT;
    let mut buf = [0u8; 1500];
    while let Some(left) = end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) {
        let _ = socket.set_read_timeout(Some(left));
        let Ok(n) = socket.recv(&mut buf) else { break };
        let found = answers(&buf[..n], name);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

const MDNS_V4: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);

/// A socket on the shared mDNS port in the group on every interface, hearing what it sends.
fn mdns_socket() -> std::io::Result<socket2::Socket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 5353)).into())?;
    s.set_multicast_loop_v4(true)?;
    let mut joined = s.join_multicast_v4(&MDNS_V4, &Ipv4Addr::UNSPECIFIED).is_ok();
    for ip in crate::net::local_ips() {
        if let IpAddr::V4(v4) = ip {
            joined |= s.join_multicast_v4(&MDNS_V4, &v4).is_ok();
        }
    }
    if !joined {
        return Err(std::io::Error::other("not in the mDNS group"));
    }
    Ok(s)
}

const A: u16 = 1;
const AAAA: u16 = 28;

/// An mDNS query of one question: `name`'s records of `kind`.
fn question(name: &str, kind: u16) -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&kind.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes()); // IN
    q
}

/// The A and AAAA records for `name` in an mDNS message; nothing from one that does not parse.
fn answers(msg: &[u8], name: &str) -> Vec<IpAddr> {
    let word = |i: usize| msg.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize);
    let (Some(questions), Some(records)) = (word(4), word(6)) else { return Vec::new() };
    let mut at = 12;
    for _ in 0..questions {
        let Some((_, next)) = read_name(msg, at) else { return Vec::new() };
        at = next + 4;
    }
    let mut out = Vec::new();
    for _ in 0..records {
        let Some((owner, next)) = read_name(msg, at) else { break };
        let (Some(kind), Some(len)) = (word(next), word(next + 8)) else { break };
        let data = next + 10;
        let Some(rdata) = msg.get(data..data + len) else { break };
        if owner.eq_ignore_ascii_case(name) {
            match (kind as u16, rdata.len()) {
                (A, 4) => out.push(IpAddr::from(<[u8; 4]>::try_from(rdata).unwrap())),
                (AAAA, 16) => out.push(IpAddr::from(<[u8; 16]>::try_from(rdata).unwrap())),
                _ => {}
            }
        }
        at = data + len;
    }
    out
}

/// The name at `at` (following compression pointers), and where what follows it starts.
fn read_name(msg: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut after = None;
    for _ in 0..64 {
        let len = *msg.get(at)? as usize;
        if len == 0 {
            return Some((labels.join("."), after.unwrap_or(at + 1)));
        }
        if len & 0xc0 == 0xc0 {
            after.get_or_insert(at + 2);
            at = ((len & 0x3f) << 8) | *msg.get(at + 1)? as usize;
            continue;
        }
        labels.push(String::from_utf8_lossy(msg.get(at + 1..at + 1 + len)?).into_owned());
        at += 1 + len;
    }
    None
}

/// An mDNS host name as WebRTC gives one: letters, digits and dashes, ending in `.local`.
fn mdns_name(s: &str) -> bool {
    s.strip_suffix(".local").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

const MACHINE: &str = "machine:";

/// This computer as a locator: its system's machine id, hashed (the id itself never leaves
/// it). None where the system does not say.
pub fn machine() -> Option<String> {
    let id = machine_id()?;
    Some(format!("{MACHINE}{}", wire::crypto::hex(&wire::crypto::sha256(format!("deskway-machine-v1\0{id}").as_bytes()))))
}

/// The id Windows gives this installation (`MachineGuid`).
#[cfg(windows)]
fn machine_id() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY};
    let mut buf = [0u16; 64];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let r = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, w!(r"SOFTWARE\Microsoft\Cryptography"), w!("MachineGuid"), RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
    if r.is_err() {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(size as usize / 2)]).trim_end_matches('\0').to_string();
    (!s.is_empty()).then_some(s)
}

/// The Mac's hardware UUID (`IOPlatformUUID`).
#[cfg(target_os = "macos")]
fn machine_id() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("\"IOPlatformUUID\""))?;
    let id = line.rsplit('"').nth(1)?.to_string();
    (!id.is_empty()).then_some(id)
}

/// The id of this Linux installation.
#[cfg(target_os = "linux")]
fn machine_id() -> Option<String> {
    let id = std::fs::read_to_string("/etc/machine-id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn machine_id() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computers_own_addresses_are_loopback_and_its_interfaces() {
        let mine = crate::net::local_ips().into_iter().map(|ip| (ip, true));
        let others = [("127.0.0.1", true), ("::1", true), ("192.0.2.1", false), ("2001:db8::77", false)].map(|(s, own)| (s.parse().unwrap(), own));
        for (ip, want) in mine.chain(others) {
            assert_eq!(own(ip), want, "{ip}");
        }
    }

    /// What a computer takes as itself. (A browser's own mDNS names are checked end to end:
    /// they exist only while its page does, e2e `devices.test.mjs`.)
    #[test]
    fn a_computer_knows_itself_by_its_machine_and_nothing_else_says_so() {
        let me = machine().expect("this machine has a system id");
        assert!(me.starts_with(MACHINE) && me.len() == MACHINE.len() + 64, "{me}");
        assert_eq!(machine(), Some(me.clone()), "the same every time");
        let other = format!("{MACHINE}{}", "0".repeat(64));
        let long = format!("{}.local", "a".repeat(LONGEST));
        for (locator, want) in [
            (me.as_str(), true),
            (other.as_str(), false),
            ("localhost", false),
            ("127.0.0.1", false),
            ("no-such-name-deskway-test.local", false),
            ("bad_name!.local", false),
            (long.as_str(), false),
        ] {
            assert_eq!(is_here(locator), want, "{locator}");
        }
        assert!(any_here(&["x.local".into(), me.clone()]));
        let many: Vec<String> = std::iter::repeat_n(other, MOST).chain([me]).collect();
        assert!(!any_here(&many), "no more than MOST are looked at");
    }

    #[test]
    fn an_mdns_answer_gives_the_names_addresses() {
        let q = question("ab-1.local", A);
        assert_eq!(q, [&[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0][..], &[4], b"ab-1", &[5], b"local", &[0, 0, 1, 0, 1]].concat(), "one question");
        // The answer: the question echoed, then an A record by a pointer to it and an AAAA one.
        let mut m = vec![0, 0, 0x84, 0, 0, 1, 0, 3, 0, 0, 0, 0];
        m.extend_from_slice(&q[12..]);
        let rec = |kind: u16, data: &[u8]| [&[0xc0, 12][..], &kind.to_be_bytes(), &[0x80, 1, 0, 0, 0, 120], &(data.len() as u16).to_be_bytes(), data].concat();
        m.extend(rec(1, &[192, 168, 1, 7]));
        m.extend(rec(28, &[0xfd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9]));
        m.extend([&[5][..], b"other", &[5], b"local", &[0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4, 10, 0, 0, 1]].concat());
        assert_eq!(answers(&m, "AB-1.local"), vec!["192.168.1.7".parse::<IpAddr>().unwrap(), "fd00::9".parse().unwrap()], "its own records, not another name's");
        assert!(answers(&m[..20], "ab-1.local").is_empty(), "cut short: nothing");
    }
}
