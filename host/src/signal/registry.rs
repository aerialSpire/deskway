//! A seat's session registry: at most one session holds a seat of this computer (the desk seat
//! for a person's browser, the AI seat for an AI client; `seats`), and the single-use nonces
//! that new sessions prove themselves with. Pure (no network, no threads), so the rules are
//! tested on their own: admission by device (docs/session-lifecycle.md, "Admission"), the
//! holder the status names, and that a nonce works once, for its session, within its time.
//!
//! The holder is the session admitted last, connecting or established, until it ends. The
//! status names it either way; the panel shows a controller only once it is established
//! (control channel open).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wire::types::Holder;

/// How long an issued nonce stays valid (docs/protocol.md, "Connecting").
pub const NONCE_VALID: Duration = Duration::from_secs(60);
/// Nonces kept at most: a flood of challenges cannot grow memory.
const MAX_NONCES: usize = 64;

/// The single-use connect nonces, per session id.
#[derive(Default)]
pub struct Nonces {
    issued: HashMap<String, (String, Instant)>,
}

impl Nonces {
    /// The nonce of `session`: the one issued before while it is unused and valid (the
    /// challenge is idempotent: an offer signed with it still works however often the browser
    /// asks again), else a new one.
    pub fn issue(&mut self, session: &str, now: Instant) -> String {
        self.issued.retain(|_, (_, at)| now.duration_since(*at) < NONCE_VALID);
        if let Some((n, _)) = self.issued.get(session) {
            return n.clone();
        }
        if self.issued.len() >= MAX_NONCES {
            if let Some(oldest) = self.issued.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
                self.issued.remove(&oldest);
            }
        }
        let n = wire::challenge::new_nonce();
        self.issued.insert(session.to_string(), (n.clone(), now));
        n
    }

    /// Uses up the nonce of `session`: true only if it is `nonce` and still valid. Any attempt
    /// consumes it, so it cannot be tried twice.
    pub fn take(&mut self, session: &str, nonce: &str, now: Instant) -> bool {
        match self.issued.remove(session) {
            Some((n, at)) => !nonce.is_empty() && n == nonce && now.duration_since(at) < NONCE_VALID,
            None => false,
        }
    }
}

/// The session holding the computer and who it is.
pub struct Entry<T> {
    pub id: String,
    /// The controller's device name, as this host verified it.
    pub name: String,
    /// The controller's account, as the server gave it.
    pub user: String,
    /// The device in control (its credential id; empty with device checks off).
    pub controller: String,
    /// Unix seconds: when it was admitted.
    pub since: u64,
    pub handle: T,
}

/// The session that holds the computer, and whether it is established yet.
struct Current<T> {
    entry: Entry<T>,
    established: bool,
}

impl<T> Current<T> {
    fn holder(&self) -> Holder {
        let e = &self.entry;
        Holder { session: e.id.clone(), cred: Some(e.controller.clone()).filter(|c| !c.is_empty()), name: e.name.clone(), user: e.user.clone(), established: self.established, since: Some(e.since) }
    }
}

/// What a new offer meets (docs/session-lifecycle.md, "Admission").
#[derive(Debug, PartialEq)]
pub enum Admit {
    /// Nobody holds the computer.
    Free,
    /// The same device's session still connecting (on the AI seat: in any state): the new one
    /// replaces it (the same person trying again, an AI client's command started again).
    Replace,
    /// The offer was signed with `force`: the holder ends `taken`.
    TakeOver,
    /// Anyone else holds it: the browser is told who, and the person is asked.
    Busy { holder: Holder },
}

pub struct Registry<T> {
    current: Option<Current<T>>,
    /// The same device's new session replaces its established one too: the AI seat, whose one
    /// AI client per machine may have started again with its old session still held.
    rejoin: bool,
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Registry { current: None, rejoin: false }
    }
}

impl<T> Registry<T> {
    /// The AI seat's registry.
    pub fn rejoining() -> Registry<T> {
        Registry { current: None, rejoin: true }
    }

    /// Admission of an offer from the device `cred` (empty: no device, never the same one).
    pub fn admit(&self, cred: &str, force: bool) -> Admit {
        match &self.current {
            None => Admit::Free,
            Some(c) if (!c.established || self.rejoin) && !cred.is_empty() && c.entry.controller == cred => Admit::Replace,
            Some(_) if force => Admit::TakeOver,
            Some(c) => Admit::Busy { holder: c.holder() },
        }
    }

    /// Makes `e` the session holding the computer, still connecting; returns the one it replaces.
    pub fn start(&mut self, e: Entry<T>) -> Option<Entry<T>> {
        self.current.replace(Current { entry: e, established: false }).map(|c| c.entry)
    }

    /// The session holding the computer, established or not (to route to it, or end it).
    pub fn current(&self) -> Option<&Entry<T>> {
        self.current.as_ref().map(|c| &c.entry)
    }

    /// The session `id`, if it is the one holding the computer.
    pub fn get(&self, id: &str) -> Option<&Entry<T>> {
        self.current().filter(|c| c.id == id)
    }

    /// Session `id` reached Active: from now on it controls the computer. True if that changed
    /// what is reported.
    pub fn established(&mut self, id: &str) -> bool {
        match self.current.as_mut() {
            Some(c) if c.entry.id == id && !c.established => {
                c.established = true;
                true
            }
            _ => false,
        }
    }

    /// Session `id` is over; true if it was the one holding the computer (an older, replaced
    /// one is not).
    pub fn ended(&mut self, id: &str) -> bool {
        if self.get(id).is_some() {
            self.current = None;
            true
        } else {
            false
        }
    }

    /// The session holding the computer, connecting or established, as the status names it.
    pub fn holder(&self) -> Option<Holder> {
        self.current.as_ref().map(Current::holder)
    }

    /// The device (credential id) in control, if a session is established with one.
    pub fn controlling_device(&self) -> Option<&str> {
        match &self.current {
            Some(Current { entry: c, established: true }) if !c.controller.is_empty() => Some(c.controller.as_str()),
            _ => None,
        }
    }

    /// The controller's device name, if a session is established (the panel and indicator).
    pub fn controller(&self) -> Option<String> {
        match &self.current {
            Some(Current { entry: c, established: true }) => Some(c.name.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, device: &str) -> Entry<u32> {
        Entry { id: id.into(), name: format!("{device}-name"), user: format!("{device}-account"), controller: device.into(), since: 7, handle: 0 }
    }

    #[test]
    fn a_nonce_works_once_for_its_session() {
        let t0 = Instant::now();
        let mut n = Nonces::default();
        let a = n.issue("s1", t0);
        assert_eq!(a.len(), 22);
        assert!(!n.take("s2", &a, t0), "another session's nonce");
        assert!(n.take("s1", &a, t0));
        assert!(!n.take("s1", &a, t0), "single use");
        // A wrong guess uses it up too.
        let b = n.issue("s1", t0);
        assert!(!n.take("s1", "guess", t0));
        assert!(!n.take("s1", &b, t0));
        assert!(!n.take("s9", "", t0), "never issued");
    }

    #[test]
    fn a_nonce_expires() {
        let t0 = Instant::now();
        let mut n = Nonces::default();
        let a = n.issue("s1", t0);
        assert!(!n.take("s1", &a, t0 + NONCE_VALID), "expired");
        let b = n.issue("s1", t0);
        assert!(n.take("s1", &b, t0 + NONCE_VALID - Duration::from_millis(1)));
    }

    #[test]
    fn asking_again_gives_the_same_nonce_until_it_is_used_or_expires() {
        let t0 = Instant::now();
        let mut n = Nonces::default();
        let a = n.issue("s1", t0);
        assert_eq!(n.issue("s1", t0 + Duration::from_secs(10)), a, "the challenge again: the same nonce");
        assert_ne!(n.issue("s2", t0), a, "another session's is its own");
        assert!(n.take("s1", &a, t0 + Duration::from_secs(20)), "an offer signed with the first still works");
        let b = n.issue("s1", t0 + Duration::from_secs(21));
        assert_ne!(b, a, "used: a new one");
        let c = n.issue("s1", t0 + Duration::from_secs(21) + NONCE_VALID);
        assert_ne!(c, b, "expired: a new one");
    }

    #[test]
    fn nonces_are_bounded() {
        let t0 = Instant::now();
        let mut n = Nonces::default();
        for i in 0..1000 {
            n.issue(&format!("s{i}"), t0 + Duration::from_millis(i));
        }
        assert!(n.issued.len() <= MAX_NONCES);
    }

    #[test]
    fn admission_by_device() {
        let mut r: Registry<u32> = Registry::default();
        assert_eq!(r.admit("A", false), Admit::Free, "nobody holds it");
        r.start(entry("s1", "A"));
        let connecting = Admit::Busy { holder: Holder { session: "s1".into(), cred: Some("A".into()), name: "A-name".into(), user: "A-account".into(), established: false, since: Some(7) } };
        assert_eq!(r.admit("A", false), Admit::Replace, "the same device still connecting: replaced without asking");
        assert_eq!(r.admit("A", true), Admit::Replace);
        assert_eq!(r.admit("B", false), connecting, "another device still connecting: busy, named");
        assert_eq!(r.admit("B", true), Admit::TakeOver);
        r.established("s1");
        let Admit::Busy { holder } = r.admit("A", false) else { panic!("an established session of the same device is busy") };
        assert!(holder.established);
        assert_eq!(r.admit("A", true), Admit::TakeOver);
        assert!(matches!(r.admit("B", false), Admit::Busy { .. }));
        // Without a device (checks off) nobody is "the same device".
        let mut r: Registry<u32> = Registry::default();
        r.start(entry("s1", ""));
        assert!(matches!(r.admit("", false), Admit::Busy { .. }), "an empty credential is never the same device");
        assert_eq!(r.admit("", true), Admit::TakeOver);
    }

    #[test]
    fn on_the_ai_seat_the_same_device_always_takes_its_place_and_no_other_does() {
        let mut r: Registry<u32> = Registry::rejoining();
        r.start(entry("s1", "A"));
        r.established("s1");
        assert_eq!(r.admit("A", false), Admit::Replace, "the AI client started again, its old session still held");
        assert!(matches!(r.admit("B", false), Admit::Busy { .. }), "another AI client");
    }

    #[test]
    fn the_holder_is_reported_connecting_and_established() {
        let mut r: Registry<u32> = Registry::default();
        assert_eq!(r.holder(), None);
        r.start(entry("s1", "A"));
        assert_eq!(r.holder(), Some(Holder { session: "s1".into(), cred: Some("A".into()), name: "A-name".into(), user: "A-account".into(), established: false, since: Some(7) }));
        assert_eq!(r.controller(), None, "the panel shows a controller only once established");
        assert!(r.established("s1"));
        assert!(!r.established("s1"), "reported once");
        assert!(r.holder().unwrap().established);
        assert_eq!(r.controller(), Some("A-name".into()));
        assert_eq!(r.controlling_device(), Some("A"));
        r.start(entry("s2", ""));
        assert_eq!(r.holder().unwrap().cred, None, "no device: no credential on the wire");
    }

    #[test]
    fn a_replaced_session_ending_leaves_its_successor() {
        let mut r: Registry<u32> = Registry::default();
        r.start(entry("s1", "A"));
        r.established("s1");
        let old = r.start(entry("s2", "B")).expect("the replaced session");
        assert_eq!(old.id, "s1");
        assert!(r.get("s1").is_none() && r.get("s2").is_some());
        assert!(!r.established("s1"), "a late report for the replaced one changes nothing");
        assert!(!r.ended("s1"), "the replaced session finishing does not free the computer");
        assert_eq!(r.current().unwrap().id, "s2");
        assert!(r.ended("s2"));
        assert_eq!(r.admit("A", false), Admit::Free);
    }
}
