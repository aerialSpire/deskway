//! Linux: the unsealed scalar is stored in device.json with mode 0600.

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

pub fn seal(_cred: &str, scalar: &[u8]) -> Result<String> {
    Ok(STANDARD.encode(scalar))
}

pub fn open(_cred: &str, encoded: &str) -> Result<Vec<u8>> {
    STANDARD.decode(encoded).context("the device key is not base64")
}

pub fn forget(_cred: &str) {}

#[cfg(test)]
mod tests {
    use super::super::DeviceKey;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_device_key_round_trips_in_a_private_file() {
        let dir = std::env::temp_dir().join(format!("deskway-key-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        crate::os::create_private_dir(&dir).unwrap();
        let path = dir.join("device.json");
        let made = DeviceKey::load_or_create_at(&path).unwrap();
        let saved = DeviceKey::load_or_create_at(&path).unwrap();
        let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(saved.cred, made.cred);
        assert_eq!(saved.key.to_bytes(), made.key.to_bytes());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
