//! macOS: the key's scalar in the login keychain, as a generic password of the service
//! "Deskway AI client" under the credential id. Built and tested on a Mac.

use anyhow::{Context, Result};
use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

const SERVICE: &str = "Deskway AI client";

pub fn seal(cred: &str, scalar: &[u8]) -> Result<String> {
    set_generic_password(SERVICE, cred, scalar).context("the keychain could not keep the device key")?;
    Ok(String::new())
}

pub fn open(cred: &str, _sealed: &str) -> Result<Vec<u8>> {
    get_generic_password(SERVICE, cred).context("the keychain has no device key for this AI client")
}

pub fn forget(cred: &str) {
    let _ = delete_generic_password(SERVICE, cred);
}
