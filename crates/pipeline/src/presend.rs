//! The last check before an enrichment request leaves (§18.7, §20.9).
//!
//! The enrichment runtimes ([`adult_enrich`](crate::adult_enrich) and
//! [`identify`](crate::identify)) judge an item with their whole gate
//! before they call the plugin. A plugin's request can then wait in its
//! client for the host's pacing or back-off, for as long as the host asks.
//! So the plugin is handed a client of its own ([`kit`]): the same limiter
//! (the pacing is shared), and a [`Fetch`] that asks the runtime's gate
//! again, for the same item, after every wait and just before the request
//! is handed to the plugin's egress. Every way a plugin sends (`send`,
//! `poll`, `poll_stream`) goes through it.
//!
//! - **Fail closed.** A gate that refuses, errs or panics sends nothing,
//!   and the refusal holds for the rest of the lookup: no later request
//!   of the same lookup goes, whatever the gate would say by then.
//! - **Not the host's fault.** A refused request fails with
//!   [`HttpError::EgressDown`], which the limiter doesn't count against
//!   the host.
//!
//! Nothing here logs a name, a query or a hash.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lodestar_sources::http::{StreamLimits, StreamResponse};
use lodestar_sources::{BoxFuture, Client, Fetch, HttpError, Kit, Request, Response};

/// What a refused request fails with.
pub(crate) const WITHHELD: &str = "withheld by the enrichment gate";

/// The runtime's gate for one lookup's item: true when the request may go
/// now. Called once per request, after the client's waits.
pub(crate) type Admit = Arc<dyn Fn() -> BoxFuture<'static, bool> + Send + Sync>;

/// A [`Fetch`] that asks [`Admit`] before each request.
struct Gated {
    inner: Arc<dyn Fetch>,
    admit: Admit,
    refused: Arc<AtomicBool>,
}

impl Gated {
    async fn admitted(&self) -> bool {
        if self.refused.load(Ordering::Acquire) {
            return false;
        }
        let ok = (self.admit)().await;
        if !ok {
            self.refused.store(true, Ordering::Release);
        }
        // A refusal from a concurrent request of the same lookup counts.
        ok && !self.refused.load(Ordering::Acquire)
    }
}

impl Fetch for Gated {
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
/// `kit`'s limiter whose every request asks `admit` first. The flag is
/// set once a request was refused.
pub(crate) fn kit(kit: &Kit, admit: Admit) -> (Kit, Arc<AtomicBool>) {
    let refused = Arc::new(AtomicBool::new(false));
    let fetch: Arc<dyn Fetch> = Arc::new(Gated {
        inner: kit.http.fetcher().clone(),
        admit,
        refused: refused.clone(),
    });
    let gated = Kit {
        state: kit.state.clone(),
        http: Client::new(fetch, kit.http.limiter().clone()),
    };
    (gated, refused)
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
        let (k, refused) = kit(&base(wire.clone()), admit(open, Arc::clone(&asked)));
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
        assert!(!refused.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_refusal_sends_nothing_and_holds_for_the_lookup() {
        let wire = Arc::new(Wire::default());
        let (open, asked) = (Arc::new(AtomicBool::new(false)), Arc::default());
        let base = base(wire.clone());
        let (k, refused) = kit(&base, admit(open.clone(), Arc::clone(&asked)));
        let req = Request::get("https://a.example.test/x");
        let e = k.http.send(&req).await.unwrap_err();
        assert_eq!(e, HttpError::EgressDown(WITHHELD.into()));
        assert!(refused.load(Ordering::SeqCst));
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
