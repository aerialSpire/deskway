//! The parts of a WebRTC peer that do not depend on what the connection is for, shared by the
//! host and the `deskway` command: the network paths (`net`: local sockets, the address a STUN
//! server sees, TURN relays, binding again when the network changes) and the session's state
//! machine (`state`, docs/session-lifecycle.md), the same on both ends.
//!
//! Reader threads deliver what arrives as [`NetEvent`] into the owner's own event queue: the
//! owner's event type converts from it, so one queue carries everything its loop waits on.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub mod here;
pub mod net;
pub mod outbox;
pub mod pipe;
pub mod state;
pub mod stun;
pub mod turn;

/// What the network paths report to the connection's owner.
pub enum NetEvent {
    /// A packet arrived, on a local socket or through a relay (`destination` is the relayed address).
    Udp { data: Vec<u8>, source: SocketAddr, destination: SocketAddr },
    /// A relay was allocated and can be offered.
    RelayReady(turn::Relay),
    /// The STUN server's address, resolved off the owner's thread.
    StunServer(SocketAddr),
}

/// Stops one reader thread (a socket's or a relay's): by itself, or with the whole connection.
#[derive(Clone)]
pub struct Stop {
    all: Arc<AtomicBool>,
    own: Arc<AtomicBool>,
}

impl Stop {
    pub fn new(all: &Arc<AtomicBool>) -> Stop {
        Stop { all: all.clone(), own: Arc::new(AtomicBool::new(false)) }
    }

    pub fn stopped(&self) -> bool {
        self.all.load(Ordering::Relaxed) || self.own.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.own.store(true, Ordering::Relaxed);
    }
}
