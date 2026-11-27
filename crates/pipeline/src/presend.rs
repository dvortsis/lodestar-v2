//! The last check before an enrichment request leaves (§18.7, §20.9).
//!
//! The enrichment runtimes ([`adult_enrich`](crate::adult_enrich) and
//! [`identify`](crate::identify)) judge an item with their whole gate
//! before they call the plugin, and again for every request the plugin
//! makes: the plugin is handed a client of its own ([`for_item`]) on the
//! same limiter (the pacing is shared), whose [`Fetch`] asks the runtime's
//! gate again, for the same item and query, after the client's waits and
//! just before each request is handed to the plugin's egress. Every way a
//! plugin sends (`send`, `poll`, `poll_stream`) goes through it.
//!
//! - **Fail closed.** A gate that refuses, errs or panics, or a query that
//!   differs, sends nothing, and the refusal holds for the rest of the
//!   lookup: no later request of the same lookup goes, whatever the gate
//!   would say by then.
//! - **Not the host's fault.** A refused request fails with
//!   [`HttpError::EgressDown`], which the limiter doesn't count against
//!   the host.
//!
//! Nothing here logs a name, a query or a hash.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_sources::http::{StreamLimits, StreamResponse};
use lodestar_sources::{BoxFuture, Client, Fetch, HttpError, Kit, Request, Response};

/// What a refused request fails with.
pub(crate) const WITHHELD: &str = "withheld by the enrichment gate";

/// One request's check: true when it may go now. Called once per request,
/// after the client's waits.
pub(crate) type Admit = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

/// A runtime's gate (its `check_with`, run on the blocking pool) for item
/// `id` at time `now`.
pub(crate) type Gate<C, W> =
    Arc<dyn Fn(i64, i64) -> BoxFuture<'static, Answer<C, W>> + Send + Sync>;

/// The gate's answer: a candidate, or why not; a store error; a panic.
pub(crate) type Answer<C, W> =
    Result<Result<Result<C, W>, lodestar_store::Error>, tokio::task::JoinError>;

/// What one lookup's requests met.
#[derive(Debug)]
pub(crate) struct Outcome<W> {
    refused: AtomicBool,
    sent: AtomicBool,
    why: Mutex<Option<W>>,
}

impl<W> Default for Outcome<W> {
    fn default() -> Outcome<W> {
        Outcome {
            refused: AtomicBool::new(false),
            sent: AtomicBool::new(false),
            why: Mutex::new(None),
        }
    }
}

impl<W: Copy> Outcome<W> {
    /// `Some` once a request was refused: the first reason, and whether an
    /// earlier request of the lookup had gone.
    pub(crate) fn refused(&self) -> Option<(Option<W>, bool)> {
        self.refused.load(Ordering::Acquire).then(|| {
            let why = *self.why.lock().unwrap_or_else(|e| e.into_inner());
            (why, self.sent.load(Ordering::Acquire))
        })
    }
}

/// A [`Fetch`] that asks [`Admit`] before each request.
struct Gated<W> {
    inner: Arc<dyn Fetch>,
    admit: Admit,
    outcome: Arc<Outcome<W>>,
}

impl<W> Gated<W> {
    async fn admitted(&self) -> bool {
        if self.outcome.refused.load(Ordering::Acquire) {
            return false;
        }
        let ok = (self.admit)().await;
        if !ok {
            self.outcome.refused.store(true, Ordering::Release);
        }
        // A refusal from a concurrent request of the same lookup counts.
        let ok = ok && !self.outcome.refused.load(Ordering::Acquire);
        if ok {
            self.outcome.sent.store(true, Ordering::Release);
        }
        ok
    }
}

impl<W: Send + Sync> Fetch for Gated<W> {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(async move {
            if !self.admitted().await {
                return Err(HttpError::EgressDown(WITHHELD.into()));
            }
            self.inner.fetch(req).await
        })
    }

    fn fetch_stream<'a>(
        &'a self,
        req: &'a Request,
        limits: StreamLimits,
    ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
        Box::pin(async move {
            if !self.admitted().await {
                return Err(HttpError::EgressDown(WITHHELD.into()));
            }
            self.inner.fetch_stream(req, limits).await
        })
    }
}

/// The kit one lookup hands its plugin: `kit`'s state, and a client on
/// `kit`'s limiter whose every request asks `admit` first.
fn kit<W: Send + Sync + 'static>(kit: &Kit, admit: Admit, outcome: Arc<Outcome<W>>) -> Kit {
    let fetch: Arc<dyn Fetch> = Arc::new(Gated {
        inner: kit.http.fetcher().clone(),
        admit,
        outcome,
    });
    Kit {
        state: kit.state.clone(),
        http: Client::new(fetch, kit.http.limiter().clone()),
    }
}

/// The kit for a lookup of item `id`, whose gate passed `expected`: each
/// request asks `gate` again at `clock()` and goes only if it answers
/// `expected` again. Otherwise the reason is kept: the gate's own,
/// `changed` for a different candidate, `failed` for a store error or a
/// panic.
pub(crate) fn for_item<C, W>(
    base: &Kit,
    gate: Gate<C, W>,
    id: i64,
    expected: C,
    clock: fn() -> i64,
    changed: W,
    failed: W,
) -> (Kit, Arc<Outcome<W>>)
where
    C: PartialEq + Clone + Send + Sync + 'static,
    W: Copy + Send + Sync + 'static,
{
    let outcome: Arc<Outcome<W>> = Arc::default();
    let admit: Admit = {
        let outcome = outcome.clone();
        Arc::new(move || {
            let (gate, outcome, expected) = (gate.clone(), outcome.clone(), expected.clone());
            Box::pin(async move {
                let why = match gate(id, clock()).await {
                    Ok(Ok(Ok(c))) if c == expected => return true,
                    Ok(Ok(Ok(_))) => changed,
                    Ok(Ok(Err(why))) => why,
                    _ => failed,
                };
                outcome
                    .why
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_or_insert(why);
                false
            })
        })
    };
    (kit(base, admit, outcome.clone()), outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_sources::{HostLimiter, Policy};
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct Wire(Mutex<Vec<String>>);

    impl Fetch for Wire {
        fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
            self.0.lock().unwrap().push(req.url.clone());
            Box::pin(async {
                Ok(Response {
                    status: 200,
                    headers: Vec::new(),
                    body: Vec::new(),
                })
            })
        }
    }

    fn base(wire: Arc<Wire>) -> Kit {
        Kit::with_limiter(
            Arc::new(lodestar_sources::MemoryState::new()),
            wire,
            HostLimiter::new(Policy::STRICT.limits(&Default::default())),
        )
    }

    fn admit(open: Arc<AtomicBool>, asked: Arc<AtomicUsize>) -> Admit {
        Arc::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
            let ok = open.load(Ordering::SeqCst);
            Box::pin(async move { ok })
        })
    }

    #[tokio::test]
    async fn every_path_asks_the_gate_before_it_sends() {
        let wire = Arc::new(Wire::default());
        let (open, asked) = (Arc::new(AtomicBool::new(true)), Arc::default());
        let outcome: Arc<Outcome<()>> = Arc::default();
        let k = kit(
            &base(wire.clone()),
            admit(open, Arc::clone(&asked)),
            outcome.clone(),
        );
        let req = Request::get("https://a.example.test/x");
        k.http.send(&req).await.unwrap();
        k.http.poll("unit", &req).await.unwrap();
        let limits = StreamLimits {
            max_bytes: 1024,
            timeout: std::time::Duration::from_secs(10),
            idle: std::time::Duration::from_secs(10),
        };
        k.http.poll_stream("unit2", &req, limits).await.unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        assert_eq!(wire.0.lock().unwrap().len(), 3);
        assert!(outcome.refused().is_none());
    }

    #[tokio::test]
    async fn a_refusal_sends_nothing_and_holds_for_the_lookup() {
        let wire = Arc::new(Wire::default());
        let (open, asked) = (Arc::new(AtomicBool::new(false)), Arc::default());
        let base = base(wire.clone());
        let outcome: Arc<Outcome<()>> = Arc::default();
        let k = kit(
            &base,
            admit(open.clone(), Arc::clone(&asked)),
            outcome.clone(),
        );
        let req = Request::get("https://a.example.test/x");
        let e = k.http.send(&req).await.unwrap_err();
        assert_eq!(e, HttpError::EgressDown(WITHHELD.into()));
        assert_eq!(outcome.refused(), Some((None, false)));
        // The gate opening again doesn't reopen this lookup.
        open.store(true, Ordering::SeqCst);
        assert!(k.http.send(&req).await.is_err());
        assert!(wire.0.lock().unwrap().is_empty());
        // Not the host's fault: it isn't backed off.
        let host = req.host().unwrap();
        assert_eq!(
            base.limiter().state(&host, tokio::time::Instant::now()),
            lodestar_sources::HostState::Ready
        );
    }
}
