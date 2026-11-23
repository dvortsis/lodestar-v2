//! Cancelling a search (architecture §8, "Cancellation"): the server
//! checks a token between segments and every few thousand documents
//! inside the scans, so a request the client gave up on stops early.
//!
//! A token is cancelled explicitly (the API's drop guard does it when
//! the client disconnects) or runs out of time (a budget, e.g. for an
//! exact count on demand). A token made with [`CancelToken::with_budget`]
//! also stops when its parent does.
//!
//! **The regex budget** (RX-1, S8-11): every `/…/` clause compiled under
//! one search verifies against one deadline, [`CancelToken::regex_deadline`],
//! started by the search's first regex compile. It lives on the token's
//! scope: the root token, or the nearest [`CancelToken::regex_scope`]. A
//! [`CancelToken::with_budget`] child shares its parent's, so facet
//! variants, the adult-facet pass and recovery under one request all
//! spend the same 2 s.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::index::{Error, Result};

/// Documents a scan visits between checks of its token. At the scans'
/// few nanoseconds per document this is well under a millisecond.
pub(crate) const CHECK_EVERY: u32 = 4096;

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    deadline: Option<Instant>,
    parent: Option<CancelToken>,
    /// A regex-budget scope ([`CancelToken::regex_deadline`]): the root
    /// and [`CancelToken::regex_scope`] tokens. Its deadline, once a
    /// regex compile has started it.
    regex: Option<OnceLock<Instant>>,
}

/// Shared by every clone: cancelling one cancels them all.
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

impl Default for CancelToken {
    fn default() -> Self {
        CancelToken::new()
    }
}

impl CancelToken {
    /// A token that stops only when cancelled.
    pub fn new() -> CancelToken {
        CancelToken {
            inner: Arc::new(Inner {
                regex: Some(OnceLock::new()),
                ..Inner::default()
            }),
        }
    }

    /// A child with a regex budget of its own, that stops when this token
    /// does: one per saved search in a percolation run, which shares one
    /// token but must not share one regex deadline.
    pub fn regex_scope(&self) -> CancelToken {
        CancelToken {
            inner: Arc::new(Inner {
                regex: Some(OnceLock::new()),
                parent: Some(self.clone()),
                ..Inner::default()
            }),
        }
    }

    /// A child whose regex budget ends at `deadline`, already started:
    /// for passes over a lease taken by an earlier request (its facets,
    /// its recovery counts), which spend what's left of the search's
    /// budget, never a fresh one (S8-11).
    pub fn regex_scope_until(&self, deadline: Instant) -> CancelToken {
        CancelToken {
            inner: Arc::new(Inner {
                regex: Some(OnceLock::from(deadline)),
                parent: Some(self.clone()),
                ..Inner::default()
            }),
        }
    }

    /// The deadline every `/…/` clause compiled under this token's scope
    /// verifies against: `budget` from the first call in the scope (see
    /// the module docs). Later calls get the same instant, whatever
    /// `budget` they pass.
    pub fn regex_deadline(&self, budget: Duration) -> Instant {
        let mut chain = std::iter::successors(Some(self), |t| t.inner.parent.as_ref());
        let start = || {
            let now = Instant::now();
            now.checked_add(budget).unwrap_or(now)
        };
        // Every chain ends at a root, which is a scope; without one the
        // budget would just start now.
        match chain.find_map(|t| t.inner.regex.as_ref()) {
            Some(slot) => *slot.get_or_init(start),
            None => start(),
        }
    }

    /// A child that also stops after `budget`, or when this token does.
    pub fn with_budget(&self, budget: Duration) -> CancelToken {
        CancelToken {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                deadline: Instant::now().checked_add(budget),
                parent: Some(self.clone()),
                regex: None,
            }),
        }
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Relaxed);
    }

    /// `Err(Cancelled)` once cancelled (here or in a parent), or
    /// `Err(OutOfTime)` once a budget has run out.
    pub fn check(&self) -> Result<()> {
        let chain = || std::iter::successors(Some(self), |t| t.inner.parent.as_ref());
        // A cancel wins over a budget: the client is gone either way.
        if chain().any(|t| t.inner.cancelled.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let deadlines = chain().filter_map(|t| t.inner.deadline);
        if deadlines.min().is_some_and(|d| Instant::now() >= d) {
            return Err(Error::OutOfTime);
        }
        Ok(())
    }

    pub fn is_stopped(&self) -> bool {
        self.check().is_err()
    }

    /// Cancels this token when dropped, unless [`CancelOnDrop::disarm`]ed
    /// first. Hold it in a future: if the future is dropped (axum drops a
    /// handler's when the client disconnects), the work it started stops.
    pub fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(Some(self.clone()))
    }
}

/// See [`CancelToken::cancel_on_drop`].
#[derive(Debug)]
pub struct CancelOnDrop(Option<CancelToken>);

impl CancelOnDrop {
    pub fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            t.cancel();
        }
    }
}

/// What a scan inside a Tantivy collector returns when its token stops
/// it. Tantivy only carries its own error type through a search, so the
/// caller asks the token why ([`stopped_or`]).
pub(crate) fn interrupted() -> tantivy::TantivyError {
    tantivy::TantivyError::InternalError("search stopped".into())
}

/// The token's reason when it has stopped, else the error itself.
pub(crate) fn stopped_or(token: &CancelToken, e: impl Into<Error>) -> Error {
    match token.check() {
        Err(stop) => stop,
        Ok(()) => e.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_any_clone_stops_every_clone_and_child() {
        let t = CancelToken::new();
        let child = t.with_budget(Duration::from_secs(3600));
        assert!(t.check().is_ok() && child.check().is_ok());
        t.clone().cancel();
        assert!(matches!(t.check(), Err(Error::Cancelled)));
        assert!(matches!(child.check(), Err(Error::Cancelled)));
    }

    #[test]
    fn a_budget_runs_out_without_touching_the_parent() {
        let t = CancelToken::new();
        let child = t.with_budget(Duration::ZERO);
        assert!(matches!(child.check(), Err(Error::OutOfTime)));
        assert!(t.check().is_ok());
        child.cancel();
        assert!(t.check().is_ok(), "a child never cancels its parent");
    }

    #[test]
    fn the_drop_guard_cancels_unless_disarmed() {
        let t = CancelToken::new();
        t.cancel_on_drop().disarm();
        assert!(!t.is_stopped());
        drop(t.cancel_on_drop());
        assert!(t.is_stopped());
    }

    #[test]
    fn a_dropped_future_cancels_its_work() {
        // What the API relies on: a handler future dropped mid-await
        // (the client went away) cancels the blocking search it awaits.
        let t = CancelToken::new();
        let fut = {
            let guard = t.cancel_on_drop();
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            }
        };
        drop(fut);
        assert!(t.is_stopped());
    }
}
