//! Images and files from the controller to this computer.
//!
//! The browser talks on its own "file" channel (`wire::file::File` as text, file contents as
//! binary chunks). One transfer at a time; the browser queues the rest.
//!
//! `put` (names and sizes) → `ready` → the bytes, in order → `done`. Files are written as the
//! signed-in user into the Deskway folder in Downloads and put on the clipboard; an image goes
//! on the clipboard only. Nothing is ever run.
//!
//! The clipboard uses the same transfers for data too large for a control message, under its
//! request id and generation: a `put` with `gen` answers a paste waiting on this computer, and
//! [`Transfers::send_out`] streams this computer's clipboard data to the browser as `meta`,
//! binary chunks and `fin`.

use std::fs::File as FsFile;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::clipboard::{Data, Native};
use crate::platform::{Bytes, PlatformEvent};
pub use crate::platform::TransferNote as Note;
use wire::file::File;
use wire::types::{FormatKind, Item, TransferError, TransferKind};

pub const FILE_MAX: u64 = 25 << 20;
pub const TOTAL_MAX: u64 = 50 << 20;
pub const FILES_MAX: usize = 10;
pub const IMAGE_MAX: u64 = 20 << 20;

/// What a transfer needs from the computer's files. Access goes through here so it can run as
/// the signed-in user, never as SYSTEM.
pub trait Host {
    /// The lock screen or a secure prompt is up: nothing moves.
    fn locked(&mut self) -> bool;
    fn downloads(&mut self) -> Option<PathBuf>;
    /// Creates a new file (never replaces one), making its folder if needed.
    fn create(&mut self, path: &Path) -> std::io::Result<FsFile>;
    fn remove(&mut self, path: &Path);
}

struct Put {
    id: String,
    image: bool,
    /// Clipboard data for a paste of this generation, of this kind; kept in memory.
    clip: Option<(u64, FormatKind)>,
    items: Vec<Item>,
    /// The item being written and how much of it has arrived.
    idx: usize,
    got_item: u64,
    total: u64,
    got: u64,
    file: Option<FsFile>,
    paths: Vec<PathBuf>,
    dir: PathBuf,
    buf: Vec<u8>,
    last_check: u64,
    last_note: Instant,
    /// When bytes last arrived: a transfer that stops moving is ended, its half file removed.
    last_data: Instant,
}

impl Put {
    /// Opens the file for the current item, and finishes empty ones on the way.
    fn open_items<H: Host>(&mut self, host: &mut H) -> std::io::Result<()> {
        while self.idx < self.items.len() && self.file.is_none() {
            let (path, file) = create_unique(host, &self.dir, &self.items[self.idx].n)?;
            self.paths.push(path);
            if self.items[self.idx].s == 0 {
                self.idx += 1;
            } else {
                self.file = Some(file);
            }
        }
        Ok(())
    }

    /// Takes the next bytes: into memory for a picture or clipboard data, else into the files
    /// in turn.
    fn take<H: Host>(&mut self, host: &mut H, mut data: &[u8]) -> Result<(), TransferError> {
        if data.len() as u64 > self.total - self.got {
            return Err(TransferError::Size);
        }
        self.got += data.len() as u64;
        self.last_data = Instant::now();
        if self.image {
            self.buf.extend_from_slice(data);
            return Ok(());
        }
        while !data.is_empty() {
            let size = self.items[self.idx].s;
            let n = ((size - self.got_item) as usize).min(data.len());
            if !self.file.as_mut().is_some_and(|f| f.write_all(&data[..n]).is_ok()) {
                return Err(TransferError::Io);
            }
            self.got_item += n as u64;
            data = &data[n..];
            if self.got_item == size {
                self.file = None;
                self.idx += 1;
                self.got_item = 0;
                self.open_items(host).map_err(|_| TransferError::Io)?;
            }
        }
        Ok(())
    }
}

/// A transfer with no bytes for this long is given up.
const STALL: Duration = Duration::from_secs(30);
/// “Received 2 files” stays on screen this long, then goes.
const NOTE_FOR: Duration = Duration::from_secs(10);

pub struct Transfers<H: Host> {
    pub host: H,
    /// Where received pictures and files go, and the waiting paste a clipboard answer is for.
    clipboard: Arc<dyn Native>,
    out: Sender<PlatformEvent>,
    put: Option<Put>,
    /// When the last result note goes away.
    note_until: Option<Instant>,
}

pub enum Command {
    Message(String),
    Data(Vec<u8>),
    /// This computer's clipboard data for a browser request.
    Send { id: String, gen: u64, name: String, mime: String, data: Vec<u8> },
    Stop,
}

/// Binary chunks on the file channel: the browser's own chunk size.
pub const CHUNK: usize = 16 * 1024;

/// A transfer id as the browser makes them.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
}

/// Runs transfers on a thread of their own, so input never waits for them. The host is made
/// on that thread. It ends when the sender is dropped.
pub fn spawn<H: Host + 'static>(make: impl FnOnce() -> H + Send + 'static, clipboard: Arc<dyn Native>, out: Sender<PlatformEvent>) -> Sender<Command> {
    let (tx, rx) = channel::<Command>();
    std::thread::Builder::new()
        .name("transfers".into())
        .spawn(move || {
            let mut t = Transfers::new(make(), clipboard, out);
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Command::Message(m)) => t.message(&m),
                    Ok(Command::Data(d)) => t.data(&d),
                    Ok(Command::Send { id, gen, name, mime, data }) => t.send_out(&id, gen, &name, &mime, &data),
                    Ok(Command::Stop) => t.stop(),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                t.tick();
            }
            t.stop();
        })
        .expect("spawn transfer thread");
    tx
}

impl<H: Host> Transfers<H> {
    pub fn new(host: H, clipboard: Arc<dyn Native>, out: Sender<PlatformEvent>) -> Self {
        Transfers { host, clipboard, out, put: None, note_until: None }
    }

    fn send(&self, m: File) {
        let _ = self.out.send(PlatformEvent::FileMessage(serde_json::to_string(&m).expect("file message serializes")));
    }

    fn refuse(&self, id: String, why: TransferError) {
        self.send(File::Err { id, why });
    }

    /// Clipboard data for the browser: `meta`, the bytes in chunks, `fin`. All are queued at
    /// once; the session sends them as the channel drains and drops the rest on `cancel`.
    pub fn send_out(&self, id: &str, gen: u64, name: &str, mime: &str, data: &[u8]) {
        self.send(File::Meta { id: id.into(), name: name.into(), size: data.len() as u64, mime: Some(mime.into()), gen });
        for chunk in data.chunks(CHUNK) {
            let _ = self.out.send(PlatformEvent::FileData(Bytes(chunk.to_vec())));
        }
        self.send(File::Fin { id: id.into() });
    }

    fn note(&self, note: Option<Note>) {
        let _ = self.out.send(PlatformEvent::TransferNote(note));
    }

    /// Called every second or so: ends a stalled transfer and clears an old result note.
    pub fn tick(&mut self) {
        if self.put.as_ref().is_some_and(|p| p.last_data.elapsed() >= STALL) {
            self.note(None);
            self.fail(TransferError::Timeout);
        }
        if self.note_until.is_some_and(|t| Instant::now() >= t) {
            self.note_until = None;
            self.note(None);
        }
    }

    pub fn message(&mut self, text: &str) {
        match serde_json::from_str::<File>(text) {
            Ok(File::Put { id, kind, items, gen }) if valid_id(&id) => self.start_put(id, kind, items, gen),
            Ok(File::Cancel { id }) => {
                if self.put.as_ref().is_some_and(|p| p.id == id) {
                    self.abort();
                    self.note(None);
                }
            }
            // Nothing else is served, and what this host cannot read (from a newer browser,
            // say) neither: answered, so the browser does not wait for it.
            _ => {
                #[derive(serde::Deserialize)]
                struct Named {
                    id: String,
                }
                if let Ok(Named { id }) = serde_json::from_str(text) {
                    if valid_id(&id) {
                        self.refuse(id, TransferError::Unsupported);
                    }
                }
            }
        }
    }

    /// The session ended: whatever was half done is removed.
    pub fn stop(&mut self) {
        if self.put.is_some() {
            self.abort();
        }
        self.note_until = None;
        self.note(None);
    }

    fn start_put(&mut self, id: String, kind: TransferKind, items: Vec<Item>, gen: Option<u64>) {
        if self.put.is_some() {
            self.abort();
        }
        let clip = match (gen, kind) {
            (Some(g), TransferKind::Image) => Some((g, FormatKind::Image)),
            (Some(g), TransferKind::Text) => Some((g, FormatKind::Text)),
            (Some(g), TransferKind::Html) => Some((g, FormatKind::Html)),
            (None, TransferKind::Files | TransferKind::Image) => None,
            _ => return self.refuse(id, TransferError::Unsupported),
        };
        // Clipboard data is held in memory, as an image is.
        let image = kind == TransferKind::Image || clip.is_some();
        let fits = |total: u64| {
            if image {
                items.len() == 1 && (1..=IMAGE_MAX).contains(&total)
            } else {
                (1..=FILES_MAX).contains(&items.len()) && items.iter().all(|i| i.s <= FILE_MAX) && total <= TOTAL_MAX
            }
        };
        let Some(total) = items.iter().try_fold(0u64, |sum, item| sum.checked_add(item.s)).filter(|&n| fits(n)) else {
            return self.refuse(id, TransferError::Big);
        };
        if self.host.locked() {
            return self.refuse(id, TransferError::Locked);
        }
        let dir = if image {
            PathBuf::new()
        } else {
            match self.host.downloads() {
                Some(d) => d.join("Deskway"),
                None => return self.refuse(id, TransferError::Io),
            }
        };
        let mut put = Put { id: id.clone(), image, clip, items, idx: 0, got_item: 0, total, got: 0, file: None, paths: Vec::new(), dir, buf: Vec::new(), last_check: 0, last_note: Instant::now(), last_data: Instant::now() };
        self.note_until = None;
        let opened = if image { Ok(()) } else { put.open_items(&mut self.host) };
        self.put = Some(put);
        if opened.is_err() {
            return self.fail(TransferError::Io);
        }
        self.send(File::Ready { id });
        if total == 0 {
            self.finish();
        }
    }

    pub fn data(&mut self, data: &[u8]) {
        let Some(p) = self.put.as_mut() else { return };
        if let Err(why) = p.take(&mut self.host, data) {
            return self.fail(why);
        }
        // Every megabyte: the computer may have been locked meanwhile.
        if p.got - p.last_check >= 1 << 20 {
            p.last_check = p.got;
            if self.host.locked() {
                return self.fail(TransferError::Locked);
            }
        }
        if p.got == p.total {
            self.finish();
        } else if !p.image && p.last_note.elapsed() >= Duration::from_secs(1) {
            p.last_note = Instant::now();
            let text = crate::i18n::tn("note.receiving", p.items.len(), &[("p", (p.got * 100 / p.total.max(1)).to_string())]);
            self.note(Some(Note { text, folder: None }));
        }
    }

    fn finish(&mut self) {
        let Some(p) = self.put.take() else { return };
        if self.host.locked() {
            for path in &p.paths {
                self.host.remove(path);
            }
            return self.refuse(p.id, TransferError::Locked);
        }
        if let Some((gen, kind)) = p.clip {
            // The paste was waiting: no note, pasting it was the person's own doing.
            match Data::from_bytes(kind, p.buf) {
                Some(d) => self.clipboard.provide(gen, Some(d)),
                None => {
                    self.clipboard.provide(gen, None);
                    return self.refuse(p.id, TransferError::Io);
                }
            }
            return self.send(File::Done { id: p.id, names: Vec::new() });
        }
        if p.image {
            if !self.clipboard.put_image(&p.buf) {
                return self.refuse(p.id, TransferError::Image);
            }
            self.note(Some(Note { text: crate::i18n::t("note.image"), folder: None }));
        } else {
            drop(p.file);
            self.clipboard.put_files(&p.paths);
            let text = crate::i18n::tn("note.received", p.paths.len(), &[]);
            self.note(Some(Note { text, folder: Some(p.dir.to_string_lossy().into_owned()) }));
        }
        self.note_until = Some(Instant::now() + NOTE_FOR);
        let names: Vec<String> = p.paths.iter().filter_map(|x| x.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        self.send(File::Done { id: p.id, names });
    }

    /// Ends the transfer in progress, telling the browser why.
    fn fail(&mut self, why: TransferError) {
        let Some(id) = self.put.as_ref().map(|p| p.id.clone()) else { return };
        self.abort();
        self.refuse(id, why);
    }

    fn abort(&mut self) {
        if let Some(p) = self.put.take() {
            if let Some((gen, _)) = p.clip {
                self.clipboard.provide(gen, None);
            }
            drop(p.file);
            for path in &p.paths {
                self.host.remove(path);
            }
        }
    }
}

/// A name from the controller, made safe as one file name: no folders, no device names, no
/// characters Windows refuses.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base.chars().filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c)).collect();
    while s.ends_with(['.', ' ']) {
        s.pop();
    }
    let s = s.trim_start().to_string();
    let mut s = if s.is_empty() { "file".to_string() } else { s };
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        s.insert(0, '_');
    }
    if s.chars().count() > 150 {
        let ext: String = Path::new(&s).extension().map(|e| format!(".{}", e.to_string_lossy())).filter(|e| e.chars().count() <= 16).unwrap_or_default();
        let keep = 150 - ext.chars().count();
        s = s.chars().take(keep).collect::<String>() + &ext;
    }
    s
}

/// `name`, or `name (2)`, `name (3)` … whichever does not exist yet. Taken by creating it.
fn create_unique<H: Host>(host: &mut H, dir: &Path, name: &str) -> std::io::Result<(PathBuf, FsFile)> {
    let name = safe_name(name);
    let p = Path::new(&name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1..1000 {
        let candidate = if n == 1 { dir.join(&name) } else { dir.join(format!("{stem} ({n}){ext}")) };
        match host.create(&candidate) {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free name"))
}

#[cfg(test)]
mod tests;
