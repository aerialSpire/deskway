use super::*;
use std::fs::OpenOptions;
use std::sync::mpsc::Receiver;
use std::sync::Mutex;

use crate::clipboard::{Format, Read};
use wire::types::ClipboardError;

struct Fake {
    dir: PathBuf,
    locked: bool,
}

impl Host for Fake {
    fn locked(&mut self) -> bool {
        self.locked
    }
    fn downloads(&mut self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
    fn create(&mut self, path: &Path) -> std::io::Result<FsFile> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        OpenOptions::new().write(true).create_new(true).open(path)
    }
    fn remove(&mut self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }
}

/// What reached the clipboard.
#[derive(Default)]
struct Clip {
    image: Mutex<Option<Vec<u8>>>,
    files: Mutex<Vec<PathBuf>>,
    provided: Mutex<Vec<(u64, Option<Data>)>>,
}

impl Native for Clip {
    fn share(&self, _: bool) {}
    fn announce(&self, _: u64, _: Vec<Format>) {}
    fn provide(&self, gen: u64, data: Option<Data>) {
        self.provided.lock().unwrap().push((gen, data));
    }
    fn read(&self, _: FormatKind, _: Option<u32>) -> Result<Read, ClipboardError> {
        Err(ClipboardError::Unsupported)
    }
    fn put_image(&self, png: &[u8]) -> bool {
        *self.image.lock().unwrap() = Some(png.to_vec());
        true
    }
    fn put_files(&self, paths: &[PathBuf]) -> bool {
        *self.files.lock().unwrap() = paths.to_vec();
        true
    }
}

struct Test {
    t: Transfers<Fake>,
    clip: Arc<Clip>,
    events: Receiver<PlatformEvent>,
    sent: Vec<String>,
    notes: Vec<Option<Note>>,
    bytes: Vec<usize>,
}

impl Test {
    fn new(name: &str) -> Test {
        let dir = std::env::temp_dir().join(format!("deskway-transfer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (out, events) = channel();
        let clip = Arc::new(Clip::default());
        Test { t: Transfers::new(Fake { dir, locked: false }, clip.clone(), out), clip, events, sent: vec![], notes: vec![], bytes: vec![] }
    }

    fn take(&mut self) {
        for e in self.events.try_iter() {
            match e {
                PlatformEvent::FileMessage(m) => self.sent.push(m),
                PlatformEvent::TransferNote(n) => self.notes.push(n),
                PlatformEvent::FileData(b) => self.bytes.push(b.0.len()),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn last(&mut self) -> serde_json::Value {
        self.take();
        serde_json::from_str(self.sent.last().unwrap()).unwrap()
    }

    fn last_note(&mut self) -> Option<Note> {
        self.take();
        self.notes.last().unwrap().clone()
    }
}

#[test]
fn files_arrive_split_across_chunks_with_safe_unique_names() {
    let mut x = Test::new("put");
    let folder = x.t.host.dir.join("Deskway");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("a.txt"), b"old").unwrap();
    x.t.message(r#"{"t":"put","id":"p1","kind":"files","items":[{"n":"..\\..\\a.txt","s":5},{"n":"","s":0},{"n":"CON.txt","s":3}]}"#);
    assert_eq!(x.last()["t"], "ready");
    x.t.data(b"hel");
    x.t.data(b"loxyz");
    let done = x.last();
    assert_eq!(done["t"], "done");
    assert_eq!(done["names"][0], "a (2).txt");
    assert_eq!(std::fs::read(folder.join("a (2).txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"old", "never replaced");
    assert_eq!(std::fs::read(folder.join("file")).unwrap(), b"");
    assert_eq!(std::fs::read(folder.join("_CON.txt")).unwrap(), b"xyz");
    assert_eq!(x.clip.files.lock().unwrap().len(), 3);
    assert!(x.last_note().unwrap().folder.is_some());
}

#[test]
fn a_stalled_transfer_ends_and_leaves_nothing() {
    let mut x = Test::new("stall");
    x.t.message(r#"{"t":"put","id":"s1","kind":"files","items":[{"n":"half.bin","s":10}]}"#);
    x.t.data(b"12345");
    x.t.tick();
    assert_eq!(x.last()["t"], "ready", "still moving: nothing happens");
    x.t.put.as_mut().unwrap().last_data = Instant::now() - STALL - Duration::from_secs(1);
    x.t.tick();
    assert_eq!(x.last()["why"], "timeout");
    assert!(x.t.put.is_none());
    assert_eq!(std::fs::read_dir(x.t.host.dir.join("Deskway")).unwrap().count(), 0, "the half file is removed");
    assert!(x.last_note().is_none(), "the progress note is cleared");
    // Late bytes of the given-up transfer change nothing.
    x.t.data(b"67890");
    assert_eq!(x.last()["why"], "timeout");
    x.t.tick();
}

#[test]
fn the_received_note_goes_away_by_itself() {
    let mut x = Test::new("note");
    x.t.message(r#"{"t":"put","id":"n1","kind":"files","items":[{"n":"a.txt","s":2}]}"#);
    x.t.data(b"hi");
    assert_eq!(x.last()["t"], "done");
    assert!(x.last_note().is_some());
    x.t.tick();
    assert!(x.last_note().is_some(), "shown for a while");
    x.t.note_until = Some(Instant::now() - Duration::from_secs(1));
    x.t.tick();
    assert!(x.last_note().is_none(), "then gone");
    let n = x.notes.len();
    x.t.tick();
    x.take();
    assert_eq!(x.notes.len(), n, "cleared once, not again and again");
    assert!(x.t.host.dir.join("Deskway/a.txt").exists(), "the received file itself stays");
}

#[test]
fn zero_byte_files_finish_without_a_data_message() {
    let mut x = Test::new("empty");
    x.t.message(r#"{"t":"put","id":"empty","kind":"files","items":[{"n":"empty.bin","s":0}]}"#);
    assert_eq!(x.last()["t"], "done");
    assert_eq!(std::fs::metadata(x.t.host.dir.join("Deskway/empty.bin")).unwrap().len(), 0);
}

#[test]
fn cancelled_or_oversized_puts_leave_nothing() {
    let mut x = Test::new("cancel");
    x.t.message(r#"{"t":"put","id":"p1","kind":"files","items":[{"n":"big.bin","s":10}]}"#);
    x.t.data(b"12345");
    x.t.message(r#"{"t":"cancel","id":"p1"}"#);
    let folder = x.t.host.dir.join("Deskway");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    x.t.message(r#"{"t":"put","id":"p2","kind":"files","items":[{"n":"x","s":3}]}"#);
    x.t.data(b"1234");
    assert_eq!(x.last()["why"], "size");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    x.t.message(&format!(r#"{{"t":"put","id":"p3","kind":"files","items":[{{"n":"x","s":{}}}]}}"#, FILE_MAX + 1));
    assert_eq!(x.last()["why"], "big");
    let eleven: Vec<String> = (0..11).map(|i| format!(r#"{{"n":"f{i}","s":1}}"#)).collect();
    x.t.message(&format!(r#"{{"t":"put","id":"p4","kind":"files","items":[{}]}}"#, eleven.join(",")));
    assert_eq!(x.last()["why"], "big");
    x.t.message(&format!(r#"{{"t":"put","id":"p5","kind":"files","items":[{{"n":"a","s":{}}},{{"n":"b","s":1}}]}}"#, u64::MAX));
    assert_eq!(x.last()["why"], "big");
}

#[test]
fn nothing_moves_while_locked() {
    let mut x = Test::new("lock");
    x.t.host.locked = true;
    x.t.message(r#"{"t":"put","id":"p1","kind":"image","items":[{"n":"image.png","s":3}]}"#);
    assert_eq!(x.last()["why"], "locked");
    x.t.host.locked = false;
    x.t.message(r#"{"t":"put","id":"p2","kind":"image","items":[{"n":"image.png","s":3}]}"#);
    x.t.data(b"png");
    assert_eq!(x.last()["t"], "done");
    assert_eq!(x.clip.image.lock().unwrap().as_deref(), Some(&b"png"[..]));
}

#[test]
fn clipboard_answers_go_to_the_waiting_paste_not_to_downloads() {
    let mut x = Test::new("clip");
    x.t.message(r#"{"t":"put","id":"p7","kind":"image","items":[{"n":"image.png","s":4}],"gen":3}"#);
    assert_eq!(x.last()["t"], "ready");
    x.t.data(b"\x89PN");
    x.t.data(b"G");
    assert_eq!(x.last()["t"], "done");
    assert_eq!(*x.clip.provided.lock().unwrap(), vec![(3, Some(Data::Image(b"\x89PNG".to_vec())))]);
    assert!(x.clip.image.lock().unwrap().is_none(), "not put on the clipboard as a received picture");
    assert!(x.notes.is_empty(), "no note: the person pasted it");
    // Large text the same way; cancelled, the paste gets nothing.
    x.t.message(r#"{"t":"put","id":"p8","kind":"text","items":[{"n":"t","s":9}],"gen":4}"#);
    x.t.data(b"half");
    x.t.message(r#"{"t":"cancel","id":"p8"}"#);
    assert_eq!(x.clip.provided.lock().unwrap()[1], (4, None));
    // Text without a generation is no transfer.
    x.t.message(r#"{"t":"put","id":"p9","kind":"text","items":[{"n":"t","s":1}]}"#);
    assert_eq!(x.last()["why"], "unsupported");
}

#[test]
fn clipboard_data_goes_out_as_meta_chunks_and_fin() {
    let mut x = Test::new("out");
    x.t.send_out("g1", 5, "a.png", "image/png", &vec![1u8; CHUNK * 2 + 1]);
    x.take();
    let meta: serde_json::Value = serde_json::from_str(&x.sent[0]).unwrap();
    assert_eq!((meta["t"].as_str(), meta["gen"].as_u64(), meta["size"].as_u64(), meta["mime"].as_str()), (Some("meta"), Some(5), Some((CHUNK * 2 + 1) as u64), Some("image/png")));
    assert_eq!(x.bytes, vec![CHUNK, CHUNK, 1]);
    assert_eq!(x.last()["t"], "fin");
}

#[test]
fn requests_for_anything_else_are_answered() {
    let mut x = Test::new("other");
    x.t.message(r#"{"t":"zz","id":"g1"}"#);
    assert_eq!(x.last()["why"], "unsupported");
    x.t.message(r#"{"t":"done","id":"g2","names":[]}"#);
    assert_eq!((x.last()["id"].as_str(), x.last()["why"].as_str()), (Some("g2"), Some("unsupported")), "the host's own messages are not taken");
}

#[test]
fn names_are_made_safe() {
    assert_eq!(safe_name("C:\\Windows\\evil.exe"), "evil.exe");
    assert_eq!(safe_name("../../x"), "x");
    assert_eq!(safe_name("a<b>c?.txt. "), "abc.txt");
    assert_eq!(safe_name("nul"), "_nul");
    assert_eq!(safe_name("lpt1.log"), "_lpt1.log");
    assert_eq!(safe_name("COM10"), "COM10");
    assert!(safe_name(&"x".repeat(400)).chars().count() <= 150);
    assert!(safe_name(&format!("{}.pdf", "y".repeat(400))).ends_with(".pdf"));
}
