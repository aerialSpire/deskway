use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context, Result};

pub const INSTALLED_ROOT: &str = "installed-ca";

fn security(command: &mut Command) -> Result<String> {
    let output = command.output().context("run macOS security")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn login_keychain() -> Result<String> {
    let mut command = Command::new("/usr/bin/security");
    let keychain = security(command.arg("login-keychain"))?;
    let keychain = keychain.trim().trim_matches('"');
    ensure!(
        !keychain.is_empty(),
        "macOS did not return a login keychain"
    );
    Ok(keychain.to_string())
}

fn contains_root(fingerprint: &str, keychain: Option<&str>) -> Result<bool> {
    let certs = security(
        Command::new("/usr/bin/security")
            .args(["find-certificate", "-a", "-Z", "-p"])
            .args(keychain),
    )?;
    Ok(certs.lines().any(|line| {
        line.strip_prefix("SHA-256 hash: ")
            .is_some_and(|hash| hash.eq_ignore_ascii_case(fingerprint))
    }))
}

fn installed_root(dir: &Path) -> Result<Option<String>> {
    match fs::read_to_string(dir.join(INSTALLED_ROOT)) {
        Ok(pin) => Ok(Some(pin)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn install_server_root(dir: &Path, fingerprint: &str) -> Result<()> {
    let owned = installed_root(dir)?;
    if owned.as_deref().is_some_and(|pin| pin != fingerprint) {
        remove_installed_server_root(dir)?;
    }
    let keychain = login_keychain()?;
    let root = crate::config::root_path(dir);
    if !contains_root(fingerprint, Some(&keychain))? {
        if contains_root(fingerprint, None)? {
            return Ok(());
        }
        security(
            Command::new("/usr/bin/security")
                .args(["add-certificates", "-k", &keychain])
                .arg(&root),
        )?;
        // Record the import before asking for permission to trust the certificate.
        if let Err(e) =
            crate::config::write_private(&dir.join(INSTALLED_ROOT), fingerprint.as_bytes())
        {
            delete_root(fingerprint, &keychain)
                .with_context(|| format!("record the installed root: {e:#}"))?;
            return Err(e).context("record the installed root");
        }
    } else if owned.as_deref() != Some(fingerprint) {
        return Ok(());
    }
    if let Err(error) = security(
        Command::new("/usr/bin/security")
            .args(["add-trusted-cert", "-r", "trustRoot", "-p", "ssl"])
            .arg(root),
    ) {
        remove_installed_server_root(dir)
            .with_context(|| format!("remove the untrusted local root: {error:#}"))?;
        return Err(error);
    }
    Ok(())
}

/// Successful browser trust for the hub persists in the login keychain across app launches.
pub fn install_hub_root(dir: &Path, fingerprint: &str) -> Result<()> {
    if installed_root(dir)?.as_deref() == Some(fingerprint)
        && contains_root(fingerprint, Some(&login_keychain()?))?
    {
        return Ok(());
    }
    install_server_root(dir, fingerprint)
}

fn delete_root(fingerprint: &str, keychain: &str) -> Result<()> {
    security(Command::new("/usr/bin/security").args([
        "delete-certificate",
        "-Z",
        fingerprint,
        "-t",
        keychain,
    ]))?;
    Ok(())
}

pub fn remove_installed_server_root(dir: &Path) -> Result<()> {
    let Some(fingerprint) = installed_root(dir)? else {
        return Ok(());
    };
    let keychain = login_keychain()?;
    if contains_root(&fingerprint, Some(&keychain))? {
        delete_root(&fingerprint, &keychain)?;
    }
    fs::remove_file(dir.join(INSTALLED_ROOT))?;
    Ok(())
}
