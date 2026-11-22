//! `copy_to` and `copy_from`: a file between this machine and a computer. This end does the
//! copying (docs/architecture.md, "AI control": the computer's side is its own tools): pieces through
//! the computer's `fs_write` and `fs_read`, then both ends' SHA-256 compared (`fs_hash`), so a
//! copy that says it is done is the same bytes.

use std::fs::File;
use std::io::{Read, Write};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};

/// A piece: what one `fs_read` gives at most, and what fits one message as base64 (an answer
/// carries its data twice: as text, and structured).
const PIECE: usize = 256 * 1024;

/// Why a copy stopped: a code, what happened, what to do next.
#[derive(Debug, PartialEq)]
pub struct Fail {
    pub code: String,
    pub message: String,
    pub next: String,
}

impl Fail {
    pub fn new(code: &str, message: impl Into<String>, next: &str) -> Fail {
        Fail { code: code.into(), message: message.into(), next: next.into() }
    }
}

/// One of the computer's tools, called with its arguments: its answer's data, or its error.
pub type Call<'a> = &'a dyn Fn(&str, Value) -> Result<Value, Fail>;

fn here(path: &str, e: std::io::Error) -> Fail {
    Fail::new("local", format!("{path} on this machine: {e}"), "Check the path here.")
}

/// This machine's file `from` to the computer's `to`, which it replaces.
pub fn to(call: Call, from: &str, to: &str) -> Result<Value, Fail> {
    let mut f = File::open(from).map_err(|e| here(from, e))?;
    let mut piece = vec![0u8; PIECE];
    let mut sent = 0u64;
    loop {
        let n = fill(&mut f, &mut piece).map_err(|e| here(from, e))?;
        call("fs_write", json!({ "path": to, "base64": STANDARD.encode(&piece[..n]), "append": sent > 0 }))?;
        sent += n as u64;
        if n < PIECE {
            break;
        }
    }
    let sum = sha256_of(from)?;
    same(call, to, &sum)?;
    Ok(json!({ "from": from, "to": to, "bytes": sent, "sha256": sum }))
}

/// The computer's file `from` to this machine's `to`, which it replaces.
pub fn from(call: Call, from: &str, to: &str) -> Result<Value, Fail> {
    let mut out = File::create(to).map_err(|e| here(to, e))?;
    let mut offset = 0u64;
    loop {
        let r = call("fs_read", json!({ "path": from, "offset": offset, "length": PIECE }))?;
        let bytes = match (r["text"].as_str(), r["base64"].as_str()) {
            (Some(t), _) => t.as_bytes().to_vec(),
            (None, Some(b)) => STANDARD.decode(b).map_err(|_| Fail::new("failed", "the computer sent bytes that are not base64", "Copy it again."))?,
            _ => return Err(Fail::new("failed", "the computer's answer has no content", "Copy it again.")),
        };
        out.write_all(&bytes).map_err(|e| here(to, e))?;
        offset = r["next"].as_u64().unwrap_or(offset + bytes.len() as u64);
        if r["more"] != true {
            break;
        }
    }
    drop(out);
    let sum = sha256_of(to)?;
    same(call, from, &sum)?;
    Ok(json!({ "from": from, "to": to, "bytes": offset, "sha256": sum }))
}

/// Reads until `buf` is full or the file ends.
fn fill(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn sha256_of(path: &str) -> Result<String, Fail> {
    let f = File::open(path).map_err(|e| here(path, e))?;
    Ok(wire::crypto::hex(&wire::crypto::sha256_reader(f).map_err(|e| here(path, e))?))
}

/// The computer's file has `sum`.
fn same(call: Call, remote: &str, sum: &str) -> Result<(), Fail> {
    let theirs = call("fs_hash", json!({ "path": remote }))?;
    if theirs["sha256"].as_str() != Some(sum) {
        return Err(Fail::new("failed", format!("{remote} on the computer is not the same bytes (SHA-256 {} here, {} there)", sum, theirs["sha256"]), "Copy it again; the file may have changed meanwhile."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A computer's files in memory, answering fs_write, fs_read and fs_hash as a computer does;
    /// `bend` changes what it says a file's hash is.
    struct Computer {
        files: RefCell<HashMap<String, Vec<u8>>>,
        bend: bool,
    }

    impl Computer {
        fn call(&self, tool: &str, a: Value) -> Result<Value, Fail> {
            let path = a["path"].as_str().unwrap().to_string();
            let mut files = self.files.borrow_mut();
            match tool {
                "fs_write" => {
                    let data = STANDARD.decode(a["base64"].as_str().unwrap()).unwrap();
                    let f = files.entry(path).or_default();
                    if a["append"] != true {
                        f.clear();
                    }
                    f.extend(data);
                    Ok(json!({}))
                }
                "fs_read" => {
                    let f = files.get(&path).ok_or_else(|| Fail::new("not_found", path.clone(), ""))?;
                    let at = a["offset"].as_u64().unwrap() as usize;
                    let end = (at + a["length"].as_u64().unwrap() as usize).min(f.len());
                    let piece = &f[at..end];
                    let content = match std::str::from_utf8(piece) {
                        Ok(t) => json!({ "text": t }),
                        Err(_) => json!({ "base64": STANDARD.encode(piece) }),
                    };
                    Ok(json!({ "next": end, "more": end < f.len() }).as_object().unwrap().clone().into_iter().chain(content.as_object().unwrap().clone()).collect())
                }
                "fs_hash" => {
                    let f = files.get(&path).ok_or_else(|| Fail::new("not_found", path.clone(), ""))?;
                    let mut sum = wire::crypto::hex(&wire::crypto::sha256(f));
                    if self.bend {
                        sum.replace_range(0..1, if sum.starts_with('0') { "1" } else { "0" });
                    }
                    Ok(json!({ "sha256": sum, "size": f.len() }))
                }
                _ => unreachable!(),
            }
        }
    }

    /// A file of `n` bytes, text and not, there and back: the same bytes, in as many pieces as
    /// it takes; an empty one too; a computer whose copy differs is said to.
    #[test]
    fn a_file_goes_there_and_back_in_pieces_and_a_different_copy_is_caught() {
        let dir = std::env::temp_dir().join(format!("deskway-copy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, n) in [0usize, 10, PIECE, 2 * PIECE + 7].into_iter().enumerate() {
            let data: Vec<u8> = (0..n).map(|k| if i == 1 { b'a' + (k % 26) as u8 } else { (k * 31 % 251) as u8 }).collect();
            let (mine, back) = (dir.join(format!("a{i}")), dir.join(format!("b{i}")));
            std::fs::write(&mine, &data).unwrap();
            let c = Computer { files: RefCell::new(HashMap::new()), bend: false };
            let call = |t: &str, a: Value| c.call(t, a);
            let sent = to(&call, mine.to_str().unwrap(), "/there").unwrap();
            assert_eq!((sent["bytes"].as_u64(), c.files.borrow()["/there"].len()), (Some(n as u64), n), "{n} bytes there");
            let got = from(&call, "/there", back.to_str().unwrap()).unwrap();
            assert_eq!((got["bytes"].as_u64(), std::fs::read(&back).unwrap()), (Some(n as u64), data), "{n} bytes back");
        }
        let c = Computer { files: RefCell::new(HashMap::new()), bend: true };
        let call = |t: &str, a: Value| c.call(t, a);
        assert_eq!(to(&call, dir.join("a1").to_str().unwrap(), "/there").unwrap_err().code, "failed");
        assert_eq!(to(&call, dir.join("none").to_str().unwrap(), "/there").unwrap_err().code, "local");
        assert_eq!(from(&call, "/none", dir.join("c").to_str().unwrap()).unwrap_err().code, "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
