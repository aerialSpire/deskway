//! Keeping `deskway` itself up to date, unasked (docs/architecture.md, "Distribution and updates"). The server hands
//! out the program for each system with its SHA-256. `deskway mcp` compares the file it was
//! started from with it as it starts and every hour after; when they differ, it downloads the
//! server's, checks it and puts it in that file's place. Nobody is asked or told: what runs goes
//! on running, the AI client's next start of `deskway mcp` is the new one, and that replaces a
//! proxy of another version (`mcp`). `deskway update` does the same at once.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::store::Account;

/// How often a running `deskway mcp` looks again.
const EVERY: Duration = Duration::from_secs(3600);

/// The name the server gives this system's program (internal/server/aiclients.go).
fn file_name() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "deskway-windows-x64.exe",
        ("windows", "aarch64") => "deskway-windows-arm64.exe",
        ("macos", "aarch64") => "deskway-macos-arm64",
        ("macos", "x86_64") => "deskway-macos-x64",
        _ => return None,
    })
}

fn sha256_of(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path).with_context(|| format!("read {}", path.display()))?;
    Ok(wire::crypto::hex(&wire::crypto::sha256_reader(f)?))
}

/// The program file this process started from, and its SHA-256 then: taken before anything
/// replaces it, since the file is where later starts come from, whatever runs now.
fn started() -> &'static (PathBuf, String) {
    static STARTED: OnceLock<(PathBuf, String)> = OnceLock::new();
    STARTED.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        let sha = sha256_of(&exe).unwrap_or_default();
        (exe, sha)
    })
}

/// The program file that later starts run (the proxy's, too).
pub fn program() -> &'static Path {
    &started().0
}

/// This process's version: its program's SHA-256.
pub fn version() -> &'static str {
    &started().1
}

/// Puts the server's program for this system in the program file's place when they differ:
/// true when it did.
fn bring_up_to_date() -> Result<bool> {
    let Some(name) = file_name() else { return Ok(false) };
    let mut a = Account::load()?;
    if !a.signed_in() {
        return Ok(false);
    }
    let list = crate::account::get_json(&mut a, "/api/ai/clients")?;
    let Some(sha) = list.as_array().into_iter().flatten().find(|f| f["file"] == name).and_then(|f| f["sha256"].as_str()) else { return Ok(false) };
    if sha == sha256_of(program())? {
        return Ok(false);
    }
    let bytes = crate::account::download(&mut a, &format!("/download/{name}"))?;
    if wire::crypto::hex(&wire::crypto::sha256(&bytes)) != sha {
        bail!("the downloaded program does not match its SHA-256: nothing was changed");
    }
    replace(program(), &bytes)?;
    Ok(true)
}

/// For `deskway mcp`: brought up to date as it starts and every hour after, in the background.
pub fn in_background() {
    started();
    std::thread::spawn(|| loop {
        match bring_up_to_date() {
            Ok(true) => eprintln!("deskway: updated; the next start runs the new one"),
            Ok(false) => {}
            Err(e) => eprintln!("deskway: cannot update now: {e:#}"),
        }
        std::thread::sleep(EVERY);
    });
}

/// `deskway update`: the same, at once.
pub fn run() -> Result<()> {
    if bring_up_to_date()? {
        println!("deskway is updated: the AI client's next start of it runs the new one.");
    } else {
        println!("deskway is up to date.");
    }
    Ok(())
}

/// Puts `bytes` in place of `exe`: written beside it, `exe` moved aside under a name of its own
/// (Windows cannot overwrite a running program, but can rename it; earlier ones may still run),
/// the new one moved in. Programs moved aside before, and no longer running, are removed.
fn replace(exe: &Path, bytes: &[u8]) -> Result<()> {
    let name = exe.file_name().context("the program's name")?.to_string_lossy().into_owned();
    let stem = exe.file_stem().context("the program's name")?.to_string_lossy().into_owned();
    let dir = exe.parent().context("the program's folder")?;
    for e in std::fs::read_dir(dir)?.flatten() {
        if e.file_name().to_string_lossy().starts_with(&format!("{stem}.old")) {
            // One still running stays until the next time.
            let _ = std::fs::remove_file(e.path());
        }
    }
    let new = dir.join(format!("{name}.new"));
    let old = dir.join(format!("{stem}.old-{}", wire::crypto::hex(&wire::crypto::random::<4>())));
    std::fs::write(&new, bytes).with_context(|| format!("write {}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(exe, &old).context("move the running program aside")?;
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e).context("put the new program in place");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The new program takes the file's place; the one there is kept aside; ones kept aside
    /// before are removed.
    #[test]
    fn a_new_program_takes_the_files_place() {
        let dir = std::env::temp_dir().join(format!("deskway-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("deskway.exe");
        std::fs::write(&exe, b"one").unwrap();
        std::fs::write(dir.join("deskway.old-earlier"), b"zero").unwrap();
        replace(&exe, b"two").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"two");
        let mut aside: Vec<Vec<u8>> = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.path() != exe).map(|e| std::fs::read(e.path()).unwrap()).collect();
        aside.sort();
        assert_eq!(aside, [b"one".to_vec()], "the one that was there, and only it");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
