//! What `deskway` keeps on this machine, in a folder only its user can read: the account it
//! signed in to (`account.json`, with its two cookies), the computers' host keys as first seen
//! (`hostkeys.json`), where the local proxy listens (`proxy.json`), and the device key (`keys`).
//!
//! The platform chooses the default folder. `DESKWAY_CLI_HOME` puts it elsewhere
//! (tests, or two accounts side by side). On Unix the folder is made 0700.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub fn dir() -> Result<PathBuf> {
    let d = match std::env::var_os("DESKWAY_CLI_HOME") {
        Some(d) => PathBuf::from(d),
        None => crate::os::default_state_dir()?,
    };
    crate::os::create_private_dir(&d)?;
    Ok(d)
}

/// Writes `bytes` so only this user reads them (Unix files are 0600).
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    crate::os::set_unix_mode(&tmp, 0o600)?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

fn read<T: DeserializeOwned + Default>(dir: &Path, name: &str) -> Result<T> {
    match std::fs::read(dir.join(name)) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("{name} is damaged")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("read {name}")),
    }
}

fn write<T: Serialize>(dir: &Path, name: &str, v: &T) -> Result<()> {
    write_private(&dir.join(name), &serde_json::to_vec_pretty(v)?)
}

const ACCOUNT_FILE: &str = "account.json";

fn lock_account(dir: &Path) -> Result<File> {
    // The lock stays on the same file across atomic replacements of the account.
    let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .open(dir.join("account.lock")).context("open the profile sign-in lock")?;
    lock.lock().context("lock the profile sign-in")?;
    Ok(lock)
}

/// The account this AI client signed in to.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct Account {
    /// The server, as `https://host`.
    pub server: String,
    pub user: String,
    /// This device's name, as the account's devices list it.
    pub name: String,
    /// The long-lived cookie that names this device's record, and the session cookie.
    pub browser: String,
    pub session: String,
}

impl Account {
    pub fn load() -> Result<Account> {
        Self::load_from(&dir()?)
    }

    pub fn load_from(dir: &Path) -> Result<Self> {
        read(dir, ACCOUNT_FILE)
    }

    pub fn save(&self) -> Result<()> {
        let dir = dir()?;
        let _lock = lock_account(&dir)?;
        write(&dir, ACCOUNT_FILE, self)
    }

    /// Clears the current sign-in and returns the credentials for the server logout.
    pub fn clear() -> Result<Self> {
        let dir = dir()?;
        let _lock = lock_account(&dir)?;
        let saved = Self::load_from(&dir)?;
        let mut cleared = saved.clone();
        cleared.session.clear();
        write(&dir, ACCOUNT_FILE, &cleared)?;
        Ok(saved)
    }

    /// Saves a renewal only while the sign-in it renewed is still the one on disk.
    pub fn save_renewal(&mut self, previous: &Self) -> Result<()> {
        let dir = dir()?;
        let _lock = lock_account(&dir)?;
        let saved = Self::load_from(&dir)?;
        previous.check_sign_in(&saved)?;
        if &saved == previous {
            write(&dir, ACCOUNT_FILE, self)?;
        } else {
            *self = saved;
        }
        Ok(())
    }

    /// Checks the signed-in device even when its session cookie has changed.
    pub fn check_sign_in(&self, saved: &Self) -> Result<()> {
        if !saved.signed_in() {
            bail!("the profile was signed out");
        }
        if self.server != saved.server || self.user != saved.user || self.browser != saved.browser {
            bail!("the profile sign-in changed");
        }
        Ok(())
    }

    /// The Cookie header the server knows this device by.
    pub fn cookies(&self) -> String {
        [("deskway_browser", &self.browser), ("deskway_session", &self.session)].iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }

    pub fn signed_in(&self) -> bool {
        !self.server.is_empty() && !self.session.is_empty()
    }
}

/// Each computer's host key, pinned the first time its signed answer arrived. A key that differs
/// later ends the connection: someone may be posing as the computer.
pub fn host_key(computer: &str) -> Result<Option<String>> {
    Ok(read::<BTreeMap<String, String>>(&dir()?, "hostkeys.json")?.get(computer).cloned())
}

pub fn pin_host_key(computer: &str, hk: &str) -> Result<()> {
    let dir = dir()?;
    let mut all: BTreeMap<String, String> = read(&dir, "hostkeys.json")?;
    all.insert(computer.to_string(), hk.to_string());
    write(&dir, "hostkeys.json", &all)
}

/// Where the local proxy listens, the token its clients prove themselves with, and its version
/// (`update::version`; none from a proxy older than that).
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct ProxyPlace {
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub version: String,
}

impl ProxyPlace {
    pub fn load() -> Result<ProxyPlace> {
        read(&dir()?, "proxy.json")
    }

    pub fn save(&self) -> Result<()> {
        write(&dir()?, "proxy.json", self)
    }
}
