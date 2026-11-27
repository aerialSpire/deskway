//! Child process lifetime and restart backoff, shared by the service and the local hub.

use std::time::{Duration, Instant};

use anyhow::Result;

pub trait Child {
    fn running(&mut self) -> bool;
    fn stop(&mut self);
}

pub struct Kept<C: Child, K = ()> {
    what: &'static str,
    child: Option<(C, K, Instant)>,
    next_try: Instant,
    failures: u32,
    last_error: Option<String>,
}

impl<C: Child, K: Eq> Kept<C, K> {
    pub fn new(what: &'static str) -> Self {
        Self {
            what,
            child: None,
            next_try: Instant::now(),
            failures: 0,
            last_error: None,
        }
    }

    pub fn keep(&mut self, key: K, launch: impl FnOnce() -> Result<C>) {
        self.tick(Instant::now(), key, launch);
    }

    fn tick(&mut self, now: Instant, key: K, launch: impl FnOnce() -> Result<C>) {
        if let Some((child, previous, started)) = &mut self.child {
            if *previous != key {
                child.stop();
                self.child = None;
                self.failures = 0;
                self.next_try = now;
            } else if child.running() {
                if now.duration_since(*started) >= Duration::from_secs(60) {
                    self.failures = 0;
                }
                return;
            } else {
                eprintln!("{} exited", self.what);
                child.stop();
                self.child = None;
                self.back_off(now);
            }
        }
        if now < self.next_try {
            return;
        }
        match launch() {
            Ok(child) => {
                eprintln!("{} started", self.what);
                self.child = Some((child, key, now));
                self.last_error = None;
            }
            Err(err) => {
                let error = format!("{err:#}");
                if self.last_error.as_ref() != Some(&error) {
                    eprintln!("cannot start {}: {error}", self.what);
                }
                self.last_error = Some(error);
                self.back_off(now);
            }
        }
    }

    fn back_off(&mut self, now: Instant) {
        self.next_try = now + retry_delay(self.failures);
        self.failures = self.failures.saturating_add(1);
    }

    pub fn stop(&mut self) {
        if let Some((mut child, _, _)) = self.child.take() {
            child.stop();
        }
        self.reset();
    }

    pub fn reset(&mut self) {
        self.failures = 0;
        self.last_error = None;
        self.next_try = Instant::now();
    }

    pub fn child(&mut self) -> Option<&mut C> {
        self.child.as_mut().map(|(child, _, _)| child)
    }

    #[cfg(desktop_host)]
    pub fn next_retry(&self) -> Option<Instant> {
        self.child.is_none().then_some(self.next_try)
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    #[cfg(desktop_host)]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

pub fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((1 << failures.min(6)).min(60))
}

impl<C: Child, K> Drop for Kept<C, K> {
    fn drop(&mut self) {
        if let Some((mut child, _, _)) = self.child.take() {
            child.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Exited;

    impl Child for Exited {
        fn running(&mut self) -> bool {
            false
        }
        fn stop(&mut self) {}
    }

    #[test]
    fn a_child_that_exits_repeatedly_backs_off_and_can_be_retried() {
        let mut kept = Kept::new("test");
        let now = Instant::now();
        for (seconds, launch, failures) in [
            (0, true, 0),
            (1, false, 1),
            (2, true, 1),
            (3, false, 2),
            (4, false, 2),
            (5, true, 2),
        ] {
            let mut launched = false;
            kept.tick(now + Duration::from_secs(seconds), (), || {
                launched = true;
                Ok(Exited)
            });
            assert_eq!(
                (launched, kept.failures()),
                (launch, failures),
                "at {seconds}s"
            );
        }
        kept.stop();
        assert_eq!(kept.failures(), 0);
    }
}
