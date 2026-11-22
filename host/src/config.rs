use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Enrollment result and local settings. The password is never stored: only this device's own token.
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub server: String,
    pub id: String,
    pub token: String,
    pub name: String,
    /// Locks the computer when a remote session ends.
    #[serde(default)]
    pub auto_lock: bool,
    /// Shares this computer's clipboard with whoever controls it. Off unless turned on from a
    /// session's toolbar. Sending files or clipboard contents explicitly works either way.
    #[serde(default)]
    pub clip_share: bool,
    /// The language this host shows text in ("en", "zh-Hans", "zh-Hant"). None follows the
    /// system's UI language.
    #[serde(default)]
    pub lang: Option<String>,
    /// Keeps the computer from sleeping while Deskway runs (`power`). The display may still
    /// turn off; a closed laptop lid follows the system's setting.
    #[serde(default)]
    pub prevent_sleep: bool,
    /// The account this computer belongs to, as the server last said (`account`), for the
    /// panel. Shown only, never trusted.
    #[serde(default)]
    pub account: Option<Account>,
    /// A newly enrolled Mac still needs the app to register its login item.
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub login_item_setup_pending: bool,
}

/// An account's sign-in name and the name others see ("" until the server has said).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub user: String,
    #[serde(default)]
    pub display: String,
}

#[cfg(windows)]
static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
static CONFIG_WRITE: Mutex<()> = Mutex::new(());

/// Service mode keeps everything in a folder only SYSTEM, administrators and the network
/// service can open, instead of the user's profile.
#[cfg(windows)]
pub fn use_dir(path: PathBuf) {
    let _ = DIR.set(path);
}

/// Writes through a temporary file and a rename, so a crash never leaves half a file. The
/// folder's permissions decide who can read it.
///
/// The temporary file is always new: anything already at that name, such as a hard link
/// another account planted to make a privileged writer overwrite some other file, is removed
/// first rather than written through.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn dir() -> Result<PathBuf> {
    #[cfg(windows)]
    if let Some(d) = DIR.get() {
        return Ok(d.clone());
    }
    let base = std::env::var_os("DESKWAY_HOME")
        .map(PathBuf::from)
        .or_else(crate::platform::config_dir)
        .context("no config directory")?;
    std::fs::create_dir_all(&base)?;
    Ok(base)
}

/// The config's file name, in its folder.
const FILE: &str = "host.json";

impl Config {
    pub fn load() -> Result<Config> {
        Self::load_from(&dir()?)
    }

    /// The config kept in `dir` (the service's, read by the installer window).
    pub fn load_from(dir: &Path) -> Result<Config> {
        Ok(serde_json::from_slice(&std::fs::read(dir.join(FILE))?)?)
    }

    pub fn save(&self) -> Result<()> {
        let _write = CONFIG_WRITE.lock().unwrap();
        self.write_to(&dir()?)
    }

    /// Update just the selected settings. The panel and session can write independently.
    pub fn update(change: impl FnOnce(&mut Self)) -> Result<Self> {
        Self::update_in(&dir()?, change)
    }

    /// `update`, for the config kept in `dir`.
    pub fn update_in(dir: &Path, change: impl FnOnce(&mut Self)) -> Result<Self> {
        let _write = CONFIG_WRITE.lock().unwrap();
        let mut config = Self::load_from(dir)?;
        change(&mut config);
        config.write_to(dir)?;
        Ok(config)
    }

    fn write_to(&self, dir: &Path) -> Result<()> {
        write_private(&dir.join(FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// Signs this computer out here: removes everything that ties it to its account (`ACCOUNT`),
    /// in the folder in use. Settings of the computer itself stay.
    pub fn sign_out() -> Result<()> {
        Config::sign_out_in(&dir()?)
    }

    fn sign_out_in(dir: &Path) -> Result<()> {
        let _write = CONFIG_WRITE.lock().unwrap();
        for f in ACCOUNT {
            match std::fs::remove_file(dir.join(f)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("remove {f}")),
                _ => {}
            }
        }
        Ok(())
    }
}

/// What ties a computer to its account: the sign-in, its host key, the devices it trusts or
/// blocks, a pairing under way, the AI switch, and the record of what AI clients did there,
/// which the next account must not read.
const ACCOUNT: &[&str] = &[FILE, "host-key", "controllers.json", "blocked.json", "pairing.json", "ai.json", "audit.jsonl", "audit.old.jsonl"];

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("deskway-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_config_from_an_older_version_takes_the_defaults() {
        let d = temp("defaults");
        std::fs::write(d.join(FILE), r#"{"server":"https://s.example","id":"H1","token":"t","name":""}"#).unwrap();
        let c = Config::load_from(&d).unwrap();
        assert!(!c.auto_lock && !c.clip_share && !c.prevent_sleep && c.lang.is_none());
    }

    #[test]
    fn an_update_changes_only_what_it_names_and_is_kept() {
        let d = temp("update");
        assert!(Config::update_in(&d, |c| c.auto_lock = true).is_err(), "nothing to update before enrolling");
        std::fs::write(d.join(FILE), r#"{"server":"https://s.example","id":"H1","token":"t","name":"Desk","clip_share":true}"#).unwrap();
        let c = Config::update_in(&d, |c| c.lang = Some("zh-Hant".into())).unwrap();
        assert_eq!((c.lang.as_deref(), c.clip_share, c.name.as_str()), (Some("zh-Hant"), true, "Desk"));
        let again = Config::load_from(&d).unwrap();
        assert_eq!((again.lang.as_deref(), again.token.as_str()), (Some("zh-Hant"), "t"));
        assert!(!d.join("host.tmp").exists(), "written through a temporary file, which is gone");
    }

    #[test]
    fn signing_out_removes_what_ties_the_computer_to_its_account_and_keeps_its_settings() {
        let d = temp("sign-out");
        for f in ACCOUNT.iter().chain(["settings.json", "ipc.key"].iter()) {
            std::fs::write(d.join(f), "x").unwrap();
        }
        Config::sign_out_in(&d).unwrap();
        assert!(ACCOUNT.iter().all(|f| !d.join(f).exists()), "every account file is gone");
        assert!(d.join("settings.json").exists() && d.join("ipc.key").exists(), "the computer's own files stay");
        Config::sign_out_in(&d).unwrap();
    }
}
