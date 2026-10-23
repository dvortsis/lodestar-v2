//! The guards for routes that API keys or feed tokens may reach (§12).
//! Every other route stays behind [`super::protected`], session only.
//!
//! - **API keys** come as `Authorization: Bearer <key>`, an `X-Api-Key`
//!   header, or an `apikey` query parameter (Torznab clients send that).
//! - **Feed tokens** come as a `token` query parameter, on feed routes
//!   only ([`feed_token`]).
//! - A session (cookie or trusted-proxy header) always works too, and is
//!   checked first.
//! - **One answer for every bad secret:** malformed, unknown prefix,
//!   wrong secret, revoked, expired, or the wrong kind for how it came
//!   are all `401 bad_api_key`, and each counts against a per-IP bucket
//!   of failed checks (separate from the login bucket). A key that's
//!   valid but lacks the route's scope is `403 key_scope`.
//! - Each key also has a request bucket, answered with `429` and an
//!   accurate `Retry-After`.
//! - **Key requests only read** (GET and HEAD), and never set a cookie:
//!   any `Set-Cookie` a handler adds is dropped.
//! - The handler finds an [`Access`] in the request's extensions.
//! - **Behind an SSO proxy** that forwards its own `Authorization: Bearer`
//!   token, a key route without a session answers that token as a bad
//!   key (and spends a failed check). Routes that must work through such
//!   a proxy rely on its user header for the session instead.
//!
//! Secrets, their hashes and the headers or parameters they came in are
//! never logged or echoed.

use std::net::IpAddr;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use lodestar_store::{KeyCheck, KeyKind};

use super::Auth;
use super::http::{SessionAuth, logged_in, login_required};
use super::limit::retry_after;
use crate::ApiState;
use crate::error::{ApiError, internal, wrong_method};
use crate::extract::peer_ip;
use crate::keys::{self, scope};

/// A presented value longer than this isn't a key.
const MAX_PRESENTED_BYTES: usize = 256;

/// Who a guarded request is, in its extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A signed-in browser (or the trusted proxy's user).
    Session,
    /// An API key with these scope bits ([`scope`]).
    Key { id: i64, scopes: u32 },
    /// A feed token: one saved search's feed, or (`None`) every feed.
    Feed {
        id: i64,
        saved_search_id: Option<i64>,
    },
}

impl Access {
    /// Whether this caller may read saved search `id`'s feed. A feed
    /// route answers `false` as 404, so a token for one search can't
    /// tell which others exist.
    pub fn reaches_feed(&self, id: i64) -> bool {
        match *self {
            Access::Session => true,
            Access::Key { scopes, .. } => scopes & scope::FEEDS != 0,
            Access::Feed {
                saved_search_id, ..
            } => saved_search_id.is_none_or(|s| s == id),
        }
    }
}

/// Lets API keys with `scope` (one of [`scope`]'s bits) reach `routes`,
/// besides sessions. With [`scope::FEEDS`] feed tokens may too: that's
/// [`feed_token`].
pub fn key_or_session(routes: Router<ApiState>, scope: u32) -> Router<ApiState> {
    routes
        .method_not_allowed_fallback(wrong_method)
        .route_layer(middleware::from_fn(move |req: Request, next: Next| {
            guard(scope, req, next)
        }))
}

/// Lets feed tokens (`?token=`) and API keys with the feeds scope reach
/// `routes`, besides sessions. The handler checks
/// [`Access::reaches_feed`] for the feed it serves.
pub fn feed_token(routes: Router<ApiState>) -> Router<ApiState> {
    key_or_session(routes, scope::FEEDS)
}

/// How a secret came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// `Authorization: Bearer`, `X-Api-Key` or `?apikey=`.
    Key,
    /// `?token=`.
    Token,
}

/// What the request presented: nothing, one secret (in one or more
/// forms), or something no key could be.
enum Presented {
    Nothing,
    One(String, Vec<Form>),
    Bad,
}

fn presented(req: &Request, feeds: bool) -> Presented {
    let mut found: Vec<(String, Form)> = Vec::new();
    let mut bad = false;
    let mut add = |v: Option<&str>, form: Form| match v.map(str::trim) {
        Some(v) if !v.is_empty() && v.len() <= MAX_PRESENTED_BYTES => {
            found.push((v.to_string(), form))
        }
        _ => bad = true,
    };
    for v in req.headers().get_all(header::AUTHORIZATION) {
        let Ok(text) = v.to_str() else {
            continue;
        };
        // Other schemes (Basic for a proxy, say) aren't ours.
        let Some((scheme, rest)) = text.trim().split_once(' ') else {
            continue;
        };
        if scheme.eq_ignore_ascii_case("bearer") {
            add(Some(rest), Form::Key);
        }
    }
    for v in req.headers().get_all("x-api-key") {
        add(v.to_str().ok(), Form::Key);
    }
    if let Some(q) = req.uri().query() {
        for (k, v) in form_urlencoded::parse(q.as_bytes()) {
            match &*k {
                "apikey" => add(Some(&v), Form::Key),
                "token" if feeds => add(Some(&v), Form::Token),
                _ => {}
            }
        }
    }
    if bad {
        return Presented::Bad;
    }
    let Some((first, _)) = found.first() else {
        return Presented::Nothing;
    };
    if found.iter().any(|(v, _)| v != first) {
        // Two different secrets: refuse rather than pick one.
        return Presented::Bad;
    }
    let first = first.clone();
    Presented::One(first, found.into_iter().map(|(_, f)| f).collect())
}

async fn guard(scope: u32, mut req: Request, next: Next) -> Response {
    let Some(SessionAuth { auth, user_data }) = req.extensions().get::<SessionAuth>().cloned()
    else {
        return internal("a key route was mounted without the guard's state").into_response();
    };
    let peer = peer_ip(req.extensions());
    if logged_in(&auth, req.headers(), peer) {
        req.extensions_mut().insert(Access::Session);
        return next.run(req).await;
    }
    let (secret, forms) = match presented(&req, scope == scope::FEEDS) {
        Presented::Nothing => return login_required(&auth).into_response(),
        Presented::Bad => return failed(&auth, client(&auth, &req, peer)),
        Presented::One(s, f) => (s, f),
    };
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "key_read_only",
            "API keys and feed tokens can only read; sign in to make changes.",
        )
        .into_response();
    }
    let ip = client(&auth, &req, peer);
    if let Err(wait) = auth.key_failures.check(ip) {
        return limited(wait);
    }
    let Some(p) = keys::parse(&secret) else {
        return failed(&auth, ip);
    };
    let (kind, prefix, hash) = (p.kind, p.prefix.to_string(), p.hash);
    drop(secret);
    let want = match kind {
        KeyKind::ApiKey => Form::Key,
        KeyKind::FeedToken => Form::Token,
    };
    if forms.iter().any(|f| *f != want) {
        return failed(&auth, ip);
    }
    let Some(store) = user_data else {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "keys_unavailable",
            "API keys aren't available.",
        )
        .into_response();
    };
    let checked = tokio::task::spawn_blocking(move || {
        let mut st = keys::lock(&store)?;
        let now = crate::unix_now();
        let check = st.verify_api_key(&prefix, &hash, now).map_err(internal)?;
        if let KeyCheck::Valid(k) = &check {
            if st.touch_api_key(k.id, now).is_err() {
                // Bookkeeping only: the request goes ahead.
                tracing::warn!("api keys: couldn't record a key's last use");
            }
        }
        Ok::<_, ApiError>(check)
    })
    .await;
    let key = match checked {
        Ok(Ok(KeyCheck::Valid(k))) if k.kind == kind => k,
        Ok(Ok(_)) => return failed(&auth, ip),
        Ok(Err(e)) => return e.into_response(),
        Err(e) => return internal(e).into_response(),
    };
    let access = match key.kind {
        KeyKind::ApiKey => {
            if key.scopes & scope != scope {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "key_scope",
                    "This key isn't allowed here; give it the scope in Settings → Security.",
                )
                .into_response();
            }
            Access::Key {
                id: key.id,
                scopes: key.scopes,
            }
        }
        KeyKind::FeedToken => Access::Feed {
            id: key.id,
            saved_search_id: key.saved_search_id,
        },
    };
    if let Err(wait) = auth.key_requests.take(key.id) {
        return limited(wait);
    }
    req.extensions_mut().insert(access);
    let mut resp = next.run(req).await;
    resp.headers_mut().remove(header::SET_COOKIE);
    resp
}

fn client(auth: &Auth, req: &Request, peer: IpAddr) -> IpAddr {
    auth.client_ip(peer, req.headers())
}

/// A failed check: one answer, whatever failed, and one attempt spent.
fn failed(auth: &Auth, ip: IpAddr) -> Response {
    if let Err(wait) = auth.key_failures.take(ip) {
        return limited(wait);
    }
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "bad_api_key",
        "That API key or token isn't valid.",
    )
    .into_response()
}

fn limited(wait: std::time::Duration) -> Response {
    let mut resp = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "Too many requests; wait and try again.",
    )
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&retry_after(wait)) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_feed_token_reaches_its_own_feed_only() {
        let one = Access::Feed {
            id: 1,
            saved_search_id: Some(7),
        };
        assert!(one.reaches_feed(7) && !one.reaches_feed(8));
        let all = Access::Feed {
            id: 1,
            saved_search_id: None,
        };
        assert!(all.reaches_feed(8));
        assert!(Access::Session.reaches_feed(8));
        assert!(
            Access::Key {
                id: 2,
                scopes: scope::FEEDS
            }
            .reaches_feed(8)
        );
        assert!(
            !Access::Key {
                id: 2,
                scopes: scope::TORZNAB
            }
            .reaches_feed(8)
        );
    }

    fn req(uri: &str, headers: &[(&str, &str)]) -> Request {
        let mut b = Request::get(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(axum::body::Body::empty()).unwrap()
    }

    fn one(p: Presented) -> Option<(String, Vec<Form>)> {
        match p {
            Presented::One(s, f) => Some((s, f)),
            _ => None,
        }
    }

    #[test]
    fn every_presentation_form() {
        let k = "lsk_x";
        assert_eq!(
            one(presented(
                &req("/", &[("authorization", "Bearer lsk_x")]),
                false
            )),
            Some((k.into(), vec![Form::Key]))
        );
        assert_eq!(
            one(presented(
                &req("/", &[("authorization", "bearer  lsk_x ")]),
                false
            )),
            Some((k.into(), vec![Form::Key]))
        );
        assert_eq!(
            one(presented(&req("/", &[("x-api-key", "lsk_x")]), false)),
            Some((k.into(), vec![Form::Key]))
        );
        assert_eq!(
            one(presented(&req("/?t=search&apikey=lsk_x", &[]), false)),
            Some((k.into(), vec![Form::Key]))
        );
        assert_eq!(
            one(presented(&req("/?token=lsk_x", &[]), true)),
            Some((k.into(), vec![Form::Token]))
        );
        // `token` counts on feed routes only.
        assert!(matches!(
            presented(&req("/?token=lsk_x", &[]), false),
            Presented::Nothing
        ));
        // Another scheme isn't ours.
        assert!(matches!(
            presented(&req("/", &[("authorization", "Basic YTpi")]), false),
            Presented::Nothing
        ));
        // The same secret twice is fine; two different ones aren't.
        assert!(
            one(presented(
                &req("/?apikey=lsk_x", &[("x-api-key", "lsk_x")]),
                false
            ))
            .is_some()
        );
        assert!(matches!(
            presented(&req("/?apikey=lsk_y", &[("x-api-key", "lsk_x")]), false),
            Presented::Bad
        ));
        assert!(matches!(
            presented(&req("/?apikey=", &[]), false),
            Presented::Bad
        ));
        let long = format!("/?apikey={}", "a".repeat(300));
        assert!(matches!(presented(&req(&long, &[]), false), Presented::Bad));
    }
}
