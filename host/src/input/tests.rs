use super::*;
use crate::platform::{Call, Forward};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;

fn mods(shift: bool, ctrl: bool, alt: bool, meta: bool) -> Mods {
    Mods { shift, ctrl, alt, meta }
}

// ---- the decoder ----

#[derive(Default)]
struct Calls(Mutex<Vec<Call>>);
impl Forward for Calls {
    fn forward(&self, call: Call) {
        self.0.lock().unwrap().push(call);
    }
}

fn decode(msgs: &[&str]) -> Vec<Call> {
    let calls = Calls::default();
    let mut d = Decoder::default();
    for m in msgs {
        match crate::session::desk::channels::incoming(m.as_bytes()) {
            Some(crate::session::desk::channels::Incoming::Move(m)) => d.pointer(m, "s1", &calls),
            Some(crate::session::desk::channels::Incoming::Control(c)) => d.control(c, "s1", &calls),
            None => {}
        }
    }
    calls.0.into_inner().unwrap()
}

#[test]
fn stale_moves_are_dropped_and_coordinates_clamped() {
    let calls = decode(&[r#"{"t":"m","x":2.0,"y":-1,"s":5}"#, r#"{"t":"m","x":0.1,"y":0.1,"s":4}"#, r#"{"t":"b","b":7,"d":true,"x":0,"y":0}"#]);
    assert_eq!(calls, vec![Call::MouseMove { session: "s1".into(), x: 1.0, y: 0.0, seq: 5 }]);
}

#[test]
fn relative_moves_share_the_sequence_and_buttons_then_stay_put() {
    let calls = decode(&[
        r#"{"t":"mr","dx":3,"dy":-2,"s":1}"#,
        r#"{"t":"m","x":0.5,"y":0.5,"s":1}"#,
        r#"{"t":"b","b":0,"d":true,"x":0.9,"y":0.9}"#,
        r#"{"t":"m","x":0.5,"y":0.5,"s":2}"#,
        r#"{"t":"b","b":0,"d":false,"x":0.25,"y":0.75}"#,
    ]);
    assert_eq!(
        calls,
        vec![
            Call::MouseMoveBy { session: "s1".into(), dx: 3.0, dy: -2.0, seq: 1 },
            // Under Pointer Lock the button does not move the pointer.
            Call::MouseButton { session: "s1".into(), button: 0, down: true },
            Call::MouseMove { session: "s1".into(), x: 0.5, y: 0.5, seq: 2 },
            Call::MouseMove { session: "s1".into(), x: 0.25, y: 0.75, seq: 2 },
            Call::MouseButton { session: "s1".into(), button: 0, down: false },
        ]
    );
}

#[test]
fn the_last_move_repeated_reliably_applies_once() {
    // Lost on the move channel: the reliable copy moves the pointer, so reports catch up.
    let calls = decode(&[r#"{"t":"m","x":0.1,"y":0.1,"s":1}"#, r#"{"t":"m","x":0.3,"y":0.3,"s":2}"#]);
    assert_eq!(calls, vec![Call::MouseMove { session: "s1".into(), x: 0.1, y: 0.1, seq: 1 }, Call::MouseMove { session: "s1".into(), x: 0.3, y: 0.3, seq: 2 }]);
    // Arrived on both: once.
    let calls = decode(&[r#"{"t":"m","x":0.3,"y":0.3,"s":2}"#, r#"{"t":"m","x":0.3,"y":0.3,"s":2}"#, r#"{"t":"mr","dx":1,"dy":1,"s":3}"#, r#"{"t":"mr","dx":1,"dy":1,"s":3}"#]);
    assert_eq!(calls, vec![Call::MouseMove { session: "s1".into(), x: 0.3, y: 0.3, seq: 2 }, Call::MouseMoveBy { session: "s1".into(), dx: 1.0, dy: 1.0, seq: 3 }]);
}

#[test]
fn keys_characters_and_sync_become_calls() {
    let calls = decode(&[
        r#"{"t":"k","c":"KeyA","d":true}"#,
        r#"{"t":"kc","ch":"é","mods":{"shift":false,"ctrl":true,"alt":false,"meta":false}}"#,
        r#"{"t":"kc","ch":"ab","mods":{"shift":false,"ctrl":true,"alt":false,"meta":false}}"#,
        r#"{"t":"sync","locks":{"caps":true,"num":false,"scroll":false},"mods":{"shift":true,"ctrl":false,"alt":false,"meta":false}}"#,
        r#"{"t":"x","v":"ж ok"}"#,
        r#"{"t":"lc","on":true}"#,
    ]);
    assert_eq!(
        calls,
        vec![
            Call::Key { session: "s1".into(), code: "KeyA".into(), down: true },
            Call::Char { session: "s1".into(), ch: "é".into(), mods: mods(false, true, false, false) },
            Call::SyncKeys { session: "s1".into(), locks: Locks { caps: true, num: false, scroll: false }, mods: mods(true, false, false, false) },
            Call::Text { session: "s1".into(), text: "ж ok".into() },
        ]
    );
}

// ---- pure decisions ----

fn held(codes: &[&str]) -> HashSet<String> {
    codes.iter().map(|c| c.to_string()).collect()
}

#[test]
fn sync_presses_what_is_wanted_and_releases_what_is_not() {
    let os = KeyState { mods: Some(mods(false, true, false, false)), ..Default::default() };
    // Shift wanted and up: pressed. Ctrl down on the computer, not wanted: both released.
    assert_eq!(plan_sync(&os, &held(&[]), &mods(true, false, false, false)), vec![("ShiftLeft", true), ("ControlLeft", false), ("ControlRight", false)]);
    // Held here and not wanted: only the keys held here are released.
    let os = KeyState { mods: Some(mods(false, false, true, false)), ..Default::default() };
    assert_eq!(plan_sync(&os, &held(&["AltRight"]), &no_mods()), vec![("AltRight", false)]);
    // Wanted and already down (by anyone): nothing.
    let os = KeyState { mods: Some(mods(false, false, false, true)), ..Default::default() };
    assert!(plan_sync(&os, &held(&[]), &mods(false, false, false, true)).is_empty());
    // The OS state unknown: held keys still follow.
    assert_eq!(plan_sync(&KeyState::default(), &held(&["ShiftRight"]), &mods(false, false, false, true)), vec![("ShiftRight", false), ("MetaLeft", true)]);
}

#[test]
fn locks_change_only_where_they_differ_and_are_known() {
    let os = KeyState { caps: Some(false), num: Some(true), scroll: None, mods: None };
    assert_eq!(plan_locks(&os, &Locks { caps: true, num: true, scroll: true }), vec![(Lock::Caps, true)]);
    assert_eq!(plan_locks(&os, &Locks { caps: false, num: false, scroll: false }), vec![(Lock::Num, false)]);
}

/// A French AZERTY layout, enough of it: A and Q swapped, digits shifted, AltGr+0 is @, ^ dead.
fn azerty(key: &Key, m: &Mods) -> Option<Typed> {
    let t = |s: &str| Some(Typed { text: s.into(), dead: false });
    let altgr = m.ctrl && m.alt;
    match (key.code, m.shift, altgr) {
        ("KeyQ", false, false) => t("a"),
        ("KeyQ", true, false) => t("A"),
        ("KeyA", false, false) => t("q"),
        ("KeyA", true, false) => t("Q"),
        ("KeyC", false, false) => t("c"),
        ("KeyC", true, false) => t("C"),
        ("Digit2", false, false) => t("é"),
        ("Digit2", true, false) => t("2"),
        ("Digit0", false, true) => t("@"),
        ("Numpad2", false, false) => t("2"),
        ("BracketLeft", false, false) => Some(Typed { text: "^".into(), dead: true }),
        ("Enter", _, false) => t("\r"),
        ("F1", _, false) => t("\u{f704}"),
        _ => None,
    }
}

#[test]
fn characters_resolve_through_the_hosts_layout() {
    let r = |c| resolve(c, azerty).map(|(k, m)| (k.code, m));
    assert_eq!(r('a'), Some(("KeyQ", no_mods())));
    assert_eq!(r('Q'), Some(("KeyA", mods(true, false, false, false))));
    assert_eq!(r('@'), Some(("Digit0", mods(false, true, true, false))));
    // The main block before the keypad.
    assert_eq!(r('2'), Some(("Digit2", mods(true, false, false, false))));
    // A dead key types nothing by itself.
    assert_eq!(r('^'), None);
    assert_eq!(r('ж'), None);
}

#[test]
fn a_character_with_modifiers_is_one_combination() {
    let (key, need) = resolve('A', azerty).unwrap();
    // Ctrl from the browser, Shift from the layout; Ctrl already held is not pressed again.
    assert_eq!(
        char_presses(key, &need, &mods(false, true, false, false), &held(&[])),
        vec![("ShiftLeft", true), ("ControlLeft", true), ("KeyQ", true), ("KeyQ", false), ("ControlLeft", false), ("ShiftLeft", false)]
    );
    assert_eq!(char_presses(key, &no_mods(), &mods(false, true, false, false), &held(&["ControlRight"])), vec![("KeyQ", true), ("KeyQ", false)]);
}

#[test]
fn labels_are_the_layouts_visible_characters() {
    let (base, shift) = labels(azerty);
    assert_eq!(base.get("KeyQ").map(String::as_str), Some("a"));
    assert_eq!(shift.get("Digit2").map(String::as_str), Some("2"));
    assert_eq!(base.get("BracketLeft").map(String::as_str), Some("^"), "dead keys are labelled");
    assert!(!base.contains_key("Enter") && !base.contains_key("F1"), "control and private-use characters are not labels");
}

// ---- the injector ----

#[derive(Clone, Default)]
struct Log {
    log: Arc<Mutex<Vec<String>>>,
    state: Arc<Mutex<KeyState>>,
    layout: Arc<Mutex<Option<u64>>>,
    /// A secure desktop has input.
    secure: Arc<Mutex<bool>>,
}

const P: Hand = Hand::Person;
const A: Hand = Hand::Ai;

impl Log {
    fn push(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }
}

impl Backend for Log {
    fn mouse_move(&mut self, x: f64, y: f64) {
        self.push(format!("move {x} {y}"));
    }
    fn mouse_move_by(&mut self, dx: f64, dy: f64) {
        self.push(format!("by {dx} {dy}"));
    }
    fn mouse_button(&mut self, b: u8, d: bool) {
        self.push(format!("button {b} {d}"));
    }
    fn wheel(&mut self, _: f64, _: f64) {}
    fn key(&mut self, k: &Key, d: bool, repeat: bool) -> bool {
        // A platform without this key.
        if k.code == "F24" {
            return false;
        }
        self.push(format!("key {} {d}{}", k.code, if repeat { " repeat" } else { "" }));
        true
    }
    fn text(&mut self, s: &str) {
        self.push(format!("text {s}"));
    }
    fn key_state(&mut self) -> KeyState {
        self.state.lock().unwrap().clone()
    }
    fn set_lock(&mut self, lock: Lock, on: bool) {
        self.push(format!("lock {lock:?} {on}"));
    }
    fn layout_id(&mut self) -> Option<u64> {
        *self.layout.lock().unwrap()
    }
    fn layout_name(&mut self) -> String {
        "fr-FR".into()
    }
    fn translate(&mut self, key: &Key, m: &Mods) -> Option<Typed> {
        azerty(key, m)
    }
    fn set_display(&mut self, d: &DisplayInfo) {
        self.push(format!("display {}", d.id));
    }
    fn secure_desktop(&self) -> bool {
        *self.secure.lock().unwrap()
    }
}

fn injector(log: &Log) -> (Injector<Log>, Receiver<PlatformEvent>, AppliedInput) {
    let (tx, rx) = channel();
    let moves = AppliedInput::default();
    (Injector::new(log.clone(), tx, moves.clone()), rx, moves)
}

fn none() -> Log {
    Log::default()
}

/// A fresh backend on a computer with a layout.
fn with_layout() -> Log {
    let log = Log::default();
    *log.layout.lock().unwrap() = Some(7);
    log
}

#[test]
fn release_all_lifts_only_what_is_held() {
    let log = Log::default();
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(P, Command::Key("KeyA".into(), true), none);
    inj.apply(P, Command::Key("KeyB".into(), true), none);
    inj.apply(P, Command::Key("KeyB".into(), false), none);
    inj.apply(P, Command::Button(0, true), none);
    log.take();
    inj.apply(P, Command::ReleaseAll, none);
    assert_eq!(log.take(), vec!["key KeyA false", "button 0 false"]);
    inj.release(P);
    assert!(log.take().is_empty(), "a second release does nothing");
}

/// `Done` is answered after what came before it was injected, and on a secure desktop too,
/// where an AI client's other input is held back: whoever waits for it never waits in vain.
#[test]
fn done_is_answered_after_what_came_before() {
    let log = with_layout();
    let (mut inj, rx, _) = injector(&log);
    inj.apply(Hand::Ai, Command::Text("hi\r\nthere".into()), none);
    inj.apply(Hand::Ai, Command::Done("q1".into()), none);
    assert!(log.take().contains(&"text hi\nthere".to_string()), "the text came first, a CRLF as one line end");
    assert!(rx.try_iter().any(|e| matches!(e, PlatformEvent::InputDone { ref id } if id == "q1")));
    *log.secure.lock().unwrap() = true;
    inj.apply(Hand::Ai, Command::Done("q2".into()), none);
    assert!(rx.try_iter().any(|e| matches!(e, PlatformEvent::InputDone { ref id } if id == "q2")), "answered on a secure desktop too");
}

#[test]
fn input_ending_any_way_lifts_what_is_held() {
    let log = Log::default();
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(P, Command::Key("ControlLeft".into(), true), none);
    inj.apply(P, Command::Button(2, true), none);
    log.take();
    drop(inj);
    assert_eq!(log.take(), vec!["key ControlLeft false", "button 2 false"]);
}

#[test]
fn a_new_session_lifts_what_is_held_starts_fresh_and_reports_the_layout() {
    let log = Log::default();
    *log.layout.lock().unwrap() = Some(7);
    let (mut inj, rx, moves) = injector(&log);
    inj.apply(P, Command::Move(0.5, 0.5, 9), none);
    inj.apply(P, Command::Key("ShiftLeft".into(), true), none);
    assert!(matches!(rx.try_recv(), Ok(PlatformEvent::LayoutChanged { .. })), "the first key reports the layout");
    inj.apply(P, Command::Reset, with_layout);
    assert!(log.take().contains(&"key ShiftLeft false".to_string()));
    assert_eq!(moves.move_sequence(), 0, "the applied move sequence starts again");
    match rx.try_recv() {
        Ok(PlatformEvent::LayoutChanged { name, labels, shift }) => {
            assert_eq!(name, "fr-FR");
            assert_eq!(labels["KeyQ"], "a");
            assert_eq!(shift["KeyQ"], "A");
        }
        other => panic!("a new session reports the layout: {other:?}"),
    }
    drop(inj);
    assert!(log.take().is_empty(), "nothing was held by the fresh backend");
}

#[test]
fn the_layout_is_reported_again_only_when_it_changes() {
    let log = Log::default();
    *log.layout.lock().unwrap() = Some(1);
    let (mut inj, rx, _) = injector(&log);
    inj.apply(P, Command::Key("KeyA".into(), true), none);
    inj.apply(P, Command::Key("KeyA".into(), false), none);
    assert_eq!(rx.try_iter().count(), 1);
    *log.layout.lock().unwrap() = Some(2);
    inj.apply(P, Command::Key("KeyA".into(), true), none);
    assert_eq!(rx.try_iter().count(), 1);
}

#[test]
fn held_keys_repeat_and_unknown_keys_are_skipped() {
    let log = Log::default();
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(P, Command::Key("KeyA".into(), true), none);
    inj.apply(P, Command::Key("KeyA".into(), true), none);
    inj.apply(P, Command::Key("NoSuchKey".into(), true), none);
    inj.apply(P, Command::Key("F24".into(), true), none);
    assert_eq!(log.take(), vec!["key KeyA true", "key KeyA true repeat"]);
    assert_eq!(inj.unknown, held(&["NoSuchKey", "F24"]), "each unknown code is noted once");
}

#[test]
fn moves_record_the_applied_sequence() {
    let log = Log::default();
    let (mut inj, _rx, moves) = injector(&log);
    inj.apply(P, Command::Move(0.25, 0.5, 3), none);
    assert_eq!(moves.move_sequence(), 3);
    inj.apply(P, Command::MoveBy(4.0, -1.0, 4), none);
    assert_eq!(moves.move_sequence(), 4);
    assert_eq!(log.take(), vec!["move 0.25 0.5", "by 4 -1"]);
}

#[test]
fn sync_sets_locks_and_modifiers_from_the_computers_state() {
    let log = Log::default();
    *log.state.lock().unwrap() = KeyState { caps: Some(false), num: Some(true), scroll: Some(false), mods: Some(no_mods()) };
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(P, Command::Sync(Locks { caps: true, num: true, scroll: false }, mods(false, true, false, false)), none);
    assert_eq!(log.take(), vec!["lock Caps true", "key ControlLeft true"]);
    // Ctrl is now held here; a later sync without it lets it go.
    inj.apply(P, Command::Sync(Locks { caps: false, num: true, scroll: false }, no_mods()), none);
    assert_eq!(log.take(), vec!["key ControlLeft false"]);
}

#[test]
fn characters_are_typed_with_the_layout_or_as_text() {
    let log = Log::default();
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(P, Command::Char("a".into(), mods(false, true, false, false)), none);
    assert_eq!(log.take(), vec!["key ControlLeft true", "key KeyQ true", "key KeyQ false", "key ControlLeft false"]);
    inj.apply(P, Command::Char("ж".into(), mods(false, true, false, false)), none);
    assert_eq!(log.take(), vec!["text ж"]);
}

fn display(id: &str) -> DisplayInfo {
    DisplayInfo { id: id.into(), x: 0, y: 0, w: 100, h: 100, primary: id == "A" }
}

/// The person and an AI client at once: each lets go of only its own; a key both hold goes up
/// when the last lets go.
#[test]
fn two_hands_each_hold_their_own() {
    let log = Log::default();
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(A, Command::Key("ControlLeft".into(), true), none);
    inj.apply(P, Command::Key("ControlLeft".into(), true), none);
    inj.apply(P, Command::Key("KeyA".into(), true), none);
    assert_eq!(log.take(), vec!["key ControlLeft true", "key KeyA true"], "a key both hold is pressed once");
    inj.apply(P, Command::ReleaseAll, none);
    assert_eq!(log.take(), vec!["key KeyA false"], "the person's release leaves the AI client's Ctrl down");
    inj.apply(A, Command::Key("ControlLeft".into(), false), none);
    assert_eq!(log.take(), vec!["key ControlLeft false"]);
    inj.apply(A, Command::Button(0, true), none);
    inj.apply(P, Command::Button(0, true), none);
    inj.apply(A, Command::Button(0, false), none);
    assert_eq!(log.take(), vec!["button 0 true"], "the button stays down while the person holds it");
    inj.apply(P, Command::Button(0, false), none);
    assert_eq!(log.take(), vec!["button 0 false"]);
    // A new session of the person (its backend made afresh) lifts nothing of the AI client's.
    inj.apply(A, Command::Key("AltLeft".into(), true), none);
    log.take();
    inj.apply(P, Command::Reset, none);
    assert!(log.take().is_empty(), "the AI client's Alt stays down");
}

/// The person's page syncs its modifiers: one the AI client holds is not its to lift.
#[test]
fn the_persons_sync_leaves_the_ai_clients_modifier_down() {
    let log = Log::default();
    log.state.lock().unwrap().mods = Some(Mods { ctrl: true, ..no_mods() });
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(A, Command::Key("ControlLeft".into(), true), none);
    log.take();
    inj.apply(P, Command::Sync(Locks { caps: false, num: false, scroll: false }, no_mods()), none);
    assert!(!log.take().iter().any(|l| l.starts_with("key Control")), "the AI client's Ctrl stays down");
}

/// Each hand's moves refer to its own display, and only the person's move the reported sequence.
#[test]
fn moves_follow_each_hands_display_and_only_the_persons_count() {
    let log = Log::default();
    let (mut inj, _rx, moves) = injector(&log);
    inj.apply(P, Command::Display(display("A")), none);
    inj.apply(A, Command::Display(display("B")), none);
    inj.apply(P, Command::Move(0.5, 0.5, 7), none);
    inj.apply(A, Command::Move(0.1, 0.1, 0), none);
    inj.apply(A, Command::Move(0.2, 0.2, 0), none);
    assert_eq!(log.take(), vec!["display A", "display B", "display A", "move 0.5 0.5", "display B", "move 0.1 0.1", "move 0.2 0.2"]);
    assert_eq!(moves.move_sequence(), 7, "the AI client's moves leave the person's sequence alone");
}

/// On a secure desktop (UAC), the AI client's input is held back where it would land; the
/// person's is not, and the AI client can still let go.
#[test]
fn an_ai_clients_input_never_lands_on_a_secure_desktop() {
    let log = Log::default();
    let (mut inj, _rx, _) = injector(&log);
    inj.apply(A, Command::Key("ShiftLeft".into(), true), none);
    log.take();
    *log.secure.lock().unwrap() = true;
    for c in [Command::Key("Enter".into(), true), Command::Button(0, true), Command::Text("yes".into()), Command::Move(0.5, 0.5, 0)] {
        inj.apply(A, c, none);
    }
    assert!(log.take().is_empty(), "nothing of the AI client's reaches a secure desktop");
    inj.apply(P, Command::Key("Enter".into(), true), none);
    assert_eq!(log.take(), vec!["key Enter true"], "the person answers it");
    inj.apply(A, Command::ReleaseAll, none);
    assert_eq!(log.take(), vec!["key ShiftLeft false"], "letting go still works");
}

#[test]
fn only_personal_discrete_inputs_request_a_video_response_and_bursts_coalesce() {
    let (mut inj, _rx, applied) = injector(&Log::default());
    inj.apply(P, Command::Move(0.4, 0.5, 1), none);
    inj.apply(P, Command::Wheel(0.0, 1.0), none);
    inj.apply(Hand::Ai, Command::Text("hello".into()), none);
    assert!(!applied.take_interaction());
    inj.apply(P, Command::Button(0, true), none);
    inj.apply(P, Command::Button(0, false), none);
    inj.apply(P, Command::Text("hello".into()), none);
    assert!(applied.take_interaction());
    assert!(!applied.take_interaction());
}
