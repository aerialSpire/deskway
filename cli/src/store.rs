//! What `deskway` keeps on this machine, in a folder only its user can read: the account it
//! signed in to (`account.json`, with its two cookies), the computers' host keys as first seen
//! (`hostkeys.json`), where the local proxy listens (`proxy.json`), and the device key (`keys`).
//!
//! The folder is `%LOCALAPPDATA%\Deskway\cli` on Windows and
//! `~/Library/Application Support/Deskway/cli` on a Mac; `DESKWAY_CLI_HOME` puts it elsewhere
//! (tests, or two accounts side by side). On a Mac the folder is made 0700.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub fn dir() -> Result<PathBuf> {
    let d = match std::env::var_os("DESKWAY_CLI_HOME") {
        Some(d) => PathBuf::from(d),
        None => base()?.join("Deskway").join("cli"),
    };
    std::fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).with_context(|| format!("make {} private", d.display()))?;
    }
    Ok(d)
}

#[cfg(windows)]
fn base() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).context("LOCALAPPDATA is not set")
}

#[cfg(target_os = "macos")]
fn base() -> Result<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support")).context("HOME is not set")
}

/// Writes `bytes` so only this user reads them (the folder already is; a Mac file is 0600 too).
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

fn read<T: DeserializeOwned + Default>(name: &str) -> Result<T> {
    match std::fs::read(dir()?.join(name)) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("{name} is damaged")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("read {name}")),
    }
}

fn write<T: Serialize>(name: &str, v: &T) -> Result<()> {
    write_private(&dir()?.join(name), &serde_json::to_vec_pretty(v)?)
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
        read("account.json")
    }

    pub fn save(&self) -> Result<()> {
        write("account.json", self)
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
    Ok(read::<BTreeMap<String, String>>("hostkeys.json")?.get(computer).cloned())
}

pub fn pin_host_key(computer: &str, hk: &str) -> Result<()> {
    let mut all: BTreeMap<String, String> = read("hostkeys.json")?;
    all.insert(computer.to_string(), hk.to_string());
    write("hostkeys.json", &all)
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
        read("proxy.json")
    }

    pub fn save(&self) -> Result<()> {
        write("proxy.json", self)
    }
}
