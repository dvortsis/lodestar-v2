//! The loopback gate (X-cmp09-fix-2): a process-wide check, set once,
//! that a connection to a loopback address on a given port may be made
//! now.
//!
//! Lodestar sets it at its start ([`set_loopback_gate`]) over the ports of
//! the companions it runs: such a port is allowed only while its companion
//! is ready. Every [`super::GuardedClient`] and [`super::resolve_tcp`]
//! asks it before connecting to a loopback address, whatever the caller.
//! With no gate set, nothing changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Whether a loopback port may be connected to now.
pub trait LoopbackGate: Send + Sync {
    /// Whether a connection to a loopback address on `port` may be made
    /// now. A port the gate doesn't know is always allowed.
    fn allow(&self, port: u16) -> bool;
}

static GATE: OnceLock<Arc<dyn LoopbackGate>> = OnceLock::new();

/// The gate was set already: it's set once and never swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateAlreadySet;

impl std::fmt::Display for GateAlreadySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the loopback gate is already set")
    }
}

impl std::error::Error for GateAlreadySet {}

/// Sets the process's loopback gate. Once only: a second call is refused
/// and leaves the first in place.
pub fn set_loopback_gate(gate: Arc<dyn LoopbackGate>) -> Result<(), GateAlreadySet> {
    GATE.set(gate).map_err(|_| GateAlreadySet)
}

/// Whether the gate is set.
pub fn loopback_gate_is_set() -> bool {
    GATE.get().is_some()
}

/// Whether a connection to a loopback address on `port` may be made now:
/// always, with no gate.
pub(super) fn allows(port: u16) -> bool {
    GATE.get().is_none_or(|g| g.allow(port))
}

/// The ports each host name is being sent to by one client's requests now,
/// so its resolver can ask the gate about a name that resolves to loopback
/// (the resolver sees only the name).
#[derive(Debug, Default)]
pub(super) struct Pending {
    names: Mutex<HashMap<String, Vec<u16>>>,
}

impl Pending {
    fn names(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<u16>>> {
        self.names.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a request to `host` on `port` until the guard drops.
    pub(super) fn enter(self: &Arc<Self>, host: &str, port: u16) -> Entered {
        let host = normal(host);
        self.names().entry(host.clone()).or_default().push(port);
        Entered {
            pending: Arc::clone(self),
            host,
            port,
        }
    }

    /// Whether every port `host` is being sent to now is allowed. A name
    /// no request has entered has none, so nothing to refuse.
    pub(super) fn allowed(&self, host: &str) -> bool {
        let ports = self.names().get(&normal(host)).cloned().unwrap_or_default();
        ports.into_iter().all(allows)
    }
}

/// A request entered in [`Pending`], removed when dropped.
#[derive(Debug)]
pub(super) struct Entered {
    pending: Arc<Pending>,
    host: String,
    port: u16,
}

impl Drop for Entered {
    fn drop(&mut self) {
        let mut names = self.pending.names();
        if let Some(ports) = names.get_mut(&self.host) {
            if let Some(i) = ports.iter().position(|p| *p == self.port) {
                ports.swap_remove(i);
            }
            if ports.is_empty() {
                names.remove(&self.host);
            }
        }
    }
}

/// A host name as both the URL and the resolver give it: lower case, no
/// root dot, no IPv6 brackets.
fn normal(host: &str) -> String {
    let h = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    h.trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_names_are_entered_and_left() {
        let p = Arc::new(Pending::default());
        assert!(p.allowed("localhost"));
        let a = p.enter("LocalHost.", 8080);
        let b = p.enter("localhost", 9696);
        assert_eq!(p.names()["localhost"].len(), 2);
        drop(a);
        assert_eq!(p.names()["localhost"], vec![9696]);
        drop(b);
        assert!(p.names().is_empty());
        assert_eq!(normal("[::1]"), "::1");
    }
}
