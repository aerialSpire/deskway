//! The host's clipboard state `{gen, owner, formats}`, the one source of truth for the shared
//! clipboard, and the rules around it:
//!
//! - The host assigns `gen`; the latest announcement (from either side) wins.
//! - Nothing is announced, read or answered while sharing is off.
//! - A request names its generation; one for an older generation is refused (`gone`).
//! - A paste on this computer waits for the browser; whatever ends that wait (data, `cberr`,
//!   a newer announcement, sharing off, the session ending) answers it exactly once.
//! - The browser hears that its copy is the clipboard's state only once the platform reports
//!   the placeholders in place; if they cannot be placed, it hears why.

use super::Format;
use wire::control::{Control, Owner};
use wire::types::{ClipboardError, FormatKind};

/// What the platform is asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    /// Watch and serve the clipboard or not.
    Share(bool),
    /// Placeholders for the browser's formats.
    Announce { gen: u64, formats: Vec<Format> },
    /// The answer to a paste waiting on this computer; neither text nor html: nothing.
    Provide { gen: u64, format: FormatKind, text: Option<String>, html: Option<String> },
    /// Read one format for the browser.
    Read { gen: u64, format: FormatKind, id: String, i: Option<u32> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A control message for the browser.
    Browser(Control),
    Platform(Act),
}

/// A paste on this computer waiting for the browser.
struct Waiting {
    id: String,
    gen: u64,
    format: FormatKind,
}

pub struct Sync {
    share: bool,
    gen: u64,
    owner: Option<Owner>,
    formats: Vec<Format>,
    /// The browser's announcement of the current generation, until the platform confirms it.
    announcing: Option<String>,
    waiting: Option<Waiting>,
    next_id: u64,
}

/// Request ids are also file-channel transfer ids, which are at most 64 characters.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
}

impl Sync {
    pub fn new(share: bool) -> Sync {
        Sync { share, gen: 0, owner: None, formats: Vec::new(), announcing: None, waiting: None, next_id: 0 }
    }

    /// A session starts: the browser hears the setting, the platform starts watching if on.
    pub fn start(&self) -> Vec<Out> {
        vec![Out::Browser(Control::ClipShare { on: self.share }), Out::Platform(Act::Share(self.share))]
    }

    /// The computer's setting changed (from the panel or the browser).
    pub fn set_share(&mut self, on: bool) -> Vec<Out> {
        let mut out = Vec::new();
        if !on {
            out = self.give_up();
            self.owner = None;
            self.formats.clear();
            self.announcing = None;
        }
        self.share = on;
        out.push(Out::Browser(Control::ClipShare { on }));
        out.push(Out::Platform(Act::Share(on)));
        out
    }

    /// Something was copied on this computer.
    pub fn host_changed(&mut self, formats: Vec<Format>, text: Option<String>) -> Vec<Out> {
        if !self.share {
            return Vec::new();
        }
        let mut out = self.give_up();
        self.gen += 1;
        self.owner = Some(Owner::Host);
        self.formats = formats.clone();
        self.announcing = None;
        let text = text.filter(|t| t.len() <= super::INLINE_MAX);
        out.push(Out::Browser(Control::Clipboard { gen: Some(self.gen), owner: Some(Owner::Host), formats: formats.into_iter().map(Into::into).collect(), text, id: None }));
        out
    }

    /// Something was copied in the browser: its formats (no content), and the id it wants a
    /// refusal to carry. Files cannot come this way (browsers cannot read them from their
    /// clipboard); they come as transfers. The browser is answered in `announced`.
    pub fn browser_announced(&mut self, formats: Vec<Format>, id: Option<String>) -> Vec<Out> {
        if !self.share {
            return vec![Out::Browser(Control::ClipShare { on: false })];
        }
        let formats: Vec<Format> = formats.into_iter().filter(|f| matches!(f.kind, FormatKind::Text | FormatKind::Html | FormatKind::Image)).take(8).collect();
        let mut out = self.give_up();
        self.gen += 1;
        self.owner = Some(Owner::Browser);
        self.formats = formats.clone();
        self.announcing = Some(id.unwrap_or_default());
        out.push(Out::Platform(Act::Announce { gen: self.gen, formats }));
        out
    }

    /// The platform answered an announcement: the placeholders are in place (`why` None), or
    /// could not be placed. An answer for a generation already replaced changes nothing.
    pub fn announced(&mut self, gen: u64, why: Option<ClipboardError>) -> Vec<Out> {
        if gen != self.gen || self.owner != Some(Owner::Browser) {
            return Vec::new();
        }
        let Some(id) = self.announcing.take() else { return Vec::new() };
        match why {
            None => vec![Out::Browser(Control::Clipboard { gen: Some(gen), owner: Some(Owner::Browser), formats: self.formats.iter().cloned().map(Into::into).collect(), text: None, id: None })],
            Some(why) => {
                // Nothing of the browser's is on the clipboard: no one owns this generation.
                self.owner = None;
                self.formats.clear();
                vec![Out::Browser(Control::ClipboardFailed { gen, id, why })]
            }
        }
    }

    /// A paste the browser cannot be asked for now (the session is recovering): it gets
    /// nothing at once, rather than waiting for the deadline.
    pub fn decline(&mut self, gen: u64, format: FormatKind) -> Vec<Out> {
        vec![Out::Platform(Act::Provide { gen, format, text: None, html: None })]
    }

    /// Something on this computer pastes from a placeholder: ask the browser, or answer
    /// nothing at once if the placeholder is not the browser's latest.
    pub fn render(&mut self, gen: u64, format: FormatKind) -> Vec<Out> {
        let placed = self.announcing.is_none();
        let current = self.share && placed && self.owner == Some(Owner::Browser) && gen == self.gen && self.formats.iter().any(|f| f.kind == format);
        if !current {
            return vec![Out::Platform(Act::Provide { gen, format, text: None, html: None })];
        }
        let mut out = self.give_up();
        self.next_id += 1;
        let id = format!("p{}", self.next_id);
        self.waiting = Some(Waiting { id: id.clone(), gen, format });
        out.push(Out::Browser(Control::ClipboardRequest { gen, format, id, i: None }));
        out
    }

    /// The browser's answer to a paste. Inline data answers it now; `size` means the bytes
    /// follow on the file channel as a transfer with this id and gen, which answers it there.
    pub fn browser_data(&mut self, gen: u64, format: FormatKind, id: &str, text: Option<String>, html: Option<String>, size: Option<u64>) -> Vec<Out> {
        let Some(w) = self.waiting.as_ref().filter(|w| w.id == id && w.gen == gen && w.format == format) else { return Vec::new() };
        let (gen, format) = (w.gen, w.format);
        if size.is_some() && text.is_none() && html.is_none() {
            return Vec::new();
        }
        self.waiting = None;
        let (text, html) = match format {
            FormatKind::Text => (text, None),
            FormatKind::Html => (None, html),
            _ => (None, None),
        };
        vec![Out::Platform(Act::Provide { gen, format, text, html })]
    }

    /// The browser cannot answer, or gave up: the waiting paste gets nothing. For one of the
    /// browser's own requests nothing is left to do here (its bytes are cancelled on the file
    /// channel).
    pub fn browser_failed(&mut self, gen: u64, id: &str) -> Vec<Out> {
        if self.waiting.as_ref().is_some_and(|w| w.id == id && w.gen == gen) {
            return self.give_up();
        }
        Vec::new()
    }

    /// The bytes of a paste's answer arrived (or failed) on the file channel: it is no longer
    /// waiting here.
    pub fn transfer_ended(&mut self, id: &str) {
        if self.waiting.as_ref().is_some_and(|w| w.id == id) {
            self.waiting = None;
        }
    }

    /// The browser asks for this computer's data (“Get it here”, or text too large to come
    /// with the announcement).
    pub fn browser_request(&mut self, gen: u64, format: FormatKind, id: String, i: Option<u32>) -> Vec<Out> {
        if !valid_id(&id) {
            return Vec::new();
        }
        let why = if !self.share {
            Some(ClipboardError::Denied)
        } else if self.owner != Some(Owner::Host) || gen != self.gen {
            Some(ClipboardError::Gone)
        } else if !self.formats.iter().any(|f| f.kind == format) {
            Some(ClipboardError::Unsupported)
        } else {
            None
        };
        match why {
            Some(why) => vec![Out::Browser(Control::ClipboardFailed { gen, id, why })],
            None => vec![Out::Platform(Act::Read { gen, format, id, i })],
        }
    }

    /// The platform read the data for a browser request.
    pub fn read_done(&mut self, gen: u64, format: FormatKind, id: String, text: Option<String>, html: Option<String>, size: Option<u64>) -> Vec<Out> {
        vec![Out::Browser(Control::ClipboardData { gen, format, id, text, html, size })]
    }

    pub fn read_failed(&mut self, gen: u64, id: String, why: ClipboardError) -> Vec<Out> {
        vec![Out::Browser(Control::ClipboardFailed { gen, id, why })]
    }

    /// Ends the wait of a paste, answering it with nothing.
    fn give_up(&mut self) -> Vec<Out> {
        match self.waiting.take() {
            Some(w) => vec![Out::Platform(Act::Provide { gen: w.gen, format: w.format, text: None, html: None })],
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> Vec<Format> {
        vec![Format { size: Some(5), ..Format::of(FormatKind::Text) }]
    }

    fn browser(out: &[Out]) -> Vec<Control> {
        out.iter().filter_map(|o| if let Out::Browser(c) = o { Some(c.clone()) } else { None }).collect()
    }

    fn platform(out: &[Out]) -> Vec<Act> {
        out.iter().filter_map(|o| if let Out::Platform(a) = o { Some(a.clone()) } else { None }).collect()
    }

    /// A copy in the browser, placed by the platform.
    fn here(s: &mut Sync, formats: Vec<Format>) -> Vec<Out> {
        let mut out = s.browser_announced(formats, Some("c".into()));
        out.extend(s.announced(s.gen, None));
        out
    }

    fn nothing(gen: u64, format: FormatKind) -> Act {
        Act::Provide { gen, format, text: None, html: None }
    }

    #[test]
    fn sharing_off_moves_nothing() {
        let mut s = Sync::new(false);
        assert_eq!(s.start(), vec![Out::Browser(Control::ClipShare { on: false }), Out::Platform(Act::Share(false))]);
        assert!(s.host_changed(text(), Some("secret".into())).is_empty(), "a copy here is not announced");
        assert_eq!(s.browser_announced(text(), None), vec![Out::Browser(Control::ClipShare { on: false })], "the browser is told it is off");
        assert_eq!(platform(&s.render(1, FormatKind::Text)), vec![nothing(1, FormatKind::Text)]);
        let refused = browser(&s.browser_request(0, FormatKind::Text, "r1".into(), None));
        assert_eq!(refused, vec![Control::ClipboardFailed { gen: 0, id: "r1".into(), why: ClipboardError::Denied }]);
    }

    #[test]
    fn local_copy_then_remote_paste() {
        let mut s = Sync::new(true);
        let out = s.browser_announced(text(), Some("c1".into()));
        assert_eq!(platform(&out), vec![Act::Announce { gen: 1, formats: text() }]);
        assert!(browser(&out).is_empty(), "the browser hears of it once the placeholders are in place");
        assert_eq!(platform(&s.render(1, FormatKind::Text)), vec![nothing(1, FormatKind::Text)], "nothing placed yet");
        let out = browser(&s.announced(1, None));
        assert!(matches!(&out[0], Control::Clipboard { gen: Some(1), owner: Some(Owner::Browser), text: None, id: None, .. }), "{out:?}");
        assert!(s.announced(1, None).is_empty(), "answered once");
        // Something on this computer pastes: the browser is asked for that generation.
        let out = browser(&s.render(1, FormatKind::Text));
        let Control::ClipboardRequest { gen: 1, format: FormatKind::Text, id, i: None } = &out[0] else { panic!("{out:?}") };
        let id = id.clone();
        // The answer with the wrong id or generation is not taken.
        assert!(s.browser_data(1, FormatKind::Text, "other", Some("x".into()), None, None).is_empty());
        assert!(s.browser_data(0, FormatKind::Text, &id, Some("x".into()), None, None).is_empty());
        let out = s.browser_data(1, FormatKind::Text, &id, Some("hello".into()), None, None);
        assert_eq!(platform(&out), vec![Act::Provide { gen: 1, format: FormatKind::Text, text: Some("hello".into()), html: None }]);
        // Answered once only.
        assert!(s.browser_data(1, FormatKind::Text, &id, Some("again".into()), None, None).is_empty());
    }

    #[test]
    fn a_placeholder_of_an_older_generation_gets_nothing() {
        let mut s = Sync::new(true);
        here(&mut s, text());
        here(&mut s, text());
        assert_eq!(s.render(1, FormatKind::Text), vec![Out::Platform(nothing(1, FormatKind::Text))]);
        assert_eq!(platform(&s.render(2, FormatKind::Image)), vec![nothing(2, FormatKind::Image)], "a format not announced");
    }

    #[test]
    fn whatever_ends_a_wait_answers_it_with_nothing() {
        // Cancelled in the browser.
        let mut s = Sync::new(true);
        here(&mut s, text());
        let id = match &browser(&s.render(1, FormatKind::Text))[0] {
            Control::ClipboardRequest { id, .. } => id.clone(),
            other => panic!("{other:?}"),
        };
        assert!(s.browser_failed(1, "nope").is_empty());
        assert_eq!(platform(&s.browser_failed(1, &id)), vec![nothing(1, FormatKind::Text)]);
        assert!(s.browser_failed(1, &id).is_empty(), "once");
        // A newer copy on either side.
        s.render(1, FormatKind::Text);
        let out = s.host_changed(text(), Some("t".into()));
        assert_eq!(platform(&out), vec![nothing(1, FormatKind::Text)]);
        assert!(matches!(&browser(&out)[0], Control::Clipboard { gen: Some(2), owner: Some(Owner::Host), text: Some(_), .. }));
        // Sharing off, and the session ending.
        here(&mut s, text());
        s.render(3, FormatKind::Text);
        assert_eq!(platform(&s.set_share(false)), vec![nothing(3, FormatKind::Text), Act::Share(false)]);
        // (The session ending switches the platform off, which answers a waiting paste there.)
    }

    #[test]
    fn large_answers_follow_on_the_file_channel() {
        let mut s = Sync::new(true);
        here(&mut s, vec![Format { w: Some(4), h: Some(4), size: Some(900_000), ..Format::of(FormatKind::Image) }]);
        let id = match &browser(&s.render(1, FormatKind::Image))[0] {
            Control::ClipboardRequest { id, .. } => id.clone(),
            other => panic!("{other:?}"),
        };
        assert!(s.browser_data(1, FormatKind::Image, &id, None, None, Some(900_000)).is_empty(), "the transfer answers it");
        // Cancelled while the bytes are under way: still answered with nothing.
        assert_eq!(platform(&s.browser_failed(1, &id)), vec![nothing(1, FormatKind::Image)]);
        s.render(1, FormatKind::Image);
        s.transfer_ended("p2");
        assert!(s.set_share(false).iter().all(|o| !matches!(o, Out::Platform(Act::Provide { .. }))), "the transfer answered it");
    }

    #[test]
    fn remote_copy_then_paste_here() {
        let mut s = Sync::new(true);
        let files = vec![Format { items: Some(vec![wire::types::Item { n: "a.pdf".into(), s: 10 }]), ..Format::of(FormatKind::Files) }, Format::of(FormatKind::Text)];
        let out = browser(&s.host_changed(files, Some("C:\\a.pdf".into())));
        assert!(matches!(&out[0], Control::Clipboard { gen: Some(1), owner: Some(Owner::Host), .. }));
        assert_eq!(platform(&s.browser_request(1, FormatKind::Files, "g1".into(), Some(0))), vec![Act::Read { gen: 1, format: FormatKind::Files, id: "g1".into(), i: Some(0) }]);
        let gone = |s: &mut Sync, gen| browser(&s.browser_request(gen, FormatKind::Files, "g2".into(), Some(0)));
        assert_eq!(gone(&mut s, 0), vec![Control::ClipboardFailed { gen: 0, id: "g2".into(), why: ClipboardError::Gone }]);
        assert!(matches!(browser(&s.browser_request(1, FormatKind::Image, "g3".into(), None))[0], Control::ClipboardFailed { why: ClipboardError::Unsupported, .. }));
        assert!(s.browser_request(1, FormatKind::Files, String::new(), None).is_empty(), "no id");
        // The browser copies something itself: the computer's data is gone.
        here(&mut s, text());
        assert_eq!(gone(&mut s, 1)[0], Control::ClipboardFailed { gen: 1, id: "g2".into(), why: ClipboardError::Gone });
        let out = browser(&s.read_done(1, FormatKind::Image, "g1".into(), None, None, Some(7)));
        assert_eq!(out, vec![Control::ClipboardData { gen: 1, format: FormatKind::Image, id: "g1".into(), text: None, html: None, size: Some(7) }]);
    }

    #[test]
    fn an_announcement_that_cannot_be_placed_is_refused_by_its_id() {
        let mut s = Sync::new(true);
        here(&mut s, text());
        s.browser_announced(text(), Some("c2".into()));
        let out = browser(&s.announced(2, Some(ClipboardError::Io)));
        assert_eq!(out, vec![Control::ClipboardFailed { gen: 2, id: "c2".into(), why: ClipboardError::Io }]);
        // Nobody owns that generation: a paste from it, or from the older one, gets nothing.
        assert_eq!(platform(&s.render(2, FormatKind::Text)), vec![nothing(2, FormatKind::Text)]);
        assert_eq!(platform(&s.render(1, FormatKind::Text)), vec![nothing(1, FormatKind::Text)]);
        // A late answer for a generation already replaced changes nothing.
        s.browser_announced(text(), Some("c3".into()));
        s.browser_announced(text(), Some("c4".into()));
        assert!(s.announced(3, None).is_empty());
        assert!(matches!(&browser(&s.announced(4, None))[0], Control::Clipboard { gen: Some(4), .. }));
        // A copy on this computer meanwhile wins: the browser's answer is dropped.
        s.browser_announced(text(), Some("c5".into()));
        s.host_changed(text(), None);
        assert!(s.announced(5, None).is_empty());
    }

    #[test]
    fn the_browser_offers_only_what_it_can_read() {
        let mut s = Sync::new(true);
        let out = s.browser_announced(vec![Format::of(FormatKind::Files), Format::of(FormatKind::Html)], None);
        assert_eq!(platform(&out), vec![Act::Announce { gen: 1, formats: vec![Format::of(FormatKind::Html)] }]);
        // An empty list is a copy of something nothing here takes: the clipboard empties.
        assert_eq!(platform(&s.browser_announced(Vec::new(), None)), vec![Act::Announce { gen: 2, formats: Vec::new() }]);
    }
}
