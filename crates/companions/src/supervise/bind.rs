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
//! Before a run is marked ready, the listener on its port must be that
//! run's own process ([`super::listener`], X-cmp09-fix). What can't be
//! told never counts, and the reason is kept ([`Bindings::refusal`]).
//!
//! A port stays managed for the life of the [`Bindings`] once a companion
//! was started on it, running or not, so a stopped companion's port never
//! falls back to looking on `::1`. One companion holds a port at a time:
//! a second start on a port another holds is refused ([`Bindings::claim`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::watch;

use super::listener::{self, Owner};
use super::{State, Status};

/// Who listens on a port, against a process id ([`listener::owner`]).
type Verify = Arc<dyn Fn(u16, u32) -> Owner + Send + Sync>;

/// The managed ports, each with its child's status and the run it was
/// last found ready in. Shared by the supervisor's side and the client's.
pub struct Bindings {
    ports: Mutex<HashMap<u16, Port>>,
    /// The last claim's number: each claim takes the next, so nothing
    /// from an earlier claim of a port counts for a later one.
    claims: AtomicU64,
    verify: Verify,
}

impl std::fmt::Debug for Bindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bindings")
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

impl Default for Bindings {
    fn default() -> Bindings {
        Bindings::with_verify(Arc::new(listener::owner))
    }
}

#[derive(Debug, Default)]
struct Port {
    /// A companion holds it: claimed at its start, until its stop.
    held: bool,
    /// Which claim this is ([`Bindings::claims`]); 0 when unclaimed.
    claim: u64,
    /// Why the last readiness attempt that passed its health check
    /// didn't count (a [`listener::Owner`] code).
    refusal: Option<&'static str>,
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

    fn with_verify(verify: Verify) -> Bindings {
        Bindings {
            ports: Mutex::new(HashMap::new()),
            claims: AtomicU64::new(0),
            verify,
        }
    }

    /// One that takes every listener for the run's own, for tests whose
    /// statuses name no real process.
    #[cfg(any(test, feature = "testing"))]
    pub fn trusting() -> Bindings {
        Bindings::with_verify(Arc::new(|_, _| Owner::Ours))
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
            claim: self.claims.fetch_add(1, Ordering::Relaxed) + 1,
            refusal: None,
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
        let s = up(ports.get(&port)?.status.as_ref()?)?;
        Some(s.run)
    }

    /// Marks run `run` ready under the current claim, with no listener
    /// check: for tests of what readiness lets through.
    #[cfg(any(test, feature = "testing"))]
    pub fn mark_ready(&self, port: u16, run: u64) {
        let claim = self.ports().get(&port).map_or(0, |p| p.claim);
        self.mark(port, claim, run);
    }

    /// Ready, if `port` is still under claim `claim` and run `run` is
    /// still the one up.
    fn mark(&self, port: u16, claim: u64, run: u64) {
        let mut ports = self.ports();
        if let Some(p) = ports.get_mut(&port)
            && p.claim == claim
            && p.status.as_ref().and_then(up).is_some_and(|s| s.run == run)
        {
            p.ready = Some(run);
            p.refusal = None;
        }
    }

    /// The claim, run and process id of the run up on `port` now.
    fn attempt(&self, port: u16) -> Option<(u64, u64, Option<u32>)> {
        let ports = self.ports();
        let p = ports.get(&port)?;
        let s = up(p.status.as_ref()?)?;
        Some((p.claim, s.run, s.pid))
    }

    /// One readiness attempt on `port`: `check` (a health check on
    /// `127.0.0.1`) runs only while a run is up. A pass counts only if the
    /// listener on the port is that run's process ([`listener::owner`]),
    /// and marks the run ready if the claim and the run are still the
    /// ones checked. True once the port is ready. What
    /// [`Supervised`](crate::manager::Supervised)'s health check does.
    pub async fn try_ready<F, Fut>(&self, port: u16, check: F) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let Some((claim, run, pid)) = self.attempt(port) else {
            return false;
        };
        if !check().await {
            return false;
        }
        let owner = match pid {
            Some(pid) => {
                let verify = Arc::clone(&self.verify);
                tokio::task::spawn_blocking(move || verify(port, pid))
                    .await
                    .unwrap_or(Owner::Unknown)
            }
            None => Owner::Unknown,
        };
        if owner != Owner::Ours {
            if let Some(p) = self.ports().get_mut(&port).filter(|p| p.claim == claim) {
                p.refusal = Some(owner.code());
            }
            return false;
        }
        // A check that spanned a restart, or a new claim, readies nothing.
        self.mark(port, claim, run);
        self.ready(port)
    }

    /// Why the last readiness attempt on `port` that passed its health
    /// check didn't count, if it didn't: a [`listener::Owner`] code.
    pub fn refusal(&self, port: u16) -> Option<&'static str> {
        self.ports().get(&port).and_then(|p| p.refusal)
    }

    /// The run last found ready on `port`, if any.
    pub fn ready_run(&self, port: u16) -> Option<u64> {
        self.ports().get(&port).and_then(|p| p.ready)
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
        up(status).is_some_and(|s| s.run == ready)
    }
}

/// The child's status, if its process is up: running, or running with
/// failed health checks, not yet found gone or told to stop, and still
/// supervised. A status whose sender has gone (the supervisor's task ended
/// without a stop: a panic or an abort) never counts.
fn up(rx: &watch::Receiver<Status>) -> Option<Status> {
    if rx.has_changed().is_err() {
        return None;
    }
    let s = *rx.borrow();
    (s.up && matches!(s.state, State::Running | State::Unhealthy { .. })).then_some(s)
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
            pid: None,
        }
    }

    #[test]
    fn a_port_is_ready_only_in_the_run_found_ready() {
        let b = Bindings::trusting();
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
        let b = Bindings::trusting();
        let (_tx, rx) = watch::channel(status(State::Running, 1));
        assert!(b.hold(9696, rx));
        b.mark_ready(9696, 1);
        assert!(b.ready(9696));
        b.release(9696);
        assert!(b.managed(9696) && !b.ready(9696));
        assert_eq!(b.running_run(9696), None);
        // A stopped status isn't running either.
        let (tx, rx) = watch::channel(status(State::Running, 3));
        b.release(9696);
        assert!(b.hold(9696, rx));
        b.mark_ready(9696, 3);
        assert!(b.ready(9696));
        tx.send_replace(status(State::Stopped, 3));
        assert!(!b.ready(9696));
    }

    /// The supervisor's task ended without a stop (a panic or an abort):
    /// its last status still says running, but nothing counts.
    #[test]
    fn a_status_whose_sender_has_gone_isnt_running() {
        let b = Bindings::trusting();
        let (tx, rx) = watch::channel(status(State::Running, 1));
        assert!(b.hold(8080, rx));
        b.mark_ready(8080, 1);
        assert!(b.ready(8080));
        drop(tx);
        assert!(!b.ready(8080));
        assert_eq!(b.running_run(8080), None);
        b.mark_ready(8080, 1);
        assert!(!b.ready(8080));
    }

    #[test]
    fn one_companion_holds_a_port_at_a_time() {
        let b = Bindings::trusting();
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

    fn with_owner(owner: Owner) -> Bindings {
        Bindings::with_verify(Arc::new(move |_, _| owner))
    }

    fn running(run: u64, pid: Option<u32>) -> Status {
        Status {
            pid,
            ..status(State::Running, run)
        }
    }

    /// A passing health check counts only when the listener is the run's
    /// own process; otherwise the port stays not ready and says why.
    #[tokio::test]
    async fn only_the_runs_own_listener_makes_it_ready() {
        for (owner, code) in [
            (Owner::Other, "listener_not_ours"),
            (Owner::Unknown, "listener_unverified"),
            (Owner::Nobody, "listener_missing"),
        ] {
            let b = with_owner(owner);
            let (_tx, rx) = watch::channel(running(1, Some(40)));
            assert!(b.hold(8080, rx));
            assert!(!b.try_ready(8080, || async { true }).await);
            assert!(!b.ready(8080) && b.ready_run(8080).is_none());
            assert_eq!(b.refusal(8080), Some(code));
        }
        // A run that names no process can't be told: not ready.
        let b = with_owner(Owner::Ours);
        let (_tx, rx) = watch::channel(running(1, None));
        assert!(b.hold(8080, rx));
        assert!(!b.try_ready(8080, || async { true }).await);
        assert_eq!(b.refusal(8080), Some("listener_unverified"));
        // Its own listener: ready, and the refusal is gone.
        let (_tx, rx) = watch::channel(running(1, Some(40)));
        b.release(8080);
        assert!(b.hold(8080, rx));
        assert!(b.try_ready(8080, || async { true }).await);
        assert_eq!((b.ready_run(8080), b.refusal(8080)), (Some(1), None));
    }

    /// A check begun under one claim of a port readies nothing once the
    /// port was released and claimed again, even at the same run number.
    #[tokio::test]
    async fn a_check_from_an_earlier_claim_counts_for_nothing() {
        let b = Arc::new(with_owner(Owner::Ours));
        let (_old, rx) = watch::channel(running(1, Some(40)));
        assert!(b.hold(8080, rx));
        let (_new, rx_new) = watch::channel(running(1, Some(41)));
        let again = Arc::clone(&b);
        let got = b
            .try_ready(8080, || async move {
                // Stopped and started again while the check ran: the new
                // claim's first run is run 1 too.
                again.release(8080);
                assert!(again.hold(8080, rx_new));
                true
            })
            .await;
        assert!(!got && !b.ready(8080) && b.ready_run(8080).is_none());
    }
}
