//! Keep-awake policy: what this process asks of the operating system so the computer stays
//! reachable.
//!
//! Three reasons to stay awake, each held by the process that knows it:
//! - the `prevent_sleep` setting, by the always-running part (the Windows network service, the
//!   macOS app). The system stays awake; the display may still turn off, and a closed laptop
//!   lid follows the system's own setting.
//! - a session in progress, by the process that runs the platform (the Windows agent, the macOS
//!   app). The display stays on too: someone is watching it.
//! - an AI client on the AI seat, by the always-running part, which holds the seats. The system
//!   stays awake for it; the display may turn off: nobody needs to watch.
//!
//! The native request (`platform::PowerRequest`) is held while it lives and released on drop.

use std::sync::Mutex;

use anyhow::Result;

/// What to ask of the operating system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Need {
    /// No idle system sleep.
    pub system: bool,
    /// No idle display sleep.
    pub display: bool,
}

impl Need {
    pub fn any(self) -> bool {
        self.system || self.display
    }
}

/// The policy: the setting and an AI client keep the system awake, a session the system and the
/// display.
pub fn need(prevent_sleep: bool, session: bool, ai: bool) -> Need {
    Need { system: prevent_sleep || session || ai, display: session }
}

/// A native keep-awake request, active from creation until dropped.
pub trait Hold: Sized {
    fn hold(need: Need) -> Result<Self>;
}

/// The reasons this process knows of, and the request it holds for them.
pub struct Power<H> {
    prevent_sleep: bool,
    session: bool,
    ai: bool,
    held: Option<(Need, H)>,
}

impl<H: Hold> Power<H> {
    pub const fn new() -> Self {
        Power { prevent_sleep: false, session: false, ai: false, held: None }
    }

    pub fn set_ai(&mut self, on: bool) -> Result<()> {
        self.ai = on;
        self.apply()
    }

    pub fn set_prevent_sleep(&mut self, on: bool) -> Result<()> {
        self.prevent_sleep = on;
        self.apply()
    }

    pub fn set_session(&mut self, on: bool) -> Result<()> {
        self.session = on;
        self.apply()
    }

    /// What is held now; less than wanted when the system refused.
    pub fn held(&self) -> Need {
        self.held.as_ref().map(|(n, _)| *n).unwrap_or_default()
    }

    /// Replaces the request only when the need changed. A refused request holds nothing, and
    /// the next change tries again.
    fn apply(&mut self) -> Result<()> {
        let want = need(self.prevent_sleep, self.session, self.ai);
        if self.held() == want {
            return Ok(());
        }
        self.held = None;
        if want.any() {
            self.held = Some((want, H::hold(want)?));
        }
        Ok(())
    }
}

static POWER: Mutex<Power<crate::platform::PowerRequest>> = Mutex::new(Power::new());

/// The `prevent_sleep` setting, applied at once.
pub fn prevent_sleep(on: bool) -> Result<()> {
    POWER.lock().unwrap().set_prevent_sleep(on)
}

/// A session started or ended in this process.
pub fn session(on: bool) {
    if let Err(e) = POWER.lock().unwrap().set_session(on) {
        eprintln!("cannot keep the computer awake during the session: {e:#}");
    }
}

/// An AI client holds the AI seat, or no longer does.
pub fn ai_session(on: bool) {
    if let Err(e) = POWER.lock().unwrap().set_ai(on) {
        eprintln!("cannot keep the computer awake for the AI client: {e:#}");
    }
}

/// Whether the setting's request is in force. The macOS panel shows it; the Windows panel runs
/// in another process than the request.
#[cfg(target_os = "macos")]
pub fn awake() -> bool {
    POWER.lock().unwrap().held().system
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static HELD: AtomicUsize = AtomicUsize::new(0);
    static REFUSE: AtomicBool = AtomicBool::new(false);

    struct Fake(Need);
    impl Hold for Fake {
        fn hold(need: Need) -> Result<Self> {
            anyhow::ensure!(!REFUSE.load(Ordering::SeqCst), "refused");
            HELD.fetch_add(1, Ordering::SeqCst);
            Ok(Fake(need))
        }
    }
    impl Drop for Fake {
        fn drop(&mut self) {
            HELD.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn the_setting_and_an_ai_client_keep_the_system_and_a_session_the_display_too() {
        // (setting, session, AI client) -> need
        for (p, s, a, want) in [
            (false, false, false, Need::default()),
            (true, false, false, Need { system: true, display: false }),
            (false, true, false, Need { system: true, display: true }),
            (true, true, false, Need { system: true, display: true }),
            (false, false, true, Need { system: true, display: false }),
            (false, true, true, Need { system: true, display: true }),
        ] {
            assert_eq!(need(p, s, a), want, "{p} {s} {a}");
        }
    }

    #[test]
    fn one_request_at_a_time_released_when_nothing_needs_it() {
        let mut p = Power::<Fake>::new();
        p.set_prevent_sleep(true).unwrap();
        assert_eq!((p.held(), HELD.load(Ordering::SeqCst)), (Need { system: true, display: false }, 1));
        p.set_session(true).unwrap();
        assert_eq!((p.held(), HELD.load(Ordering::SeqCst)), (Need { system: true, display: true }, 1));
        assert_eq!(p.held.as_ref().unwrap().1 .0, Need { system: true, display: true });
        p.set_session(false).unwrap();
        assert_eq!(p.held(), Need { system: true, display: false });
        p.set_prevent_sleep(false).unwrap();
        assert_eq!((p.held(), HELD.load(Ordering::SeqCst)), (Need::default(), 0));

        // Refused: nothing is held, and the next change asks again.
        REFUSE.store(true, Ordering::SeqCst);
        assert!(p.set_prevent_sleep(true).is_err());
        assert_eq!(p.held(), Need::default());
        REFUSE.store(false, Ordering::SeqCst);
        p.set_prevent_sleep(true).unwrap();
        assert_eq!((p.held(), HELD.load(Ordering::SeqCst)), (Need { system: true, display: false }, 1));
        drop(p);
        assert_eq!(HELD.load(Ordering::SeqCst), 0);
    }
}
