//! The last check before an enrichment request leaves (§18.7, §20.9).
//!
//! The enrichment runtimes ([`adult_enrich`](crate::adult_enrich) and
//! [`identify`](crate::identify)) judge an item with their whole gate
//! before they call the plugin, and again for every request the plugin
//! makes: the plugin is handed only a checked client ([`for_item`],
//! [`Client::checked`](lodestar_sources::Client::checked)) on the same
//! limiter, which asks the runtime's gate again, for the same item and
//! query, after the client's waits and just before each request is handed
//! to the plugin's egress. Every way a plugin sends (`send`, `poll`,
//! `poll_stream`) goes through it.
//!
//! - **Fail closed.** A gate that refuses, errs or panics, or a query that
//!   differs, sends nothing, and the refusal holds for the rest of the
//!   lookup: no later request of the same lookup goes, whatever the gate
//!   would say by then.
//! - **Not the host's fault.** A refused request fails with
//!   [`HttpError::EgressDown`](lodestar_sources::HttpError::EgressDown),
//!   which the limiter doesn't count against the host.
//!
//! Nothing here logs a name, a query or a hash.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_sources::http::Admit;
use lodestar_sources::{BoxFuture, Kit};

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

    /// Records a refusal for `why`, keeping the first reason.
    fn refuse(&self, why: W) {
        self.why
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(why);
        self.refused.store(true, Ordering::Release);
    }
}

/// The check for one lookup: `ok` decides each request, until one is
/// refused; from then on every request of the lookup is.
fn sticky<W: Copy + Send + Sync + 'static>(
    outcome: Arc<Outcome<W>>,
    ok: Arc<dyn Fn() -> BoxFuture<'static, Result<(), W>> + Send + Sync>,
) -> Admit {
    Arc::new(move || {
        let (outcome, ok) = (outcome.clone(), ok.clone());
        Box::pin(async move {
            if outcome.refused.load(Ordering::Acquire) {
                return false;
            }
            if let Err(why) = ok().await {
                outcome.refuse(why);
                return false;
            }
            // A refusal from a concurrent request of the same lookup counts.
            if outcome.refused.load(Ordering::Acquire) {
                return false;
            }
            outcome.sent.store(true, Ordering::Release);
            true
        })
    })
}

/// The kit for a lookup of item `id`, whose gate passed `expected`: `base`'s
/// state, and only a checked client on `base`'s limiter. Each request asks
/// `gate` again at `clock()` and goes only if it answers `expected` again.
/// Otherwise the reason is kept: the gate's own, `changed` for a different
/// candidate, `failed` for a store error or a panic.
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
    let ok: Arc<dyn Fn() -> BoxFuture<'static, Result<(), W>> + Send + Sync> =
        Arc::new(move || {
            let (gate, expected) = (gate.clone(), expected.clone());
            Box::pin(async move {
                match gate(id, clock()).await {
                    Ok(Ok(Ok(c))) if c == expected => Ok(()),
                    Ok(Ok(Ok(_))) => Err(changed),
                    Ok(Ok(Err(why))) => Err(why),
                    _ => Err(failed),
                }
            })
        });
    let kit = Kit {
        state: base.state.clone(),
        http: base.http.checked(sticky(outcome.clone(), ok)),
    };
    (kit, outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_sources::{
        Fetch, HostLimiter, HttpError, Policy, Request, Response, http::WITHHELD,
    };
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

    /// A gate whose answer the test sets: `Ok(1)` is the expected
    /// candidate, `Ok(2)` a different one, `Err` a reason.
    fn gate(answer: Arc<Mutex<Result<u8, char>>>, asked: Arc<AtomicUsize>) -> Gate<u8, char> {
        Arc::new(move |_, _| {
            asked.fetch_add(1, Ordering::SeqCst);
            let a = *answer.lock().unwrap();
            Box::pin(async move { Ok(Ok(a)) })
        })
    }

    fn zero() -> i64 {
        0
    }

    #[tokio::test(start_paused = true)]
    async fn each_request_asks_the_gate_and_a_refusal_holds_for_the_lookup() {
        let wire = Arc::new(Wire::default());
        let base = base(wire.clone());
        let answer = Arc::new(Mutex::new(Ok(1)));
        let asked = Arc::new(AtomicUsize::new(0));
        let (k, outcome) = for_item(
            &base,
            gate(answer.clone(), asked.clone()),
            7,
            1u8,
            zero,
            'c',
            'f',
        );
        let req = Request::get("https://a.example.test/x");
        k.http.send(&req).await.unwrap();
        k.http.poll("unit", &req).await.unwrap();
        assert_eq!(
            (asked.load(Ordering::SeqCst), wire.0.lock().unwrap().len()),
            (2, 2)
        );
        assert!(outcome.refused().is_none());
        *answer.lock().unwrap() = Err('r');
        let e = k.http.send(&req).await.unwrap_err();
        assert_eq!(e, HttpError::EgressDown(WITHHELD.into()));
        assert_eq!(outcome.refused(), Some((Some('r'), true)));
        // The gate passing again doesn't reopen this lookup, and isn't
        // asked.
        *answer.lock().unwrap() = Ok(1);
        assert!(k.http.send(&req).await.is_err());
        assert_eq!(
            (asked.load(Ordering::SeqCst), wire.0.lock().unwrap().len()),
            (3, 2)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_different_candidate_is_refused() {
        let wire = Arc::new(Wire::default());
        let base = base(wire.clone());
        let answer = Arc::new(Mutex::new(Ok(2)));
        let (k, outcome) = for_item(&base, gate(answer, Arc::default()), 7, 1u8, zero, 'c', 'f');
        assert!(
            k.http
                .send(&Request::get("https://a.example.test/x"))
                .await
                .is_err()
        );
        assert_eq!(outcome.refused(), Some((Some('c'), false)));
        assert!(wire.0.lock().unwrap().is_empty());
    }
}
