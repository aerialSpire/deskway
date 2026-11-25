//! Keep-awake policy: what this process asks of the operating system so the computer stays
//! reachable.
//!
//! Three reasons to stay awake, each held by the process that knows it:
//! - the `prevent_sleep` setting, by the always-running part (the Windows network service, the
//!   macOS app), except when running on battery below the threshold. The display may still
//!   turn off, and a closed laptop lid follows the system's own setting.
//! - a session in progress, by the process that runs the platform (the Windows agent, the macOS
//!   app). The display stays on too: someone is watching it.
//! - an AI client on the AI seat, by the always-running part, which holds the seats. The system
//!   stays awake for it; the display may turn off: nobody needs to watch.
//!
//! The native request (`platform::PowerRequest`) is held while it lives and released on drop.

use std::sync::Mutex;

use anyhow::Result;

use crate::platform::{PowerNotifications, PowerRequest, PowerState};

pub const MIN_BATTERY_PERCENT: u8 = 15;

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

/// A low battery suspends the setting. A desk session keeps the system and display awake,
/// and an AI client keeps the system awake.
pub fn need(prevent_sleep: bool, session: bool, ai: bool, power: PowerState) -> Need {
    let low_battery = matches!(power, PowerState::Battery(percent) if percent < MIN_BATTERY_PERCENT);
    Need { system: (prevent_sleep && !low_battery) || session || ai, display: session }
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
    state: PowerState,
    held: Option<(Need, H)>,
}

impl<H: Hold> Power<H> {
    pub const fn new() -> Self {
        Power { prevent_sleep: false, session: false, ai: false, state: PowerState::External, held: None }
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

    fn power_changed(&mut self, state: PowerState) -> Result<()> {
        // The OS may have ended the request while on battery.
        if matches!((self.state, state), (PowerState::Battery(_), PowerState::External)) {
            self.held = None;
        }
        self.state = state;
        self.apply()
    }

    /// What is held now; less than wanted when the system refused.
    pub fn held(&self) -> Need {
        self.held.as_ref().map(|(n, _)| *n).unwrap_or_default()
    }

    /// Replaces the request only when the need changed. A refused request holds nothing, and
    /// the next change tries again.
    fn apply(&mut self) -> Result<()> {
        let want = need(self.prevent_sleep, self.session, self.ai, self.state);
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

static POWER: Mutex<(Power<PowerRequest>, Option<PowerNotifications>)> = Mutex::new((Power::new(), None));

/// The `prevent_sleep` setting, applied at once.
pub fn prevent_sleep(on: bool) -> Result<()> {
    // Registration can call back immediately. Unregistering can wait for callbacks.
    let notifications = if on {
        PowerNotifications::register().map_err(|e| eprintln!("cannot register for power source notifications: {e:#}")).ok()
    } else {
        None
    };
    let mut guard = POWER.lock().unwrap();
    let (power, active) = &mut *guard;
    let previous = std::mem::replace(active, notifications);
    power.state = active.as_ref().map_or(PowerState::External, PowerNotifications::state);
    let result = power.set_prevent_sleep(on);
    drop(guard);
    drop(previous);
    result
}

/// Read and apply each fresh snapshot under the same lock.
pub fn power_source_changed() {
    let (power, notifications) = &mut *POWER.lock().unwrap();
    if let Some(notifications) = notifications {
        if let Err(e) = power.power_changed(notifications.state()) {
            eprintln!("cannot update the keep-awake request: {e:#}");
        }
    }
}

/// A session started or ended in this process.
pub fn session(on: bool) {
    if let Err(e) = POWER.lock().unwrap().0.set_session(on) {
        eprintln!("cannot keep the computer awake during the session: {e:#}");
    }
}

/// An AI client holds the AI seat, or no longer does.
pub fn ai_session(on: bool) {
    if let Err(e) = POWER.lock().unwrap().0.set_ai(on) {
        eprintln!("cannot keep the computer awake for the AI client: {e:#}");
    }
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
    fn keep_awake_policy() {
        use PowerState::{Battery, External};

        // (setting, desk session, AI client, power) -> need
        for (p, s, a, power, want) in [
            (false, false, false, External, Need::default()),
            (true, false, false, External, Need { system: true, display: false }),
            (true, false, false, Battery(14), Need::default()),
            (true, false, false, Battery(15), Need { system: true, display: false }),
            (false, false, false, Battery(100), Need::default()),
            (false, true, false, External, Need { system: true, display: true }),
            (true, true, false, Battery(14), Need { system: true, display: true }),
            (false, false, true, External, Need { system: true, display: false }),
            (true, false, true, Battery(14), Need { system: true, display: false }),
            (false, true, true, External, Need { system: true, display: true }),
        ] {
            assert_eq!(need(p, s, a, power), want, "{p} {s} {a} {power:?}");
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
