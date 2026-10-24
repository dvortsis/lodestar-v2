//! `api.request` (architecture §12.1): one line per API request, at
//! Debug and off by default, with what can't identify anyone but the
//! client.
//!
//! - **`route` is the matched route template** (`/api/v1/torrents/{hash}`),
//!   never the raw path or its query, so no hash, search query, `apikey`
//!   or `token` reaches it. A request no route matched logs `unmatched`.
//!   Templates are interned once each into a set no bigger than the
//!   router's routes (capped at [`MAX_ROUTES`]; past that, `other`), so a
//!   label stays `&'static str` without an unbounded leak.
//! - **`client` is the TCP peer**, never a forwarded-for header.
//! - **No header is ever logged,** and the URI isn't either.
//! - Logged when the response starts: a stream (`/logs/stream`,
//!   `/events`) or a download logs once, at open, never per frame.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use lodestar_log::{Client, Count, log_event};

use crate::extract::peer_ip;
use crate::log_events::API_REQUEST;

/// The most route templates interned; the router has about a hundred.
pub const MAX_ROUTES: usize = 512;

/// `api.request`'s middleware. Layered on the API router after its
/// routes and fallbacks, so it sees each request's matched route and the
/// final status, the guards' refusals included.
pub async fn log(req: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = method(req.method());
    // An `Arc` clone: nothing is copied while the event is off.
    let route = req.extensions().get::<MatchedPath>().cloned();
    let peer = peer_ip(req.extensions());
    let resp = next.run(req).await;
    let took = started.elapsed().as_millis() as u64;
    log_event!(
        API_REQUEST,
        method = method,
        route = route.as_ref().map_or("unmatched", |r| intern(r.as_str())),
        status = Count(u64::from(resp.status().as_u16())),
        took_ms = took,
        client = Client(peer)
    );
    resp
}

/// The standard methods, else `other`.
fn method(m: &Method) -> &'static str {
    match *m {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "other",
    }
}

/// A route template as a `&'static str`: leaked once per distinct
/// template, at most [`MAX_ROUTES`] of them.
fn intern(route: &str) -> &'static str {
    static ROUTES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = ROUTES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    intern_in(&mut set, route)
}

fn intern_in(set: &mut HashSet<&'static str>, route: &str) -> &'static str {
    if let Some(r) = set.get(route) {
        return r;
    }
    if set.len() >= MAX_ROUTES {
        return "other";
    }
    let leaked: &'static str = Box::leak(route.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_interned_once_and_bounded() {
        let mut set = HashSet::new();
        let a = intern_in(&mut set, "/api/v1/torrents/{hash}");
        let b = intern_in(&mut set, "/api/v1/torrents/{hash}");
        assert!(std::ptr::eq(a, b));
        assert_eq!(method(&Method::GET), "GET");
        assert_eq!(method(&Method::from_bytes(b"BREW").unwrap()), "other");
        for i in 0..MAX_ROUTES + 10 {
            let _ = intern_in(&mut set, &format!("/t/{i}"));
        }
        assert_eq!(set.len(), MAX_ROUTES);
        assert_eq!(intern_in(&mut set, "/never/seen/before"), "other");
        // Templates seen before still come back as themselves.
        assert_eq!(intern_in(&mut set, "/api/v1/torrents/{hash}"), a);
    }

    /// Placed as `router()` places it (after the routes and fallbacks), a
    /// layer sees the matched template, nested prefix included, and
    /// nothing for a request no route matched.
    #[tokio::test]
    async fn the_layer_sees_the_template_not_the_path() {
        use axum::Router;
        use axum::body::Body;
        use axum::routing::get;
        use tower::ServiceExt;

        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let inner = Router::new()
            .route("/torrents/{hash}", get(|| async { "ok" }))
            .fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "no") })
            .layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let record = record.clone();
                    async move {
                        let route = req
                            .extensions()
                            .get::<MatchedPath>()
                            .map(|m| m.as_str().to_owned());
                        let resp = next.run(req).await;
                        record.lock().unwrap().push((route, resp.status().as_u16()));
                        resp
                    }
                },
            ));
        let app = Router::new().nest("/api/v1", inner);
        for uri in [
            "/api/v1/torrents/c9e15763f722f23e98a29decdfae341b98d53056?apikey=k",
            "/api/v1/nothing/here?q=secret",
        ] {
            let req = axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let _ = app.clone().oneshot(req).await.unwrap();
        }
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            [
                (Some("/api/v1/torrents/{hash}".to_string()), 200),
                (None, 404),
            ]
        );
    }
}
