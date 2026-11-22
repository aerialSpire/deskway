//! The primitives several parts of the host share: SHA-256, hex, and the system's randomness.

use sha2::{Digest, Sha256};

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// SHA-256 of all `r` gives, read a piece at a time (a file of any size).
pub fn sha256_reader(mut r: impl std::io::Read) -> std::io::Result<[u8; 32]> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match r.read(&mut buf)? {
            0 => return Ok(h.finalize().into()),
            n => h.update(&buf[..n]),
        }
    }
}

/// Lower-case hex, as the signed strings carry hashes.
pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

/// `N` bytes from the system's random source.
pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("system random source");
    b
}
