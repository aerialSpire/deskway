//! Windows: the key's scalar sealed with DPAPI for this user (CryptProtectData), kept in the
//! file beside the credential id.

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};

fn take(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let out = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(blob.pbData as _)));
    }
    out
}

pub fn seal(_cred: &str, scalar: &[u8]) -> Result<String> {
    let input = CRYPT_INTEGER_BLOB { cbData: scalar.len() as u32, pbData: scalar.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptProtectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.context("DPAPI could not seal the device key")?;
    Ok(STANDARD.encode(take(out)))
}

pub fn open(_cred: &str, sealed: &str) -> Result<Vec<u8>> {
    let mut sealed = STANDARD.decode(sealed).context("the sealed key is not base64")?;
    let input = CRYPT_INTEGER_BLOB { cbData: sealed.len() as u32, pbData: sealed.as_mut_ptr() };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe { CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.context("DPAPI could not open the device key (another user's?)")?;
    Ok(take(out))
}

pub fn forget(_cred: &str) {}
