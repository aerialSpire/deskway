//! The macOS pasteboard for the shared clipboard (`crate::clipboard`).
//!
//! - Changes: macOS has no change notification for the general pasteboard; its `changeCount`
//!   is checked when sharing starts and every `WATCH_EVERY` while sharing is on.
//!   The watcher waits while sharing is off. Nothing is read at the login window or when
//!   the change is this host's own.
//! - The browser's formats are promised data: one `NSPasteboardItem` whose types are provided
//!   by an `NSPasteboardItemDataProvider`; a paste asks the browser through `ClipboardRender`
//!   and waits up to `RENDER_DEADLINE`.
//! - Pictures and files received from the browser are put here too.
//!
//! Types: public.utf8-plain-text, public.html, public.png (with public.tiff for programs that
//! take only that), file URLs with the paths as text.

use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::ProtocolObject;
use objc2::AnyThread;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardItem, NSPasteboardType,
    NSPasteboardTypeFileURL, NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSURL};

use crate::clipboard::{self, Data, Format, Read, INLINE_MAX};
use crate::platform::{Desk, PlatformEvent};
use wire::types::{ClipboardError, FormatKind, Item};
use crate::clipboard::image::{png_size, PIXELS_MAX};

mod promise;
use promise::Promise;

/// How often the pasteboard's change count is checked. macOS sends no change notification;
/// twice a second is what clipboard tools there use, and a copy is announced well before
/// anyone can switch to the other side and paste.
const WATCH_EVERY: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Shared {
    on: bool,
    epoch: u64,
    // The change count after this host's last write, so the watcher skips its own changes.
    own: Option<isize>,
    seen: Option<isize>,
    reported: Option<Reported>,
    promise: Option<Arc<Promise>>,
}

struct Reported {
    count: isize,
    files: Vec<PathBuf>,
}

struct Inner {
    // Serializes pasteboard writes and the state updates that accompany them.
    writes: Mutex<()>,
    shared: Mutex<Shared>,
    wake: SyncSender<()>,
    events: Sender<PlatformEvent>,
}

pub struct Clipboard {
    inner: Arc<Inner>,
}

fn types_of(kind: FormatKind) -> Vec<&'static NSPasteboardType> {
    unsafe {
        match kind {
            FormatKind::Text => vec![NSPasteboardTypeString],
            FormatKind::Html => vec![NSPasteboardTypeHTML],
            FormatKind::Image => vec![NSPasteboardTypePNG, NSPasteboardTypeTIFF],
            FormatKind::Files => vec![NSPasteboardTypeFileURL],
        }
    }
}

fn kind_of(ty: &NSPasteboardType) -> Option<FormatKind> {
    [FormatKind::Text, FormatKind::Html, FormatKind::Image]
        .into_iter()
        .find(|k| types_of(*k).iter().any(|t| **t == *ty))
}

/// This user's session is on the console and unlocked (`desk`): only then does anything move.
fn console_active() -> bool {
    super::desk::current() == Desk::Default
}

fn tiff_from_png(png: &[u8]) -> Option<Retained<NSData>> {
    let image =
        NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &NSData::with_bytes(png))?;
    image.TIFFRepresentation()
}

fn png_from_tiff(tiff: &NSData) -> Option<Vec<u8>> {
    let image = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), tiff)?;
    let png = unsafe {
        image.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(png.to_vec())
}

fn file_urls(items: &NSArray<NSPasteboardItem>) -> Vec<PathBuf> {
    items
        .iter()
        .filter_map(|item| {
            let s = item.stringForType(unsafe { NSPasteboardTypeFileURL })?;
            NSURL::URLWithString(&s)?.to_file_path()
        })
        .collect()
}

/// What is on the pasteboard now: its formats and small text itself.
fn describe(items: &NSArray<NSPasteboardItem>) -> (Vec<Format>, Option<String>, Vec<PathBuf>) {
    let mut formats = Vec::new();
    let mut text = None;
    if let Some(t) = items
        .iter()
        .find_map(|item| item.stringForType(unsafe { NSPasteboardTypeString }))
        .map(|s| s.to_string())
    {
        formats.push(Format {
            size: Some(t.len() as u64),
            ..Format::of(FormatKind::Text)
        });
        text = Some(t).filter(|t| t.len() <= INLINE_MAX);
    }
    if let Some(h) = items
        .iter()
        .find_map(|item| item.stringForType(unsafe { NSPasteboardTypeHTML }))
    {
        formats.push(Format {
            size: Some(h.to_string().len() as u64),
            ..Format::of(FormatKind::Html)
        });
    }
    if let Some(png) = items
        .iter()
        .find_map(|item| item.dataForType(unsafe { NSPasteboardTypePNG }))
    {
        // Pasteboard NSData is immutable. Read the header without copying the image.
        let (w, h) = png_size(unsafe { png.as_bytes_unchecked() }).unzip();
        formats.push(Format {
            size: Some(png.len() as u64),
            w,
            h,
            ..Format::of(FormatKind::Image)
        });
    } else if items.iter().any(|item| {
        item.availableTypeFromArray(&NSArray::from_slice(&[unsafe { NSPasteboardTypeTIFF }]))
            .is_some()
    }) {
        formats.push(Format::of(FormatKind::Image));
    }
    let paths = file_urls(items);
    let mut files = Vec::new();
    if !paths.is_empty() {
        // Folders are not sent: only the files among them are listed.
        let items: Vec<Item> = paths
            .iter()
            .filter_map(|p| {
                let m = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
                let name = p.file_name()?.to_string_lossy().into_owned();
                files.push(p.clone());
                Some(Item {
                    n: name,
                    s: m.len(),
                })
            })
            .collect();
        if !items.is_empty() {
            formats.push(Format {
                items: Some(items),
                ..Format::of(FormatKind::Files)
            });
        }
        // Finder puts the names as text; the full paths are what a target taking text wants.
        let t = clipboard::paths_text(&paths);
        formats.retain(|f| f.kind != FormatKind::Text);
        formats.push(Format {
            size: Some(t.len() as u64),
            ..Format::of(FormatKind::Text)
        });
        text = Some(t).filter(|t| t.len() <= INLINE_MAX);
    }
    (formats, text, files)
}

fn watch(inner: Weak<Inner>, wake: Receiver<()>, mut check: impl FnMut(&Inner) -> bool) {
    let mut polling = false;
    loop {
        // Only Inner owns the sender. Dropping it ends either kind of wait.
        let disconnected = if polling {
            wake.recv_timeout(WATCH_EVERY) == Err(RecvTimeoutError::Disconnected)
        } else {
            wake.recv().is_err()
        };
        if disconnected {
            break;
        }
        let Some(inner) = inner.upgrade() else { break };
        polling = check(&inner);
    }
}

/// Checks for a change and returns whether sharing is on and polling should continue.
fn check_pasteboard(inner: &Inner) -> bool {
    let writes = inner.writes.lock().unwrap();
    let mut state = inner.shared.lock().unwrap();
    if !state.on {
        return false;
    }
    autoreleasepool(|_| {
        if !console_active() {
            return;
        }
        let pb = NSPasteboard::generalPasteboard();
        let count = pb.changeCount();
        if state.seen.replace(count) == Some(count) || state.own == Some(count) {
            return;
        }
        if let Some(promise) = state.promise.take() {
            promise.cancel();
        }
        let items = pb.pasteboardItems().unwrap_or_else(NSArray::new);
        if pb.changeCount() != count {
            return;
        }
        let epoch = state.epoch;
        drop(state);
        drop(writes);

        // The items belong to this copy only. If another writer takes the pasteboard
        // while we read, these items become stale rather than fetching the new data.
        let (formats, text, files) = describe(&items);
        let mut state = inner.shared.lock().unwrap();
        if state.on && state.epoch == epoch && pb.changeCount() == count {
            state.reported = Some(Reported { count, files });
            let _ = inner
                .events
                .send(PlatformEvent::ClipboardChanged { formats, text });
        }
    });
    true
}

impl Clipboard {
    fn new(events: Sender<PlatformEvent>) -> (Self, Receiver<()>) {
        let (wake, changed) = sync_channel(1);
        let inner = Arc::new(Inner {
            writes: Mutex::new(()),
            shared: Mutex::new(Shared::default()),
            wake,
            events,
        });
        (Self { inner }, changed)
    }

    /// Starts watching the pasteboard. Changes and render requests go to `events`.
    pub fn start(events: Sender<PlatformEvent>) -> Clipboard {
        let (clipboard, changed) = Self::new(events);
        let watcher = Arc::downgrade(&clipboard.inner);
        let _ = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || watch(watcher, changed, check_pasteboard));
        clipboard
    }

    /// Writes under `writes`, so the watcher never sees a new count before it is recorded.
    fn write(&self, items: Vec<Retained<NSPasteboardItem>>) -> bool {
        let pb = NSPasteboard::generalPasteboard();
        let items: Vec<_> = items
            .into_iter()
            .map(ProtocolObject::from_retained)
            .collect();
        let count = pb.clearContents();
        let ok = items.is_empty() || pb.writeObjects(&NSArray::from_retained_slice(&items));
        self.inner.shared.lock().unwrap().own = Some(count);
        ok
    }

    fn replace(&self, items: Vec<Retained<NSPasteboardItem>>) -> bool {
        let _writes = self.inner.writes.lock().unwrap();
        if let Some(promise) = self.inner.shared.lock().unwrap().promise.take() {
            promise.cancel();
        }
        self.write(items)
    }
}

/// Files as pasteboard items: each a file URL, with its path as text.
pub fn file_items(paths: &[PathBuf]) -> Option<Vec<Retained<NSPasteboardItem>>> {
    paths
        .iter()
        .map(|path| {
            let url = NSURL::from_file_path(path)?;
            let string = url.absoluteString()?;
            let item = NSPasteboardItem::new();
            item.setString_forType(&NSString::from_str(&path.to_string_lossy()), unsafe {
                NSPasteboardTypeString
            });
            item.setString_forType(&string, unsafe { NSPasteboardTypeFileURL })
                .then_some(item)
        })
        .collect()
}

/// A picture: PNG for programs that keep transparency, TIFF for the others.
pub fn image_item(png: &[u8]) -> Option<Retained<NSPasteboardItem>> {
    let (w, h) = png_size(png)?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > PIXELS_MAX {
        return None;
    }
    let item = NSPasteboardItem::new();
    if !item.setData_forType(&NSData::with_bytes(png), unsafe { NSPasteboardTypePNG }) {
        return None;
    }
    if let Some(tiff) = tiff_from_png(png) {
        item.setData_forType(&tiff, unsafe { NSPasteboardTypeTIFF });
    }
    Some(item)
}

impl clipboard::Native for Clipboard {
    fn share(&self, on: bool) {
        let _writes = self.inner.writes.lock().unwrap();
        autoreleasepool(|_| {
            let (old, own) = {
                let mut state = self.inner.shared.lock().unwrap();
                if state.on == on {
                    return;
                }
                state.on = on;
                state.epoch += 1;
                state.reported = None;
                // What is already on the pasteboard when sharing starts at the active console is
                // not announced; at the login window it is unreadable, so nothing is marked seen.
                state.seen = (on && console_active())
                    .then(|| NSPasteboard::generalPasteboard().changeCount());
                // One pending wake is enough. The watcher reads the current sharing state.
                let _ = self.inner.wake.try_send(());
                (state.promise.take(), state.own)
            };
            if let Some(old) = old {
                old.cancel();
                // Withdraw only our still-current promise. Leave anyone else's copy alone.
                if Some(NSPasteboard::generalPasteboard().changeCount()) == own {
                    self.write(Vec::new());
                }
            }
        });
    }

    fn announce(&self, gen: u64, formats: Vec<Format>) {
        let _writes = self.inner.writes.lock().unwrap();
        let sharing = self.inner.shared.lock().unwrap().on;
        let why = if !sharing || !console_active() {
            // Said in the log: which of the two, since the browser only hears "denied".
            eprintln!("clipboard: the browser's copy is not placed: {}", if sharing { "this session is not the unlocked desktop on the console" } else { "sharing is off" });
            ClipboardError::Denied
        } else {
            let placed = autoreleasepool(|_| {
                if let Some(old) = self.inner.shared.lock().unwrap().promise.take() {
                    old.cancel();
                }
                let types: Vec<&NSPasteboardType> =
                    formats.iter().flat_map(|f| types_of(f.kind)).collect();
                let promise = Promise::new(gen, self.inner.events.clone());
                let items = if types.is_empty() {
                    Vec::new()
                } else if let Some(item) = promise.item(&types) {
                    vec![item]
                } else {
                    return false;
                };
                // Store the promise before publication so the session can answer immediately.
                self.inner.shared.lock().unwrap().promise = Some(promise.clone());
                let placed = promise.publish(|| self.write(items));
                if !placed {
                    self.inner.shared.lock().unwrap().promise = None;
                }
                placed
            });
            if placed {
                return;
            }
            ClipboardError::Io
        };
        let _ = self.inner.events.send(PlatformEvent::ClipboardAnnounced {
            gen,
            why: Some(why),
        });
    }

    fn provide(&self, gen: u64, data: Option<Data>) {
        let promise = self.inner.shared.lock().unwrap().promise.clone();
        if let Some(promise) = promise.filter(|p| p.gen == gen) {
            promise.answer(data);
        }
    }

    fn read(&self, kind: FormatKind, i: Option<u32>) -> Result<Read, ClipboardError> {
        autoreleasepool(|_| {
            let writes = self.inner.writes.lock().unwrap();
            let state = self.inner.shared.lock().unwrap();
            if !state.on || !console_active() {
                return Err(ClipboardError::Denied);
            }
            let pb = NSPasteboard::generalPasteboard();
            let count = pb.changeCount();
            let reported = state
                .reported
                .as_ref()
                .filter(|reported| reported.count == count)
                .ok_or(ClipboardError::Gone)?;
            // The index is from the announced list, even if an earlier entry went
            // away or the original selection included folders.
            let file = if kind == FormatKind::Files {
                reported.files.get(i.unwrap_or(0) as usize).cloned()
            } else {
                None
            };
            let items = pb.pasteboardItems().unwrap_or_else(NSArray::new);
            if pb.changeCount() != count {
                return Err(ClipboardError::Gone);
            }
            drop(state);
            drop(writes);
            match kind {
                FormatKind::Text => {
                    let paths = file_urls(&items);
                    if !paths.is_empty() {
                        Ok(Read::text(FormatKind::Text, clipboard::paths_text(&paths)))
                    } else {
                        items
                            .iter()
                            .find_map(|item| item.stringForType(unsafe { NSPasteboardTypeString }))
                            .map(|s| Read::text(FormatKind::Text, s.to_string()))
                            .ok_or(ClipboardError::Unsupported)
                    }
                }
                FormatKind::Html => items
                    .iter()
                    .find_map(|item| item.stringForType(unsafe { NSPasteboardTypeHTML }))
                    .map(|s| Read::text(FormatKind::Html, s.to_string()))
                    .ok_or(ClipboardError::Unsupported),
                FormatKind::Image => {
                    let png = match items
                        .iter()
                        .find_map(|item| item.dataForType(unsafe { NSPasteboardTypePNG }))
                    {
                        Some(d) => d.to_vec(),
                        None => items
                            .iter()
                            .find_map(|item| item.dataForType(unsafe { NSPasteboardTypeTIFF }))
                            .and_then(|t| png_from_tiff(&t))
                            .ok_or(ClipboardError::Unsupported)?,
                    };
                    Ok(Read::Bytes {
                        name: "image.png".into(),
                        mime: "image/png".into(),
                        data: png,
                    })
                }
                FormatKind::Files => Read::file(&file.ok_or(ClipboardError::Gone)?),
            }
        })
    }

    fn put_image(&self, png: &[u8]) -> bool {
        autoreleasepool(|_| {
            console_active() && image_item(png).is_some_and(|item| self.replace(vec![item]))
        })
    }

    fn put_files(&self, paths: &[PathBuf]) -> bool {
        autoreleasepool(|_| {
            console_active()
                && file_items(paths).is_some_and(|items| !items.is_empty() && self.replace(items))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::image::png_encode;

    fn written(pb: &NSPasteboard, items: Vec<Retained<NSPasteboardItem>>) {
        let items: Vec<_> = items
            .into_iter()
            .map(ProtocolObject::from_retained)
            .collect();
        pb.clearContents();
        assert!(pb.writeObjects(&NSArray::from_retained_slice(&items)));
    }

    #[test]
    fn watcher_wakes_when_sharing_turns_on() {
        let (events, _received) = std::sync::mpsc::channel();
        let (clipboard, changed) = Clipboard::new(events);
        let inner = Arc::downgrade(&clipboard.inner);
        let (checked, checks) = std::sync::mpsc::channel();
        let watcher = std::thread::spawn(move || {
            watch(inner, changed, |inner| {
                assert!(inner.shared.lock().unwrap().on);
                checked.send(()).unwrap();
                false
            });
        });

        clipboard::Native::share(&clipboard, true);
        let timeout = Duration::from_secs(5);
        assert_eq!(checks.recv_timeout(timeout), Ok(()));

        drop(clipboard);
        assert_eq!(checks.recv_timeout(timeout), Err(RecvTimeoutError::Disconnected));
        watcher.join().unwrap();
    }

    #[test]
    fn received_images_go_on_the_pasteboard_as_png_and_tiff() {
        autoreleasepool(|_| {
            let pb = NSPasteboard::pasteboardWithUniqueName();
            let png = png_encode(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
            written(&pb, vec![image_item(&png).unwrap()]);
            assert_eq!(
                pb.dataForType(unsafe { NSPasteboardTypePNG })
                    .map(|d| d.to_vec()),
                Some(png)
            );
            assert!(pb.dataForType(unsafe { NSPasteboardTypeTIFF }).is_some());
            let (formats, _, _) = describe(&pb.pasteboardItems().unwrap());
            assert_eq!(
                (formats[0].kind, formats[0].w, formats[0].h),
                (FormatKind::Image, Some(2), Some(1))
            );
        });
    }

    #[test]
    fn received_files_go_on_the_pasteboard_as_file_urls_and_paths() {
        autoreleasepool(|_| {
            let pb = NSPasteboard::pasteboardWithUniqueName();
            let paths = [
                PathBuf::from("/tmp/报告 (2).pdf"),
                PathBuf::from("/tmp/one#two.txt"),
            ];
            written(&pb, file_items(&paths).unwrap());
            assert_eq!(file_urls(&pb.pasteboardItems().unwrap()), paths);
            let (formats, text, files) = describe(&pb.pasteboardItems().unwrap());
            assert!(files.is_empty());
            assert!(!formats.iter().any(|f| f.kind == FormatKind::Files));
            assert_eq!(
                text.as_deref(),
                Some("/tmp/报告 (2).pdf\r\n/tmp/one#two.txt")
            );
        });
    }

    #[test]
    fn the_file_index_matches_the_announced_file_when_the_selection_has_a_folder() {
        autoreleasepool(|_| {
            let pb = NSPasteboard::pasteboardWithUniqueName();
            let directory = std::env::temp_dir().join(pb.name().to_string());
            std::fs::create_dir(&directory).unwrap();
            let file = directory.join("clipboard.txt");
            std::fs::write(&file, b"clipboard").unwrap();
            written(&pb, file_items(&[directory.clone(), file.clone()]).unwrap());
            let (formats, _, files) = describe(&pb.pasteboardItems().unwrap());
            let expected = Item {
                n: "clipboard.txt".into(),
                s: 9,
            };
            let format = formats
                .iter()
                .find(|format| format.kind == FormatKind::Files)
                .unwrap();
            assert_eq!(format.items.as_deref(), Some([expected].as_slice()));
            assert_eq!(files, [file]);
            std::fs::remove_dir_all(directory).unwrap();
        });
    }

    #[test]
    fn an_item_from_a_replaced_copy_cannot_read_the_new_owners_data() {
        autoreleasepool(|_| {
            let pb = NSPasteboard::pasteboardWithUniqueName();
            let text_type = unsafe { NSPasteboardTypeString };
            let old = NSPasteboardItem::new();
            assert!(old.setString_forType(&NSString::from_str("old"), text_type));
            written(&pb, vec![old]);
            let snapshot = pb.pasteboardItems().unwrap();
            let new = NSPasteboardItem::new();
            assert!(new.setString_forType(&NSString::from_str("new"), text_type));
            written(&pb, vec![new]);
            assert!(describe(&snapshot).1.is_none());
            assert_eq!(
                describe(&pb.pasteboardItems().unwrap()).1.as_deref(),
                Some("new")
            );
        });
    }

    #[test]
    fn pasteboard_types_map_to_formats() {
        autoreleasepool(|_| {
            assert_eq!(
                kind_of(unsafe { NSPasteboardTypeTIFF }),
                Some(FormatKind::Image)
            );
            assert_eq!(
                kind_of(unsafe { NSPasteboardTypeHTML }),
                Some(FormatKind::Html)
            );
            assert_eq!(kind_of(unsafe { NSPasteboardTypeFileURL }), None);
        });
    }

    #[test]
    fn ai_text_uses_lf_on_a_native_pasteboard() {
        autoreleasepool(|_| {
            let pb = NSPasteboard::pasteboardWithUniqueName();
            let written = write_text(&pb, "中文\r\nsecond\nthird\r\n");
            let actual = unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| s.to_string());
            let bytes = unsafe { pb.dataForType(NSPasteboardTypeString) }.map(|data| data.to_vec());
            let () = unsafe { objc2::msg_send![&*pb, releaseGlobally] };
            assert!(written);
            let expected = "中文\nsecond\nthird\n";
            assert_eq!(actual.as_deref(), Some(expected));
            assert_eq!(bytes.as_deref(), Some(expected.as_bytes()));
        });
    }
}

/// The pasteboard's text now, for an AI client (`None`: no text).
pub fn text() -> Option<String> {
    autoreleasepool(|_| unsafe { NSPasteboard::generalPasteboard().stringForType(NSPasteboardTypeString) }.map(|s| s.to_string()))
}

/// Puts `t` on the pasteboard as text, for an AI client.
pub fn set_text(t: &str) -> bool {
    autoreleasepool(|_| write_text(&NSPasteboard::generalPasteboard(), t))
}

fn write_text(pb: &NSPasteboard, t: &str) -> bool {
    // A tool argument may contain Windows line endings. The Mac pasteboard uses LF text.
    let text = t.replace("\r\n", "\n");
    pb.clearContents();
    unsafe { pb.setString_forType(&NSString::from_str(&text), NSPasteboardTypeString) }
}
