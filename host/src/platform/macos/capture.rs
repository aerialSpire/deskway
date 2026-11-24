use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread as _, DefinedClass as _};
use objc2_app_kit::NSCursor;
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    kCGColorSpaceITUR_709, kCGDisplayStreamYCbCrMatrix_ITU_R_709_2, CGDisplayBounds,
    CGDisplayCopyDisplayMode, CGDisplayMode, CGEvent, CGMainDisplayID, CGWindowID,
    CGPreflightScreenCaptureAccess,
};
use objc2_core_graphics::{
    CGColorSpace, CGContext, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::*;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::*;
use objc2_video_toolbox::VTPixelTransferSession;

use super::encoder::check;
use crate::display::{self, DisplayInfo};
use crate::video::{fit, CaptureChanges, Capturer, CursorShape, Done, NoDisplay, Still, CURSOR_MAX, FPS};

const START_TIMEOUT: Duration = Duration::from_secs(5);
const THUMB_TIMEOUT: Duration = Duration::from_secs(2);
const NV12: u32 = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
type OverlayWindows = [CGWindowID; 5];
static OVERLAY_WINDOWS: Mutex<Option<OverlayWindows>> = Mutex::new(None);
/// Set when the overlay windows change, so a running capture updates its filter.
static OVERLAYS_CHANGED: AtomicBool = AtomicBool::new(false);

pub(super) fn set_overlay_windows(ids: OverlayWindows) {
    if !ids.contains(&0) {
        *OVERLAY_WINDOWS.lock().unwrap() = Some(ids);
        OVERLAYS_CHANGED.store(true, Ordering::Release);
    }
}

// ScreenCaptureKit's immutable snapshot is transferred from the completion handler to
// the capture thread. Once retained, it can outlive the completion handler.
struct ShareableContent(Retained<SCShareableContent>);
unsafe impl Send for ShareableContent {}

// The stream owns the backing surface until all clients release the buffer. Only the
// capture thread reads the pixels, and only while it holds this retained reference.
struct ScreenFrame(CFRetained<CVPixelBuffer>);
unsafe impl Send for ScreenFrame {}

#[derive(Default)]
struct Updates {
    pending: Option<ScreenFrame>,
    seen_frame: bool,
    blank: bool,
    error: Option<String>,
    cursor: Option<CGPoint>,
}

#[derive(Default)]
struct Frames {
    updates: Mutex<Updates>,
    ready: Condvar,
}

struct OutputState {
    frames: Arc<Frames>,
    cursor_enabled: bool,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = OutputState]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.handle_frame(sample);
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            let frames = &self.ivars().frames;
            frames.updates.lock().unwrap().error = Some(error.localizedDescription().to_string());
            frames.ready.notify_one();
        }
    }
);

impl Output {
    fn new(frames: Arc<Frames>, cursor_enabled: bool) -> Retained<Self> {
        unsafe {
            msg_send![
                super(Self::alloc().set_ivars(OutputState {
                    frames,
                    cursor_enabled
                })),
                init
            ]
        }
    }

    fn handle_frame(&self, sample: &CMSampleBuffer) {
        let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
            return;
        };
        // Sample attachments are a CF array of CF dictionaries with string keys.
        let attachments = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(attachments) };
        let Some(info) = attachments
            .get(0)
            .and_then(|v| v.downcast::<CFDictionary>().ok())
        else {
            return;
        };
        let info = unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(info) };
        let status_key: &CFString = unsafe { SCStreamFrameInfoStatus }.as_ref();
        let Some(status) = info
            .get(status_key)
            .and_then(|v| v.downcast::<CFNumber>().ok())
            .and_then(|v| v.as_isize())
            .map(SCFrameStatus)
        else {
            return;
        };
        let frames = &self.ivars().frames;
        if matches!(status, SCFrameStatus::Blank | SCFrameStatus::Suspended) {
            let mut updates = frames.updates.lock().unwrap();
            updates.blank = true;
            updates.pending = None;
            updates.seen_frame = false;
            frames.ready.notify_one();
            return;
        }
        if !matches!(status, SCFrameStatus::Complete | SCFrameStatus::Started) {
            return;
        }
        let Some(buffer) = (unsafe { sample.image_buffer() }) else {
            return;
        };
        let rects_key: &CFString = unsafe { SCStreamFrameInfoDirtyRects }.as_ref();
        let dirty = info
            .get(rects_key)
            .and_then(|v| v.downcast::<CFArray>().ok())
            .is_none_or(|rects| !rects.is_empty());
        let cursor = self.ivars().cursor_enabled.then(|| {
            let event = CGEvent::new(None);
            CGEvent::location(event.as_deref())
        });
        let mut updates = frames.updates.lock().unwrap();
        if !updates.seen_frame || dirty || updates.cursor != cursor {
            updates.pending = Some(ScreenFrame(buffer));
            updates.seen_frame = true;
            updates.cursor = cursor;
            frames.ready.notify_one();
        }
    }
}

#[derive(PartialEq)]
struct Display {
    id: u32,
    bounds: CGRect,
    info: DisplayInfo,
}

impl Display {
    fn list() -> Vec<Self> {
        // A display mode may disappear between listing and opening it.
        super::displays::active().into_iter().filter_map(Self::new).collect()
    }

    fn new(id: u32) -> Option<Self> {
        let mode = CGDisplayCopyDisplayMode(id)?;
        let bounds = CGDisplayBounds(id);
        let w = CGDisplayMode::pixel_width(Some(&mode)) as i32;
        let h = CGDisplayMode::pixel_height(Some(&mode)) as i32;
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 || w <= 0 || h <= 0 {
            return None;
        }
        Some(Self {
            id,
            bounds,
            info: DisplayInfo {
                id: super::displays::uuid(id)?,
                x: (bounds.origin.x * f64::from(w) / bounds.size.width).round() as i32,
                y: (bounds.origin.y * f64::from(h) / bounds.size.height).round() as i32,
                w,
                h,
                primary: id == CGMainDisplayID(),
            },
        })
    }
}

#[derive(Default)]
pub struct Cursor {
    last_poll: Option<Instant>,
    position: Option<CGPoint>,
    shape: Option<CursorShape>,
}

impl Cursor {
    fn update(&mut self) -> CaptureChanges {
        if self
            .last_poll
            .is_some_and(|last| last.elapsed() < Duration::from_millis(33))
        {
            return CaptureChanges::default();
        }
        self.last_poll = Some(Instant::now());
        let position = CGEvent::new(None).map(|event| CGEvent::location(Some(&event)));
        // AppKit may return a fresh NSCursor object for an unchanged pointer,
        // so compare the browser-ready pixels and hotspot.
        #[allow(deprecated)]
        let shape = NSCursor::currentSystemCursor()
            .as_deref()
            .and_then(cursor_rgba);
        let changed = CaptureChanges {
            image: false,
            pointer: self.position != position,
            shape: shape.is_some() && self.shape != shape,
        };
        self.position = position;
        if changed.shape {
            self.shape = shape;
        }
        changed
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        color_space: &CGColorSpace,
        bitmap_info: u32,
    ) -> *mut CGContext;
}

fn cursor_rgba(cursor: &NSCursor) -> Option<CursorShape> {
    let image = cursor.image();
    let size = image.size();
    let cg_image = unsafe { image.CGImageForProposedRect_context_hints(null_mut(), None, None) }?;
    if size.width <= 0.0 || size.height <= 0.0 {
        return None;
    }
    let scale = (CURSOR_MAX as f64 / size.width.max(size.height)).min(1.0);
    let w = (size.width * scale).round().max(1.0) as usize;
    let h = (size.height * scale).round().max(1.0) as usize;
    let mut rgba = vec![0; w * h * 4];
    let color_space = CGColorSpace::new_device_rgb()?;
    // The bitmap context borrows rgba. Release it before reading the pixels.
    let bitmap = unsafe {
        let ctx = CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            w,
            h,
            8,
            w * 4,
            &color_space,
            CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
        );
        CFRetained::from_raw(NonNull::new(ctx)?)
    };
    CGContext::draw_image(
        Some(&bitmap),
        CGRect::new(CGPoint::ZERO, CGSize::new(w as f64, h as f64)),
        Some(&cg_image),
    );
    drop(bitmap);
    straight_rgba(&mut rgba);
    let hotspot = cursor.hotSpot();
    Some(CursorShape {
        w,
        h,
        hx: (hotspot.x * w as f64 / size.width).round() as i32,
        hy: (hotspot.y * h as f64 / size.height).round() as i32,
        rgba,
    })
}

fn straight_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 {
            for component in &mut pixel[..3] {
                *component = ((u32::from(*component) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

pub struct ScreenCapture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
    frames: Arc<Frames>,
    latest: Option<ScreenFrame>,
    size: (usize, usize),
    /// The latest frame scaled to the encoded size, when that is smaller than `size`.
    scaled: Option<(CFRetained<CVPixelBuffer>, (usize, usize))>,
    scaled_ready: bool,
    transfer: CFRetained<VTPixelTransferSession>,
    display: Display,
    excluded_overlays: Option<OverlayWindows>,
    /// The overlay windows were not registered yet: try again from this time.
    overlay_retry: Option<Instant>,
    report_cursor: bool,
}

impl ScreenCapture {
    fn start(wanted: Option<&str>, draw_cursor: bool) -> Result<Self> {
        ensure!(
            CGPreflightScreenCaptureAccess(),
            "Screen Recording permission required"
        );
        let displays = Display::list();
        ensure!(!displays.is_empty(), NoDisplay);
        let content = shareable_content(false)?;
        unsafe {
            let shareable = content.0.displays();
            let attached: Vec<_> = displays
                .into_iter()
                .filter_map(|display| {
                    let sc_display = shareable.iter().find(|d| d.displayID() == display.id)?;
                    Some((display, sc_display))
                })
                .collect();
            let infos: Vec<_> = attached.iter().map(|(d, _)| d.info.clone()).collect();
            let chosen = display::choose(&infos, wanted).ok_or(NoDisplay)?;
            let (display, sc_display) = attached
                .into_iter()
                .find(|(d, _)| d.info.id == chosen.id)
                .context("selected display unavailable")?;
            let overlay_windows = *OVERLAY_WINDOWS.lock().unwrap();
            let (filter, excluded_overlays) = overlay_windows
                .and_then(|ids| live_filter(&content.0, &sc_display, &ids).map(|f| (f, Some(ids))))
                .unwrap_or_else(|| (preview_filter(&content.0, &sc_display), None));
            let rect = filter.contentRect();
            let scale = f64::from(filter.pointPixelScale());
            let width = (rect.size.width * scale).round() as usize;
            let height = (rect.size.height * scale).round() as usize;
            ensure!(width >= 2 && height >= 2, "invalid display size");
            let config = SCStreamConfiguration::new();
            config.setWidth(width);
            config.setHeight(height);
            config.setMinimumFrameInterval(CMTime::new(1, FPS as i32));
            config.setQueueDepth(4);
            config.setPixelFormat(NV12);
            config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
            config.setColorSpaceName(kCGColorSpaceITUR_709);
            let cursor_enabled = draw_cursor
                && std::env::var_os("DESKWAY_CAPTURE_CURSOR").as_deref() != Some("0".as_ref());
            config.setShowsCursor(cursor_enabled);

            let mut transfer = null_mut();
            check(
                VTPixelTransferSession::create(None, NonNull::from(&mut transfer)),
                "create pixel transfer session",
            )?;
            let transfer =
                CFRetained::from_raw(NonNull::new(transfer).context("no pixel transfer session")?);

            let frames = Arc::new(Frames::default());
            let output = Output::new(frames.clone(), cursor_enabled);
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(ProtocolObject::from_ref(&*output)),
            );
            let queue = DispatchQueue::new("deskway.capture", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow::anyhow!("{}", e.localizedDescription()))?;
            let capture = Self {
                stream,
                _output: output,
                _queue: queue,
                frames,
                latest: None,
                size: (width, height),
                scaled: None,
                scaled_ready: false,
                transfer,
                display,
                excluded_overlays,
                overlay_retry: (overlay_windows.is_some() && excluded_overlays.is_none()).then(Instant::now),
                report_cursor: !draw_cursor,
            };
            let (tx, rx) = mpsc::channel();
            let completion = RcBlock::new(move |error: *mut NSError| {
                let result = error.as_ref().map(|e| e.localizedDescription().to_string());
                let _ = tx.send(result);
            });
            capture
                .stream
                .startCaptureWithCompletionHandler(Some(&completion));
            if let Some(error) = rx
                .recv_timeout(START_TIMEOUT)
                .context("start screen capture")?
            {
                bail!("start screen capture: {error}");
            }
            Ok(capture)
        }
    }

    /// Ok(false) while WindowServer has not registered the overlay windows yet.
    fn refresh_overlays(&mut self) -> Result<bool> {
        let Some(wanted) = *OVERLAY_WINDOWS.lock().unwrap() else { return Ok(true) };
        if self.excluded_overlays == Some(wanted) {
            return Ok(true);
        }

        let content = shareable_content(false)?;
        let displays = unsafe { content.0.displays() };
        let display = displays
            .iter()
            .find(|display| unsafe { display.displayID() } == self.display.id)
            .context("selected display unavailable")?;
        let (filter, excluded) = match live_filter(&content.0, &display, &wanted) {
            Some(filter) => (filter, Some(wanted)),
            // Keep all Deskway overlays out of the stream while WindowServer is registering them.
            None if self.excluded_overlays.is_none() => return Ok(false),
            None => (preview_filter(&content.0, &display), None),
        };
        let (tx, rx) = mpsc::channel();
        let completion = RcBlock::new(move |error: *mut NSError| {
            let result = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
            let _ = tx.send(result);
        });
        unsafe {
            self.stream.updateContentFilter_completionHandler(&filter, Some(&completion));
        }
        if let Some(error) = rx
            .recv_timeout(START_TIMEOUT)
            .context("update screen capture filter")?
        {
            bail!("update screen capture filter: {error}");
        }
        self.excluded_overlays = excluded;
        Ok(true)
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
            self.transfer.invalidate();
        }
    }
}

impl Capturer for ScreenCapture {
    type Frame = CVPixelBuffer;
    type Context = Cursor;
    const CURSOR_IN_STREAM: bool = true;

    fn open(wanted: Option<&str>, draw_cursor: bool) -> Result<Self> {
        autoreleasepool(|_| Self::start(wanted, draw_cursor))
    }

    fn display(&self) -> &DisplayInfo {
        &self.display.info
    }

    /// Reports the signed-in macOS session's lock and console state.
    fn input_desktop() -> crate::platform::Desk {
        super::desk::current()
    }

    fn displays() -> Vec<DisplayInfo> {
        let mut infos: Vec<_> = Display::list().into_iter().map(|d| d.info).collect();
        display::sort(&mut infos);
        infos
    }

    fn pixel_size(&self) -> (usize, usize) {
        self.size
    }

    fn wait(&mut self, cursor: &mut Cursor, timeout: Duration) -> Result<CaptureChanges> {
        // Display changes arrive as `Command::DisplaysChanged` (Quartz's reconfiguration
        // callback); only the overlay windows are checked here, from memory.
        let retry = self.overlay_retry.is_some_and(|t| t.elapsed() >= Duration::from_secs(1));
        if OVERLAYS_CHANGED.swap(false, Ordering::AcqRel) || retry {
            let done = autoreleasepool(|_| self.refresh_overlays())?;
            self.overlay_retry = (!done).then(Instant::now);
        }
        let updates = self.frames.updates.lock().unwrap();
        let (mut updates, _) = self
            .frames
            .ready
            .wait_timeout_while(updates, timeout, |update| {
                update.pending.is_none() && update.error.is_none() && !update.blank
            })
            .unwrap();
        if let Some(error) = updates.error.take() {
            bail!("{error}");
        }
        if std::mem::take(&mut updates.blank) {
            self.latest = None;
        }
        let pending = updates.pending.take();
        drop(updates);
        let mut changes = if self.report_cursor {
            autoreleasepool(|_| cursor.update())
        } else {
            CaptureChanges::default()
        };
        if let Some(frame) = pending {
            ensure!(
                (
                    CVPixelBufferGetWidth(&frame.0),
                    CVPixelBufferGetHeight(&frame.0)
                ) == self.size,
                "capture dimensions changed"
            );
            self.latest = Some(frame);
            self.scaled_ready = false;
            changes.image = true;
        }
        Ok(changes)
    }

    fn has_frame(&self) -> bool {
        self.latest.is_some()
    }

    /// Every still, the captured display's included, is a separately filtered screenshot
    /// (ScreenCaptureKit), taken on a thread of its own.
    fn snapshot(&mut self, d: &DisplayInfo, max: usize, done: Done) {
        let want = d.id.clone();
        std::thread::spawn(move || {
            let still = autoreleasepool(|_| {
                let content = shareable_content(false).ok()?;
                let screens = unsafe { content.0.displays() };
                let screen = screens.iter().find(|s| Display::new(unsafe { s.displayID() }).is_some_and(|x| x.info.id == want))?;
                screenshot(&content.0, &screen, max)
            });
            done(still);
        });
    }

    fn frame(&mut self, _: &Cursor, _: bool, size: (usize, usize)) -> Result<&CVPixelBuffer> {
        let source = &self.latest.as_ref().context("no screen frame")?.0;
        if size == self.size {
            return Ok(source);
        }
        if self.scaled.as_ref().is_none_or(|(_, s)| *s != size) {
            self.scaled = Some((new_nv12(size.0, size.1)?, size));
            self.scaled_ready = false;
        }
        let (scaled, _) = self.scaled.as_ref().expect("allocated above");
        if !self.scaled_ready {
            check(
                unsafe { self.transfer.transfer_image(source, scaled) },
                "downscale screen",
            )?;
            self.scaled_ready = true;
        }
        Ok(scaled)
    }

    fn cursor_position(&self, cursor: &Cursor) -> (f64, f64, bool) {
        let Some(position) = cursor.position else {
            return (0.0, 0.0, false);
        };
        let bounds = self.display.bounds;
        let x = (position.x - bounds.origin.x) / bounds.size.width;
        let y = (position.y - bounds.origin.y) / bounds.size.height;
        (x, y, (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y))
    }

    fn cursor_shape(&self, cursor: &Cursor) -> Option<CursorShape> {
        cursor.shape.clone()
    }
}

/// Enters ScreenCaptureKit's first-use authorization flow without starting a stream.
pub(super) fn request_screen_access() {
    let completion = RcBlock::new(|_: *mut SCShareableContent, error: *mut NSError| {
        if let Some(error) = unsafe { error.as_ref() } {
            eprintln!(
                "ScreenCaptureKit authorization: {}",
                error.localizedDescription()
            );
        }
    });
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&completion);
    }
}

fn shareable_content(current_process: bool) -> Result<ShareableContent> {
    let (tx, rx) = mpsc::channel();
    let completion = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // ScreenCaptureKit owns these objects for the duration of the callback.
            let result = unsafe { Retained::retain(content) }
                .map(ShareableContent)
                .ok_or_else(|| {
                    unsafe { error.as_ref() }.map_or_else(
                        || "no shareable content".into(),
                        |e| e.localizedDescription().to_string(),
                    )
                });
            let _ = tx.send(result);
        },
    );
    unsafe {
        if current_process {
            SCShareableContent::getCurrentProcessShareableContentWithCompletionHandler(&completion);
        } else {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                false, false, &completion,
            );
        }
    }
    rx.recv_timeout(START_TIMEOUT)
        .context("enumerate display content")?
        .map_err(anyhow::Error::msg)
}

fn live_filter(content: &SCShareableContent, display: &SCDisplay, ids: &OverlayWindows) -> Option<Retained<SCContentFilter>> {
    unsafe {
        let find_overlays = |content: &SCShareableContent| {
            content.windows().iter()
                .filter(|window| ids.contains(&window.windowID()))
                .collect::<Vec<_>>()
        };
        let mut excluded = find_overlays(content);
        if excluded.len() != ids.len() {
            // The process snapshot can also return overlays that are ordered out.
            excluded = find_overlays(&shareable_content(true).ok()?.0);
        }
        if excluded.len() != ids.len() {
            return None;
        }
        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(), display, &NSArray::from_retained_slice(&excluded),
        );
        filter.setIncludeMenuBar(true);
        Some(filter)
    }
}

fn preview_filter(content: &SCShareableContent, display: &SCDisplay) -> Retained<SCContentFilter> {
    unsafe {
        // Previews may be visible before the controller has been authorized.
        let pid = std::process::id() as libc::pid_t;
        let excluded: Vec<_> = content
            .applications()
            .iter()
            .filter(|app| app.processID() == pid)
            .collect();
        SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            display,
            &NSArray::from_retained_slice(&excluded),
            &NSArray::new(),
        )
    }
}

/// A still of `display`, at most `max` wide.
fn screenshot(content: &SCShareableContent, display: &SCDisplay, max: usize) -> Option<Still> {
    let filter = preview_filter(content, display);
    let (config, size) = unsafe {
        let rect = filter.contentRect();
        let (w, h) = fit(rect.size.width as usize, rect.size.height as usize, max)?;
        let size = (w.next_multiple_of(2), h.next_multiple_of(2));
        let config = SCStreamConfiguration::new();
        config.setWidth(size.0);
        config.setHeight(size.1);
        config.setPixelFormat(NV12);
        config.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        config.setColorSpaceName(kCGColorSpaceITUR_709);
        config.setShowsCursor(false);
        (config, size)
    };
    let (tx, rx) = mpsc::channel();
    let completion = RcBlock::new(move |sample: *mut CMSampleBuffer, _: *mut NSError| {
        let frame = unsafe { sample.as_ref().and_then(|s| s.image_buffer()) }.map(ScreenFrame);
        let _ = tx.send(frame);
    });
    unsafe {
        SCScreenshotManager::captureSampleBufferWithFilter_configuration_completionHandler(
            &filter, &config, Some(&completion),
        );
    }
    let frame = rx.recv_timeout(THUMB_TIMEOUT).ok()??;
    let buffer = &frame.0;
    if CVPixelBufferGetPixelFormatType(buffer) != NV12
        || (CVPixelBufferGetWidth(buffer), CVPixelBufferGetHeight(buffer)) != size
    {
        return None;
    }
    nv12_still(buffer)
}

pub(super) fn new_nv12(w: usize, h: usize) -> Result<CFRetained<CVPixelBuffer>> {
    let empty = CFDictionary::<CFType, CFType>::empty();
    unsafe {
        let attrs = CFDictionary::from_slices(
            &[kCVPixelBufferIOSurfacePropertiesKey as &CFType],
            &[&*empty as &CFType],
        );
        let mut buffer = null_mut();
        check(
            CVPixelBufferCreate(
                None,
                w,
                h,
                NV12,
                Some(attrs.as_opaque()),
                NonNull::from(&mut buffer),
            ),
            "allocate NV12 buffer",
        )?;
        Ok(CFRetained::from_raw(
            NonNull::new(buffer).context("no NV12 buffer")?,
        ))
    }
}

struct LockedPixels<'a>(&'a CVPixelBuffer);

impl Drop for LockedPixels<'_> {
    fn drop(&mut self) {
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

fn nv12_still(buffer: &CVPixelBuffer) -> Option<Still> {
    check(
        unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) },
        "lock thumbnail",
    )
    .ok()?;
    let locked = LockedPixels(buffer);
    let plane = |index| {
        let base = CVPixelBufferGetBaseAddressOfPlane(locked.0, index).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRowOfPlane(locked.0, index);
        let height = CVPixelBufferGetHeightOfPlane(locked.0, index);
        // The plane is valid for the duration of the read lock.
        (!base.is_null() && stride != 0).then(|| {
            (
                unsafe { std::slice::from_raw_parts(base, stride * height) },
                stride,
            )
        })
    };
    let (y_plane, y_stride) = plane(0)?;
    let (uv_plane, uv_stride) = plane(1)?;
    let (w, h) = (
        CVPixelBufferGetWidth(buffer),
        CVPixelBufferGetHeight(buffer),
    );
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let y_value = y_plane[y * y_stride + x];
            let uv = y / 2 * uv_stride + (x & !1);
            rgb.extend_from_slice(&nv12_pixel(y_value, uv_plane[uv], uv_plane[uv + 1]));
        }
    }
    drop(locked);
    Some(Still { rgb, w, h })
}

fn nv12_pixel(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    // BT.709 video-range coefficients in fixed point (scale 256).
    let y = (i32::from(y) - 16) * 298;
    let u = i32::from(cb) - 128;
    let v = i32::from(cr) - 128;
    [y + 459 * v, y - 55 * u - 136 * v, y + 541 * u].map(|c| ((c + 128) >> 8).clamp(0, 255) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_premultiplied_cursor_pixels_to_straight_rgba() {
        let mut rgba = [64, 32, 0, 128, 0, 0, 0, 0, 255, 128, 10, 255];
        straight_rgba(&mut rgba);
        assert_eq!(rgba, [128, 64, 0, 128, 0, 0, 0, 0, 255, 128, 10, 255]);
    }

    #[test]
    fn converts_bt709_video_range_for_thumbnails() {
        assert_eq!(nv12_pixel(16, 128, 128), [0, 0, 0]);
        assert_eq!(nv12_pixel(235, 128, 128), [255, 255, 255]);
        let [r, g, b] = nv12_pixel(63, 102, 240);
        assert!(r >= 254 && g <= 2 && b <= 2);
    }
}
