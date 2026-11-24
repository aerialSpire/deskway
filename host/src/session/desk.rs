//! The desk role: a person's browser controlling the computer. It sends the picture and hears
//! the browser's three channels: "input" (reliable, ordered: `Control`; the session's control
//! channel), "move" (neither: `Move`) and "file" (`File` and the bytes of transfers). The
//! clipboard and file transfers go both ways.

use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::bwe::{Bitrate, BweKind};
use str0m::channel::{ChannelData, ChannelId};
use str0m::media::{MediaTime, Mid, Pt};
use str0m::{Event, Rtc};

use super::{send, send_message, Cx, Role};
use crate::clipboard::{self, Act, Out};
use crate::platform::{Bytes, Platform, PlatformEvent};
use crate::video::size::ProfileLevelId;
use crate::video::START_BITRATE;
use peer::state::Signal;
use wire::control::Control;
use wire::types::View;

pub(crate) mod channels;
mod media;
mod pace;

use channels::{FileQueue, Incoming};

/// The capabilities this host announces in both hellos (signaling and control): the one place
/// they are gathered, as the union of each module's list. A view-only host takes no input
/// (and so none of the input module's caps) and no files.
pub fn caps(view_only: bool) -> Vec<String> {
    use wire::caps::*;
    // What the session and video side report: pointer, displays, capture, input desktop.
    let session: &[&str] = &[CURSOR, DISPLAYS, CAPTURE, DESK];
    let clipboard: &[&str] = &[CLIPBOARD];
    let (control, input): (&[&str], &[&str]) = if view_only { (&[], &[]) } else { (&[INPUT, FILES], crate::input::CAPS) };
    control.iter().chain(input).chain(clipboard).chain(session).map(|c| c.to_string()).collect()
}

/// What a desk session is started with, besides what every session has (`super::Params`).
pub struct Params {
    pub session: String,
    /// The browser's offer: the picture's track and level come from it.
    pub offer: String,
    /// The viewer constraints the offer carried: the picture starts at that size.
    pub view: Option<View>,
    pub platform: Arc<dyn Platform>,
    /// The browser may watch but not control.
    pub view_only: bool,
    /// The shared clipboard is on (the computer's setting).
    pub clip_share: bool,
    /// Where the browser's request to switch it goes: the host saves it and answers with
    /// `SessionEvent::ClipShare`.
    pub share: Sender<bool>,
}

/// However the session ends, an error included: the computer is given up if this session still
/// holds it (nothing held, no half transfer, capture stops). A session taken over leaves its
/// successor's video, input and transfers alone.
struct Cleanup {
    platform: Arc<dyn Platform>,
    session: String,
    started: bool,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.started {
            self.platform.video_stop(&self.session);
        }
    }
}

pub struct Desk {
    id: String,
    offer: String,
    share: Sender<bool>,
    platform: Arc<dyn Platform>,
    view_only: bool,
    lossy: Option<ChannelId>,
    file_ch: Option<ChannelId>,
    clip: clipboard::Sync,
    input: crate::input::Decoder,
    files: FileQueue,
    cleanup: Cleanup,
    /// The video's mid, from the offer: str0m reports the media only once DTLS is connected,
    /// too late to start the picture early.
    mid: Option<Mid>,
    pt: Option<Pt>,
    /// The viewer's constraints: the offer's, then each `view` from the browser, and the level
    /// its offer names for the payload type in use.
    view: (u32, u32, u32, u32, u32),
    level: u8,
    /// Probing aims at what the encoded size can use (EncodedSize), no further.
    ceiling: u32,
    sent_bitrate: u32,
    estimated_bitrate: u32,
    /// Whether what this computer sends is queuing up, and the encoder waits.
    pace: pace::Pace,
    connected: bool,
    needs_first_idr: bool,
    // Initial platform state can arrive before the control channel opens. Keep the latest
    // display list, keyboard layout and desktop state for the channel greeting.
    displays_msg: Option<String>,
    layout_msg: Option<String>,
    desktop_msg: Option<String>,
    /// No display can be captured: input waits until one can.
    capture_unavailable: bool,
}

impl Desk {
    pub fn new(p: Params) -> Desk {
        let Params { session, offer, view, platform, view_only, clip_share, share } = p;
        Desk {
            mid: media::video_mid(&offer),
            offer,
            share,
            platform: platform.clone(),
            view_only,
            lossy: None,
            file_ch: None,
            clip: clipboard::Sync::new(clip_share),
            input: crate::input::Decoder::default(),
            files: FileQueue::default(),
            cleanup: Cleanup { platform, session: session.clone(), started: false },
            id: session,
            pt: None,
            view: view.map_or((0, 0, 0, 0, 0), |v| (v.w, v.h, v.max_w, v.max_h, v.fps)),
            level: 0,
            ceiling: START_BITRATE,
            sent_bitrate: START_BITRATE,
            estimated_bitrate: START_BITRATE,
            pace: pace::Pace::default(),
            connected: false,
            needs_first_idr: true,
            displays_msg: None,
            layout_msg: None,
            desktop_msg: None,
            capture_unavailable: false,
        }
    }

    /// Carries out what the clipboard state asks: messages to the browser, calls to the platform.
    fn clipboard_do(&self, cx: &mut Cx, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Browser(c) => send_message(cx.rtc, cx.control(), &c),
                Out::Platform(Act::Share(on)) => self.platform.clipboard_share(&self.id, on),
                Out::Platform(Act::Announce { gen, formats }) => self.platform.clipboard_announce(&self.id, gen, formats),
                Out::Platform(Act::Provide { gen, format, text, html }) => self.platform.clipboard_provide(&self.id, gen, format, text, html),
                Out::Platform(Act::Read { gen, format, id, i }) => self.platform.clipboard_read(&self.id, gen, format, id, i),
            }
        }
    }

    fn on_file_channel(&mut self, rtc: &mut Rtc, d: &ChannelData) {
        if let Some(id) = channels::cancelled(&d.data, d.binary) {
            // Clipboard data on its way to the browser stops here too.
            self.files.cancel(&id);
        }
        // Watching only: nothing but clipboard answers reaches this computer.
        let refusal = if self.view_only { channels::refuse_file(&d.data, d.binary) } else { None };
        if let Some(refusal) = refusal {
            self.files.push(refusal);
            self.files.flush(rtc, self.file_ch);
        } else if d.binary {
            self.platform.file_data(&self.id, Bytes(d.data.to_vec()));
        } else if let Ok(text) = std::str::from_utf8(&d.data) {
            self.platform.file_message(&self.id, text.to_string());
        }
    }

    /// A message on the "input" channel (`control`) or the "move" channel. True once the
    /// session is over.
    fn on_message(&mut self, cx: &mut Cx, m: Incoming, control: bool) -> bool {
        match m {
            // Clipboard messages follow sharing, not view-only or the display state; they count
            // only while the session is Active.
            Incoming::Control(c) if control && channels::is_clipboard(&c) => {
                if cx.active() {
                    self.on_clipboard(cx, c);
                }
            }
            // The browser's goodbye, with its reason.
            Incoming::Control(Control::End { reason, .. }) if control => return cx.signal(Signal::PeerEnded(reason)),
            Incoming::Control(Control::ChooseDisplay { id }) if !id.is_empty() => self.platform.choose_display(&self.id, id),
            Incoming::Control(Control::LocalCursor { on }) => self.platform.local_cursor(&self.id, on),
            Incoming::Control(Control::View { w, h, max_w, max_h, fps }) => {
                cx.log(format_args!("view {w}x{h}, decodes {max_w}x{max_h} at {fps}"));
                self.view = (w, h, max_w, max_h, fps);
                self.platform.view_constraints(&self.id, w, h, max_w, max_h, fps, self.level);
            }
            // The browser lost the picture while no display was attached.
            Incoming::Control(Control::CaptureKeyframe {}) if control => self.platform.keyframe(&self.id),
            // Input (keys, sync, kc, moves) reaches the computer only while Active.
            m if !self.view_only && !self.capture_unavailable && cx.active() => match m {
                Incoming::Move(m) => self.input.pointer(m, &self.id, &*self.platform),
                Incoming::Control(c) => self.input.control(c, &self.id, &*self.platform),
            },
            _ => {}
        }
        false
    }

    fn adjust_bitrate(&mut self, cx: &Cx) {
        let target = pace::video_budget(self.estimated_bitrate, self.ceiling);
        // Apply reductions promptly; ignore small upward oscillations.
        if target < self.sent_bitrate || target.saturating_sub(self.sent_bitrate) > self.sent_bitrate / 10 {
            cx.log(format_args!("bitrate {} kbps", target / 1000));
            self.sent_bitrate = target;
            self.platform.bitrate(&self.id, target);
        }
    }

    fn on_clipboard(&mut self, cx: &mut Cx, c: Control) {
        let outs = match c {
            Control::ClipShare { on } => {
                let _ = self.share.send(on);
                Vec::new()
            }
            Control::Clipboard { formats, id, .. } => self.clip.browser_announced(formats.into_iter().map(Into::into).collect(), id),
            Control::ClipboardRequest { gen, format, id, i } => self.clip.browser_request(gen, format, id, i),
            Control::ClipboardData { gen, format, id, text, html, size } => self.clip.browser_data(gen, format, &id, text, html, size),
            Control::ClipboardFailed { gen, id, .. } => self.clip.browser_failed(gen, &id),
            _ => Vec::new(),
        };
        self.clipboard_do(cx, outs);
    }

    /// One encoded access unit into the video track.
    fn write_frame(&mut self, cx: &mut Cx, data: Vec<u8>) {
        // A capture started before DTLS can still have delta frames on the way when we request
        // the first keyframe. They cannot start a new decoder.
        if !self.connected || (self.needs_first_idr && !media::has_idr(&data)) {
            return;
        }
        let (Some(m), Some(payload)) = (self.mid, self.pt) else { return };
        let started = cx.started();
        let Some(w) = cx.rtc.writer(m) else { return };
        let now = Instant::now();
        let ticks = (now - started).as_micros() as u64 * 9 / 100;
        match w.write(payload, now, MediaTime::from_90khz(ticks), data) {
            Ok(()) => self.needs_first_idr = false,
            Err(e) => eprintln!("video write failed: {e}"),
        }
    }
}

impl Role for Desk {
    // Bandwidth estimation also turns on pacing, which spreads each frame out over time.
    fn bandwidth(&self) -> Option<u32> {
        Some(START_BITRATE)
    }

    fn releaser(&self) -> Box<dyn Fn() + Send> {
        let (platform, id) = (self.platform.clone(), self.id.clone());
        Box::new(move || platform.release_all(&id))
    }

    fn channel_open(&mut self, cx: &mut Cx, id: ChannelId, label: &str) -> bool {
        match label {
            "file" => {
                self.file_ch = Some(id);
                if let Some(mut ch) = cx.rtc.channel(id) {
                    ch.set_buffered_amount_low_threshold(channels::FILE_AHEAD / 4);
                }
                false
            }
            "move" => {
                self.lossy = Some(id);
                false
            }
            "input" => true,
            _ => false,
        }
    }

    fn control_open(&mut self, cx: &mut Cx) {
        for m in [&self.displays_msg, &self.layout_msg, &self.desktop_msg].into_iter().flatten() {
            send(cx.rtc, cx.control(), m);
        }
        if self.capture_unavailable {
            send_message(cx.rtc, cx.control(), &channels::capture_status(false));
        }
        let outs = self.clip.start();
        self.clipboard_do(cx, outs);
    }

    fn channel_data(&mut self, cx: &mut Cx, d: ChannelData) -> bool {
        if self.file_ch == Some(d.id) {
            self.on_file_channel(cx.rtc, &d);
            return false;
        }
        let control = cx.control() == Some(d.id);
        match channels::incoming(&d.data) {
            Some(m) => self.on_message(cx, m, control),
            None => false,
        }
    }

    fn rtc_event(&mut self, cx: &mut Cx, e: Event) {
        match e {
            Event::Connected => {
                self.connected = true;
                self.platform.keyframe(&self.id);
            }
            Event::KeyframeRequest(_) => self.platform.keyframe(&self.id),
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                self.estimated_bitrate = estimate.as_u64().min(u64::from(u32::MAX)) as u32;
                self.adjust_bitrate(cx);
            }
            Event::ChannelBufferedAmountLow(id) if self.file_ch == Some(id) => self.files.flush(cx.rtc, self.file_ch),
            _ => {}
        }
    }

    fn platform(&mut self, cx: &mut Cx, e: PlatformEvent) {
        match e {
            PlatformEvent::Frame(Bytes(data)) => self.write_frame(cx, data),
            PlatformEvent::Capturing { list, current } => {
                self.platform.pointer_display(&self.id, current.clone());
                let msg = serde_json::to_string(&channels::displays_message(&list, &current.id)).expect("displays serialize");
                send(cx.rtc, cx.control(), &msg);
                self.displays_msg = Some(msg);
            }
            PlatformEvent::EncodedSize { w, h, max_bitrate } => {
                cx.log(format_args!("encoding {w}x{h}, up to {} kbps", max_bitrate / 1000));
                self.ceiling = max_bitrate;
                self.adjust_bitrate(cx);
                cx.rtc.bwe().set_bitrate(Bitrate::bps(0), Bitrate::bps(self.ceiling as u64));
            }
            PlatformEvent::FileMessage(t) => {
                if let Some(id) = channels::transfer_ended(&t) {
                    self.clip.transfer_ended(&id);
                }
                self.files.push(t);
                self.files.flush(cx.rtc, self.file_ch);
            }
            PlatformEvent::FileData(Bytes(b)) => {
                self.files.push_bytes(b);
                self.files.flush(cx.rtc, self.file_ch);
            }
            PlatformEvent::ClipboardChanged { formats, text } => {
                let outs = self.clip.host_changed(formats, text);
                self.clipboard_do(cx, outs);
            }
            PlatformEvent::ClipboardAnnounced { gen, why } => {
                let outs = self.clip.announced(gen, why);
                self.clipboard_do(cx, outs);
            }
            PlatformEvent::ClipboardRender { gen, format } => {
                // A browser that cannot hear now (recovering) cannot answer: the paste gets nothing.
                let outs = if cx.active() { self.clip.render(gen, format) } else { self.clip.decline(gen, format) };
                self.clipboard_do(cx, outs);
            }
            PlatformEvent::ClipboardData { gen, format, id, text, html, size, .. } => {
                let outs = self.clip.read_done(gen, format, id, text, html, size);
                self.clipboard_do(cx, outs);
            }
            PlatformEvent::ClipboardFailed { gen, id, why } => {
                let outs = self.clip.read_failed(gen, id, why);
                self.clipboard_do(cx, outs);
            }
            e => {
                if let PlatformEvent::CaptureAvailable(available) = e {
                    if !available {
                        self.platform.release_all(&self.id);
                        self.displays_msg = None;
                    }
                    self.capture_unavailable = !available;
                }
                if let Some((r, text)) = channels::browser_message(&e) {
                    let control = cx.control();
                    send(cx.rtc, if r { control } else { self.lossy.or(control) }, &text);
                    match e {
                        PlatformEvent::LayoutChanged { .. } => self.layout_msg = Some(text),
                        PlatformEvent::Desktop(_) => self.desktop_msg = Some(text),
                        _ => {}
                    }
                }
            }
        }
    }

    fn clip_share(&mut self, cx: &mut Cx, on: bool) {
        let outs = self.clip.set_share(on);
        self.clipboard_do(cx, outs);
    }

    /// The accepted offer settles the payload type: capture and the encoder start now, at the
    /// offer's view, while ICE and DTLS are still being set up. Frames are written once
    /// connected, starting from a keyframe.
    fn after_output(&mut self, cx: &mut Cx) {
        if self.pt.is_some() {
            return;
        }
        let Some(p) = self.mid.and_then(|m| media::choose_payload(cx.rtc, m)) else { return };
        self.pt = Some(p);
        self.level = ProfileLevelId::from_sdp(&self.offer, *p).map_or(0, |id| id.level.idc);
        cx.log(format_args!("payload {}, level {}", *p, self.level));
        let (w, h, max_w, max_h, fps) = self.view;
        self.cleanup.started = true;
        self.platform.video_start(self.id.clone(), crate::video::Constraints { w, h, max_w, max_h, fps, level: self.level });
    }

    /// Real time over completeness: a send queue that fills up holds the encoder until it
    /// drains. While behind, the queue is looked at often, so encoding resumes as soon as it does.
    fn before_wait(&mut self, cx: &mut Cx, queued: usize) -> Option<Instant> {
        let now = Instant::now();
        let rtp = self.mid.and_then(|mid| {
            let mut api = cx.rtc.direct_api();
            let info = api.stream_tx_by_mid(mid, None)?.queue_info()?;
            Some((info.byte_size(), info.first_unsent()))
        });
        let (rtp_bytes, oldest) = rtp.unwrap_or_default();
        let age = oldest.map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        let behind = self.pace.update(queued.saturating_add(rtp_bytes), self.sent_bitrate, age);
        if let Some(behind) = behind {
            cx.log(format_args!("{}", if behind { "network behind: encoding waits" } else { "network caught up" }));
            self.platform.network_behind(&self.id, behind);
        }
        self.pace.behind().then(|| now + Duration::from_millis(20))
    }

    fn after_event(&mut self, cx: &mut Cx) {
        self.files.flush(cx.rtc, self.file_ch);
    }
}
