use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context, Result};

fn security(command: &mut Command) -> Result<String> {
    let output = command.output().context("run macOS security")?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

pub fn update_server_root(path: &Path, fingerprint: &str, install: bool) -> Result<()> {
    let mut command = Command::new("/usr/bin/security");
    let keychain = security(command.arg("login-keychain"))?;
    let keychain = keychain.trim().trim_matches('"');
    ensure!(
        !keychain.is_empty(),
        "macOS did not return a login keychain"
    );
    let mut command = Command::new("/usr/bin/security");
    if install {
        command
            .args([
                "add-trusted-cert",
                "-r",
                "trustRoot",
                "-p",
                "ssl",
                "-k",
                keychain,
            ])
            .arg(path);
    } else {
        command.args(["delete-certificate", "-Z", fingerprint, "-t", keychain]);
    }
    security(&mut command)?;
    Ok(())
}
