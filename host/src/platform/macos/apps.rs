//! App and window operations for the logged-in Mac user.

use std::path::Path;
use std::process::Command;

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSRunningApplication;

use crate::workspace::apps::{App, OpenError};

pub use super::windows::{act, windows};

/// The apps in the Applications folders, the person's own too.
pub fn apps() -> Vec<App> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out = Vec::new();
    for dir in ["/Applications".to_string(), "/Applications/Utilities".into(), "/System/Applications".into(), "/System/Applications/Utilities".into(), format!("{home}/Applications")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "app") {
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(App { name, path: path.to_string_lossy().into_owned() });
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    out
}

/// Opens an app (by path or name), a file or folder, or an address, as the Finder would.
pub fn open(target: &str, args: &[String]) -> Result<Option<u32>, OpenError> {
    let mut c = Command::new("/usr/bin/open");
    if Path::new(target).exists() || target.contains("://") {
        c.arg(target);
    } else {
        c.args(["-a", target]);
    }
    if !args.is_empty() {
        c.arg("--args").args(args);
    }
    let out = c.output().map_err(|e| OpenError::Failed(e.to_string()))?;
    if out.status.success() {
        return Ok(None);
    }
    let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if why.contains("Unable to find") || why.contains("does not exist") { OpenError::NotFound } else { OpenError::Failed(why) })
}

/// The programs called `name`, any case.
pub fn pids_named(name: &str) -> Vec<u32> {
    let out = Command::new("/usr/bin/pgrep").args(["-ix", name]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).collect()).unwrap_or_default()
}

/// Asks the programs `pids` to end: an app quits as with ⌘Q (it may ask about unsaved work), or
/// they are ended outright. How many were asked.
pub fn quit(pids: &[u32], force: bool) -> usize {
    pids.iter()
        .filter(|pid| {
            if force {
                return unsafe { libc::kill(**pid as i32, libc::SIGKILL) } == 0;
            }
            autoreleasepool(|_| {
                NSRunningApplication::runningApplicationWithProcessIdentifier(**pid as i32)
                    .is_some_and(|app| app.terminate())
            })
        })
        .count()
}

pub fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
