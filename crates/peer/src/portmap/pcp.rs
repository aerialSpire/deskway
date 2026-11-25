//! PCP MAP wire format (RFC 6887).

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

const VERSION: u8 = 2;
const MAP: u8 = 1;
const RESPONSE: u8 = 128;
const UDP: u8 = 17;
const UNSUPPORTED_VERSION: u8 = 1;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Reply {
    Mapped { external: SocketAddr, lifetime: u32 },
    NoMapping,
    UnsupportedVersion,
}

fn encode_ip(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(ip) => ip.to_ipv6_mapped().octets(),
        IpAddr::V6(ip) => ip.octets(),
    }
}

pub(super) fn map_request(
    local: SocketAddr,
    nonce: [u8; 12],
    suggested: Option<SocketAddr>,
    lifetime: u32,
) -> [u8; 60] {
    let mut packet = [0; 60];
    packet[0] = VERSION;
    packet[1] = MAP;
    packet[4..8].copy_from_slice(&lifetime.to_be_bytes());
    packet[8..24].copy_from_slice(&encode_ip(local.ip()));
    packet[24..36].copy_from_slice(&nonce);
    packet[36] = UDP;
    packet[40..42].copy_from_slice(&local.port().to_be_bytes());
    if lifetime != 0 {
        let ip = if local.is_ipv4() {
            IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        };
        let suggested = suggested.unwrap_or(SocketAddr::new(ip, 0));
        packet[42..44].copy_from_slice(&suggested.port().to_be_bytes());
        packet[44..60].copy_from_slice(&encode_ip(suggested.ip()));
    }
    packet
}

pub(super) fn response(data: &[u8], internal: u16, nonce: [u8; 12]) -> Option<Reply> {
    if !(24..=1100).contains(&data.len()) || data.len() % 4 != 0 || data[1] != MAP | RESPONSE {
        return None;
    }
    if data[3] == UNSUPPORTED_VERSION {
        return Some(Reply::UnsupportedVersion);
    }
    if data[0] != VERSION
        || data.len() < 60
        || data[24..36] != nonce
        || data[36] != UDP
        || data[40..42] != internal.to_be_bytes()
    {
        return None;
    }
    let mut options = &data[60..];
    while !options.is_empty() {
        let len = u16::from_be_bytes(options[2..4].try_into().unwrap()) as usize;
        options = options.get(4 + len.next_multiple_of(4)..)?;
    }
    let lifetime = u32::from_be_bytes(data[4..8].try_into().unwrap());
    if data[3] != 0 || lifetime == 0 {
        return Some(Reply::NoMapping);
    }
    let port = u16::from_be_bytes(data[42..44].try_into().unwrap());
    if port == 0 {
        return None;
    }
    let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&data[44..60]).unwrap());
    let ip = ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4);
    Some(Reply::Mapped { external: SocketAddr::new(ip, port), lifetime })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: [u8; 12] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

    #[test]
    fn rfc6887_map_requests() {
        let local = "192.168.1.50:5000".parse().unwrap();
        let external = "198.51.100.5:9000".parse().unwrap();
        let prefix = [
            2, 1, 0, 0, 0, 0, 0, 120, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 192, 168, 1, 50, 1, 2, 3, 4, 5, 6, 7, 8,
            9, 10, 11, 12, 17, 0, 0, 0, 0x13, 0x88,
        ];
        for (suggested, lifetime, tail) in [
            (None, 120, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0, 0, 0]),
            (Some(external), 120, [0x23, 0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 198, 51, 100, 5]),
            (Some(external), 0, [0; 18]),
        ] {
            let mut head = prefix;
            head[7] = lifetime as u8;
            assert_eq!(map_request(local, NONCE, suggested, lifetime).as_slice(), [head.as_slice(), &tail].concat());
        }
    }

    #[test]
    fn rfc6887_map_responses() {
        let mapped = [
            2, 129, 0, 0, 0, 0, 0, 60, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
            11, 12, 17, 0, 0, 0, 0x13, 0x88, 0x23, 0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 198, 51, 100, 5,
        ];
        let endpoint = || Reply::Mapped { external: "198.51.100.5:9000".parse().unwrap(), lifetime: 60 };
        let mut deleted = mapped;
        deleted[7] = 0;
        deleted[42..].fill(0);
        let mut error = mapped;
        error[3] = 2;
        let mut unsupported = mapped[..24].to_vec();
        unsupported[0] = 1;
        unsupported[3] = 1;
        let option = [mapped.as_slice(), &[5, 0, 0, 1, 42, 0, 0, 0]].concat();
        for (packet, expected) in [
            (&mapped[..], Some(endpoint())),
            (&deleted, Some(Reply::NoMapping)),
            (&error, Some(Reply::NoMapping)),
            (&unsupported, Some(Reply::UnsupportedVersion)),
            (&option, Some(endpoint())),
            (&option[..64], None),
            (&mapped[..59], None),
            (&mapped[..24], None),
        ] {
            assert_eq!(response(packet, 5000, NONCE), expected, "{packet:?}");
        }
        for (offset, value) in [(0, 1), (1, 1), (24, 0), (36, 6), (41, 0)] {
            let mut wrong = mapped;
            wrong[offset] = value;
            assert_eq!(response(&wrong, 5000, NONCE), None, "byte {offset}");
        }
    }
}
