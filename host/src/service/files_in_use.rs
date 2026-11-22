//! Program files in use while Deskway is installed again or removed, handled with the
//! system's own facilities: Restart Manager closes the programs still holding them, and what
//! cannot be deleted now (the uninstaller's own program) is deleted at the next restart
//! (`MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT};
use windows::Win32::System::RestartManager::*;

/// A Restart Manager session, ended when dropped.
struct Session(u32);

impl Session {
    fn start() -> Result<Session> {
        let mut handle = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        check(unsafe { RmStartSession(&mut handle, None, PWSTR(key.as_mut_ptr())) }, "RmStartSession")?;
        Ok(Session(handle))
    }

    fn files(&self, files: &[PathBuf]) -> Result<()> {
        let names: Vec<HSTRING> = files.iter().map(|f| HSTRING::from(f.as_os_str())).collect();
        let ptrs: Vec<PCWSTR> = names.iter().map(|n| PCWSTR(n.as_ptr())).collect();
        check(unsafe { RmRegisterResources(self.0, Some(&ptrs), None, None) }, "RmRegisterResources")
    }

    /// The processes (services included) holding the registered resources.
    fn holders(&self) -> Result<Vec<RM_PROCESS_INFO>> {
        let mut list = Vec::new();
        loop {
            let (mut needed, mut count, mut reasons) = (0u32, list.len() as u32, 0u32);
            let r = unsafe { RmGetList(self.0, &mut needed, &mut count, Some(list.as_mut_ptr()), &mut reasons) };
            if r == ERROR_MORE_DATA {
                list.resize(needed as usize, RM_PROCESS_INFO::default());
                continue;
            }
            check(r, "RmGetList")?;
            list.truncate(count as usize);
            return Ok(list);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = RmEndSession(self.0);
        }
    }
}

fn check(r: WIN32_ERROR, what: &str) -> Result<()> {
    if r != ERROR_SUCCESS {
        bail!("{what}: {}", r.0);
    }
    Ok(())
}

/// Process ids holding any of `files`.
#[cfg(test)]
pub fn holders(files: &[PathBuf]) -> Result<Vec<u32>> {
    let s = Session::start()?;
    s.files(files)?;
    Ok(s.holders()?.iter().map(|p| p.Process.dwProcessId).collect())
}

/// Closes every program holding `files`, except the ones in `spare` (this process, and the
/// installer window that started it). Restart Manager asks each to close and ends what does
/// not; it returns once they are gone. Returns how many were closed.
pub fn close_holders(files: &[PathBuf], spare: &[u32]) -> Result<usize> {
    let found = Session::start()?;
    found.files(files)?;
    let others: Vec<RM_UNIQUE_PROCESS> = found.holders()?.into_iter().map(|p| p.Process).filter(|p| !spare.contains(&p.dwProcessId)).collect();
    if others.is_empty() {
        return Ok(0);
    }
    // A session of their own: shutting down the files' session would close the spared ones too.
    let close = Session::start()?;
    check(unsafe { RmRegisterResources(close.0, None, Some(&others), None) }, "RmRegisterResources")?;
    check(unsafe { RmShutdown(close.0, RmForceShutdown.0 as u32, None) }, "RmShutdown")?;
    Ok(others.len())
}

/// Every file under `dir`, for Restart Manager.
pub fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Deletes `dir` and everything in it. What is still in use is handed to `later`, files
/// before the folders that hold them, so a delete-at-restart list runs in a working order.
pub fn remove_tree(dir: &Path, later: &mut dyn FnMut(&Path)) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() && !p.is_symlink() {
                remove_tree(&p, later);
            } else if std::fs::remove_file(&p).is_err() {
                later(&p);
            }
        }
    }
    if std::fs::remove_dir(dir).is_err() && dir.exists() {
        later(dir);
    }
}

/// Deletes `path` when the computer next restarts. Administrators only.
pub fn delete_at_restart(path: &Path) {
    if let Err(e) = unsafe { MoveFileExW(&HSTRING::from(path.as_os_str()), PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) } {
        eprintln!("cannot delete {} at restart: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("deskway-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        d
    }

    #[test]
    fn what_is_in_use_is_left_for_the_restart_files_first() {
        let d = temp("remove");
        std::fs::write(d.join("a.txt"), b"a").unwrap();
        std::fs::write(d.join("sub").join("busy.exe"), b"b").unwrap();
        std::fs::write(d.join("sub").join("c.txt"), b"c").unwrap();
        // Open without FILE_SHARE_DELETE, as a running program holds its own file.
        let busy = std::fs::OpenOptions::new().read(true).share_mode(1).open(d.join("sub").join("busy.exe")).unwrap();
        let mut later = Vec::new();
        remove_tree(&d, &mut |p| later.push(p.to_path_buf()));
        assert_eq!(later, [d.join("sub").join("busy.exe"), d.join("sub"), d.clone()]);
        assert!(!d.join("a.txt").exists() && !d.join("sub").join("c.txt").exists(), "the rest is gone now");
        drop(busy);
        let mut again = Vec::new();
        remove_tree(&d, &mut |p| again.push(p.to_path_buf()));
        assert!(again.is_empty() && !d.exists());
    }

    #[test]
    fn restart_manager_names_the_holder_and_spares_it() {
        let d = temp("rm");
        let f = d.join("held.bin");
        std::fs::write(&f, b"x").unwrap();
        let held = std::fs::File::open(&f).unwrap();
        let me = std::process::id();
        assert!(holders(&[f.clone()]).unwrap().contains(&me), "this test holds the file");
        // Spared: nothing is closed (and this test keeps running).
        assert_eq!(close_holders(&[f.clone()], &[me]).unwrap(), 0);
        drop(held);
        assert!(holders(&[f.clone()]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lists_every_file() {
        let d = temp("list");
        std::fs::write(d.join("sub").join("x"), b"").unwrap();
        std::fs::write(d.join("y"), b"").unwrap();
        let mut files = files_under(&d);
        files.sort();
        assert_eq!(files, [d.join("sub").join("x"), d.join("y")]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
