//! Files, as the workspace user: list, look at, read and write in parts, move, delete.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::UNIX_EPOCH;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

use crate::i18n::t;
use crate::workspace::{Answer, Code, WorkError, READ_MOST};

impl WorkError {
    fn io(path: &str, e: std::io::Error) -> WorkError {
        let (code, next) = match e.kind() {
            std::io::ErrorKind::NotFound => (Code::NotFound, "Check the path (fs_list its folder)."),
            std::io::ErrorKind::PermissionDenied => return WorkError::new(Code::Denied, format!("{path}: {e}"), t("mcp.file_permission")),
            std::io::ErrorKind::AlreadyExists => (Code::Exists, "Choose another name, or delete it first."),
            std::io::ErrorKind::DirectoryNotEmpty => (Code::NotEmpty, "Pass recursive to delete it with what is in it."),
            _ => (Code::Io, "Try again, or another path."),
        };
        WorkError::new(code, format!("{path}: {e}"), next)
    }
}
/// Entries one listing returns at most.
const LIST_MOST: usize = 2000;

fn about(path: &Path, m: &std::fs::Metadata) -> Value {
    let modified = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
    let kind = if m.is_dir() { "folder" } else if m.file_type().is_symlink() { "link" } else { "file" };
    json!({ "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string()), "kind": kind, "size": m.len(), "modified": modified })
}

#[cfg(target_os = "linux")]
pub(super) mod browse;

fn list_entries<T>(dir: impl Iterator<Item = io::Result<T>>, mut describe: impl FnMut(T) -> Option<Value>) -> (Vec<Value>, bool) {
    let mut entries = Vec::new();
    let mut more = false;
    for entry in dir.flatten() {
        if entries.len() == LIST_MOST {
            more = true;
            break;
        }
        if let Some(entry) = describe(entry) {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    (entries, more)
}

pub fn list(path: &str) -> Answer {
    let dir = fs::read_dir(path).map_err(|e| WorkError::io(path, e))?;
    let (entries, more) = list_entries(dir, |e| e.metadata().ok().map(|m| about(&e.path(), &m)));
    Ok(json!({ "path": path, "entries": entries, "more": more }))
}

pub fn stat(path: &str) -> Answer {
    let m = std::fs::metadata(path).map_err(|e| WorkError::io(path, e))?;
    let mut v = about(Path::new(path), &m);
    v["path"] = json!(path);
    Ok(v)
}

pub fn read(path: &str, offset: u64, length: u64) -> Answer {
    let mut f = std::fs::File::open(path).map_err(|e| WorkError::io(path, e))?;
    let size = f.metadata().map_err(|e| WorkError::io(path, e))?.len();
    f.seek(SeekFrom::Start(offset)).map_err(|e| WorkError::io(path, e))?;
    let mut buf = Vec::new();
    f.take(length.min(READ_MOST)).read_to_end(&mut buf).map_err(|e| WorkError::io(path, e))?;
    let next = offset + buf.len() as u64;
    let (key, data) = match String::from_utf8(buf) {
        Ok(text) => ("text", json!(text)),
        Err(e) => ("base64", json!(STANDARD.encode(e.into_bytes()))),
    };
    Ok(json!({ "path": path, "size": size, "offset": offset, key: data, "next": next, "more": next < size }))
}

pub fn write(path: &str, text: Option<String>, base64: Option<String>, append: bool) -> Answer {
    let data = match (text, base64) {
        (Some(t), None) => t.into_bytes(),
        (None, Some(b)) => STANDARD.decode(b).map_err(|_| WorkError::new(Code::InvalidArgs, "base64 is not base64", "Send the bytes base64-encoded, or text."))?,
        _ => return Err(WorkError::new(Code::InvalidArgs, "text or base64, one of them", "Pass the content once.")),
    };
    if let Some(dir) = std::path::Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| WorkError::io(path, e))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(path).map_err(|e| WorkError::io(path, e))?;
    f.write_all(&data).map_err(|e| WorkError::io(path, e))?;
    let size = f.metadata().map(|m| m.len()).unwrap_or_default();
    Ok(json!({ "path": path, "written": data.len(), "size": size }))
}

/// The file's SHA-256 and size: a copy is checked against it (`copy_to`, `copy_from`).
pub fn hash(path: &str) -> Answer {
    let f = std::fs::File::open(path).map_err(|e| WorkError::io(path, e))?;
    let size = f.metadata().map_err(|e| WorkError::io(path, e))?.len();
    let sum = wire::crypto::sha256_reader(f).map_err(|e| WorkError::io(path, e))?;
    Ok(json!({ "path": path, "size": size, "sha256": wire::crypto::hex(&sum) }))
}

pub fn rename(from: &str, to: &str) -> Answer {
    std::fs::rename(from, to).map_err(|e| WorkError::io(from, e))?;
    Ok(json!({ "moved": from, "to": to }))
}

pub fn delete(path: &str, recursive: bool) -> Answer {
    remove(Path::new(path), recursive).map_err(|e| WorkError::io(path, e))?;
    Ok(json!({ "deleted": path }))
}

fn remove(path: &Path, recursive: bool) -> io::Result<()> {
    match (fs::symlink_metadata(path)?.is_dir(), recursive) {
        (true, true) => fs::remove_dir_all(path),
        (true, false) => fs::remove_dir(path),
        (false, _) => fs::remove_file(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_listing_keeps_its_scan_limit_and_skips_unreadable_entries() {
        for (label, before, after, more, scanned, described) in [
            ("exact limit", LIST_MOST, &[][..], false, LIST_MOST, LIST_MOST),
            ("extra entry", LIST_MOST, &[Ok(true), Ok(true)], true, LIST_MOST + 1, LIST_MOST),
            ("iterator errors", LIST_MOST - 1, &[Err(io::ErrorKind::PermissionDenied), Ok(true), Err(io::ErrorKind::Other)], false, LIST_MOST + 2, LIST_MOST),
            ("entry disappears", LIST_MOST - 1, &[Ok(false), Ok(true), Ok(false)], true, LIST_MOST + 2, LIST_MOST + 1),
        ] {
            let mut read = 0;
            let mut metadata_reads = 0;
            let dir = std::iter::repeat_n(Ok(true), before).chain(after.iter().copied())
                .map(|entry| entry.map_err(io::Error::from)).inspect(|_| read += 1);
            let (entries, has_more) = list_entries(dir, |exists| {
                metadata_reads += 1;
                exists.then(|| json!({"name": "file"}))
            });
            assert_eq!((entries.len(), has_more, read, metadata_reads), (LIST_MOST, more, scanned, described), "{label}");
        }
    }

    #[test]
    fn files_are_written_read_in_parts_moved_listed_and_deleted() {
        let dir = std::env::temp_dir().join(format!("deskway-files-{}", wire::crypto::hex(&wire::crypto::random::<8>())));
        std::fs::create_dir(&dir).unwrap();
        let p = |n: &str| dir.join(n).display().to_string();
        write(&p("a.txt"), Some("héllo".into()), None, false).unwrap();
        write(&p("a.txt"), Some(" world".into()), None, true).unwrap();
        for (offset, length, text, next, more) in [(0, 3, "hé", 3, true), (3, 100, "llo world", 12, false)] {
            let r = read(&p("a.txt"), offset, length).unwrap();
            assert_eq!((r["text"].as_str(), r["next"].as_u64(), r["more"].as_bool()), (Some(text), Some(next), Some(more)));
        }
        assert_eq!(hash(&p("a.txt")).unwrap()["sha256"], wire::crypto::hex(&wire::crypto::sha256("héllo world".as_bytes())));
        write(&p("b.bin"), None, Some(STANDARD.encode([0u8, 159, 146, 150])), false).unwrap();
        assert_eq!(read(&p("b.bin"), 0, 10).unwrap()["base64"], STANDARD.encode([0u8, 159, 146, 150]), "bytes that are not UTF-8 come back as base64");
        rename(&p("a.txt"), &p("c.txt")).unwrap();
        let names: Vec<String> = list(&dir.display().to_string()).unwrap()["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["b.bin", "c.txt"]);
        assert_eq!(stat(&p("c.txt")).unwrap()["size"], 12);
        assert_eq!(read(&p("a.txt"), 0, 1).unwrap_err().code, Code::NotFound);
        // The folders a new file is in are made with it.
        write(&p("sub/deep/x"), Some("x".into()), None, false).unwrap();
        assert_eq!(delete(&p("sub"), false).unwrap_err().code, Code::NotEmpty);
        delete(&p("sub"), true).unwrap();
        delete(&dir.display().to_string(), true).unwrap();
    }
}
