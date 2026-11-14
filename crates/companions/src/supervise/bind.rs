//! Which loopback ports belong to a companion Lodestar runs, and whether
//! the child it started there is ready (X-cmp-supervisor-bind).
//!
//! A managed companion is launched bound to `127.0.0.1` on its port. While
//! Lodestar holds the child it started there, the port is *managed*, and
//! [`crate::detect::CompanionHttp`] treats it differently from a port a
//! hand-run companion listens on:
//! - every request to it goes to `127.0.0.1` alone, never to `::1`;
//! - a request that carries anything (a header, a body or a query) goes
//!   out only while the child is running and has been found ready in
//!   *this* run of it: [`Supervised`](crate::manager::Supervised) marks it
//!   ready once a health check on `127.0.0.1` answered while that run was
//!   the one running. A restart starts a new run, which isn't ready until
//!   it's found so again.
//!
//! A port stays managed for the life of the [`Bindings`] once a companion
//! was started on it, running or not, so a stopped companion's port never
//! falls back to looking on `::1`. One companion holds a port at a time:
//! a second start on a port another holds is refused ([`Bindings::claim`]).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use tokio::sync::watch;

use super::{State, Status};

/// The managed ports, each with its child's status and the run it was
/// last found ready in. Shared by the supervisor's side and the client's.
#[derive(Debug, Default)]
pub struct Bindings {
    ports: Mutex<HashMap<u16, Port>>,
}

#[derive(Debug, Default)]
struct Port {
    /// A companion holds it: claimed at its start, until its stop.
    held: bool,
    /// The child's status while Lodestar holds it; `None` before it's
    /// attached and once it's stopped or removed.
    status: Option<watch::Receiver<Status>>,
    /// The run (`Status::run`) a health check last found ready.
    ready: Option<u64>,
}

impl Bindings {
    pub fn new() -> Bindings {
        Bindings::default()
    }

    fn ports(&self) -> std::sync::MutexGuard<'_, HashMap<u16, Port>> {
        self.ports.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `port` is managed from now on and held for a companion about to
    /// start, with nothing ready on it. False, changing nothing, when
    /// another companion holds it.
    pub fn claim(&self, port: u16) -> bool {
        let mut ports = self.ports();
        let p = ports.entry(port).or_default();
        if p.held {
            return false;
        }
        *p = Port {
            held: true,
            status: None,
            ready: None,
        };
        true
    }

    /// The status of the child started on a port [`Bindings::claim`]ed;
    /// nothing is ready until [`Bindings::mark_ready`].
    pub fn attach(&self, port: u16, status: watch::Receiver<Status>) {
        if let Some(p) = self.ports().get_mut(&port).filter(|p| p.held) {
            p.status = Some(status);
            p.ready = None;
        }
    }

    /// [`Bindings::claim`] and [`Bindings::attach`] at once.
    pub fn hold(&self, port: u16, status: watch::Receiver<Status>) -> bool {
        let claimed = self.claim(port);
        if claimed {
            self.attach(port, status);
        }
        claimed
    }

    /// The child on `port` is gone (stopped or removed, or its start
    /// failed). The port stays managed, and nothing is ready on it.
    pub fn release(&self, port: u16) {
        if let Some(p) = self.ports().get_mut(&port) {
            *p = Port::default();
        }
    }

    /// Whether `port` belongs to a companion Lodestar runs (or ran).
    pub fn managed(&self, port: u16) -> bool {
        self.ports().contains_key(&port)
    }

    /// The run of the child on `port` that is running now, if one is.
    pub fn running_run(&self, port: u16) -> Option<u64> {
        let ports = self.ports();
        let s = *ports.get(&port)?.status.as_ref()?.borrow();
        running(&s).then_some(s.run)
    }

    /// A health check on `127.0.0.1` answered while run `run` was the
    /// one running: it's ready, if it's still that run.
    pub fn mark_ready(&self, port: u16, run: u64) {
        let mut ports = self.ports();
        if let Some(p) = ports.get_mut(&port) {
            let now = p.status.as_ref().map(|s| *s.borrow());
            if now.is_some_and(|s| running(&s) && s.run == run) {
                p.ready = Some(run);
            }
        }
    }

    /// Whether a request that carries something may go to `port` now: the
    /// child Lodestar holds there is running, in the run found ready.
    pub fn ready(&self, port: u16) -> bool {
        let ports = self.ports();
        let Some(p) = ports.get(&port) else {
            return false;
        };
        let (Some(status), Some(ready)) = (p.status.as_ref(), p.ready) else {
            return false;
        };
        let s = status.borrow();
        running(&s) && s.run == ready
    }
}

/// A child process is up: running, or running with failed health checks,
/// and not yet found gone or told to stop.
fn running(s: &Status) -> bool {
    s.up && matches!(s.state, State::Running | State::Unhealthy { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: State, run: u64) -> Status {
        let up = matches!(state, State::Running | State::Unhealthy { .. });
        Status {
            state,
            restarts: 0,
            last_exit: None,
            run,
            up,
        }
    }

    #[test]
    fn a_port_is_ready_only_in_the_run_found_ready() {
        let b = Bindings::new();
        let (tx, rx) = watch::channel(status(State::Starting, 0));
        assert!(!b.managed(8080));
        assert!(b.hold(8080, rx));
        assert!(b.managed(8080) && !b.ready(8080));
        // Not running yet: a check can't make it ready.
        b.mark_ready(8080, 0);
        assert!(!b.ready(8080));
        tx.send_replace(status(State::Running, 1));
        assert_eq!(b.running_run(8080), Some(1));
        b.mark_ready(8080, 1);
        assert!(b.ready(8080));
        // Failed health checks don't end the run.
        tx.send_replace(status(State::Unhealthy { failures: 1 }, 1));
        assert!(b.ready(8080));
        // Found gone: not ready at once, before its state moves on.
        tx.send_modify(|s| s.up = false);
        assert!(!b.ready(8080));
        assert_eq!(b.running_run(8080), None);
        // A restart: a new run, not ready until found so.
        tx.send_replace(status(State::Starting, 1));
        assert!(!b.ready(8080));
        tx.send_replace(status(State::Running, 2));
        assert!(!b.ready(8080));
        // A check from the old run counts for nothing.
        b.mark_ready(8080, 1);
        assert!(!b.ready(8080));
        b.mark_ready(8080, 2);
        assert!(b.ready(8080));
    }

    #[test]
    fn a_released_port_stays_managed_and_isnt_ready() {
        let b = Bindings::new();
        let (_tx, rx) = watch::channel(status(State::Running, 1));
        assert!(b.hold(9696, rx));
        b.mark_ready(9696, 1);
        assert!(b.ready(9696));
        b.release(9696);
        assert!(b.managed(9696) && !b.ready(9696));
        assert_eq!(b.running_run(9696), None);
        // A stopped status (the sender gone) isn't running either.
        let (tx, rx) = watch::channel(status(State::Running, 3));
        assert!(b.hold(9696, rx));
        b.mark_ready(9696, 3);
        tx.send_replace(status(State::Stopped, 3));
        assert!(!b.ready(9696));
    }

    #[test]
    fn one_companion_holds_a_port_at_a_time() {
        let b = Bindings::new();
        let (_a, ra) = watch::channel(status(State::Running, 1));
        let (_c, rc) = watch::channel(status(State::Running, 7));
        assert!(b.hold(8080, ra));
        b.mark_ready(8080, 1);
        // A second start on the port is refused and changes nothing.
        assert!(!b.claim(8080) && !b.hold(8080, rc.clone()));
        assert!(b.ready(8080) && b.running_run(8080) == Some(1));
        // Once the first is released the port can be held again.
        b.release(8080);
        assert!(b.hold(8080, rc) && !b.ready(8080));
        assert_eq!(b.running_run(8080), Some(7));
        // A claim with nothing attached yet is held and never ready.
        b.release(8080);
        assert!(b.claim(8080) && !b.claim(8080));
        assert!(b.managed(8080) && b.running_run(8080).is_none() && !b.ready(8080));
    }
}
