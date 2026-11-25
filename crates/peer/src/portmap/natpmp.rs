//! NAT-PMP wire format (RFC 6886).

use std::net::Ipv4Addr;

const ADDRESS: u8 = 0;
const UDP: u8 = 1;
const RESPONSE: u8 = 128;

pub(super) fn address_request() -> [u8; 2] {
    [0, ADDRESS]
}

pub(super) fn map_request(internal: u16, suggested: u16, lifetime: u32) -> [u8; 12] {
    let mut packet = [0; 12];
    packet[1] = UDP;
    packet[4..6].copy_from_slice(&internal.to_be_bytes());
    if lifetime != 0 {
        packet[6..8].copy_from_slice(&suggested.to_be_bytes());
    }
    packet[8..12].copy_from_slice(&lifetime.to_be_bytes());
    packet
}

/// RFC 6886 section 3.5 uses opcode zero without the response bit for this one error.
pub(super) fn unsupported_version(data: &[u8]) -> bool {
    matches!(data, [0, 0, 0, 1, _, _, _, _])
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Reply {
    Address(Option<Ipv4Addr>),
    Map(Option<(u16, u32)>),
}

pub(super) fn response(data: &[u8], internal: u16) -> Option<Reply> {
    if data.len() < 8 || data[0] != 0 {
        return None;
    }
    let success = data[2..4] == [0, 0];
    match (data[1] ^ RESPONSE, data.len()) {
        (ADDRESS, 12) => Some(Reply::Address(success.then(|| Ipv4Addr::new(data[8], data[9], data[10], data[11])))),
        (UDP, 16) if data[8..10] == internal.to_be_bytes() => {
            let port = u16::from_be_bytes(data[10..12].try_into().unwrap());
            let lifetime = u32::from_be_bytes(data[12..16].try_into().unwrap());
            match (success, port, lifetime) {
                (false, _, _) | (true, 0, 0) => Some(Reply::Map(None)),
                (true, 0, _) | (true, _, 0) => None,
                (true, port, lifetime) => Some(Reply::Map(Some((port, lifetime)))),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6886_requests() {
        assert_eq!(address_request(), [0, 0]);
        for (lifetime, expected) in
            [(120, [0, 1, 0, 0, 0x13, 0x88, 0x23, 0x28, 0, 0, 0, 120]), (0, [0, 1, 0, 0, 0x13, 0x88, 0, 0, 0, 0, 0, 0])]
        {
            assert_eq!(map_request(5000, 9000, lifetime), expected);
        }
    }

    #[test]
    fn rfc6886_responses() {
        let addr = [0, 128, 0, 0, 0, 0, 1, 0, 198, 51, 100, 5];
        let map = [0, 129, 0, 0, 0, 0, 1, 0, 0x13, 0x88, 0x23, 0x28, 0, 0, 0, 60];
        let deleted = [0, 129, 0, 0, 0, 0, 1, 0, 0x13, 0x88, 0, 0, 0, 0, 0, 0];
        let error = [0, 129, 0, 4, 0, 0, 1, 0, 0x13, 0x88, 0, 0, 0, 0, 0, 0];
        let unsupported = [0, 0, 0, 1, 0, 0, 1, 0];
        for (packet, expected) in [
            (&addr[..], Some(Reply::Address(Some(Ipv4Addr::new(198, 51, 100, 5))))),
            (&map, Some(Reply::Map(Some((9000, 60))))),
            (&deleted, Some(Reply::Map(None))),
            (&error, Some(Reply::Map(None))),
            (&map[..15], None),
            (&addr[..8], None),
            (&[1, 128, 0, 0, 0, 0, 1, 0, 198, 51, 100, 5], None),
            (&[0, 130, 0, 0, 0, 0, 1, 0, 0x13, 0x88, 0x23, 0x28, 0, 0, 0, 60], None),
            (&unsupported, None),
        ] {
            assert_eq!(response(packet, 5000), expected, "{packet:?}");
        }
        assert_eq!(response(&map, 5001), None);
        assert!(unsupported_version(&unsupported));
        assert!(!unsupported_version(&unsupported[..7]));
    }
}
