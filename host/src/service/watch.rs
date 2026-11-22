//! File-change notification (`ReadDirectoryChangesW`) for the settings an administrator changes
//! from the command line while the network service runs. Nothing is polled: the thread sleeps in
//! the kernel until the folder changes.

use std::path::Path;

use anyhow::{Context, Result};
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::*;

/// Calls `changed` on its own thread each time `name` in `dir` is written, created or replaced
/// (settings are saved through a rename), until `changed` returns false.
pub fn watch_file(dir: &Path, name: &str, mut changed: impl FnMut() -> bool + Send + 'static) -> Result<()> {
    let handle = unsafe {
        CreateFileW(
            &HSTRING::from(dir.as_os_str()),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .with_context(|| format!("watch {}", dir.display()))?;
    let path = dir.to_path_buf();
    let dir = Dir(handle);
    let name = name.to_string();
    std::thread::Builder::new().name("settings-watch".into()).spawn(move || {
        // The whole handle moves here (and closes when the thread ends), not just its field.
        let dir = dir;
        // DWORD-aligned, as the call requires.
        let mut buf = vec![0u32; 16 * 1024];
        loop {
            let mut got = 0u32;
            let r = unsafe {
                ReadDirectoryChangesW(
                    dir.0,
                    buf.as_mut_ptr().cast(),
                    (buf.len() * 4) as u32,
                    false,
                    FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE,
                    Some(&mut got),
                    None,
                    None,
                )
            };
            if let Err(e) = r {
                eprintln!("settings watch on {} stopped: {e}", path.display());
                return;
            }
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), got as usize) };
            // Nothing returned means the list overflowed: anything may have changed.
            if (got == 0 || names(bytes).iter().any(|n| n.eq_ignore_ascii_case(&name))) && !changed() {
                return;
            }
        }
    })?;
    Ok(())
}

struct Dir(HANDLE);
// A kernel handle, used by the watch thread only.
unsafe impl Send for Dir {}
impl Drop for Dir {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The file names in a buffer of `FILE_NOTIFY_INFORMATION` records.
fn names(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let u32_at = |i: usize| buf.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    while let (Some(next), Some(len)) = (u32_at(at), u32_at(at + 8)) {
        let start = at + 12;
        let Some(raw) = buf.get(start..start + len as usize) else { break };
        let wide: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push(String::from_utf16_lossy(&wide));
        if next == 0 {
            break;
        }
        at += next as usize;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    fn record(name: &str, last: bool) -> Vec<u8> {
        let wide: Vec<u8> = name.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        let size = (12 + wide.len()).next_multiple_of(4);
        let mut r = Vec::new();
        r.extend_from_slice(&(if last { 0 } else { size as u32 }).to_le_bytes());
        r.extend_from_slice(&3u32.to_le_bytes()); // FILE_ACTION_MODIFIED
        r.extend_from_slice(&(wide.len() as u32).to_le_bytes());
        r.extend_from_slice(&wide);
        r.resize(size, 0);
        r
    }

    #[test]
    fn reads_every_record() {
        let buf = [record("host.tmp", false), record("host.json", true)].concat();
        assert_eq!(names(&buf), ["host.tmp", "host.json"]);
        assert!(names(&[]).is_empty());
        assert_eq!(names(&record("长名字.json", true)), ["长名字.json"]);
    }

    #[test]
    fn hears_a_settings_file_replaced_but_not_other_files() {
        let dir = std::env::temp_dir().join(format!("deskway-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = channel();
        watch_file(&dir, "host.json", move || tx.send(()).is_ok()).unwrap();
        std::fs::write(dir.join("other.json"), b"{}").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "another file is not the settings");
        // Saved the way `config::write_private` saves: a new file renamed over the old one.
        crate::config::write_private(&dir.join("host.json"), b"{}").unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("the change is reported");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
