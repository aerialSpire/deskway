//! The capture loop: encoding policy, thumbnails and the pointer, on top of a native
//! capturer and encoder. Frames exist only in memory and are never written to disk.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::display::DisplayInfo;
use crate::platform::{Bytes, Desk, PlatformEvent};

pub mod size;
mod tiles;
pub use size::{Constraints, FPS};
pub(crate) const SHARP_FPS: u32 = 2;

/// Where bandwidth estimation starts; the estimate takes over from there.
pub const START_BITRATE: u32 = 6_000_000;
const THUMB_WIDTH: usize = 480;
const THUMB_EVERY: Duration = Duration::from_secs(3);
/// After the screen stops changing the last image is encoded a few more times.
/// Each pass refines it, then output stops completely so a static screen costs nothing.
const REFINE_PASSES: u32 = 6;
const REFINE_EVERY: Duration = Duration::from_millis(120);

/// Keyframes come only when the browser asks (it lost something). Several requests in a
/// burst are served by one keyframe, since each costs many times a normal frame.
const KEYFRAME_GAP: Duration = Duration::from_millis(500);
/// How often the pointer position is reported to a browser that draws the pointer itself.
const CURSOR_EVERY: Duration = Duration::from_millis(33);
/// Pointer shapes above this size are scaled down before they are sent.
pub(crate) const CURSOR_MAX: usize = 64;
/// How often the input desktop (normal, lock screen, UAC) is checked.
const DESK_EVERY: Duration = Duration::from_millis(500);

/// No display is attached: capture is unavailable rather than failing.
#[derive(Debug)]
pub struct NoDisplay;

impl std::fmt::Display for NoDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no capturable display")
    }
}

impl std::error::Error for NoDisplay {}

#[derive(Default)]
pub struct CaptureChanges {
    pub image: bool,
    pub pointer: bool,
    pub shape: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorShape {
    pub w: usize,
    pub h: usize,
    pub hx: i32,
    pub hy: i32,
    #[serde(with = "crate::platform::raw_bytes")]
    pub rgba: Vec<u8>,
}

/// Owns the latest image and the native data needed to reuse it on a static screen.
pub trait Capturer: Sized {
    type Frame: ?Sized;
    type Context: Default;

    const CURSOR_IN_STREAM: bool = false;

    /// Fails with [`NoDisplay`] when no display is attached.
    fn open(wanted: Option<&str>, draw_cursor: bool) -> anyhow::Result<Self>;
    /// Opens a capture that can return original RGB pixels for lossless updates.
    fn open_rgb(wanted: Option<&str>, draw_cursor: bool) -> anyhow::Result<Self> {
        Self::open(wanted, draw_cursor)
    }
    fn display(&self) -> &DisplayInfo;
    /// Every display attached now, each with its stable id.
    fn displays() -> Vec<DisplayInfo>;
    /// The captured image's size in pixels.
    fn pixel_size(&self) -> (usize, usize);
    fn wait(&mut self, context: &mut Self::Context, timeout: Duration) -> anyhow::Result<CaptureChanges>;
    fn has_frame(&self) -> bool;
    /// A still of `display` (this capture's or another), at most `max_width` wide, given to
    /// `done` now or from a thread of its own; `None` when there is nothing to see (a secure
    /// desktop through a path that cannot read it). Previews and AI clients' screenshots.
    fn snapshot(&mut self, display: &DisplayInfo, max_width: usize, done: Done);
    /// The latest image at `size` (even, at most `pixel_size`).
    fn frame(&mut self, context: &Self::Context, draw_cursor: bool, size: (usize, usize)) -> anyhow::Result<&Self::Frame>;
    fn rgb(&mut self, _context: &Self::Context, _draw_cursor: bool) -> anyhow::Result<Still> {
        anyhow::bail!("RGB capture unavailable")
    }
    fn cursor_position(&self, context: &Self::Context) -> (f64, f64, bool);
    fn cursor_shape(&self, context: &Self::Context) -> Option<CursorShape>;
    /// The desktop that has input now.
    fn input_desktop() -> Desk {
        Desk::Default
    }
    /// Moves the calling thread to the desktop that has input, where capture must run.
    fn follow_input_desktop() {}
}

/// A still image: RGB, three bytes a pixel, row by row.
pub struct Still {
    pub rgb: Vec<u8>,
    pub w: usize,
    pub h: usize,
}

/// Where a still goes.
pub type Done = Box<dyn FnOnce(Option<Still>) + Send>;

/// Encodes a native frame into zero or more Annex B access units.
pub trait Encoder: Sized {
    type Frame: ?Sized;
    type Catalog;
    /// The highest H.264 `level_idc` the encoder's API can be set to.
    const MAX_LEVEL: u8;

    /// The encoders this computer offers, best first.
    fn catalog() -> anyhow::Result<Self::Catalog>;
    /// How many encoders the catalog lists.
    fn count(catalog: &Self::Catalog) -> usize;
    /// Encoder `index` of the catalog for `w`×`h` (even; it pads to whole macroblocks and crops
    /// in the SPS), writing H.264 `level` into the stream.
    fn open(catalog: &mut Self::Catalog, index: usize, w: u32, h: u32, fps: u32, bitrate: u32, level: u8) -> anyhow::Result<Self>;
    fn set_bitrate(&self, bitrate: u32);
    fn force_keyframe(&self);
    fn encode(&mut self, frame: &Self::Frame) -> anyhow::Result<Vec<Vec<u8>>>;
}

pub enum Command {
    /// A session takes the computer, at the viewer's constraints.
    Start(Constraints),
    Stop,
    Keyframe,
    /// Nonzero opens or continues a lossless capture and grants one tile. Zero returns to video.
    SharpTile(u32),
    Bitrate(u32),
    /// The network is behind: encoding waits (the screen's changes are kept, not encoded) until
    /// it has caught up; then the screen as it is goes out.
    Behind(bool),
    Thumbs(bool),
    /// Switches to another display, by `DisplayInfo::id`.
    Display(String),
    /// The browser draws the pointer itself: report it instead of drawing it into the video.
    LocalCursor(bool),
    /// What the viewer accepts; the encoded size follows it.
    View(Constraints),
    /// The system says the displays changed (`platform::DisplayWatch`).
    DisplaysChanged,
    /// One still of a display (by id; `None`: the primary), at most `max` wide, answered as
    /// `Snapshot` (or `SnapshotFailed`) under `id`.
    Snapshot { id: String, display: Option<String>, max: usize },
}

/// Starts the capture thread. It reports through `events` and ends when the returned sender
/// is dropped while idle.
pub fn spawn<C: Capturer + 'static, E: Encoder<Frame = C::Frame> + 'static>(events: Sender<PlatformEvent>, moves: crate::input::AppliedMoves) -> Sender<Command> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || {
            if let Err(e) = run::<C, E>(rx, events, moves) {
                eprintln!("capture thread stopped: {e}");
            }
        })
        .expect("spawn capture thread");
    tx
}

/// An encoder for `t`, the preferred one or, if it cannot take this size, the next ones in
/// the catalog. A failed open says nothing about other sizes, so it changes no preference:
/// the next encoder is again tried from the preferred one. Only an encoder that fails while
/// it runs moves the preference on (`run`).
fn open_encoder<E: Encoder>(catalog: &mut E::Catalog, preferred: usize, t: &size::Encoding, bitrate: u32) -> Option<E> {
    (preferred..E::count(catalog)).find_map(|i| match E::open(catalog, i, t.w as u32, t.h as u32, t.fps, bitrate, t.level.idc) {
        Ok(e) => Some(e),
        Err(err) => {
            eprintln!("encoder {i} cannot take {}x{}: {err:#}", t.w, t.h);
            None
        }
    })
}

/// The size of an image `w`×`h` brought down to at most `max` wide, its shape kept.
pub(crate) fn fit(w: usize, h: usize, max: usize) -> Option<(usize, usize)> {
    if w == 0 || h == 0 || max == 0 {
        return None;
    }
    let tw = max.min(w);
    Some((tw, (h * tw / w).max(1)))
}

fn jpeg(s: &Still, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, quality).encode(&s.rgb, s.w as u16, s.h as u16, jpeg_encoder::ColorType::Rgb).ok()?;
    Some(out)
}

/// Previews are small and soft; a screenshot an AI client reads text from is sharper.
const THUMB_QUALITY: u8 = 70;
const SNAPSHOT_QUALITY: u8 = 82;

/// Where a still taken for `what` goes: encoded off the capture thread, sent as its event.
fn deliver(out: Sender<PlatformEvent>, what: Wanted) -> Done {
    Box::new(move |still| {
        std::thread::spawn(move || {
            let e = match (what, still) {
                (Wanted::Thumb(display), Some(s)) => jpeg(&s, THUMB_QUALITY).map(|j| PlatformEvent::Thumbnail { display, jpeg: Bytes(j) }),
                (Wanted::Thumb(_), None) => None,
                (Wanted::Snapshot(id, display), Some(s)) => Some(match jpeg(&s, SNAPSHOT_QUALITY) {
                    Some(j) => PlatformEvent::Snapshot { id, display, jpeg: Bytes(j), w: s.w as u32, h: s.h as u32 },
                    None => PlatformEvent::SnapshotFailed { id },
                }),
                (Wanted::Snapshot(id, _), None) => Some(PlatformEvent::SnapshotFailed { id }),
            };
            if let Some(e) = e {
                let _ = out.send(e);
            }
        });
    })
}

/// What a still is taken for.
enum Wanted {
    /// The preview of a display.
    Thumb(String),
    /// A screenshot asked for under an id, of a display.
    Snapshot(String, String),
}

/// On a slow link, fewer frames keep each one readable instead of smearing them all. Never
/// faster than the viewer decodes smoothly (`fps`).
fn frame_interval(bitrate: u32, fps: u32) -> Duration {
    if fps == SHARP_FPS {
        return Duration::from_millis((150_000_000 / u64::from(bitrate.max(1))).clamp(500, 2_000));
    }
    let fps = match bitrate {
        0..1_000_000 => 10,
        1_000_000..2_500_000 => 15,
        _ => FPS,
    }
    .min(fps.max(1));
    Duration::from_millis(1000 / fps as u64)
}

/// What the browser drawing its own pointer needs: shape changes and positions.
struct CursorReport {
    on: bool,
    shape_due: bool,
    pos_due: bool,
    last: Instant,
    /// The injector's last applied move, read before each position so a report never claims a
    /// move it predates.
    moves: crate::input::AppliedMoves,
}

impl CursorReport {
    fn send<C: Capturer>(&mut self, out: &Sender<PlatformEvent>, d: &C, context: &C::Context) {
        if !self.on {
            return;
        }
        if self.shape_due {
            self.shape_due = false;
            if let Some(shape) = d.cursor_shape(context) {
                let _ = out.send(PlatformEvent::CursorShape(shape));
            }
        }
        if self.pos_due && self.last.elapsed() >= CURSOR_EVERY {
            self.pos_due = false;
            self.last = Instant::now();
            let seq = self.moves.get();
            let (x, y, visible) = d.cursor_position(context);
            let _ = out.send(PlatformEvent::CursorPosition { x, y, visible, seq });
        }
    }
}

/// The capture loop's state: what the session wants, what is open, and when it last did what.
struct Pipeline<C: Capturer, E: Encoder<Frame = C::Frame>> {
    out: Sender<PlatformEvent>,
    encoders: E::Catalog,
    /// The catalog's encoder to try first: moved on only by one failing while it runs.
    preferred: usize,
    context: C::Context,
    /// A session is watching: video, the pointer and desktop changes are sent.
    active: bool,
    /// Reported unavailable to this session; available again with the next encoded frame.
    no_display: bool,
    known: Option<Vec<DisplayInfo>>,
    want_thumbs: bool,
    /// Screenshots asked for: id, display (`None`: the primary), most width.
    snapshots: Vec<(String, Option<String>, usize)>,
    dup: Option<C>,
    tiles: Option<tiles::Tiles>,
    tile_requested: bool,
    enc: Option<E>,
    selected: Option<String>,
    announce: bool,
    cursor: CursorReport,
    network_bitrate: u32,
    /// What was sent is still queuing up: nothing new is encoded until it drains.
    network_behind: bool,
    /// The viewer's constraints, and how the running encoder encodes under them.
    constraints: Constraints,
    encoding: Option<size::Encoding>,
    max_level: size::Level,
    dirty: bool,
    force_key: bool,
    key_due: Option<Instant>,
    last_key: Instant,
    refine_left: u32,
    last_encode: Instant,
    last_thumb: Instant,
    desk: Option<Desk>,
    desk_checked: Instant,
    /// No display is attached: nothing to do until the system says one is.
    wait_for_display: bool,
}

impl<C: Capturer, E: Encoder<Frame = C::Frame>> Pipeline<C, E> {
    fn new(out: Sender<PlatformEvent>, moves: crate::input::AppliedMoves) -> anyhow::Result<Self> {
        Ok(Pipeline {
            out,
            encoders: E::catalog()?,
            preferred: 0,
            context: C::Context::default(),
            active: false,
            no_display: false,
            known: None,
            want_thumbs: false,
            snapshots: Vec::new(),
            dup: None,
            tiles: None,
            tile_requested: false,
            enc: None,
            selected: None,
            announce: false,
            cursor: CursorReport { on: false, shape_due: false, pos_due: false, last: Instant::now(), moves },
            network_bitrate: START_BITRATE,
            network_behind: false,
            constraints: Constraints::default(),
            encoding: None,
            max_level: size::Level::from_idc(E::MAX_LEVEL).expect("encoder level is in Table A-1"),
            dirty: false,
            force_key: false,
            key_due: None,
            last_key: Instant::now() - KEYFRAME_GAP,
            refine_left: 0,
            last_encode: Instant::now() - Duration::from_secs(1),
            last_thumb: Instant::now() - THUMB_EVERY,
            desk: None,
            desk_checked: Instant::now() - DESK_EVERY,
            wait_for_display: false,
        })
    }

    /// Nothing to capture for: the thread sleeps on its commands with nothing held open.
    fn idle(&self) -> bool {
        (!self.active && !self.want_thumbs && self.snapshots.is_empty()) || self.wait_for_display
    }

    fn displays_changed(&mut self, list: Vec<DisplayInfo>) {
        if self.known.as_ref() != Some(&list) {
            self.known = Some(list.clone());
            let _ = self.out.send(PlatformEvent::DisplaysChanged(list));
        }
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Start(c) => {
                if self.tiles.take().is_some() {
                    self.dup = None;
                }
                self.tile_requested = false;
                self.constraints = c;
                self.wait_for_display = false;
                self.active = true;
                self.no_display = false;
                self.network_bitrate = START_BITRATE;
                self.network_behind = false;
                self.enc = None; // a fresh encoder guarantees the new viewer starts on a keyframe
                self.dirty = true;
                self.refine_left = 0;
                self.announce = true;
                if self.cursor.on && C::CURSOR_IN_STREAM {
                    self.dup = None;
                }
                self.cursor.on = false;
            }
            Command::Stop => {
                if self.tiles.take().is_some() {
                    self.dup = None;
                }
                self.tile_requested = false;
                self.active = false;
                self.enc = None;
                // Constraints belong to the session that set them.
                self.constraints = Constraints::default();
                // The overview thumbnail shows the primary display again.
                if self.selected.take().is_some() || (self.cursor.on && C::CURSOR_IN_STREAM) {
                    self.dup = None;
                }
                self.cursor.on = false;
            }
            Command::Keyframe => {
                if self.last_key.elapsed() >= KEYFRAME_GAP {
                    self.force_key = true;
                    self.dirty = true;
                } else {
                    self.key_due = Some(self.last_key + KEYFRAME_GAP);
                }
            }
            Command::SharpTile(epoch) => {
                if self.tiles.as_ref().map_or(0, |tiles| tiles.epoch) != epoch {
                    self.tiles = (epoch != 0).then(|| tiles::Tiles::new(epoch));
                    self.dup = None;
                    self.enc = None;
                    self.refine_left = 0;
                }
                self.tile_requested = epoch != 0;
            }
            Command::Behind(b) => self.network_behind = b,
            Command::Bitrate(b) => {
                self.network_bitrate = b;
                if let (Some(e), Some(now)) = (&self.enc, &self.encoding) {
                    e.set_bitrate(b.min(now.max_bitrate));
                }
            }
            // Applied with the next frame; a static screen gets one so the size follows.
            Command::View(c) => {
                self.constraints = c;
                self.dirty = true;
            }
            // A request for one round of previews. Capture runs only until they are taken.
            Command::Thumbs(on) => {
                if on {
                    self.want_thumbs = true;
                    self.last_thumb = Instant::now() - THUMB_EVERY;
                }
            }
            Command::Snapshot { id, display, max } => self.snapshots.push((id, display, max)),
            Command::Display(id) => {
                self.wait_for_display = false;
                if self.dup.as_ref().is_none_or(|d| d.display().id != id) {
                    self.selected = Some(id);
                    self.dup = None;
                } else {
                    self.announce = true;
                }
            }
            Command::LocalCursor(on) => {
                if self.cursor.on != on && C::CURSOR_IN_STREAM {
                    self.dup = None;
                }
                self.cursor.on = on;
                self.cursor.shape_due = on;
                self.cursor.pos_due = on;
                // The video must gain or lose the drawn pointer.
                self.dirty = true;
                self.refine_left = REFINE_PASSES;
            }
            Command::DisplaysChanged => {
                self.wait_for_display = false;
                let list = C::displays();
                // The captured display went away, moved or changed size: open again (the
                // wanted one while it is attached, otherwise the primary).
                if self.dup.as_ref().is_some_and(|d| !list.contains(d.display())) {
                    self.dup = None;
                }
                self.displays_changed(list);
            }
        }
    }

    /// A keyframe that was held back is due; and the input desktop is followed. The lock
    /// screen, sign-in screen and UAC prompts are separate desktops on Windows: each change is
    /// reported, and capture moves to the new desktop.
    fn tick(&mut self) {
        if self.key_due.is_some_and(|t| Instant::now() >= t) {
            self.key_due = None;
            self.force_key = true;
            self.dirty = true;
        }
        if self.desk_checked.elapsed() >= DESK_EVERY || self.announce {
            self.desk_checked = Instant::now();
            let now = C::input_desktop();
            if Some(now) != self.desk || self.announce {
                if self.desk.is_some() && Some(now) != self.desk {
                    eprintln!("input desktop is now {}", now.as_str());
                }
                self.desk = Some(now);
                if self.active {
                    let _ = self.out.send(PlatformEvent::Desktop(now));
                }
            }
        }
    }

    /// Opens capture if nothing is open. True if a capture was already open; after opening (or
    /// failing to), the commands that arrived meanwhile come first.
    fn open(&mut self) -> bool {
        if self.dup.is_some() {
            return true;
        }
        C::follow_input_desktop();
        let open = if self.tiles.is_some() { C::open_rgb } else { C::open };
        match open(self.selected.as_deref(), !self.cursor.on) {
            Ok(d) => {
                let info = d.display();
                eprintln!("capture: {} {}x{} at {},{}", info.id, info.w, info.h, info.x, info.y);
                self.displays_changed(C::displays());
                self.enc = None;
                if let Some(tiles) = self.tiles.as_mut() {
                    tiles.reset();
                }
                self.dirty = true;
                self.announce = true;
                self.context = C::Context::default();
                self.cursor.shape_due = self.cursor.on;
                self.cursor.pos_due = self.cursor.on;
                // Opening takes a while: what arrived meanwhile (the viewer's constraints) is
                // applied before the first frame, so the first encoder has the right size.
                self.dup = Some(d);
            }
            Err(e) if e.is::<NoDisplay>() => {
                if self.active && !self.no_display {
                    let _ = self.out.send(PlatformEvent::CaptureAvailable(false));
                    self.no_display = true;
                }
                self.displays_changed(Vec::new());
                eprintln!("no display attached; waiting for one");
                self.wait_for_display = true;
            }
            Err(e) => {
                if let Some(tiles) = self.tiles.take() {
                    let _ = self.out.send(PlatformEvent::SharpTile { epoch: tiles.epoch, packet: None });
                    self.tile_requested = false;
                    return false;
                }
                // Lock screen or sleep: keep trying without taking down the host.
                eprintln!("capture unavailable: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        false
    }

    /// Waits briefly for the screen to change and takes what changed. True if there is a frame
    /// to work with.
    fn capture(&mut self) -> bool {
        let Some(d) = self.dup.as_mut() else { return false };
        if self.announce && self.active {
            self.announce = false;
            let _ = self.out.send(PlatformEvent::Capturing { list: C::displays(), current: d.display().clone() });
        }
        match d.wait(&mut self.context, Duration::from_millis(15)) {
            Ok(c) => {
                if c.image || (c.pointer && !self.cursor.on) {
                    self.dirty = true;
                    self.refine_left = REFINE_PASSES;
                }
                self.cursor.shape_due |= c.shape;
                self.cursor.pos_due |= c.pointer;
            }
            Err(e) => {
                eprintln!("capture lost: {e}");
                self.dup = None;
                return false;
            }
        }
        d.has_frame()
    }

    /// One round of display previews, when asked for and due: one per display, so the
    /// controller can show each.
    fn thumbnails(&mut self) {
        let Some(d) = self.dup.as_mut() else { return };
        if self.want_thumbs && self.last_thumb.elapsed() >= THUMB_EVERY {
            self.last_thumb = Instant::now();
            self.want_thumbs = false;
            for info in C::displays() {
                d.snapshot(&info, THUMB_WIDTH, deliver(self.out.clone(), Wanted::Thumb(info.id.clone())));
            }
        }
    }

    /// The screenshots asked for, of the displays attached now.
    fn snapshots(&mut self) {
        let Some(d) = self.dup.as_mut() else { return };
        let list = C::displays();
        for (id, want, max) in std::mem::take(&mut self.snapshots) {
            let info = crate::display::choose(&list, want.as_deref()).filter(|i| want.as_deref().is_none_or(|w| w == i.id));
            match info {
                Some(info) => d.snapshot(info, max, deliver(self.out.clone(), Wanted::Snapshot(id, info.id.clone()))),
                None => {
                    let _ = self.out.send(PlatformEvent::SnapshotFailed { id });
                }
            }
        }
    }

    /// The encode step: when a frame is due (the screen changed, at the pace the network and
    /// the viewer take, or a refinement pass of a still screen), it is encoded and sent, with an
    /// encoder for the size the viewer wants.
    fn encode(&mut self) {
        let Some(d) = self.dup.as_mut() else { return };
        self.cursor.send(&self.out, d, &self.context);
        if let Some(tiles) = self.tiles.as_mut() {
            if !self.tile_requested {
                return;
            }
            if self.dirty {
                if !tiles.can_sample() {
                    return;
                }
                self.dirty = false;
                if let Err(e) = d.rgb(&self.context, !self.cursor.on).and_then(|pixels| tiles.sample(pixels)) {
                    eprintln!("lossless capture: {e:#}");
                    let _ = self.out.send(PlatformEvent::SharpTile { epoch: tiles.epoch, packet: None });
                    self.tile_requested = false;
                    return;
                }
            }
            let (x, y, _) = d.cursor_position(&self.context);
            let packet = match tiles.next(x, y) {
                Ok(Some(packet)) => Some(Bytes(packet)),
                Ok(None) => return,
                Err(e) => {
                    eprintln!("lossless tile: {e:#}");
                    None
                }
            };
            if self.no_display && packet.is_some() {
                self.no_display = false;
                let _ = self.out.send(PlatformEvent::CaptureAvailable(true));
            }
            self.tile_requested = false;
            let _ = self.out.send(PlatformEvent::SharpTile { epoch: tiles.epoch, packet });
            return;
        }
        // Behind: nothing is encoded, so nothing more queues up; the changes wait in `dirty`.
        if self.network_behind {
            return;
        }
        let fps = self.encoding.map_or(FPS, |e| e.fps);
        let interval = frame_interval(self.network_bitrate, fps);
        let refine_interval = if fps == SHARP_FPS && self.network_bitrate < 300_000 { interval } else { REFINE_EVERY };
        let refine_due = !self.dirty && self.refine_left > 0 && self.last_encode.elapsed() >= refine_interval;
        if !(self.dirty && self.last_encode.elapsed() >= interval) && !refine_due {
            return;
        }
        if refine_due {
            self.refine_left -= 1;
        }

        // A new size (viewer resized or turned, display switched or turned) needs a new
        // encoder, which starts with a keyframe: at most one per KEYFRAME_GAP, the latest wins.
        let target = size::choose(d.pixel_size(), &self.constraints, self.max_level);
        if self.enc.is_some() && self.encoding != Some(target) {
            if self.last_key.elapsed() >= KEYFRAME_GAP {
                self.enc = None;
            } else {
                self.key_due = Some(self.last_key + KEYFRAME_GAP);
            }
        }
        if self.enc.is_none() {
            self.enc = open_encoder::<E>(&mut self.encoders, self.preferred, &target, self.network_bitrate.min(target.max_bitrate));
            if self.enc.is_none() {
                eprintln!("no usable H.264 encoder");
                std::thread::sleep(Duration::from_secs(1));
                return;
            }
            self.encoding = Some(target);
            let _ = self.out.send(PlatformEvent::EncodedSize { w: target.w as u32, h: target.h as u32, max_bitrate: target.max_bitrate });
            // A new encoder starts with a keyframe.
            self.last_key = Instant::now();
            self.force_key = false;
        }
        let (Some(encoder), Some(now)) = (self.enc.as_mut(), self.encoding) else { return };
        let Ok(frame) = d.frame(&self.context, !self.cursor.on, (now.w, now.h)) else {
            self.dup = None;
            return;
        };
        if self.force_key {
            encoder.force_keyframe();
            self.force_key = false;
            self.last_key = Instant::now();
        }
        self.last_encode = Instant::now();
        self.dirty = false;
        match encoder.encode(frame) {
            Ok(frames) => {
                if self.no_display && !frames.is_empty() {
                    let _ = self.out.send(PlatformEvent::CaptureAvailable(true));
                    self.no_display = false;
                }
                for data in frames {
                    let _ = self.out.send(PlatformEvent::Frame(Bytes(data)));
                }
            }
            Err(e) => {
                eprintln!("encoder failed, trying the next one: {e}");
                self.enc = None;
                self.preferred = (self.preferred + 1) % E::count(&self.encoders).max(1);
                self.dirty = true;
            }
        }
    }
}

fn run<C: Capturer, E: Encoder<Frame = C::Frame>>(rx: Receiver<Command>, out: Sender<PlatformEvent>, moves: crate::input::AppliedMoves) -> anyhow::Result<()> {
    let mut p = Pipeline::<C, E>::new(out, moves)?;
    // The displays as they are now, before anything opens a capture: an AI client finds the
    // display to take a picture of in this list, so it cannot wait for a picture to fill it.
    p.displays_changed(C::displays());
    loop {
        // Commands. When idle, sleep on the channel with nothing held open.
        loop {
            let cmd = if p.idle() {
                p.dup = None;
                p.enc = None;
                match rx.recv_timeout(Duration::from_secs(3600)) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                match rx.try_recv() {
                    Ok(c) => c,
                    Err(_) => break,
                }
            };
            p.command(cmd);
        }
        p.tick();
        if !p.open() || !p.capture() {
            continue;
        }
        p.thumbnails();
        p.snapshots();
        if p.active {
            p.encode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    static DISPLAY_ATTACHED: AtomicBool = AtomicBool::new(false);

    struct TestCapture(DisplayInfo);

    impl Capturer for TestCapture {
        type Frame = ();
        type Context = ();

        fn open(_: Option<&str>, _: bool) -> anyhow::Result<Self> {
            anyhow::ensure!(DISPLAY_ATTACHED.load(Ordering::Relaxed), NoDisplay);
            Ok(Self(DisplayInfo { id: "test".into(), x: 0, y: 0, w: 16, h: 16, primary: true }))
        }

        fn display(&self) -> &DisplayInfo { &self.0 }
        fn displays() -> Vec<DisplayInfo> {
            Self::open(None, false).map(|d| vec![d.0]).unwrap_or_default()
        }
        fn pixel_size(&self) -> (usize, usize) { (16, 16) }
        fn wait(&mut self, _: &mut (), timeout: Duration) -> anyhow::Result<CaptureChanges> {
            std::thread::sleep(timeout);
            Ok(CaptureChanges { image: true, ..Default::default() })
        }
        fn has_frame(&self) -> bool { true }
        fn snapshot(&mut self, d: &DisplayInfo, max: usize, done: Done) {
            let (w, h) = fit(d.w as usize, d.h as usize, max).expect("a size");
            done(Some(Still { rgb: vec![128; w * h * 3], w, h }));
        }
        fn frame(&mut self, _: &(), _: bool, _: (usize, usize)) -> anyhow::Result<&()> { Ok(&()) }
        fn cursor_position(&self, _: &()) -> (f64, f64, bool) { (0.0, 0.0, false) }
        fn cursor_shape(&self, _: &()) -> Option<CursorShape> { None }
    }

    struct TestEncoder;

    impl Encoder for TestEncoder {
        type Frame = ();
        type Catalog = ();
        const MAX_LEVEL: u8 = 52;

        fn catalog() -> anyhow::Result<()> { Ok(()) }
        fn count(_: &()) -> usize { 1 }
        fn open(_: &mut (), _: usize, _: u32, _: u32, _: u32, _: u32, _: u8) -> anyhow::Result<Self> { Ok(Self) }
        fn set_bitrate(&self, _: u32) {}
        fn force_keyframe(&self) {}
        fn encode(&mut self, _: &()) -> anyhow::Result<Vec<Vec<u8>>> { Ok(vec![vec![1]]) }
    }

    /// Two encoders: the first takes only widths from 64, the second any; every open is noted.
    struct Picky;

    impl Encoder for Picky {
        type Frame = ();
        type Catalog = Vec<usize>;
        const MAX_LEVEL: u8 = 52;

        fn catalog() -> anyhow::Result<Vec<usize>> { Ok(Vec::new()) }
        fn count(_: &Vec<usize>) -> usize { 2 }
        fn open(tried: &mut Vec<usize>, i: usize, w: u32, _: u32, _: u32, _: u32, _: u8) -> anyhow::Result<Self> {
            tried.push(i);
            anyhow::ensure!(i == 1 || w >= 64, "too small");
            Ok(Self)
        }
        fn set_bitrate(&self, _: u32) {}
        fn force_keyframe(&self) {}
        fn encode(&mut self, _: &()) -> anyhow::Result<Vec<Vec<u8>>> { Ok(Vec::new()) }
    }

    #[test]
    fn a_size_an_encoder_cannot_take_does_not_rule_it_out() {
        let at = |w: usize| size::Encoding { w, h: w / 2, fps: 30, level: size::Level::from_idc(31).unwrap(), max_bitrate: 1 };
        let mut tried = Vec::new();
        assert!(open_encoder::<Picky>(&mut tried, 0, &at(2), 1).is_some(), "the next encoder takes what the first cannot");
        assert_eq!(tried, [0, 1]);
        tried.clear();
        assert!(open_encoder::<Picky>(&mut tried, 0, &at(1440), 1).is_some());
        assert_eq!(tried, [0], "the preferred encoder is tried first again");
        tried.clear();
        assert!(open_encoder::<Picky>(&mut tried, 1, &at(1440), 1).is_some());
        assert_eq!(tried, [1], "after a failure while running, the next one is preferred");
    }

    #[test]
    fn no_display_is_reported_and_recovers_with_an_encoded_frame() {
        let (events, updates) = channel();
        let commands = spawn::<TestCapture, TestEncoder>(events, crate::input::AppliedMoves::default());
        let next = || loop {
            match updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                PlatformEvent::Desktop(_) => continue,
                event => break event,
            }
        };
        assert_eq!(next(), PlatformEvent::DisplaysChanged(Vec::new()), "the displays are reported as it starts, before anything is captured");
        commands.send(Command::Start(Constraints::default())).unwrap();
        assert_eq!(next(), PlatformEvent::CaptureAvailable(false));
        // Nothing happens until the system says a display came.
        DISPLAY_ATTACHED.store(true, Ordering::Relaxed);
        assert!(updates.recv_timeout(Duration::from_millis(300)).is_err(), "no polling for displays");
        commands.send(Command::DisplaysChanged).unwrap();
        assert!(matches!(next(), PlatformEvent::DisplaysChanged(list) if list.len() == 1));
        assert!(matches!(next(), PlatformEvent::Capturing { current, .. } if current.id == "test"));
        assert!(matches!(next(), PlatformEvent::EncodedSize { w: 16, h: 16, .. }));
        assert_eq!(next(), PlatformEvent::CaptureAvailable(true));
        assert_eq!(next(), PlatformEvent::Frame(Bytes(vec![1])));
        commands.send(Command::Stop).unwrap();
    }

    /// A 640x400 display; records the size each frame is asked at.
    struct Wide(DisplayInfo);
    static ASKED: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

    impl Capturer for Wide {
        type Frame = ();
        type Context = ();

        fn open(_: Option<&str>, _: bool) -> anyhow::Result<Self> {
            Ok(Self(DisplayInfo { id: "wide".into(), x: 0, y: 0, w: 640, h: 400, primary: true }))
        }
        fn display(&self) -> &DisplayInfo { &self.0 }
        fn displays() -> Vec<DisplayInfo> {
            Self::open(None, false).map(|d| vec![d.0]).unwrap_or_default()
        }
        fn pixel_size(&self) -> (usize, usize) { (640, 400) }
        fn wait(&mut self, _: &mut (), timeout: Duration) -> anyhow::Result<CaptureChanges> {
            std::thread::sleep(timeout);
            Ok(CaptureChanges { image: true, ..Default::default() })
        }
        fn has_frame(&self) -> bool { true }
        fn snapshot(&mut self, _: &DisplayInfo, _: usize, done: Done) { done(None) }
        fn frame(&mut self, _: &(), _: bool, size: (usize, usize)) -> anyhow::Result<&()> {
            ASKED.lock().unwrap().push(size);
            Ok(&())
        }
        fn cursor_position(&self, _: &()) -> (f64, f64, bool) { (0.0, 0.0, false) }
        fn cursor_shape(&self, _: &()) -> Option<CursorShape> { None }
    }

    #[test]
    fn a_new_view_reconfigures_the_encoder_and_frames_follow() {
        let (events, updates) = channel();
        let commands = spawn::<Wide, TestEncoder>(events, crate::input::AppliedMoves::default());
        commands.send(Command::Start(Constraints::default())).unwrap();
        let size = || loop {
            if let PlatformEvent::EncodedSize { w, h, max_bitrate } = updates.recv_timeout(Duration::from_secs(5)).unwrap() {
                break (w, h, max_bitrate);
            }
        };
        let (w, h, full_rate) = size();
        assert_eq!((w, h), (640, 400), "no constraints yet: the display's size");
        // The viewer is three quarters as wide: a smaller stream, and less bandwidth for it.
        commands.send(Command::View(Constraints { w: 480, h: 1200, max_w: 1920, max_h: 1080, fps: 30, level: 31 })).unwrap();
        let (w, h, rate) = size();
        assert_eq!((w, h), (480, 300));
        assert!(rate < full_rate);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(ASKED.lock().unwrap().last(), Some(&(480, 300)), "frames are taken at the new size");
        // Turned to portrait at the same width: the same size, no new encoder.
        commands.send(Command::View(Constraints { w: 480, h: 1200, max_w: 1080, max_h: 1920, fps: 30, level: 31 })).unwrap();
        std::thread::sleep(Duration::from_millis(700));
        assert!(!updates.try_iter().any(|e| matches!(e, PlatformEvent::EncodedSize { .. })));
        commands.send(Command::Stop).unwrap();
    }

    /// A screen that changes all the time, like a video playing.
    struct Busy(DisplayInfo);

    impl Capturer for Busy {
        type Frame = ();
        type Context = ();

        fn open(_: Option<&str>, _: bool) -> anyhow::Result<Self> {
            Ok(Self(DisplayInfo { id: "busy".into(), x: 0, y: 0, w: 320, h: 200, primary: true }))
        }
        fn display(&self) -> &DisplayInfo { &self.0 }
        fn displays() -> Vec<DisplayInfo> {
            Self::open(None, false).map(|d| vec![d.0]).unwrap_or_default()
        }
        fn pixel_size(&self) -> (usize, usize) { (320, 200) }
        fn wait(&mut self, _: &mut (), timeout: Duration) -> anyhow::Result<CaptureChanges> {
            std::thread::sleep(timeout.min(Duration::from_millis(10)));
            Ok(CaptureChanges { image: true, ..Default::default() })
        }
        fn has_frame(&self) -> bool { true }
        fn snapshot(&mut self, _: &DisplayInfo, _: usize, done: Done) { done(None) }
        fn frame(&mut self, _: &(), _: bool, _: (usize, usize)) -> anyhow::Result<&()> { Ok(&()) }
        fn cursor_position(&self, _: &()) -> (f64, f64, bool) { (0.0, 0.0, false) }
        fn cursor_shape(&self, _: &()) -> Option<CursorShape> { None }
    }

    /// The network behind: nothing is encoded however much the screen changes; caught up: the
    /// screen as it is goes out at once.
    #[test]
    fn a_network_that_falls_behind_holds_encoding_until_it_catches_up() {
        let (events, updates) = channel();
        let commands = spawn::<Busy, TestEncoder>(events, crate::input::AppliedMoves::default());
        commands.send(Command::Start(Constraints::default())).unwrap();
        let frame = |within: Duration| {
            let until = Instant::now() + within;
            while let Some(left) = until.checked_duration_since(Instant::now()) {
                if let Ok(PlatformEvent::Frame(_)) = updates.recv_timeout(left) {
                    return true;
                }
            }
            false
        };
        assert!(frame(Duration::from_secs(5)), "a changing screen is encoded");
        commands.send(Command::Behind(true)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let _ = updates.try_iter().count();
        assert!(!frame(Duration::from_millis(600)), "behind: nothing new is encoded");
        commands.send(Command::Behind(false)).unwrap();
        assert!(frame(Duration::from_millis(300)), "caught up: the screen as it is goes out at once");
        commands.send(Command::Stop).unwrap();
    }
}
