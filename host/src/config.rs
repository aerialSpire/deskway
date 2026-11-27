use anyhow::{Context, Result};
use deskway_server_connection::{Address, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Enrollment result and local settings. The password is never stored: only this device's own token.
#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub server: String,
    #[serde(skip)]
    pub server_root: Option<String>,
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
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&tmp)?;
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
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&base)?;
    Ok(base)
}

/// The config's file name, in its folder.
const FILE: &str = "host.json";
pub const ROOT_FILE: &str = "host-ca.pem";

impl Config {
    pub fn connection(&self) -> Result<Connection> {
        Address::parse(&self.server)?.connection(self.server_root.as_deref())
    }

    pub fn load() -> Result<Config> {
        Self::load_from(&dir()?)
    }

    /// The config kept in `dir` (the service's, read by the installer window).
    pub fn load_from(dir: &Path) -> Result<Config> {
        let mut config = Config::load_from_file(&dir.join(FILE))?;
        if Address::parse(&config.server)?.is_pinned() {
            match std::fs::read_to_string(root_path(dir)) {
                Ok(root) => config.server_root = Some(root),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("read the local server root"),
            }
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let _write = CONFIG_WRITE.lock().unwrap();
        self.write_sign_in(&dir()?)
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

    fn write_sign_in(&self, dir: &Path) -> Result<()> {
        let address = Address::parse(&self.server)?;
        if address.is_pinned() {
            let root = self.server_root.as_deref().context("the pinned local server root is missing")?;
            address.connection(Some(root))?;
            write_private(&root_path(dir), root.as_bytes())?;
        }
        self.write_to(dir)
    }

    /// Signs this computer out here: it stops using its account until someone signs in again.
    /// The server keeps it, and so does this folder: the sign-in moves aside (`KEPT`), and the
    /// devices it trusts, its host key and its AI switch stay, so the same account signing in
    /// again takes this computer back as it was (`settle`). Only removing it from the account
    /// makes it a new computer.
    pub fn sign_out() -> Result<()> {
        Config::sign_out_in(&dir()?)
    }

    fn sign_out_in(dir: &Path) -> Result<()> {
        let _write = CONFIG_WRITE.lock().unwrap();
        match std::fs::rename(dir.join(FILE), dir.join(KEPT)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).context("keep the sign-in"),
            _ => Ok(()),
        }
    }

    /// The computer signed out here, if it was: what to tell the server to take it back.
    pub fn kept() -> Option<Kept> {
        Config::kept_in(&dir().ok()?)
    }

    pub fn kept_in(dir: &Path) -> Option<Kept> {
        let c = Config::load_from_file(&dir.join(KEPT)).ok()?;
        Some(Kept { server: c.server, id: c.id, user: c.account.map(|a| a.user) })
    }

    /// Takes a sign-in `enroll_with` made, in `dir`. When the server answered with the id of the
    /// computer kept here, it is that computer again: its sign-in comes back with what it trusts.
    /// Otherwise it is a new computer, and whatever tied this one to its earlier account goes
    /// first (`forget_in`), so the next account neither inherits its trust nor reads its audit.
    pub fn settle_in(dir: &Path, new: Config) -> Result<Config> {
        let kept = Config::load_from_file(&dir.join(KEPT)).ok().filter(|k| k.id == new.id && k.server == new.server);
        let cfg = match kept {
            Some(mut k) => {
                k.account = new.account;
                k.server_root = new.server_root;
                // Signing out took the login item away; signing in sets it up again.
                #[cfg(target_os = "macos")]
                {
                    k.login_item_setup_pending = new.login_item_setup_pending;
                }
                k
            }
            None => {
                anyhow::ensure!(!new.token.is_empty(), "the computer signed out here is gone; sign in again");
                Config::forget_in(dir)?;
                new
            }
        };
        let _write = CONFIG_WRITE.lock().unwrap();
        cfg.write_sign_in(dir)?;
        let _ = std::fs::remove_file(dir.join(KEPT));
        Ok(cfg)
    }

    /// `settle_in`, in the folder in use.
    pub fn settle(new: Config) -> Result<Config> {
        Config::settle_in(&dir()?, new)
    }

    /// Removes everything that ties this computer to an account (`ACCOUNT`), in `dir`. Settings
    /// of the computer itself stay.
    pub fn forget_in(dir: &Path) -> Result<()> {
        let _write = CONFIG_WRITE.lock().unwrap();
        for f in ACCOUNT {
            match std::fs::remove_file(dir.join(f)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("remove {f}")),
                _ => {}
            }
        }
        match std::fs::remove_file(root_path(dir)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).context("remove the local server root"),
            _ => {}
        }
        Ok(())
    }

    fn load_from_file(path: &Path) -> Result<Config> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
}

/// A computer signed out here, as its sign-in names it: none of it is secret. The token stays in
/// the kept sign-in, which only this computer reads.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Kept {
    pub server: String,
    pub id: String,
    pub user: Option<String>,
}

/// The sign-in of a computer signed out here, kept for the same account to take it back.
const KEPT: &str = "signed-out.json";

/// What ties a computer to its account: the sign-in (in use or kept), its host key, the devices
/// it trusts or blocks and whether it took the account's list on first use, a pairing under
/// way, the AI switch, and the record of what AI clients did there, which the next account must
/// not read.
const ACCOUNT: &[&str] = &[FILE, KEPT, "host-key", "controllers.json", "blocked.json", "adopted", "pairing.json", "ai.json", "audit.jsonl", "audit.old.jsonl"];

pub fn root_path(dir: &Path) -> PathBuf {
    #[cfg(windows)]
    if dir == crate::service::machine_dir() {
        return dir.join(crate::service::install::TRUST_DIR).join(ROOT_FILE);
    }
    dir.join(ROOT_FILE)
}

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
    fn a_pinned_sign_in_keeps_its_address_and_a_separate_pem() {
        use wire::crypto::{hex, sha256};

        let dir = temp("pinned");
        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let root = params.self_signed(&rcgen::KeyPair::generate().unwrap()).unwrap();
        let mut cfg = sign_in("H1", "t", "me");
        cfg.server = format!("https://hub.local:18599#ca={}", hex(&sha256(root.der())));
        let address = Address::parse(&cfg.server).unwrap();
        let pem = address.pin_root(root.der()).unwrap();
        cfg.server_root = Some(pem.clone());
        Config::settle_in(&dir, cfg).unwrap();
        let stored = Config::load_from(&dir).unwrap();
        assert_eq!(stored.server, address.saved());
        assert_eq!(stored.server_root.as_deref(), Some(pem.as_str()));
        assert_eq!(std::fs::read_to_string(root_path(&dir)).unwrap(), pem);
        assert!(!std::fs::read_to_string(dir.join(FILE)).unwrap().contains("CERTIFICATE"));
        stored.connection().unwrap();
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
    fn forgetting_removes_what_ties_the_computer_to_its_account_and_keeps_its_settings() {
        let d = temp("forget");
        for f in ACCOUNT.iter().chain(["settings.json", "ipc.key"].iter()) {
            std::fs::write(d.join(f), "x").unwrap();
        }
        Config::forget_in(&d).unwrap();
        assert!(ACCOUNT.iter().all(|f| !d.join(f).exists()), "every account file is gone");
        assert!(d.join("settings.json").exists() && d.join("ipc.key").exists(), "the computer's own files stay");
        Config::forget_in(&d).unwrap();
    }

    fn sign_in(id: &str, token: &str, user: &str) -> Config {
        serde_json::from_value(serde_json::json!({"server": "https://s.example", "id": id, "token": token, "name": "", "account": {"user": user}})).unwrap()
    }

    #[test]
    fn signing_out_keeps_the_computer_for_the_server_to_give_back_and_a_new_one_starts_clean() {
        let d = temp("keep");
        Config::settle_in(&d, sign_in("H1", "t1", "me")).unwrap();
        std::fs::write(d.join("controllers.json"), "trusted").unwrap();
        Config::update_in(&d, |c| c.auto_lock = true).unwrap();

        Config::sign_out_in(&d).unwrap();
        assert!(Config::load_from(&d).is_err(), "signed out");
        assert_eq!(Config::kept_in(&d), Some(Kept { server: "https://s.example".into(), id: "H1".into(), user: Some("me".into()) }));

        // The server gave this computer back: its token, settings and trust return.
        let back = Config::settle_in(&d, sign_in("H1", "", "me")).unwrap();
        assert_eq!((back.token.as_str(), back.auto_lock), ("t1", true));
        assert_eq!(std::fs::read_to_string(d.join("controllers.json")).unwrap(), "trusted");
        assert_eq!(Config::kept_in(&d), None);

        // Another computer (another account, or this one removed): nothing of the old one stays.
        Config::sign_out_in(&d).unwrap();
        let new = Config::settle_in(&d, sign_in("H2", "t2", "other")).unwrap();
        assert_eq!((new.id.as_str(), new.token.as_str(), new.auto_lock), ("H2", "t2", false));
        assert!(!d.join("controllers.json").exists() && Config::kept_in(&d).is_none());

        // Given back, but nothing is kept here any more: refused, nothing written.
        assert!(Config::settle_in(&temp("keep-none"), sign_in("H1", "", "me")).is_err());
    }
}
