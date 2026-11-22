//! The signature checks under every proof: browser-made device keys (plain ECDSA) and WebAuthn
//! assertions, checked as a relying party would.

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::pkcs8::DecodePublicKey;
use serde_json::Value;
use sha2::Sha256;

use super::b64u;
use wire::crypto::sha256;

/// COSE algorithm identifiers accepted for passkeys.
pub const ES256: i64 = -7;
pub const RS256: i64 = -257;

pub(super) enum PublicKey {
    Ec(p256::ecdsa::VerifyingKey),
    Rsa(rsa::pkcs1v15::VerifyingKey<Sha256>),
}

pub(super) fn public_key(alg: i64, der: &[u8]) -> Result<PublicKey> {
    match alg {
        ES256 => Ok(PublicKey::Ec(p256::ecdsa::VerifyingKey::from_public_key_der(der).context("bad P-256 key")?)),
        RS256 => {
            let k = rsa::RsaPublicKey::from_public_key_der(der).context("bad RSA key")?;
            ensure!(rsa::traits::PublicKeyParts::size(&k) >= 256, "RSA key shorter than 2048 bits");
            Ok(PublicKey::Rsa(rsa::pkcs1v15::VerifyingKey::new(k)))
        }
        _ => bail!("unsupported passkey algorithm {alg}"),
    }
}

/// A device key made by the browser itself (no passkey, no prompt): a plain ECDSA P-256
/// signature, r||s, over the 32-byte challenge.
pub(super) fn raw_ok(alg: i64, key_b64: &str, challenge: &[u8], sig_b64: &str) -> bool {
    if alg != ES256 {
        return false;
    }
    let (Ok(der), Ok(sig)) = (STANDARD.decode(key_b64), b64u(sig_b64)) else { return false };
    let (Ok(k), Ok(sig)) = (p256::ecdsa::VerifyingKey::from_public_key_der(&der), p256::ecdsa::Signature::from_slice(&sig)) else { return false };
    k.verify(challenge, &sig).is_ok()
}

pub(super) struct AssertionCheck<'a> {
    pub rp_id: &'a str,
    pub origin: &'a str,
    pub challenge: &'a [u8],
    pub alg: i64,
    pub key: &'a [u8],
    pub auth_data: &'a [u8],
    pub client_data: &'a [u8],
    pub sig: &'a [u8],
}

/// Verifies a WebAuthn assertion as a relying party would, and returns its signature counter.
pub(super) fn check_assertion(a: &AssertionCheck) -> Result<u32> {
    let cd: Value = serde_json::from_slice(a.client_data).context("bad client data")?;
    ensure!(cd["type"] == "webauthn.get", "not an assertion");
    ensure!(cd["challenge"].as_str().is_some_and(|c| b64u(c).ok().as_deref() == Some(a.challenge)), "challenge does not match this offer");
    ensure!(cd["origin"].as_str() == Some(a.origin), "passkey used on another site");
    ensure!(cd["crossOrigin"].as_bool() != Some(true), "passkey used in a frame");
    ensure!(a.auth_data.len() >= 37, "authenticator data too short");
    ensure!(a.auth_data[..32] == sha256(a.rp_id.as_bytes()), "passkey belongs to another site");
    let flags = a.auth_data[32];
    // User present and user verified: someone unlocked the passkey with a PIN or biometric.
    ensure!(flags & 0x01 != 0 && flags & 0x04 != 0, "passkey was used without verifying the user");
    let count = u32::from_be_bytes(a.auth_data[33..37].try_into().unwrap());
    let mut signed = a.auth_data.to_vec();
    signed.extend_from_slice(&sha256(a.client_data));
    match public_key(a.alg, a.key)? {
        PublicKey::Ec(k) => {
            let sig = p256::ecdsa::Signature::from_der(a.sig).context("bad signature encoding")?;
            k.verify(&signed, &sig).context("signature does not match")?;
        }
        PublicKey::Rsa(k) => {
            let sig = rsa::pkcs1v15::Signature::try_from(a.sig).context("bad signature encoding")?;
            rsa::signature::Verifier::verify(&k, &signed, &sig).context("signature does not match")?;
        }
    }
    Ok(count)
}
