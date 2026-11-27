//! The platform in this process: the shared pipelines on top of this computer's native pieces.
//!
//! It is the one place that decides which session's acts reach the computer (see the module
//! docs of `platform`). Two sessions may hold it side by side: the person's, which started video
//! last, with all of a session's acts; and the AI client's (`ai_session`), with input only. A
//! call `held` by any other session is ignored.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use super::{Call, Forward, Platform, PlatformEvent};
use crate::clipboard::{self, Data, Native};
use crate::input::Hand;
use crate::{input, transfer, video};

pub struct Local {
    video: Sender<video::Command>,
    input: Sender<(Hand, input::Command)>,
    files: Sender<transfer::Command>,
    clipboard: Arc<super::NativeClipboard>,
    events: Sender<PlatformEvent>,
    state: Mutex<State>,
    /// The system's notice that displays changed, passed to the capture loop.
    _displays: super::DisplayWatch,
}

#[derive(Default)]
struct State {
    /// The session that holds the computer: the one that started video last and has not
    /// stopped. All its acts are carried out.
    owner: Option<String>,
    /// The AI client's session beside it: its input is carried out, nothing else of it.
    ai: Option<String>,
    /// Lock the computer when a session ends without its controlling side (`Drop`).
    auto_lock: bool,
}

/// Who holds the computer: `session` takes it; true if another one held it before.
fn take(owner: &mut Option<String>, session: String) -> bool {
    let replaced = owner.as_deref().is_some_and(|o| o != session);
    *owner = Some(session);
    replaced
}

/// Whose `call` it is, when it is carried out: a session's act only from the session holding the
/// computer, or input from the AI client's; what concerns the whole computer always (as the
/// person's, which has no effect for calls that are no one's).
fn obeyed(owner: Option<&str>, ai: Option<&str>, call: &Call) -> Option<Hand> {
    match call.held_by() {
        None => Some(Hand::Person),
        Some(s) if owner == Some(s) => Some(Hand::Person),
        Some(s) if ai == Some(s) && call.is_input() => Some(Hand::Ai),
        Some(_) => None,
    }
}

impl Local {
    /// Starts the pipelines. They report through `events`, and end when this is dropped.
    pub fn new(events: Sender<PlatformEvent>) -> Local {
        // Capture observes applied input independently of the session transport.
        let applied = input::AppliedInput::default();
        let video = video::spawn::<super::NativeCapture, super::NativeEncoder>(events.clone(), applied.clone());
        let to_video = Mutex::new(video.clone());
        let displays = super::DisplayWatch::start(move || {
            let _ = to_video.lock().unwrap().send(video::Command::DisplaysChanged);
        });
        let clipboard = Arc::new(super::NativeClipboard::start(events.clone()));
        Local {
            video,
            input: input::spawn(super::NativeInput::new, events.clone(), applied),
            files: transfer::spawn(super::NativeFiles::new, clipboard.clone(), events.clone()),
            clipboard,
            events,
            state: Mutex::new(State::default()),
            _displays: displays,
        }
    }

    /// What a session leaves behind when it gives the computer up, or loses it to another.
    fn clear_session(&self) {
        let _ = self.input.send((Hand::Person, input::Command::ReleaseAll));
        let _ = self.files.send(transfer::Command::Stop);
        let _ = self.video.send(video::Command::LocalCursor(false));
        // A paste waiting for that session's browser gets nothing now; its placeholders go.
        self.clipboard.share(false);
    }

    /// Reads one clipboard format for the browser, off the caller's thread: reading a file can
    /// take a while.
    fn clipboard_read(&self, gen: u64, format: wire::types::FormatKind, id: String, i: Option<u32>) {
        let (native, events, files) = (self.clipboard.clone(), self.events.clone(), self.files.clone());
        std::thread::spawn(move || {
            let e = match native.read(format, i) {
                Ok(clipboard::Read::Text(text)) => PlatformEvent::ClipboardData { gen, format, id, name: None, text: Some(text), html: None, size: None },
                Ok(clipboard::Read::Html(html)) => PlatformEvent::ClipboardData { gen, format, id, name: None, text: None, html: Some(html), size: None },
                Ok(clipboard::Read::Bytes { name, mime, data }) => {
                    let size = Some(data.len() as u64);
                    let e = PlatformEvent::ClipboardData { gen, format, id: id.clone(), name: Some(name.clone()), text: None, html: None, size };
                    let _ = events.send(e);
                    let _ = files.send(transfer::Command::Send { id, gen, name, mime, data });
                    return;
                }
                Err(why) => PlatformEvent::ClipboardFailed { gen, id, why },
            };
            let _ = events.send(e);
        });
    }
}

/// Whatever drove this platform is gone (the Windows network service stopped, or the host it
/// ran for ended) without ending the session: the session ends here, and "lock when it ends"
/// still holds.
impl Drop for Local {
    fn drop(&mut self) {
        let (owner, auto_lock) = {
            let s = self.state.lock().unwrap();
            (s.owner.clone(), s.auto_lock)
        };
        if let Some(session) = owner {
            if auto_lock {
                eprintln!("the controlling side is gone during a session; locking");
                super::lock_screen();
            }
            self.video_stop(&session);
        }
    }
}

impl Forward for Local {
    /// Carries out a call. The state stays locked throughout, so a session that takes the
    /// computer is in place before anything after it is weighed.
    fn forward(&self, call: Call) {
        let mut s = self.state.lock().unwrap();
        let Some(hand) = obeyed(s.owner.as_deref(), s.ai.as_deref(), &call) else { return };
        let input = |c: input::Command| {
            let _ = self.input.send((hand, c));
        };
        match call {
            Call::VideoStart { session, view, display } => {
                if take(&mut s.owner, session) {
                    self.clear_session();
                }
                // While a session runs the computer neither sleeps nor turns its display off.
                crate::power::session(true);
                // Each session starts from the computer's own key state, not the last session's.
                input(input::Command::Reset);
                let _ = self.video.send(video::Command::Start { view, display });
            }
            Call::VideoStop { .. } => {
                s.owner = None;
                self.clear_session();
                crate::power::session(false);
                let _ = self.video.send(video::Command::Stop);
            }
            Call::Keyframe { .. } => {
                let _ = self.video.send(video::Command::Keyframe);
            }
            Call::NetworkBehind { behind, .. } => {
                let _ = self.video.send(video::Command::Behind(behind));
            }
            Call::Bitrate { bps, .. } => {
                let _ = self.video.send(video::Command::Bitrate(bps));
            }
            Call::ViewConstraints { w, h, max_w, max_h, fps, level, .. } => {
                let _ = self.video.send(video::Command::View(video::Constraints { w, h, max_w, max_h, fps, level }));
            }
            Call::LocalCursor { on, .. } => {
                let _ = self.video.send(video::Command::LocalCursor(on));
            }
            Call::ChooseDisplay { id, .. } => {
                let _ = self.video.send(video::Command::Display(id));
            }
            Call::Thumbnails { on } => {
                let _ = self.video.send(video::Command::Thumbs(on));
            }
            Call::AiSession { session } => {
                // An AI client that leaves, or gives way to another, lets go of what it held.
                if s.ai.is_some() && s.ai != session {
                    let _ = self.input.send((Hand::Ai, input::Command::ReleaseAll));
                }
                s.ai = session;
            }
            Call::Snapshot { id, display, max } => {
                let _ = self.video.send(video::Command::Snapshot { id, display, max: max as usize });
            }

            Call::MouseMove { x, y, seq, .. } => input(input::Command::Move(x, y, seq)),
            Call::MouseMoveBy { dx, dy, seq, .. } => input(input::Command::MoveBy(dx, dy, seq)),
            Call::MouseButton { button, down, .. } => input(input::Command::Button(button, down)),
            Call::Wheel { dx, dy, .. } => input(input::Command::Wheel(dx, dy)),
            Call::Key { code, down, .. } => input(input::Command::Key(code, down)),
            Call::Char { ch, mods, .. } => input(input::Command::Char(ch, mods)),
            Call::Text { text, .. } => input(input::Command::Text(text)),
            Call::SyncKeys { locks, mods, .. } => input(input::Command::Sync(locks, mods)),
            Call::PointerDisplay { display, .. } => input(input::Command::Display(display)),
            Call::ReleaseAll { .. } => input(input::Command::ReleaseAll),
            Call::InputDone { id, .. } => input(input::Command::Done(id)),

            Call::ClipboardShare { on, .. } => self.clipboard.share(on),
            Call::ClipboardAnnounce { gen, formats, .. } => self.clipboard.announce(gen, formats),
            Call::ClipboardProvide { gen, text, html, .. } => self.clipboard.provide(gen, text.map(Data::Text).or(html.map(Data::Html))),
            Call::ClipboardRead { gen, format, id, i, .. } => self.clipboard_read(gen, format, id, i),

            Call::FileMessage { text, .. } => {
                let _ = self.files.send(transfer::Command::Message(text));
            }
            Call::FileData { data, .. } => {
                let _ = self.files.send(transfer::Command::Data(data.0));
            }

            Call::Lock {} => super::lock_screen(),
            Call::AutoLock { on } => s.auto_lock = on,
            Call::AutoStart { on } => {
                if let Err(e) = super::set_autostart(on) {
                    eprintln!("cannot change automatic start: {e:#}");
                }
            }
            Call::Language { code } => crate::i18n::set(code.as_deref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_that_starts_video_last_holds_the_computer() {
        let mut owner = None;
        assert!(!take(&mut owner, "old".into()));
        // A takeover: the new session starts before the old one's cleanup runs.
        assert!(take(&mut owner, "new".into()), "the old session loses the computer");
        assert!(!take(&mut owner, "new".into()), "taking it again replaces nobody");
        assert_eq!(owner.as_deref(), Some("new"));
    }

    #[test]
    fn acts_of_a_session_not_holding_the_computer_are_ignored() {
        let act = |session: &str| [Call::ReleaseAll { session: session.into() }, Call::NetworkBehind { session: session.into(), behind: true }, Call::Key { session: session.into(), code: "KeyA".into(), down: true }, Call::VideoStop { session: session.into() }];
        // A replaced session's late acts (its network backed up) never reach its successor.
        for c in act("old") {
            assert_eq!(obeyed(Some("new"), Some("ai"), &c), None, "{c:?}");
            assert_eq!(obeyed(None, None, &c), None, "nobody holds it: {c:?}");
        }
        for c in act("new") {
            assert_eq!(obeyed(Some("new"), Some("ai"), &c), Some(Hand::Person), "{c:?}");
        }
        // What concerns the whole computer is not a session's.
        assert!(obeyed(Some("new"), None, &Call::VideoStart { session: "next".into(), view: Default::default(), display: None }).is_some());
        assert!(obeyed(Some("new"), None, &Call::AutoLock { on: true }).is_some());
        assert!(obeyed(None, None, &Call::Thumbnails { on: true }).is_some());
    }

    /// The AI client's session beside the person's: its input is carried out as the AI client's
    /// hand, nothing else of it (it never stops the person's picture or touches the clipboard).
    #[test]
    fn the_ai_clients_session_makes_input_and_nothing_else() {
        let ai = |c: Call| obeyed(Some("person"), Some("ai"), &c);
        for c in [
            Call::Key { session: "ai".into(), code: "KeyA".into(), down: true },
            Call::MouseMove { session: "ai".into(), x: 0.5, y: 0.5, seq: 0 },
            Call::Text { session: "ai".into(), text: "hi".into() },
            Call::ReleaseAll { session: "ai".into() },
            Call::InputDone { session: "ai".into(), id: "q1".into() },
        ] {
            assert_eq!(ai(c.clone()), Some(Hand::Ai), "{c:?}");
        }
        for c in [Call::VideoStop { session: "ai".into() }, Call::Keyframe { session: "ai".into() }, Call::ClipboardShare { session: "ai".into(), on: true }, Call::SyncKeys { session: "ai".into(), locks: wire::types::Locks { caps: false, num: false, scroll: false }, mods: crate::input::no_mods() }] {
            assert_eq!(ai(c.clone()), None, "{c:?}");
        }
        assert_eq!(obeyed(Some("person"), None, &Call::Key { session: "ai".into(), code: "KeyA".into(), down: true }), None, "an AI client that left");
    }
}
