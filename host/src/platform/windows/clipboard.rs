//! The Windows clipboard for the shared clipboard (`crate::clipboard`), in the agent.
//!
//! One owner window on a thread of its own:
//! - Changes are reported from `WM_CLIPBOARDUPDATE` (`AddClipboardFormatListener`): no polling.
//!   Nothing is read while sharing is off, on the secure desktop, or when the change is this
//!   window's own.
//! - The browser's formats are placeholders (`SetClipboardData(format, NULL)`, delayed
//!   rendering). `WM_RENDERFORMAT` asks the browser through `ClipboardRender` and waits for
//!   its answer up to `RENDER_DEADLINE`; on timeout or cancel the paste gets nothing.
//! - Pictures and files received from the browser are put here too.
//!
//! Formats: `CF_UNICODETEXT`, "HTML Format", "PNG" with `CF_DIB` (Windows makes `CF_DIBV5`
//! and `CF_BITMAP` from it), `CF_HDROP` with the files' paths as text.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};

use windows::core::w;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::clipboard::{self, Answer, Data, Format, Read, RenderWait, INLINE_MAX, RENDER_DEADLINE};
use crate::platform::{Desk, PlatformEvent};
use wire::types::{ClipboardError, FormatKind, Item};
use crate::clipboard::image;
use super::dib;

/// Wakes the owner thread for its commands.
const WM_COMMANDS: u32 = WM_APP + 1;

enum Cmd {
    Announce(u64, Vec<Format>),
    Withdraw,
    Read(FormatKind, Sender<Result<Raw, ClipboardError>>),
    Put(Vec<(u32, Vec<u8>)>, Sender<bool>),
}

/// A format as read on the owner thread; converted (and files read) off it.
enum Raw {
    Text(String),
    Html(String),
    Png(Vec<u8>),
    Dib(Vec<u8>),
    Paths(Vec<PathBuf>),
}

pub struct Clipboard {
    commands: Sender<Cmd>,
    answers: Sender<Answer>,
    /// For answers when the owner window cannot be reached.
    events: Sender<PlatformEvent>,
    /// The owner window (0 if it could not be made).
    window: AtomicIsize,
}

/// What the owner thread knows; its window procedure reaches it through `OWNER`.
struct Owner {
    events: Sender<PlatformEvent>,
    commands: Receiver<Cmd>,
    wait: RenderWait,
    /// The clipboard's sequence number when it was last reported: a read finds it changed.
    reported: u32,
    /// Data already rendered for the announced generation, for its other formats.
    cache: Vec<Data>,
    /// Kinds of the announced generation the browser did not provide. Windows asks again
    /// within the same paste (through formats it synthesizes); the answer stays nothing
    /// until the browser copies anew.
    refused: Vec<FormatKind>,
}

thread_local! {
    static OWNER: RefCell<Option<Owner>> = const { RefCell::new(None) };
}

// One clipboard per process, as there is one OS clipboard.
/// Sharing is on and a session runs.
static ON: AtomicBool = AtomicBool::new(false);
/// The generation whose placeholders are on the clipboard (0: none).
static ANNOUNCED: AtomicU64 = AtomicU64::new(0);

fn registered(name: windows::core::PCWSTR) -> u32 {
    unsafe { RegisterClipboardFormatW(name) }
}

fn cf_png() -> u32 {
    registered(w!("PNG"))
}

fn cf_html() -> u32 {
    registered(w!("HTML Format"))
}

/// The Windows formats that hold one kind, in the order they are offered.
pub fn formats_of(kind: FormatKind) -> Vec<u32> {
    match kind {
        FormatKind::Text => vec![CF_UNICODETEXT.0 as u32],
        FormatKind::Html => vec![cf_html()],
        FormatKind::Image => vec![cf_png(), CF_DIB.0 as u32],
        FormatKind::Files => vec![CF_HDROP.0 as u32],
    }
}

fn kind_of(format: u32) -> Option<FormatKind> {
    [FormatKind::Text, FormatKind::Html, FormatKind::Image].into_iter().find(|k| formats_of(*k).contains(&format) || (*k == FormatKind::Image && format == CF_DIBV5.0 as u32))
}

/// UTF-16 with its terminating NUL, as `CF_UNICODETEXT` holds text.
pub fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// Text from `CF_UNICODETEXT`, up to its NUL.
pub fn from_utf16z(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// The bytes a Windows format holds for this data, if it can hold it.
pub fn render_bytes(format: u32, data: &Data) -> Option<Vec<u8>> {
    match data {
        Data::Text(t) if format == CF_UNICODETEXT.0 as u32 => Some(utf16z(t)),
        Data::Html(h) if format == cf_html() => Some(cf_html_wrap(h)),
        Data::Image(png) if format == cf_png() => Some(png.clone()),
        Data::Image(png) if format == CF_DIB.0 as u32 || format == CF_DIBV5.0 as u32 => {
            let (w, h, rgba) = image::png_decode(png)?;
            Some(dib::rgba_to_dib(w, h, &rgba))
        }
        _ => None,
    }
}

/// HTML as the Windows "HTML Format" holds it: a header of byte offsets, then the document
/// with the fragment between markers.
pub fn cf_html_wrap(fragment: &str) -> Vec<u8> {
    const HEAD: &str = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let pre = "<html><body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body></html>";
    let start_html = HEAD.len();
    let start_frag = start_html + pre.len();
    let end_frag = start_frag + fragment.len();
    let end_html = end_frag + post.len();
    let head = format!("Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_frag:010}\r\nEndFragment:{end_frag:010}\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(pre.as_bytes());
    out.extend_from_slice(fragment.as_bytes());
    out.extend_from_slice(post.as_bytes());
    out
}

/// The fragment of an "HTML Format" block, by its offsets; the whole HTML part when the
/// fragment offsets are missing.
pub fn cf_html_fragment(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data.split(|&b| b == 0).next().unwrap_or(&[]));
    let offset = |name: &str| -> Option<usize> {
        let at = text.find(&format!("{name}:"))? + name.len() + 1;
        text[at..].lines().next()?.trim().parse::<i64>().ok().filter(|n| *n >= 0).map(|n| n as usize)
    };
    let bytes = text.as_bytes();
    let (start, end) = match (offset("StartFragment"), offset("EndFragment")) {
        (Some(s), Some(e)) => (s, e),
        _ => (offset("StartHTML")?, offset("EndHTML").unwrap_or(bytes.len())),
    };
    let end = end.min(bytes.len());
    (start <= end).then(|| String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

/// DROPFILES (offset of the list, a point, two flags; wide characters) and the paths.
pub fn dropfiles(paths: &[PathBuf]) -> Vec<u8> {
    let mut d: Vec<u8> = Vec::new();
    for v in [20u32, 0, 0, 0, 1] {
        d.extend_from_slice(&v.to_le_bytes());
    }
    for p in paths {
        for u in p.as_os_str().to_string_lossy().encode_utf16().chain([0]) {
            d.extend_from_slice(&u.to_le_bytes());
        }
    }
    d.extend_from_slice(&[0, 0]);
    d
}

fn secure_desktop() -> bool {
    super::desktop::input() != Desk::Default
}

fn open(hwnd: HWND) -> bool {
    // Another program may hold it for a moment.
    for _ in 0..5 {
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

/// Copies a global memory block. The clipboard must be open.
fn get(format: u32) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(format).ok()?;
        let g = HGLOBAL(h.0);
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let out = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(out)
    }
}

/// Puts data in a new global block on the clipboard (open, or rendering).
fn set(format: u32, data: &[u8]) -> bool {
    unsafe {
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)) else { return false };
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(g));
            return false;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = GlobalUnlock(g);
        // On success the system owns the memory.
        if SetClipboardData(format, Some(HANDLE(g.0))).is_err() {
            let _ = GlobalFree(Some(g));
            return false;
        }
        true
    }
}

fn available(format: u32) -> bool {
    unsafe { IsClipboardFormatAvailable(format).is_ok() }
}

/// The files of a `CF_HDROP`. The clipboard must be open.
fn dropped_paths() -> Vec<PathBuf> {
    unsafe {
        let Ok(h) = GetClipboardData(CF_HDROP.0 as u32) else { return Vec::new() };
        let drop = HDROP(h.0);
        let n = DragQueryFileW(drop, u32::MAX, None);
        (0..n)
            .filter_map(|i| {
                let len = DragQueryFileW(drop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(drop, i, Some(&mut buf));
                (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
            })
            .collect()
    }
}

/// The first bytes of a block, without copying the rest (a bitmap can be large).
fn peek(format: u32, n: usize) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(format).ok()?;
        let g = HGLOBAL(h.0);
        let size = GlobalSize(g).min(n);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let out = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Some(out)
    }
}

fn is_mine(hwnd: HWND) -> bool {
    unsafe { GetClipboardOwner() }.is_ok_and(|o| o == hwnd)
}

/// What is on the clipboard now: its formats, small text itself, and its sequence number.
fn describe(hwnd: HWND) -> Option<(Vec<Format>, Option<String>, u32)> {
    if !open(hwnd) {
        return None;
    }
    let seq = unsafe { GetClipboardSequenceNumber() };
    let mut formats = Vec::new();
    let mut text = None;
    let paths = if available(CF_HDROP.0 as u32) { dropped_paths() } else { Vec::new() };
    if available(CF_UNICODETEXT.0 as u32) {
        if let Some(t) = get(CF_UNICODETEXT.0 as u32).map(|b| from_utf16z(&b)) {
            formats.push(Format { size: Some(t.len() as u64), ..Format::of(FormatKind::Text) });
            text = Some(t).filter(|t| t.len() <= INLINE_MAX);
        }
    }
    if available(cf_html()) {
        let size = peek(cf_html(), usize::MAX).map(|b| b.len() as u64);
        formats.push(Format { size, ..Format::of(FormatKind::Html) });
    }
    if available(cf_png()) {
        if let Some(png) = get(cf_png()) {
            let (w, h) = image::png_size(&png).unzip();
            formats.push(Format { size: Some(png.len() as u64), w, h, ..Format::of(FormatKind::Image) });
        }
    } else if available(CF_DIB.0 as u32) || available(CF_DIBV5.0 as u32) {
        let (w, h) = peek(CF_DIB.0 as u32, 16).and_then(|b| dib::dib_size(&b)).unzip();
        formats.push(Format { w, h, ..Format::of(FormatKind::Image) });
    }
    unsafe {
        let _ = CloseClipboard();
    }
    if !paths.is_empty() {
        // Folders are not sent: only the files among them are listed.
        let items: Vec<Item> = paths
            .iter()
            .filter_map(|p| {
                let m = super::user::as_user(|| std::fs::metadata(p)).ok().filter(|m| m.is_file())?;
                Some(Item { n: p.file_name()?.to_string_lossy().into_owned(), s: m.len() })
            })
            .collect();
        formats.push(Format { items: Some(items), ..Format::of(FormatKind::Files) });
        // Files also as their full paths, for targets that take text only.
        if !formats.iter().any(|f| f.kind == FormatKind::Text) {
            let t = clipboard::paths_text(&paths);
            formats.push(Format { size: Some(t.len() as u64), ..Format::of(FormatKind::Text) });
            text = Some(t).filter(|t| t.len() <= INLINE_MAX);
        }
    }
    Some((formats, text, seq))
}

fn on_update(hwnd: HWND) {
    // Never read while sharing is off, from the lock screen or a secure prompt, or what this
    // window put there itself.
    if !ON.load(Ordering::SeqCst) || is_mine(hwnd) || secure_desktop() {
        return;
    }
    let Some((formats, text, seq)) = describe(hwnd) else { return };
    OWNER.with(|o| {
        if let Some(o) = o.borrow_mut().as_mut() {
            o.reported = seq;
            let _ = o.events.send(PlatformEvent::ClipboardChanged { formats, text });
        }
    });
}

/// Something pastes from a placeholder: the clipboard is open in that program; this only
/// sets the data (or nothing) before returning.
fn on_render(format: u32) {
    let gen = ANNOUNCED.load(Ordering::SeqCst);
    let Some(kind) = kind_of(format).filter(|_| gen != 0) else { return };
    let (cached, refused) = OWNER.with(|o| match o.borrow().as_ref() {
        Some(o) => (o.cache.iter().find(|d| d.kind() == kind).cloned(), o.refused.contains(&kind)),
        None => (None, true),
    });
    if refused {
        return;
    }
    let data = cached.or_else(|| {
        OWNER.with(|o| {
            let o = o.borrow();
            let o = o.as_ref()?;
            o.events.send(PlatformEvent::ClipboardRender { gen, format: kind }).ok()?;
            o.wait.wait(gen, kind, RENDER_DEADLINE)
        })
    });
    let Some(data) = data else {
        OWNER.with(|o| {
            if let Some(o) = o.borrow_mut().as_mut() {
                o.refused.push(kind);
            }
        });
        return;
    };
    if let Some(bytes) = render_bytes(format, &data) {
        set(format, &bytes);
    }
    OWNER.with(|o| {
        if let Some(o) = o.borrow_mut().as_mut() {
            if !o.cache.contains(&data) {
                o.cache.push(data);
            }
        }
    });
}

fn forget_placeholders() {
    ANNOUNCED.store(0, Ordering::SeqCst);
    OWNER.with(|o| {
        if let Some(o) = o.borrow_mut().as_mut() {
            o.cache.clear();
            o.refused.clear();
        }
    });
}

fn run_commands(hwnd: HWND) {
    loop {
        let Some(cmd) = OWNER.with(|o| o.borrow().as_ref().and_then(|o| o.commands.try_recv().ok())) else { return };
        match cmd {
            Cmd::Announce(gen, formats) => {
                // Nothing goes onto the clipboard of the lock screen or a secure prompt.
                let why = if !ON.load(Ordering::SeqCst) || secure_desktop() {
                    Some(ClipboardError::Denied)
                } else if !open(hwnd) {
                    Some(ClipboardError::Io)
                } else {
                    None
                };
                if why.is_some() {
                    announced(gen, why);
                    continue;
                }
                unsafe {
                    let _ = EmptyClipboard();
                }
                forget_placeholders();
                for f in &formats {
                    for format in formats_of(f.kind) {
                        // NULL: rendered when something pastes.
                        let _ = unsafe { SetClipboardData(format, None) };
                    }
                }
                // Clipboard history and other monitors would otherwise render every
                // placeholder at once: the browser's data moves only when something pastes.
                set(registered(w!("ExcludeClipboardContentFromMonitorProcessing")), &[0]);
                set(registered(w!("CanIncludeInClipboardHistory")), &0u32.to_le_bytes());
                set(registered(w!("CanUploadToCloudClipboard")), &0u32.to_le_bytes());
                unsafe {
                    let _ = CloseClipboard();
                }
                ANNOUNCED.store(gen, Ordering::SeqCst);
                announced(gen, None);
            }
            Cmd::Withdraw => {
                if ANNOUNCED.load(Ordering::SeqCst) != 0 && is_mine(hwnd) && open(hwnd) {
                    unsafe {
                        let _ = EmptyClipboard();
                        let _ = CloseClipboard();
                    }
                }
                forget_placeholders();
            }
            Cmd::Read(kind, reply) => {
                let _ = reply.send(read_raw(hwnd, kind));
            }
            Cmd::Put(items, reply) => {
                let ok = !secure_desktop() && open(hwnd) && unsafe {
                    let _ = EmptyClipboard();
                    let ok = items.iter().all(|(f, d)| set(*f, d));
                    let _ = CloseClipboard();
                    ok
                };
                forget_placeholders();
                let _ = reply.send(ok);
            }
        }
    }
}

/// Tells the session whether the browser's placeholders are in place.
fn announced(gen: u64, why: Option<ClipboardError>) {
    OWNER.with(|o| {
        if let Some(o) = o.borrow().as_ref() {
            let _ = o.events.send(PlatformEvent::ClipboardAnnounced { gen, why });
        }
    });
}

fn read_raw(hwnd: HWND, kind: FormatKind) -> Result<Raw, ClipboardError> {
    if !ON.load(Ordering::SeqCst) || secure_desktop() {
        return Err(ClipboardError::Denied);
    }
    if !open(hwnd) {
        return Err(ClipboardError::Io);
    }
    let reported = OWNER.with(|o| o.borrow().as_ref().map(|o| o.reported));
    let raw = if Some(unsafe { GetClipboardSequenceNumber() }) != reported {
        Err(ClipboardError::Gone)
    } else {
        match kind {
            FormatKind::Text if available(CF_UNICODETEXT.0 as u32) => get(CF_UNICODETEXT.0 as u32).map(|b| Raw::Text(from_utf16z(&b))),
            FormatKind::Text if available(CF_HDROP.0 as u32) => Some(Raw::Text(clipboard::paths_text(&dropped_paths()))),
            FormatKind::Html => get(cf_html()).and_then(|b| cf_html_fragment(&b)).map(Raw::Html),
            FormatKind::Image if available(cf_png()) => get(cf_png()).map(Raw::Png),
            FormatKind::Image => get(CF_DIB.0 as u32).map(Raw::Dib),
            FormatKind::Files => Some(dropped_paths()).filter(|p| !p.is_empty()).map(Raw::Paths),
            FormatKind::Text => None,
        }
        .ok_or(ClipboardError::Unsupported)
    };
    unsafe {
        let _ = CloseClipboard();
    }
    raw
}

unsafe extern "system" fn owner_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => on_update(hwnd),
        WM_RENDERFORMAT => on_render(w.0 as u32),
        // This window is going away while it owns placeholders: the browser can no longer
        // answer, so the formats are left unrendered and disappear.
        WM_RENDERALLFORMATS => {}
        // Another program emptied the clipboard: the placeholders are gone.
        WM_DESTROYCLIPBOARD => forget_placeholders(),
        WM_COMMANDS => run_commands(hwnd),
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        _ => return unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
    LRESULT(0)
}

impl Clipboard {
    /// Starts the owner window's thread. Changes and render requests go to `events`.
    pub fn start(events: Sender<PlatformEvent>) -> Clipboard {
        let (commands, commands_rx) = channel();
        let (answers, wait) = RenderWait::new();
        let (made_tx, made) = channel();
        let for_answers = events.clone();
        let started = std::thread::Builder::new().name("clipboard".into()).spawn(move || unsafe {
            let class = w!("DeskwayClipboard");
            let instance = GetModuleHandleW(None).ok();
            let wc = WNDCLASSW { lpfnWndProc: Some(owner_proc), hInstance: instance.map(Into::into).unwrap_or_default(), lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc); // already registered is fine
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, instance.map(Into::into), None);
            let Ok(hwnd) = hwnd else {
                let _ = made_tx.send(0);
                return;
            };
            if let Err(e) = AddClipboardFormatListener(hwnd) {
                eprintln!("clipboard changes cannot be followed: {e}");
            }
            OWNER.with(|o| *o.borrow_mut() = Some(Owner { events, commands: commands_rx, wait, reported: 0, cache: Vec::new(), refused: Vec::new() }));
            let _ = made_tx.send(hwnd.0 as isize);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
        });
        let window = started.ok().and_then(|_| made.recv().ok()).unwrap_or(0);
        if window == 0 {
            eprintln!("no clipboard window: the shared clipboard is unavailable");
        }
        Clipboard { commands, answers, events: for_answers, window: AtomicIsize::new(window) }
    }

    fn command(&self, cmd: Cmd) -> bool {
        let window = self.window.load(Ordering::SeqCst);
        window != 0 && self.commands.send(cmd).is_ok() && unsafe { PostMessageW(Some(HWND(window as _)), WM_COMMANDS, WPARAM(0), LPARAM(0)) }.is_ok()
    }

    fn put(&self, items: Vec<(u32, Vec<u8>)>) -> bool {
        let (tx, rx) = channel();
        self.command(Cmd::Put(items, tx)) && rx.recv().unwrap_or(false)
    }
}

impl Drop for Clipboard {
    /// The window goes (and with it the listener); its thread ends.
    fn drop(&mut self) {
        let window = self.window.load(Ordering::SeqCst);
        if window != 0 {
            let _ = unsafe { PostMessageW(Some(HWND(window as _)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
    }
}

impl clipboard::Native for Clipboard {
    fn share(&self, on: bool) {
        ON.store(on, Ordering::SeqCst);
        if !on {
            // A paste waiting for the browser gets nothing now.
            let _ = self.answers.send(Answer { gen: ANNOUNCED.load(Ordering::SeqCst), data: None });
            self.command(Cmd::Withdraw);
        }
    }

    fn announce(&self, gen: u64, formats: Vec<Format>) {
        if !self.command(Cmd::Announce(gen, formats)) {
            let _ = self.events.send(PlatformEvent::ClipboardAnnounced { gen, why: Some(ClipboardError::Io) });
        }
    }

    fn provide(&self, gen: u64, data: Option<Data>) {
        let _ = self.answers.send(Answer { gen, data });
    }

    fn read(&self, kind: FormatKind, i: Option<u32>) -> Result<Read, ClipboardError> {
        let (tx, rx) = channel();
        if !self.command(Cmd::Read(kind, tx)) {
            return Err(ClipboardError::Io);
        }
        match rx.recv().map_err(|_| ClipboardError::Io)?? {
            Raw::Text(t) => Ok(Read::text(FormatKind::Text, t)),
            Raw::Html(h) => Ok(Read::text(FormatKind::Html, h)),
            Raw::Png(png) => Ok(Read::Bytes { name: "image.png".into(), mime: "image/png".into(), data: png }),
            Raw::Dib(dib) => {
                let (w, h, rgba) = dib::dib_to_rgba(&dib).ok_or(ClipboardError::Unsupported)?;
                let png = image::png_encode(w, h, &rgba).ok_or(ClipboardError::Io)?;
                Ok(Read::Bytes { name: "image.png".into(), mime: "image/png".into(), data: png })
            }
            Raw::Paths(paths) => {
                let path = paths.get(i.unwrap_or(0) as usize).ok_or(ClipboardError::Gone)?;
                // Read as the person signed in, never as SYSTEM.
                super::user::as_user(|| Ok(Read::file(path))).unwrap_or(Err(ClipboardError::Io))
            }
        }
    }

    fn put_image(&self, png: &[u8]) -> bool {
        let Some((w, h, rgba)) = image::png_decode(png) else { return false };
        self.put(vec![(cf_png(), png.to_vec()), (CF_DIB.0 as u32, dib::rgba_to_dib(w, h, &rgba))])
    }

    fn put_files(&self, paths: &[PathBuf]) -> bool {
        let effect = registered(w!("Preferred DropEffect"));
        self.put(vec![(CF_HDROP.0 as u32, dropfiles(paths)), (effect, 1u32.to_le_bytes().to_vec()), (CF_UNICODETEXT.0 as u32, utf16z(&clipboard::paths_text(paths)))])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::Native;
    use std::time::Duration;

    #[test]
    fn text_for_windows_has_crlf_line_ends() {
        for (given, want) in [("a\nb", "a\r\nb"), ("a\r\nb\nc", "a\r\nb\r\nc"), ("one line", "one line"), ("\n", "\r\n")] {
            assert_eq!(crlf(given), want, "{given:?}");
        }
    }

    #[test]
    fn html_format_round_trips_with_exact_offsets() {
        let frag = "<b>粗体</b> &amp; text";
        let block = cf_html_wrap(frag);
        let s = String::from_utf8(block.clone()).unwrap();
        assert!(s.starts_with("Version:0.9\r\nStartHTML:"));
        assert_eq!(cf_html_fragment(&block).as_deref(), Some(frag));
        // The offsets are bytes, not characters.
        let start: usize = s[s.find("StartFragment:").unwrap() + 14..][..10].parse().unwrap();
        assert_eq!(&block[start..start + 3], b"<b>");
        // Trailing NULs (as some programs write) and missing fragment markers.
        let mut nul = block.clone();
        nul.extend_from_slice(&[0, 0]);
        assert_eq!(cf_html_fragment(&nul).as_deref(), Some(frag));
        let only_html = b"Version:0.9\r\nStartHTML:0000000055\r\nEndHTML:0000000066\r\n<p>hi</p>\r\n";
        assert_eq!(cf_html_fragment(only_html).as_deref(), Some("<p>hi</p>\r\n"));
        assert_eq!(cf_html_fragment(b"no header"), None);
    }

    #[test]
    fn formats_convert_both_ways() {
        assert_eq!(from_utf16z(&utf16z("héllo 你好")), "héllo 你好");
        assert_eq!(from_utf16z(&[104, 0, 105, 0, 0, 0, 120, 0]), "hi", "up to the NUL");
        assert_eq!(render_bytes(CF_UNICODETEXT.0 as u32, &Data::Text("a".into())), Some(vec![97, 0, 0, 0]));
        let html = render_bytes(cf_html(), &Data::Html("<b>x</b>".into())).unwrap();
        assert_eq!(cf_html_fragment(&html).as_deref(), Some("<b>x</b>"));
        let png = image::png_encode(2, 1, &[1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        assert_eq!(render_bytes(cf_png(), &Data::Image(png.clone())), Some(png.clone()));
        let dib = render_bytes(CF_DIB.0 as u32, &Data::Image(png)).unwrap();
        assert_eq!(dib::dib_to_rgba(&dib), Some((2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])));
        assert_eq!(render_bytes(CF_UNICODETEXT.0 as u32, &Data::Html("x".into())), None, "a format holds its own kind only");
        assert_eq!([kind_of(CF_UNICODETEXT.0 as u32), kind_of(cf_html()), kind_of(CF_DIBV5.0 as u32), kind_of(CF_HDROP.0 as u32)], [Some(FormatKind::Text), Some(FormatKind::Html), Some(FormatKind::Image), None]);
        let d = dropfiles(&[PathBuf::from(r"C:\a.txt")]);
        assert_eq!(&d[..4], &20u32.to_le_bytes());
        assert_eq!(from_utf16z(&d[20..]), r"C:\a.txt");
        assert!(d.ends_with(&[0, 0, 0, 0]), "the list ends with an empty name");
    }

    // ---- the real clipboard of this computer ----
    //
    // These use the clipboard of whoever runs them. They save what is on it and put it back
    // afterwards, but a copy made meanwhile by the person at the computer can be lost, so they
    // are opt-in:
    //   cd host; $env:CARGO_TARGET_DIR='target/dev'; cargo test --release real_clipboard -- --ignored --test-threads=1

    /// The clipboard as it was: every format held in global memory, put back on drop.
    struct Saved(Vec<(u32, Vec<u8>)>);

    impl Saved {
        fn take() -> Saved {
            let hwnd = test_window();
            let mut all = Vec::new();
            if open(hwnd) {
                let mut f = 0;
                loop {
                    f = unsafe { EnumClipboardFormats(f) };
                    if f == 0 {
                        break;
                    }
                    // Bitmaps, metafiles and palettes are GDI handles: Windows remakes them from the DIB.
                    if [2u32, 3, 9, 14].contains(&f) {
                        continue;
                    }
                    if let Some(d) = get(f) {
                        all.push((f, d));
                    }
                }
                unsafe {
                    let _ = CloseClipboard();
                }
            }
            // A picture is offered both as CF_DIB and CF_DIBV5, one made from the other. Only
            // CF_DIB goes back and Windows makes the rest from it: with both, the bitmap it
            // makes comes back shifted by the 12 bytes of bit-field masks (checked by pixels).
            if all.iter().any(|(f, _)| *f == CF_DIB.0 as u32) {
                all.retain(|(f, _)| *f != CF_DIBV5.0 as u32);
            }
            Saved(all)
        }
    }

    impl Drop for Saved {
        fn drop(&mut self) {
            if open(test_window()) {
                unsafe {
                    let _ = EmptyClipboard();
                }
                for (f, d) in &self.0 {
                    set(*f, d);
                }
                unsafe {
                    let _ = CloseClipboard();
                }
            }
        }
    }

    /// A window of its own for the test thread, as another program would have.
    fn test_window() -> HWND {
        thread_local!(static W: HWND = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None).unwrap() });
        W.with(|w| *w)
    }

    /// Another program copies.
    fn copy(items: &[(u32, Vec<u8>)]) {
        assert!(open(test_window()));
        unsafe {
            let _ = EmptyClipboard();
        }
        for (f, d) in items {
            assert!(set(*f, d));
        }
        unsafe {
            let _ = CloseClipboard();
        }
    }

    /// Another program pastes: the data of `format`, rendered if need be.
    fn paste(format: u32) -> Option<Vec<u8>> {
        assert!(open(test_window()));
        let d = get(format);
        unsafe {
            let _ = CloseClipboard();
        }
        d
    }

    /// The latest change reported. Programs that follow the clipboard (WSLg's msrdc) may copy
    /// it once more right after; the last report is the one a read must match.
    fn next(events: &Receiver<PlatformEvent>) -> PlatformEvent {
        let mut e = events.recv_timeout(Duration::from_secs(5)).expect("an event");
        while let Ok(later) = events.recv_timeout(Duration::from_millis(300)) {
            e = later;
        }
        e
    }

    #[test]
    #[ignore = "uses this computer's clipboard; see the comment above"]
    fn real_clipboard_copy_here_is_reported_and_read() {
        let _saved = Saved::take();
        let (tx, events) = channel();
        let c = Clipboard::start(tx);
        copy(&[(CF_UNICODETEXT.0 as u32, utf16z("not shared"))]);
        std::thread::sleep(Duration::from_millis(300));
        assert!(events.try_recv().is_err(), "sharing off: nothing is read");
        c.share(true);
        copy(&[(CF_UNICODETEXT.0 as u32, utf16z("deskway test 1")), (cf_html(), cf_html_wrap("<b>deskway</b>"))]);
        let PlatformEvent::ClipboardChanged { formats, text } = next(&events) else { panic!() };
        assert_eq!(text.as_deref(), Some("deskway test 1"));
        assert_eq!(formats.iter().map(|f| f.kind).collect::<Vec<_>>(), vec![FormatKind::Text, FormatKind::Html]);
        assert_eq!(c.read(FormatKind::Html, None), Ok(Read::Html("<b>deskway</b>".into())));
        // Files, with their paths as text.
        let dir = std::env::temp_dir().join(format!("deskway-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.txt");
        std::fs::write(&file, b"file body").unwrap();
        copy(&[(CF_HDROP.0 as u32, dropfiles(std::slice::from_ref(&file)))]);
        let PlatformEvent::ClipboardChanged { formats, text } = next(&events) else { panic!() };
        assert_eq!(formats[0].items, Some(vec![Item { n: "a b.txt".into(), s: 9 }]));
        assert_eq!(text, Some(file.to_string_lossy().into_owned()));
        assert_eq!(c.read(FormatKind::Files, Some(0)), Ok(Read::Bytes { name: "a b.txt".into(), mime: "application/octet-stream".into(), data: b"file body".to_vec() }));
        // A screenshot-like bitmap is offered and read as PNG.
        copy(&[(CF_DIB.0 as u32, dib::rgba_to_dib(2, 1, &[9, 8, 7, 255, 1, 2, 3, 255]))]);
        let PlatformEvent::ClipboardChanged { formats, .. } = next(&events) else { panic!() };
        assert_eq!((formats[0].kind, formats[0].w, formats[0].h), (FormatKind::Image, Some(2), Some(1)));
        let Ok(Read::Bytes { data, .. }) = c.read(FormatKind::Image, None) else { panic!() };
        assert_eq!(image::png_decode(&data), Some((2, 1, vec![9, 8, 7, 255, 1, 2, 3, 255])));
        // Sharing off: nothing is read.
        c.share(false);
        assert_eq!(c.read(FormatKind::Text, None), Err(ClipboardError::Denied));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[ignore = "uses this computer's clipboard; see the comment above"]
    fn real_clipboard_placeholders_are_rendered_on_paste() {
        let _saved = Saved::take();
        let (tx, events) = channel();
        let c = std::sync::Arc::new(Clipboard::start(tx));
        c.share(true);
        // The browser answers each render request: text, then a cancelled one, then a picture.
        let png = image::png_encode(1, 1, &[10, 20, 30, 255]).unwrap();
        let (placed_tx, placed) = channel();
        let answer = {
            let (c, png) = (c.clone(), png.clone());
            std::thread::spawn(move || {
                let mut asked = Vec::new();
                while let Ok(e) = events.recv_timeout(Duration::from_secs(10)) {
                    if let PlatformEvent::ClipboardAnnounced { gen, why } = e {
                        let _ = placed_tx.send((gen, why));
                    } else if let PlatformEvent::ClipboardRender { gen, format } = e {
                        asked.push((gen, format));
                        c.provide(gen, match gen {
                            5 => Some(Data::Text("from the browser".into())),
                            6 => None,
                            _ => Some(Data::Image(png.clone())),
                        });
                    }
                }
                asked
            })
        };
        c.announce(5, vec![Format::of(FormatKind::Text)]);
        assert_eq!(placed.recv_timeout(Duration::from_secs(5)).unwrap(), (5, None), "in place before anything pastes");
        assert_eq!(paste(CF_UNICODETEXT.0 as u32).map(|b| from_utf16z(&b)).as_deref(), Some("from the browser"));
        assert_eq!(paste(CF_UNICODETEXT.0 as u32).map(|b| from_utf16z(&b)).as_deref(), Some("from the browser"), "rendered once, then kept");
        c.announce(6, vec![Format::of(FormatKind::Text)]);
        assert_eq!(placed.recv_timeout(Duration::from_secs(5)).unwrap(), (6, None), "in place before anything pastes");
        assert_eq!(paste(CF_UNICODETEXT.0 as u32), None, "cancelled: nothing is pasted");
        assert_eq!(paste(CF_UNICODETEXT.0 as u32), None, "…and the browser is not asked again for it");
        c.announce(7, vec![Format::of(FormatKind::Image)]);
        assert_eq!(placed.recv_timeout(Duration::from_secs(5)).unwrap(), (7, None), "in place before anything pastes");
        let dib = paste(CF_DIB.0 as u32).unwrap();
        assert_eq!(dib::dib_to_rgba(&dib).map(|x| x.2), Some(vec![10, 20, 30, 255]));
        assert_eq!(paste(cf_png()), Some(png), "the other picture format from the same answer");
        // Sharing off takes the placeholders away.
        c.announce(8, vec![Format::of(FormatKind::Text)]);
        assert_eq!(placed.recv_timeout(Duration::from_secs(5)).unwrap(), (8, None), "in place before anything pastes");
        c.share(false);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!available(CF_UNICODETEXT.0 as u32));
        // Sharing off: an announcement is refused, explicitly.
        c.announce(9, vec![Format::of(FormatKind::Text)]);
        assert_eq!(placed.recv_timeout(Duration::from_secs(5)).unwrap(), (9, Some(ClipboardError::Denied)));
        // Received files: as files and as their paths.
        assert!(c.put_files(&[PathBuf::from(r"C:\x\y.txt")]));
        assert_eq!(paste(CF_UNICODETEXT.0 as u32).map(|b| from_utf16z(&b)).as_deref(), Some(r"C:\x\y.txt"));
        assert!(paste(CF_HDROP.0 as u32).is_some());
        drop(c);
        // Each paste asked once. Programs that read every copy at once despite the exclusion
        // formats (WSLg's msrdc does) may also have rendered 8 before sharing went off.
        let asked: Vec<_> = answer.join().unwrap().into_iter().filter(|a| a.0 != 8).collect();
        assert_eq!(asked, vec![(5, FormatKind::Text), (6, FormatKind::Text), (7, FormatKind::Image)]);
    }
}

/// The clipboard's text now, for an AI client (`None`: no text, or another program holds it).
pub fn text() -> Option<String> {
    with_window(|hwnd| {
        if !open(hwnd) {
            return None;
        }
        // Lines end in \n, as an AI client writes them (`set_text` makes them CRLF again).
        let t = if available(CF_UNICODETEXT.0 as u32) { get(CF_UNICODETEXT.0 as u32).map(|b| from_utf16z(&b).replace("\r\n", "\n")) } else { None };
        unsafe {
            let _ = CloseClipboard();
        }
        t
    })
}

/// Lines as Windows programs take them: every line end CRLF. A lone LF (what an AI client
/// writes) would run the lines together in a Windows text box.
pub fn crlf(t: &str) -> String {
    t.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Puts `t` on the clipboard as text, for an AI client, with Windows line ends.
pub fn set_text(t: &str) -> bool {
    with_window(|hwnd| {
        if !open(hwnd) {
            return false;
        }
        let ok = unsafe { EmptyClipboard() }.is_ok() && set(CF_UNICODETEXT.0 as u32, &utf16z(&crlf(t)));
        unsafe {
            let _ = CloseClipboard();
        }
        ok
    })
}

/// Runs `f` with a message-only window of its own: the clipboard needs an owner window for
/// what is put on it.
fn with_window<T>(f: impl FnOnce(HWND) -> T) -> T {
    let hwnd = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("Deskway clipboard"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None) }.unwrap_or_default();
    let out = f(hwnd);
    if !hwnd.is_invalid() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    out
}
