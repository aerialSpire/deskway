//! The shared clipboard (RDP CLIPRDR model): format lists are announced, data moves only when
//! something pastes, and every announcement gets a generation from the host.
//!
//! - [`Sync`]: the host's clipboard state `{gen, owner, formats}` and every rule about it. Pure
//!   logic; the session feeds it browser messages and platform events and carries out what it
//!   returns.
//! - [`RenderWait`]: how a platform's delayed rendering waits for the browser's answer.
//! - [`Native`]: what each platform's clipboard provides (`platform/windows/clipboard.rs`,
//!   `platform/macos/clipboard.rs`).
//! - The format model and the conversions that are the same on every platform.

use std::time::Duration;

use wire::types::{self as types, ClipboardError, FormatKind};

pub mod image;
#[cfg(any(windows, test))]
mod render;
mod sync;

#[cfg(windows)]
pub use render::{Answer, RenderWait};
pub use sync::{Act, Out, Sync};

/// Text and HTML up to this size travel inline in control messages; larger data goes on the
/// file channel. Well under the 256 KB SCTP message size every browser accepts.
pub const INLINE_MAX: usize = 48 * 1024;

/// How long a paste on this computer waits for the browser's data. Long enough for a
/// permission prompt in the browser and a 20 MB picture on a slow link; a paste that waits
/// longer is given up, as a stalled file transfer is (`transfer::STALL`).
pub const RENDER_DEADLINE: Duration = Duration::from_secs(30);

pub use crate::platform::ClipFormat as Format;

impl From<types::Format> for Format {
    fn from(f: types::Format) -> Format {
        Format { kind: f.kind, size: f.size, w: f.w, h: f.h, items: f.items }
    }
}

impl From<Format> for types::Format {
    fn from(f: Format) -> types::Format {
        types::Format { kind: f.kind, size: f.size, w: f.w, h: f.h, items: f.items }
    }
}

/// Clipboard data handed to a waiting paste.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    Text(String),
    Html(String),
    /// PNG.
    Image(Vec<u8>),
}

impl Data {
    pub fn kind(&self) -> FormatKind {
        match self {
            Data::Text(_) => FormatKind::Text,
            Data::Html(_) => FormatKind::Html,
            Data::Image(_) => FormatKind::Image,
        }
    }

    /// Data that arrived on the file channel for a paste: text and HTML are UTF-8.
    pub fn from_bytes(kind: FormatKind, bytes: Vec<u8>) -> Option<Data> {
        match kind {
            FormatKind::Text => String::from_utf8(bytes).ok().map(Data::Text),
            FormatKind::Html => String::from_utf8(bytes).ok().map(Data::Html),
            FormatKind::Image => Some(Data::Image(bytes)),
            FormatKind::Files => None,
        }
    }
}

/// What a read of this computer's clipboard gives the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum Read {
    /// Small text or HTML, sent inline.
    Text(String),
    Html(String),
    /// Bytes for the file channel: a picture (PNG), a file, or large text.
    Bytes { name: String, mime: String, data: Vec<u8> },
}

impl Read {
    /// Text or HTML, inline when small enough.
    pub fn text(kind: FormatKind, s: String) -> Read {
        match kind {
            _ if s.len() > INLINE_MAX => {
                let (name, mime) = if kind == FormatKind::Html { ("clipboard.html", "text/html") } else { ("clipboard.txt", "text/plain") };
                Read::Bytes { name: name.into(), mime: mime.into(), data: s.into_bytes() }
            }
            FormatKind::Html => Read::Html(s),
            _ => Read::Text(s),
        }
    }
}

impl Read {
    /// A copied file, for the browser: refused when larger than the browser may send here
    /// (`transfer::FILE_MAX`).
    pub fn file(path: &std::path::Path) -> Result<Read, ClipboardError> {
        let m = std::fs::metadata(path).map_err(|_| ClipboardError::Io)?;
        if !m.is_file() || m.len() > crate::transfer::FILE_MAX {
            return Err(ClipboardError::Big);
        }
        let data = std::fs::read(path).map_err(|_| ClipboardError::Io)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        Ok(Read::Bytes { name, mime: "application/octet-stream".into(), data })
    }
}

/// The OS clipboard, as the agent uses it. Each platform implements it on a thread of its own
/// and reports `ClipboardChanged` and `ClipboardRender` through the events it was started with.
pub trait Native: Send + std::marker::Sync {
    /// Watch and serve the clipboard, or not: off, nothing is read and placeholders this
    /// computer put there are removed.
    fn share(&self, on: bool);
    /// The browser owns the clipboard: placeholders for `formats`, rendered on demand.
    fn announce(&self, gen: u64, formats: Vec<Format>);
    /// The answer to a render request of `gen` (None: the paste gets nothing).
    fn provide(&self, gen: u64, data: Option<Data>);
    /// One format of the clipboard as it is now; `i` picks one of several files. `Gone` when
    /// it changed since it was last reported.
    fn read(&self, kind: FormatKind, i: Option<u32>) -> Result<Read, ClipboardError>;
    /// A picture sent from the browser, onto the clipboard (PNG).
    fn put_image(&self, png: &[u8]) -> bool;
    /// Files received from the browser, onto the clipboard as files and as their paths.
    fn put_files(&self, paths: &[std::path::PathBuf]) -> bool;
}

/// Files as text: their full paths, one per line, for targets that take no files.
pub fn paths_text(paths: &[std::path::PathBuf]) -> String {
    paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_text_goes_as_bytes() {
        assert_eq!(Read::text(FormatKind::Text, "hi".into()), Read::Text("hi".into()));
        assert_eq!(Read::text(FormatKind::Html, "<p>".into()), Read::Html("<p>".into()));
        match Read::text(FormatKind::Text, "x".repeat(INLINE_MAX + 1)) {
            Read::Bytes { mime, data, .. } => assert_eq!((mime.as_str(), data.len()), ("text/plain", INLINE_MAX + 1)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn formats_convert_to_the_wire_and_back() {
        let f = Format { kind: FormatKind::Image, size: Some(9), w: Some(3), h: Some(2), items: None };
        let w: types::Format = f.clone().into();
        assert_eq!(serde_json::to_value(&w).unwrap(), serde_json::json!({ "kind": "image", "size": 9, "w": 3, "h": 2 }));
        assert_eq!(Format::from(w), f);
        assert_eq!(paths_text(&["C:\\a b.txt".into(), "C:\\c".into()]), "C:\\a b.txt\r\nC:\\c");
        assert_eq!(Data::from_bytes(FormatKind::Text, b"ok".to_vec()), Some(Data::Text("ok".into())));
        assert_eq!(Data::from_bytes(FormatKind::Text, vec![0xff]), None);
    }

    #[test]
    fn a_copied_file_is_read_whole_and_a_folder_is_refused() {
        let dir = std::env::temp_dir().join(format!("deskway-clip-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a b.txt"), b"body").unwrap();
        assert_eq!(Read::file(&dir.join("a b.txt")), Ok(Read::Bytes { name: "a b.txt".into(), mime: "application/octet-stream".into(), data: b"body".to_vec() }));
        assert_eq!(Read::file(&dir), Err(ClipboardError::Big), "only files");
        assert_eq!(Read::file(&dir.join("gone")), Err(ClipboardError::Io));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
