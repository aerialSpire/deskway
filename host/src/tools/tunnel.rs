//! `tunnel`: lets this AI session's client reach one port of this computer's loopback, on data
//! channels named for it (`peer::pipe`, carried by `session::ai`). Only loopback, and only a
//! port something listens on now: the call is how the gate and the audit log see a tunnel.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

/// The ports an AI session's client may reach: one set, which its tools add to and its session
/// reads when a channel opens.
pub type Allowed = Arc<Mutex<BTreeSet<u16>>>;

/// Where on this computer a tunnel's connections go.
pub fn place(port: u16) -> SocketAddr {
    (Ipv4Addr::LOCALHOST, port).into()
}
