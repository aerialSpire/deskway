//! This AI client's device key: P-256, made once, kept protected for this user (Windows:
//! DPAPI; macOS: the login keychain), and used without
//! asking anyone. The computers
//! know it as an AI client's (docs/security.md, "Devices").
//!
//! Both keep the key so that this user's own programs could read it (docs/security.md,
//! R2: a key held by the TPM or the Secure Enclave comes later).

use anyhow::{Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::SigningKey;
use p256::pkcs8::EncodePublicKey;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as native;

/// What is kept: the key's credential id, and its private scalar (protected).
#[derive(Serialize, Deserialize)]
struct Kept {
    cred: String,
    /// The protected scalar, base64 (Windows); empty where the keychain holds it (macOS).
    #[serde(default)]
    sealed: String,
}

pub struct DeviceKey {
    pub cred: String,
    key: SigningKey,
}

impl DeviceKey {
    /// The key made before, or a new one, kept from now on.
    pub fn load_or_create() -> Result<DeviceKey> {
        let path = crate::store::dir()?.join("device.json");
        if let Ok(b) = std::fs::read(&path) {
            let kept: Kept = serde_json::from_slice(&b).context("device.json is damaged")?;
            let scalar = native::open(&kept.cred, &kept.sealed).context("the device key cannot be read")?;
            return Ok(DeviceKey { key: SigningKey::from_slice(&scalar).context("the device key is damaged")?, cred: kept.cred });
        }
        let cred = URL_SAFE_NO_PAD.encode(wire::crypto::random::<16>());
        let key = SigningKey::from_slice(&wire::crypto::random::<32>()).context("make a device key")?;
        let sealed = native::seal(&cred, &key.to_bytes())?;
        crate::store::write_private(&path, &serde_json::to_vec(&Kept { cred: cred.clone(), sealed })?)?;
        Ok(DeviceKey { cred, key })
    }

    /// Forgets the key (signing out for good: a new sign-in makes a new device).
    pub fn forget() -> Result<()> {
        let path = crate::store::dir()?.join("device.json");
        if let Ok(b) = std::fs::read(&path) {
            if let Ok(kept) = serde_json::from_slice::<Kept>(&b) {
                native::forget(&kept.cred);
            }
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// The public key as the wire carries it: SubjectPublicKeyInfo, base64.
    pub fn spki(&self) -> String {
        STANDARD.encode(self.key.verifying_key().to_public_key_der().expect("encode a P-256 key").as_bytes())
    }

    /// Signs `challenge` (a SHA-256 the caller made) as the browsers do: ECDSA P-256 with
    /// SHA-256, r||s, base64url.
    pub fn sign(&self, challenge: &[u8]) -> String {
        let sig: p256::ecdsa::Signature = self.key.sign(challenge);
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    }
}
