//! The two seats of a computer (docs/architecture.md, "Two seats side by side"): the desk
//! seat, held by a person's browser, and the AI seat, held by an AI client. They do not exclude
//! each other; on each, one session at a time.

use wire::types::Role;

use super::registry::{Entry, Registry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    Desk,
    Ai,
}

impl Seat {
    /// The seat a device of `role` takes.
    pub fn of(role: Role) -> Seat {
        match role {
            Role::Browser => Seat::Desk,
            Role::Ai => Seat::Ai,
        }
    }

    pub const BOTH: [Seat; 2] = [Seat::Desk, Seat::Ai];
}

pub struct Seats<T> {
    desk: Registry<T>,
    ai: Registry<T>,
}

impl<T> Default for Seats<T> {
    fn default() -> Self {
        Seats { desk: Registry::default(), ai: Registry::rejoining() }
    }
}

impl<T> Seats<T> {
    pub fn of(&self, seat: Seat) -> &Registry<T> {
        match seat {
            Seat::Desk => &self.desk,
            Seat::Ai => &self.ai,
        }
    }

    pub fn of_mut(&mut self, seat: Seat) -> &mut Registry<T> {
        match seat {
            Seat::Desk => &mut self.desk,
            Seat::Ai => &mut self.ai,
        }
    }

    /// The seat holding session `id`, and its entry.
    pub fn find(&self, id: &str) -> Option<(Seat, &Entry<T>)> {
        Seat::BOTH.into_iter().find_map(|s| self.of(s).get(id).map(|e| (s, e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, device: &str) -> Entry<u32> {
        Entry { id: id.into(), name: device.into(), user: "u".into(), controller: device.into(), since: 0, handle: 0 }
    }

    #[test]
    fn a_browser_and_an_ai_client_each_hold_their_own_seat() {
        let mut s: Seats<u32> = Seats::default();
        s.of_mut(Seat::of(Role::Browser)).start(entry("d1", "phone"));
        assert!(matches!(s.of(Seat::of(Role::Ai)).admit("claude", false), super::super::registry::Admit::Free), "the desk seat held, the AI seat free");
        s.of_mut(Seat::Ai).start(entry("a1", "claude"));
        assert_eq!(s.find("d1").map(|(seat, e)| (seat, e.controller.as_str())), Some((Seat::Desk, "phone")));
        assert_eq!(s.find("a1").map(|(seat, _)| seat), Some(Seat::Ai));
        assert!(s.find("x").is_none());
    }
}
