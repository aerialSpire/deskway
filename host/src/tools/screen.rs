//! `screenshot` and `wait_for`, and the pointer and keys in a screenshot's coordinates:
//! `click`, `move`, `drag`, `scroll`, `key`, `type`. Positions are pixels of the latest screenshot this session took;
//! they become a display and a place on it (0..1 each way), which is what the computer's input
//! takes (`Platform`, the AI client's hand: `platform::local`).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{Ctx, Output, Shot};
use crate::mcp::gate::{Granted, ToolError};

const DEFAULT_WIDTH: u64 = 1280;
const MAX_WIDTH: u64 = 1920;
/// A still waits at most this long: capture may have to start first.
const WAIT: Duration = Duration::from_secs(15);

/// The display `args["display"]` names, the primary one by default.
fn display(ctx: &Ctx, args: &Value) -> Result<crate::display::DisplayInfo, ToolError> {
    let facts = ctx.facts();
    let want = args["display"].as_str();
    crate::display::choose(&facts.displays, want)
        .filter(|d| want.is_none_or(|w| w == d.id))
        .cloned()
        .ok_or_else(|| ToolError::new("not_found", format!("no display {}", want.unwrap_or_default()), "Call computer_info for the displays."))
}

/// A picture of `display` at most `max` pixels wide: JPEG, width, height.
fn capture(ctx: &Ctx, display: &crate::display::DisplayInfo, max: u64) -> Result<(Vec<u8>, u32, u32), ToolError> {
    let id = format!("{}-{}", ctx.session, wire::crypto::hex(&wire::crypto::random::<6>()));
    let answer = ctx.replies.expect(&id);
    ctx.platform.snapshot(id, Some(display.id.clone()), max as u32);
    match answer.recv_timeout(WAIT) {
        Ok(crate::platform::PlatformEvent::Snapshot { jpeg, w, h, .. }) => Ok((jpeg.0, w, h)),
        _ => Err(ToolError::new("unavailable", "the screen could not be captured now", "Check computer_info: a secure prompt or no display may be showing; try again.")),
    }
}

pub fn screenshot(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let display = display(ctx, args)?;
    let (jpeg, w, h) = capture(ctx, &display, args["max_width"].as_u64().unwrap_or(DEFAULT_WIDTH).clamp(160, MAX_WIDTH))?;
    *ctx.shot.lock().unwrap() = Some(Shot { display: display.clone(), w, h });
    let about = json!({ "display": display.id, "width": w, "height": h, "scale": display.w as f64 / w.max(1) as f64 });
    Ok(Output::Image { jpeg, about })
}

/// How wide the pictures `wait_for` compares are, and how often it looks.
const WATCH_WIDTH: u64 = 320;
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// Until a display changes or holds still. Pictures of it are compared as they come: the same
/// screen encodes to the same bytes.
pub fn wait_for(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let display = display(ctx, args)?;
    let still = args["until"].as_str() == Some("still");
    let hold = Duration::from_millis(args["still_ms"].as_u64().unwrap_or(1000).clamp(200, 10_000));
    let limit = Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(10_000).min(60_000));
    let start = Instant::now();
    let mut watch = Watch::new(still, hold, capture(ctx, &display, WATCH_WIDTH)?.0, start);
    let met = loop {
        if watch.met(Instant::now()) {
            break true;
        }
        if start.elapsed() >= limit {
            break false;
        }
        std::thread::sleep(WATCH_EVERY);
        let now = capture(ctx, &display, WATCH_WIDTH)?.0;
        watch.saw(now, Instant::now());
    };
    Ok(Output::Data(json!({ "met": met, "until": if still { "still" } else { "change" }, "waited_ms": start.elapsed().as_millis() as u64 })))
}

/// What `wait_for` has seen: the first picture, the latest, and since when it is the latest.
struct Watch {
    still: bool,
    hold: Duration,
    first: Vec<u8>,
    last: Vec<u8>,
    since: Instant,
    changed: bool,
}

impl Watch {
    fn new(still: bool, hold: Duration, first: Vec<u8>, at: Instant) -> Watch {
        Watch { still, hold, last: first.clone(), first, since: at, changed: false }
    }

    fn saw(&mut self, picture: Vec<u8>, at: Instant) {
        if picture != self.last {
            self.since = at;
            self.last = picture;
        }
        self.changed |= self.last != self.first;
    }

    /// The display changed since the wait began, or (until still) has held still long enough.
    fn met(&self, now: Instant) -> bool {
        if self.still {
            now.duration_since(self.since) >= self.hold
        } else {
            self.changed
        }
    }
}

/// A place of the latest screenshot as a display and a place on it.
fn place(ctx: &Ctx, x: &Value, y: &Value) -> Result<(crate::display::DisplayInfo, f64, f64), ToolError> {
    let shot = ctx.shot.lock().unwrap().clone().ok_or_else(|| ToolError::new("invalid_args", "no screenshot yet: positions are its pixels", "Take a screenshot first."))?;
    let (Some(x), Some(y)) = (x.as_f64(), y.as_f64()) else { return Err(ToolError::new("invalid_args", "x and y are numbers", "Pass the position as pixels of the screenshot.")) };
    if !(0.0..=shot.w as f64).contains(&x) || !(0.0..=shot.h as f64).contains(&y) {
        return Err(ToolError::new("invalid_args", format!("({x}, {y}) is outside the screenshot ({}×{})", shot.w, shot.h), "Pass a position inside it."));
    }
    Ok((shot.display, x / shot.w as f64, y / shot.h as f64))
}

fn button(v: &Value) -> u8 {
    match v.as_str() {
        Some("middle") => 1,
        Some("right") => 2,
        _ => 0,
    }
}

/// Moves the pointer to a place of the latest screenshot.
fn point(ctx: &Ctx, x: &Value, y: &Value) -> Result<(), ToolError> {
    let (display, nx, ny) = place(ctx, x, y)?;
    ctx.platform.pointer_display(&ctx.session, display);
    ctx.platform.mouse_move(&ctx.session, nx, ny, 0);
    Ok(())
}

fn done(what: &str) -> Result<Output, ToolError> {
    Ok(Output::Data(json!({ "done": what })))
}

pub fn click(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    point(ctx, &args["x"], &args["y"])?;
    let b = button(&args["button"]);
    for _ in 0..args["count"].as_u64().unwrap_or(1).clamp(1, 3) {
        ctx.platform.mouse_button(&ctx.session, b, true);
        ctx.platform.mouse_button(&ctx.session, b, false);
    }
    done("clicked")
}

pub fn pointer_move(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    point(ctx, &args["x"], &args["y"])?;
    done("moved")
}

pub fn drag(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    // Both places are checked before anything is pressed.
    let (from, to) = (place(ctx, &args["x"], &args["y"])?, place(ctx, &args["to_x"], &args["to_y"])?);
    point(ctx, &args["x"], &args["y"])?;
    let b = button(&args["button"]);
    ctx.platform.mouse_button(&ctx.session, b, true);
    // A few steps on the way: some applications start a drag only once the pointer moves.
    for i in 1..=8 {
        let t = i as f64 / 8.0;
        ctx.platform.mouse_move(&ctx.session, from.1 + (to.1 - from.1) * t, from.2 + (to.2 - from.2) * t, 0);
        std::thread::sleep(Duration::from_millis(15));
    }
    ctx.platform.mouse_button(&ctx.session, b, false);
    done("dragged")
}

pub fn scroll(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    point(ctx, &args["x"], &args["y"])?;
    // The computer's input takes a browser's wheel deltas: about 100 a notch.
    let (down, right) = (args["down"].as_i64().unwrap_or(0), args["right"].as_i64().unwrap_or(0));
    ctx.platform.wheel(&ctx.session, (right * 100) as f64, (down * 100) as f64);
    done("scrolled")
}

/// A key name as `KeyboardEvent.code`, which the computer's input takes.
fn code(name: &str) -> Option<String> {
    let n = name.trim().to_ascii_lowercase();
    let named = match n.as_str() {
        "ctrl" | "control" => "ControlLeft",
        "shift" => "ShiftLeft",
        "alt" | "option" | "opt" => "AltLeft",
        "cmd" | "command" | "meta" | "win" | "windows" | "super" => "MetaLeft",
        "enter" | "return" => "Enter",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "space" => "Space",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "insert" => "Insert",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    let mut chars = n.chars();
    match (chars.next(), chars.next()) {
        (Some(c @ 'a'..='z'), None) => Some(format!("Key{}", c.to_ascii_uppercase())),
        (Some(c @ '0'..='9'), None) => Some(format!("Digit{c}")),
        (Some('f'), Some(_)) => n[1..].parse::<u8>().ok().filter(|f| (1..=12).contains(f)).map(|f| format!("F{f}")),
        _ => None,
    }
}

/// `ctrl+shift+t` as presses: the modifiers down in order, the key, then all up in reverse.
fn presses(keys: &str) -> Result<Vec<(String, bool)>, String> {
    let codes: Vec<String> = keys.split('+').map(|k| code(k).ok_or_else(|| format!("no key {k:?}"))).collect::<Result<_, _>>()?;
    if codes.is_empty() {
        return Err("no keys".into());
    }
    let mut out: Vec<(String, bool)> = codes.iter().map(|c| (c.clone(), true)).collect();
    out.extend(codes.iter().rev().map(|c| (c.clone(), false)));
    Ok(out)
}

/// Waits until the input sent so far has been carried out on the computer: a call that says
/// done is then done, and a screenshot right after shows it. At most a few seconds.
fn carried_out(ctx: &Ctx) {
    let id = format!("{}-{}", ctx.session, wire::crypto::hex(&wire::crypto::random::<6>()));
    let answer = ctx.replies.expect(&id);
    ctx.platform.input_done(&ctx.session, id);
    let _ = answer.recv_timeout(Duration::from_secs(5));
}

pub fn key(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let keys = args["keys"].as_str().unwrap_or_default();
    let presses = presses(keys).map_err(|e| ToolError::new("invalid_args", e, "Use names such as ctrl+c, Enter or F5."))?;
    for (code, down) in presses {
        ctx.platform.key(&ctx.session, code, down);
    }
    carried_out(ctx);
    done("pressed")
}

pub fn type_text(_: &Granted, args: &Value, ctx: &Ctx) -> Result<Output, ToolError> {
    let text = args["text"].as_str().unwrap_or_default();
    ctx.platform.text(&ctx.session, text.to_string());
    carried_out(ctx);
    done("typed")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (pictures seen at these ms, until still, ms it is asked at) -> met
    #[test]
    fn a_wait_is_met_by_a_change_or_by_holding_still_long_enough() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        type Seen<'a> = &'a [(u64, &'a [u8])];
        let cases: [(Seen, bool, u64, bool); 6] = [
            (&[(250, b"a"), (500, b"a")], false, 500, false),
            (&[(250, b"b")], false, 250, true),
            (&[(250, b"b"), (500, b"a")], false, 500, true),
            (&[], true, 999, false),
            (&[], true, 1000, true),
            (&[(250, b"a"), (750, b"b"), (1500, b"b")], true, 1500, false),
        ];
        for (i, (seen, still, now, want)) in cases.into_iter().enumerate() {
            let mut w = Watch::new(still, Duration::from_millis(1000), b"a".to_vec(), t0);
            for (ms, p) in seen {
                w.saw(p.to_vec(), at(*ms));
            }
            assert_eq!(w.met(at(now)), want, "case {i}");
        }
    }

    #[test]
    fn key_names_become_presses_modifiers_first_and_up_in_reverse() {
        let p = |k: &str| presses(k).map(|v| v.iter().map(|(c, d)| format!("{c}{}", if *d { "↓" } else { "↑" })).collect::<Vec<_>>().join(" "));
        assert_eq!(p("ctrl+shift+t").unwrap(), "ControlLeft↓ ShiftLeft↓ KeyT↓ KeyT↑ ShiftLeft↑ ControlLeft↑");
        assert_eq!(p("Enter").unwrap(), "Enter↓ Enter↑");
        assert_eq!(p("win+r").unwrap(), "MetaLeft↓ KeyR↓ KeyR↑ MetaLeft↑");
        assert_eq!(p("alt+F4").unwrap(), "AltLeft↓ F4↓ F4↑ AltLeft↑");
        assert_eq!(p("cmd+space").unwrap(), "MetaLeft↓ Space↓ Space↑ MetaLeft↑");
        assert_eq!(p("7").unwrap(), "Digit7↓ Digit7↑");
        for bad in ["ctrl+", "hyper+x", "F13", "ab"] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }
}
