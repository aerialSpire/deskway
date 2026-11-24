//! Browser input into the OS.
//!
//! The session turns browser messages into `Platform` calls ([`Decoder`]); the platform side
//! injects them on a thread of their own ([`spawn`]), tracking what is held so a session that
//! ends any way can never leave a key or mouse button stuck. Keys travel as positions and are
//! looked up in one table ([`keys`]); toggle keys and modifiers are synchronized from the
//! browser's state ([`plan_sync`]); characters are typed with the host's own layout
//! ([`resolve`]), whose key labels the host reports ([`labels`]).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use crate::display::DisplayInfo;
use crate::platform::{Platform, PlatformEvent};
use wire::control::Control;
use wire::moves::Move;
use wire::types::{Locks, Mods};

pub mod keys;
pub use keys::Key;

/// The capabilities this input module gives a host that takes input, for its `hello` (which
/// the session lifecycle sends).
pub const CAPS: &[&str] = &[wire::caps::RELMOUSE, wire::caps::LAYOUT, wire::caps::CHARS];

fn unit(v: f64) -> f64 {
    // The comparison form also maps NaN to 0.
    if !(v > 0.0) {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

pub const fn no_mods() -> Mods {
    Mods { shift: false, ctrl: false, alt: false, meta: false }
}

/// Browser input messages to platform calls, for one session.
#[derive(Default)]
pub struct Decoder {
    last_move: u64,
    /// Pointer Lock: moves are relative, and buttons do not move the pointer to their place.
    relative: bool,
}

impl Decoder {
    /// A move, from either channel. Moves are the same on both: the last move of a motion comes
    /// again on the reliable one, and the sequence drops it if the copy on the move channel
    /// arrived, or applies it if that was lost, so pointer reports always catch up. The calls
    /// are acts of `session`.
    pub fn pointer(&mut self, m: Move, session: &str, p: &dyn Platform) {
        match m {
            // Moves use an unordered channel; an older one must not jump the cursor back.
            Move::To { seq, .. } | Move::By { seq, .. } if seq != 0 && seq <= self.last_move => {}
            Move::To { x, y, seq } => {
                self.last_move = seq;
                self.relative = false;
                p.mouse_move(session, unit(x), unit(y), seq);
            }
            Move::By { dx, dy, seq } if dx.is_finite() && dy.is_finite() => {
                self.last_move = seq;
                self.relative = true;
                p.mouse_move_by(session, dx, dy, seq);
            }
            _ => {}
        }
    }

    /// A control message; anything but input is ignored. The calls are acts of `session`.
    pub fn control(&mut self, c: Control, session: &str, p: &dyn Platform) {
        match c {
            Control::Button { button, down, x, y } if button <= 2 => {
                // Buttons carry the position: the move before them may have been lost. Under
                // Pointer Lock the pointer stays where relative moves put it.
                if !self.relative {
                    p.mouse_move(session, unit(x), unit(y), self.last_move);
                }
                p.mouse_button(session, button, down);
            }
            Control::Wheel { dx, dy } if dx.is_finite() && dy.is_finite() => p.wheel(session, dx, dy),
            Control::Key { code, down } if !code.is_empty() => p.key(session, code, down),
            Control::Char { ch, mods } if ch.chars().count() == 1 => p.char_key(session, ch, mods),
            Control::Text { text } if !text.is_empty() => p.text(session, text),
            Control::Sync { locks, mods } => p.sync_keys(session, locks, mods),
            _ => {}
        }
    }
}

/// Information shared by the input injector and desktop capture thread.
#[derive(Clone, Default)]
pub struct AppliedInput {
    sequence: Arc<AtomicU64>,
    interaction: Arc<AtomicBool>,
}

impl AppliedInput {
    pub fn move_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    /// Coalesce clicks and keystrokes received between two screen captures.
    pub fn take_interaction(&self) -> bool {
        self.interaction.swap(false, Ordering::AcqRel)
    }

    fn set_move(&self, seq: u64) {
        self.sequence.store(seq, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    Caps,
    Num,
    Scroll,
}

/// Toggle keys and modifiers as the OS has them now; `None`: this platform has no such state
/// (a Mac has no Num Lock) or cannot tell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyState {
    pub caps: Option<bool>,
    pub num: Option<bool>,
    pub scroll: Option<bool>,
    pub mods: Option<Mods>,
}

/// What a key types under some modifiers, in the host's layout. `dead`: it only marks the next
/// character (an accent) and types nothing by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typed {
    pub text: String,
    pub dead: bool,
}

/// The native injector.
pub trait Backend {
    /// A position on the pointer display, 0..1 each way.
    fn mouse_move(&mut self, x: f64, y: f64);
    /// Relative motion in pixels (Pointer Lock).
    fn mouse_move_by(&mut self, dx: f64, dy: f64);
    fn mouse_button(&mut self, button: u8, down: bool);
    fn wheel(&mut self, dx: f64, dy: f64);
    /// False when this platform has no such key. `repeat`: the key is held and repeats.
    fn key(&mut self, key: &Key, down: bool, repeat: bool) -> bool;
    fn text(&mut self, s: &str);
    fn key_state(&mut self) -> KeyState;
    /// Turns a toggle key on or off.
    fn set_lock(&mut self, lock: Lock, on: bool);
    /// Identifies the keyboard layout in use, cheaply: a new value means a new layout.
    fn layout_id(&mut self) -> Option<u64>;
    /// The layout's name for people, such as en-US.
    fn layout_name(&mut self) -> String;
    /// What `key` types with `mods` in the current layout.
    fn translate(&mut self, key: &Key, mods: &Mods) -> Option<Typed>;
    /// The display that relative coordinates refer to.
    fn set_display(&mut self, _d: &DisplayInfo) {}
    /// Called before each event: Windows moves the thread to the desktop that has input.
    fn follow_input_desktop(&mut self) {}
    /// A secure desktop has input now (UAC and the like), where an AI client's input never goes.
    fn secure_desktop(&self) -> bool {
        false
    }
}

pub enum Command {
    Move(f64, f64, u64),
    MoveBy(f64, f64, u64),
    Button(u8, bool),
    Wheel(f64, f64),
    Key(String, bool),
    Char(String, Mods),
    Text(String),
    Sync(Locks, Mods),
    Display(DisplayInfo),
    ReleaseAll,
    /// A new session: input starts from the computer's current state, and the layout is
    /// reported afresh.
    Reset,
    /// Everything before it has been injected: answered `InputDone`, once the program in front
    /// has had a moment to handle it.
    Done(String),
}

/// How long the program in front is given to handle what was just injected, before
/// `Command::Done` is answered: typing, then looking at once, shows all of it.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(60);

// ---- pure decisions ----

const CLASSES: [(fn(&Mods) -> bool, [&str; 2]); 4] =
    [(|m| m.shift, keys::SHIFT), (|m| m.ctrl, keys::CONTROL), (|m| m.alt, keys::ALT), (|m| m.meta, keys::META)];

/// The key presses that bring the modifiers to `want`, given what the OS reports and what this
/// injector holds. A modifier wanted and not down is pressed (left key); one not wanted is
/// released: the keys this injector holds, or both keys when the OS has it down by itself.
pub fn plan_sync(state: &KeyState, held: &HashSet<String>, want: &Mods) -> Vec<(&'static str, bool)> {
    let mut out = Vec::new();
    for (get, codes) in CLASSES {
        let os = state.mods.as_ref().map(get);
        let ours: Vec<&'static str> = codes.iter().copied().filter(|c| held.contains(*c)).collect();
        if get(want) {
            if ours.is_empty() && os != Some(true) {
                out.push((codes[0], true));
            }
        } else if !ours.is_empty() {
            out.extend(ours.into_iter().map(|c| (c, false)));
        } else if os == Some(true) {
            out.extend(codes.iter().map(|c| (*c, false)));
        }
    }
    out
}

/// The toggle keys to change so they match `want`. States the OS does not know are left.
pub fn plan_locks(state: &KeyState, want: &Locks) -> Vec<(Lock, bool)> {
    [(Lock::Caps, state.caps, want.caps), (Lock::Num, state.num, want.num), (Lock::Scroll, state.scroll, want.scroll)]
        .into_iter()
        .filter_map(|(lock, now, want)| now.filter(|&n| n != want).map(|_| (lock, want)))
        .collect()
}

/// The modifier combinations tried for a character, simplest first: plain, Shift, AltGr
/// (Ctrl+Alt on Windows), and Option (Alt on a Mac), each also with Shift.
fn layers() -> [Mods; 6] {
    let m = |shift, ctrl, alt| Mods { shift, ctrl, alt, meta: false };
    [m(false, false, false), m(true, false, false), m(false, true, true), m(true, true, true), m(false, false, true), m(true, false, true)]
}

/// The key and modifiers that type `ch` in the layout `translate` describes; the first key in
/// table order wins. Keypad keys are left out: what they type depends on Num Lock.
pub fn resolve(ch: char, mut translate: impl FnMut(&Key, &Mods) -> Option<Typed>) -> Option<(&'static Key, Mods)> {
    let mut buf = [0u8; 4];
    let want = ch.encode_utf8(&mut buf);
    for mods in layers() {
        for key in keys::KEYS.iter().filter(|k| !k.code.starts_with("Numpad")) {
            if translate(key, &mods).is_some_and(|t| !t.dead && t.text == *want) {
                return Some((key, mods));
            }
        }
    }
    None
}

/// The presses that type `key` with the modifiers the layout needs and those the browser
/// holds: modifiers not already down go down first and come up last, in reverse.
pub fn char_presses(key: &'static Key, need: &Mods, user: &Mods, held: &HashSet<String>) -> Vec<(&'static str, bool)> {
    let mut mods = Vec::new();
    for (get, codes) in CLASSES {
        if (get(need) || get(user)) && !codes.iter().any(|c| held.contains(*c)) {
            mods.push(codes[0]);
        }
    }
    let mut out: Vec<(&'static str, bool)> = mods.iter().map(|c| (*c, true)).collect();
    out.push((key.code, true));
    out.push((key.code, false));
    out.extend(mods.iter().rev().map(|c| (*c, false)));
    out
}

/// A label worth drawing on a key: visible characters, not control or private-use ones (Mac
/// function keys type those).
fn printable(t: &Typed) -> bool {
    !t.text.is_empty() && t.text.chars().all(|c| !c.is_control() && !c.is_whitespace() && !('\u{e000}'..='\u{f8ff}').contains(&c))
}

pub use wire::types::Labels;

/// Each key's label without and with Shift, in the layout `translate` describes.
pub fn labels(mut translate: impl FnMut(&Key, &Mods) -> Option<Typed>) -> (Labels, Labels) {
    let shift = Mods { shift: true, ..no_mods() };
    let (mut base, mut shifted) = (Labels::new(), Labels::new());
    for key in keys::KEYS {
        for (mods, out) in [(no_mods(), &mut base), (shift.clone(), &mut shifted)] {
            if let Some(t) = translate(key, &mods).filter(printable) {
                out.insert(key.code.to_string(), t.text);
            }
        }
    }
    (base, shifted)
}

// ---- the injector ----

/// Whose input it is: the person's session on the desk seat, or the AI client's on the AI seat
/// (docs/architecture.md, "Input apart per session"). Both reach the computer; each keeps
/// what it holds and the display its moves refer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Hand {
    Person,
    Ai,
}

/// What one hand holds.
#[derive(Default)]
struct Held {
    keys: HashSet<String>,
    buttons: HashSet<u8>,
    /// The display its moves refer to.
    display: Option<DisplayInfo>,
}

/// Tracks what each hand holds, so it can be lifted, and reports the layout.
pub struct Injector<B: Backend> {
    backend: B,
    hands: std::collections::HashMap<Hand, Held>,
    /// The display the backend has now, by id.
    shown: Option<String>,
    /// Codes this host has no key for, each logged once.
    unknown: HashSet<String>,
    /// The layout last reported.
    layout: Option<u64>,
    events: Sender<PlatformEvent>,
    applied: AppliedInput,
}

impl<B: Backend> Injector<B> {
    pub fn new(backend: B, events: Sender<PlatformEvent>, applied: AppliedInput) -> Self {
        Injector { backend, hands: Default::default(), shown: None, unknown: HashSet::new(), layout: None, events, applied }
    }

    fn held(&mut self, hand: Hand) -> &mut Held {
        self.hands.entry(hand).or_default()
    }

    /// Whether a hand other than `hand` holds key `code` (it stays down while one does).
    fn other_holds_key(&self, hand: Hand, code: &str) -> bool {
        self.hands.iter().any(|(h, held)| *h != hand && held.keys.contains(code))
    }

    fn other_holds_button(&self, hand: Hand, b: u8) -> bool {
        self.hands.iter().any(|(h, held)| *h != hand && held.buttons.contains(&b))
    }

    /// Moves refer to the hand's own display: the backend is switched to it when it is not.
    fn use_display(&mut self, hand: Hand) {
        let Some(d) = self.held(hand).display.clone() else { return };
        if self.shown.as_deref() != Some(d.id.as_str()) {
            self.backend.set_display(&d);
            self.shown = Some(d.id);
        }
    }

    pub fn apply(&mut self, hand: Hand, c: Command, make: fn() -> B) {
        self.backend.follow_input_desktop();
        // An AI client never answers a secure prompt (UAC and the like): its input is held
        // back here, where it would land, whatever let it through before (docs/security.md).
        if hand == Hand::Ai && self.backend.secure_desktop() && !matches!(c, Command::ReleaseAll | Command::Done(_)) {
            return;
        }
        let interaction = hand == Hand::Person && matches!(c,
            Command::Button(..) | Command::Key(_, true) | Command::Char(..) | Command::Text(_));
        match c {
            Command::Move(x, y, seq) => {
                self.use_display(hand);
                self.backend.mouse_move(x, y);
                if hand == Hand::Person {
                    self.applied.set_move(seq);
                }
            }
            Command::MoveBy(dx, dy, seq) => {
                self.use_display(hand);
                self.backend.mouse_move_by(dx, dy);
                if hand == Hand::Person {
                    self.applied.set_move(seq);
                }
            }
            Command::Button(b, d) => self.button(hand, b, d),
            Command::Wheel(dx, dy) => self.backend.wheel(dx, dy),
            Command::Key(c, d) => {
                self.check_layout();
                self.key(hand, &c, d);
            }
            Command::Char(ch, mods) => {
                self.check_layout();
                self.char(hand, &ch, &mods);
            }
            // A line end is one, however it is written: CRLF typed as two would give two lines.
            Command::Text(t) => self.backend.text(&t.replace("\r\n", "\n")),
            Command::Sync(locks, mods) => {
                self.check_layout();
                self.sync(hand, &locks, &mods);
            }
            Command::Display(d) => {
                self.held(hand).display = Some(d);
                self.use_display(hand);
            }
            Command::ReleaseAll => self.release(hand),
            Command::Done(id) => {
                std::thread::sleep(SETTLE);
                let _ = self.events.send(PlatformEvent::InputDone { id });
            }
            Command::Reset => {
                self.release(hand);
                self.backend = make();
                self.shown = None;
                if hand == Hand::Person {
                    self.applied.set_move(0);
                }
                self.layout = None;
                self.check_layout();
            }
        }
        if interaction {
            self.applied.interaction.store(true, Ordering::Release);
        }
    }

    fn button(&mut self, hand: Hand, b: u8, down: bool) {
        let other = self.other_holds_button(hand, b);
        let held = &mut self.held(hand).buttons;
        let changed = if down { held.insert(b) } else { held.remove(&b) };
        // Down once while either holds it; up when the last lets go.
        if (down && !other) || (!down && changed && !other) {
            self.backend.mouse_button(b, down);
        }
    }

    fn key(&mut self, hand: Hand, code: &str, down: bool) {
        let Some(key) = keys::find(code) else { return self.unknown_key(code) };
        let other = self.other_holds_key(hand, code);
        let mine = self.held(hand).keys.contains(code);
        if down {
            self.held(hand).keys.insert(code.to_string());
        } else {
            self.held(hand).keys.remove(code);
        }
        // Held by the other hand: already down, and stays so until both let go.
        if other {
            return;
        }
        if (down || mine) && !self.backend.key(key, down, down && mine) {
            self.unknown_key(code);
        }
    }

    fn unknown_key(&mut self, code: &str) {
        if self.unknown.insert(code.to_string()) {
            eprintln!("input: no key for {code:?} on this computer; ignored");
        }
    }

    fn char(&mut self, hand: Hand, ch: &str, user: &Mods) {
        let Some(c) = ch.chars().next() else { return };
        let backend = &mut self.backend;
        match resolve(c, |k, m| backend.translate(k, m)) {
            Some((key, need)) => {
                let held = self.held(hand).keys.clone();
                for (code, down) in char_presses(key, &need, user, &held) {
                    self.key(hand, code, down);
                }
            }
            None => {
                // Not on this layout: typed as text, which no modifier can apply to.
                if self.unknown.insert(format!("char {c}")) {
                    eprintln!("input: {c:?} is not on this keyboard layout; typed as text");
                }
                self.backend.text(ch);
            }
        }
    }

    /// The browser's lock and modifier state. A modifier the other hand holds is left alone:
    /// the person's page knows nothing of the AI client's keys.
    fn sync(&mut self, hand: Hand, locks: &Locks, mods: &Mods) {
        let state = self.backend.key_state();
        for (lock, on) in plan_locks(&state, locks) {
            self.backend.set_lock(lock, on);
        }
        let held = self.held(hand).keys.clone();
        let others: HashSet<String> = self.hands.iter().filter(|(h, _)| **h != hand).flat_map(|(_, h)| h.keys.iter().cloned()).collect();
        for (code, down) in plan_sync(&state, &held, mods) {
            let class_held_by_other = CLASSES.iter().find(|(_, codes)| codes.contains(&code)).is_some_and(|(_, codes)| codes.iter().any(|c| others.contains(*c)));
            if !down && class_held_by_other {
                continue;
            }
            self.key(hand, code, down);
        }
    }

    /// Reports the layout when it is not the one reported last.
    fn check_layout(&mut self) {
        let id = self.backend.layout_id();
        if id.is_none() || id == self.layout {
            return;
        }
        self.layout = id;
        let backend = &mut self.backend;
        let (labels, shift) = labels(|k, m| backend.translate(k, m));
        let name = self.backend.layout_name();
        let _ = self.events.send(PlatformEvent::LayoutChanged { name, labels, shift });
    }

    /// Lifts what `hand` holds; what the other hand holds stays down.
    pub fn release(&mut self, hand: Hand) {
        let mut keys: Vec<String> = std::mem::take(&mut self.held(hand).keys).into_iter().collect();
        keys.sort();
        for c in keys {
            if !self.other_holds_key(hand, &c) {
                if let Some(key) = keys::find(&c) {
                    self.backend.key(key, false, false);
                }
            }
        }
        for b in std::mem::take(&mut self.held(hand).buttons) {
            if !self.other_holds_button(hand, b) {
                self.backend.mouse_button(b, false);
            }
        }
    }
}

// However input ends, an error included, nothing stays held on this computer.
impl<B: Backend> Drop for Injector<B> {
    fn drop(&mut self) {
        self.backend.follow_input_desktop();
        self.release(Hand::Person);
        self.release(Hand::Ai);
    }
}

/// Injects on a thread of its own, made by `make` on that thread. The thread owns no windows,
/// so it can follow the input desktop onto the lock screen. It ends when the sender is dropped.
pub fn spawn<B: Backend + 'static>(make: fn() -> B, events: Sender<PlatformEvent>, applied: AppliedInput) -> Sender<(Hand, Command)> {
    let (tx, rx) = channel::<(Hand, Command)>();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            let mut injector = Injector::new(make(), events, applied);
            for (hand, c) in rx {
                injector.apply(hand, c, make);
            }
        })
        .expect("spawn input thread");
    tx
}

#[cfg(test)]
mod tests;
