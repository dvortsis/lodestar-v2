//! API keys, feed tokens, the fresh-password check and signing out other
//! sessions (§12, §20.11), end to end through the router, plus a test
//! router whose routes opt in to keys the way Torznab and RSS will.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Extension, Router};
use lodestar_api::auth::{self, Access};
use lodestar_api::keys::{self, scope};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{KeyKind, NewApiKey, SavedSearchInput, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// A fresh address per call, so tests don't share login buckets unless
/// they mean to.
static NEXT_IP: AtomicU32 = AtomicU32::new(1);

fn fresh_ip() -> SocketAddr {
    let n = NEXT_IP.fetch_add(1, Ordering::Relaxed);
    SocketAddr::from(([10, 50, (n >> 8) as u8, n as u8], 40000))
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    store: Arc<Mutex<Store>>,
    dir: tempfile::TempDir,
}

/// What a guarded test route answers: who the guard said the caller is.
async fn who(Extension(a): Extension<Access>) -> axum::Json<Value> {
    axum::Json(json!({ "access": format!("{a:?}") }))
}

async fn feed(
    Extension(a): Extension<Access>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<&'static str, StatusCode> {
    if a.reaches_feed(id) {
        Ok("<rss/>")
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn sets_cookie() -> ([(header::HeaderName, &'static str); 1], &'static str) {
    ([(header::SET_COOKIE, "sneaky=1")], "ok")
}

fn fixture() -> Fixture {
    fixture_with(false)
}

/// `frozen`: no limiter refills, so emptying a bucket doesn't depend on
/// how fast the machine runs.
fn fixture_with(frozen: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let mut auth = Auth::load(&dir.path().join("auth.json"), false).unwrap();
    if frozen {
        auth = auth.without_refill();
    }
    let auth = Arc::new(auth);
    let store = Arc::new(Mutex::new(store));
    let state = ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: None,
        user_data: Some(store.clone()),
        adult: Default::default(),
        insights: None,
        hot: None,
        events: None,
    };
    let guarded = Router::new()
        .merge(auth::key_or_session(
            Router::new()
                .route("/torznab", get(who))
                .route("/torznab/write", post(who))
                .route("/torznab/cookie", get(sets_cookie)),
            scope::TORZNAB,
        ))
        .merge(auth::key_or_session(
            Router::new().route("/search", get(who)),
            scope::SEARCH,
        ))
        .merge(auth::feed_token(
            Router::new().route("/feeds/{id}", get(feed)),
        ));
    let app = Router::new()
        .nest("/api/v1", router(state.clone()))
        .nest("/test", auth::mount(guarded, state));
    Fixture {
        app,
        auth,
        store,
        dir,
    }
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    retry_after: Option<String>,
    cache_control: Option<String>,
    set_cookie: bool,
    body: Value,
    text: String,
}

async fn send(app: &Router, mut req: Request<Body>, from: SocketAddr) -> Reply {
    req.extensions_mut().insert(ConnectInfo(from));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let set_cookie = resp.headers().contains_key(header::SET_COOKIE);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let retry_after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let cache_control = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Reply {
        status,
        cookie,
        retry_after,
        cache_control,
        set_cookie,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    }
}

fn req(method: Method, uri: &str, cookie: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn get_with(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut b = Request::get(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(Body::empty()).unwrap()
}

/// Sets up the instance and returns a session cookie.
async fn signed_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/setup",
            None,
            Some(json!({ "token": token, "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.cookie.unwrap()
}

async fn create(f: &Fixture, cookie: &str, body: Value) -> Reply {
    send(
        &f.app,
        req(Method::POST, "/api/v1/keys", Some(cookie), Some(body)),
        fresh_ip(),
    )
    .await
}

/// Makes a key through the API and returns `(id, secret)`.
async fn make_key(f: &Fixture, cookie: &str, kind: &str, scopes: Value) -> (i64, String) {
    let r = create(
        f,
        cookie,
        json!({ "name": "Prowlarr", "kind": kind, "scopes": scopes, "password": PASSWORD }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    (
        r.body["key"]["id"].as_i64().unwrap(),
        r.body["secret"].as_str().unwrap().to_string(),
    )
}

fn saved_search(f: &Fixture, name: &str) -> i64 {
    f.store
        .lock()
        .unwrap()
        .create_saved_search(
            &SavedSearchInput {
                name,
                query: "ubuntu",
                ast: "{}",
                sort: None,
                alert: false,
                rss: true,
                channels: 0,
                paused: false,
            },
            0,
            0,
        )
        .unwrap()
}

#[tokio::test]
async fn creating_a_key_needs_the_password_and_shows_the_secret_once() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let body = json!({ "name": " Prowlarr ", "kind": "api_key", "scopes": ["torznab"] });

    // Without the password: 428, field `password`, nothing made.
    let r = create(&f, &cookie, body.clone()).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text);
    assert_eq!(
        (&r.body["error"]["code"], &r.body["error"]["field"]),
        (&json!("confirm_required"), &json!("password"))
    );
    let mut with = body.clone();
    with["password"] = json!("");
    assert_eq!(
        create(&f, &cookie, with).await.status,
        StatusCode::PRECONDITION_REQUIRED
    );
    // A wrong one: 401 bad_password.
    with = body.clone();
    with["password"] = json!("not the password");
    let r = create(&f, &cookie, with).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        (&r.body["error"]["code"], &r.body["error"]["field"]),
        (&json!("bad_password"), &json!("password"))
    );
    assert!(f.store.lock().unwrap().api_keys().unwrap().is_empty());

    with = body.clone();
    with["password"] = json!(PASSWORD);
    let r = create(&f, &cookie, with).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.cache_control.as_deref(), Some("no-store"));
    let secret = r.body["secret"].as_str().unwrap().to_string();
    assert_eq!(secret.len(), keys::SECRET_LEN);
    assert!(secret.starts_with("lsk_"));
    let key = &r.body["key"];
    assert_eq!(key["name"], "Prowlarr", "trimmed");
    assert_eq!(key["prefix"], json!(&secret[..12]));
    assert_eq!(key["scopes"], json!(["torznab"]));
    assert_eq!(key["status"], "active");
    assert_eq!(key["kind"], "api_key");
    assert!(key["last_used_at"].is_null() && key["expires_at"].is_null());

    // The list never has it.
    let r = send(
        &f.app,
        req(Method::GET, "/api/v1/keys", Some(&cookie), None),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["keys"].as_array().unwrap().len(), 1);
    assert!(!r.text.contains(&secret[13..]), "{}", r.text);
    assert_eq!(r.body["keys"][0], *key);

    // Nor does the database: only the prefix and the hash.
    drop(f.store.lock().unwrap());
    for entry in std::fs::read_dir(f.dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let hay = String::from_utf8_lossy(&bytes);
        assert!(!hay.contains(&secret[13..]), "a file holds the secret");
    }
}

#[tokio::test]
async fn create_refuses_bad_input_before_asking_for_the_password() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let search = saved_search(&f, "s");
    for (body, code, field) in [
        (
            json!({ "name": "  ", "kind": "api_key", "scopes": ["search"] }),
            "bad_name",
            "name",
        ),
        (
            json!({ "name": "x".repeat(201), "kind": "api_key", "scopes": ["search"] }),
            "bad_name",
            "name",
        ),
        (
            json!({ "name": "a", "kind": "api_key", "scopes": [] }),
            "bad_scopes",
            "scopes",
        ),
        (
            json!({ "name": "a", "kind": "feed_token", "scopes": ["search"] }),
            "bad_scopes",
            "scopes",
        ),
        (
            json!({ "name": "a", "kind": "api_key", "scopes": ["search"], "saved_search_id": search }),
            "bad_key",
            "saved_search_id",
        ),
        (
            json!({ "name": "a", "kind": "api_key", "scopes": ["search"], "expires_at": 1 }),
            "bad_expiry",
            "expires_at",
        ),
    ] {
        let r = create(&f, &cookie, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text);
        assert_eq!(
            (&r.body["error"]["code"], &r.body["error"]["field"]),
            (&json!(code), &json!(field)),
            "{body}"
        );
    }
    let r = create(
        &f,
        &cookie,
        json!({ "name": "a", "kind": "feed_token", "saved_search_id": search + 9, "password": PASSWORD }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.body["error"]["code"], "saved_search_not_found");
}

#[tokio::test]
async fn every_presentation_form_works_and_never_sets_a_cookie() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let (id, key) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let bearer = format!("Bearer {key}");
    for request in [
        get_with("/test/torznab", &[("authorization", &bearer)]),
        get_with("/test/torznab", &[("x-api-key", &key)]),
        get_with(&format!("/test/torznab?t=caps&apikey={key}"), &[]),
        // The same key twice is fine.
        get_with(
            &format!("/test/torznab?apikey={key}"),
            &[("x-api-key", &key)],
        ),
    ] {
        let r = send(&f.app, request, fresh_ip()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(
            r.body["access"],
            json!(format!("Key {{ id: {id}, scopes: 2 }}"))
        );
    }
    // The use is recorded.
    let listed = f.store.lock().unwrap().api_key(id).unwrap().unwrap();
    assert!(listed.last_used_at.is_some());

    // A handler's cookie never goes out on a key request...
    let r = send(
        &f.app,
        get_with("/test/torznab/cookie", &[("x-api-key", &key)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.set_cookie);
    // ...and a key can't write.
    let mut w = get_with("/test/torznab/write", &[("x-api-key", &key)]);
    *w.method_mut() = Method::POST;
    let r = send(&f.app, w, fresh_ip()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["error"]["code"], "key_read_only");

    // A session reaches the same routes, cookie or not.
    let r = send(
        &f.app,
        req(Method::GET, "/test/torznab", Some(&cookie), None),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.body["access"], "Session");
    // Nothing at all: the usual 401.
    let r = send(&f.app, get_with("/test/torznab", &[]), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["error"]["code"], "login_required");
}

#[tokio::test]
async fn every_bad_secret_fails_the_same_way() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let (revoked_id, revoked) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let r = send(
        &f.app,
        req(
            Method::POST,
            &format!("/api/v1/keys/{revoked_id}/revoke"),
            Some(&cookie),
            Some(json!({})),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["status"], "revoked");
    // An expired key, straight into the store.
    let m = keys::mint(KeyKind::ApiKey).unwrap();
    f.store
        .lock()
        .unwrap()
        .create_api_key(
            &NewApiKey {
                kind: KeyKind::ApiKey,
                name: "old",
                prefix: &m.prefix,
                hash: m.hash,
                scopes: scope::ALL,
                saved_search_id: None,
                expires_at: Some(1),
            },
            0,
        )
        .unwrap();
    let expired = m.secret.expose().to_string();
    let (_, good) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let mut wrong = good.clone();
    let last = wrong.pop().unwrap();
    wrong.push(if last == 'A' { 'B' } else { 'A' });
    let unknown = keys::mint(KeyKind::ApiKey)
        .unwrap()
        .secret
        .expose()
        .to_string();
    let (_, feed) = make_key(&f, &cookie, "feed_token", json!([])).await;

    let mut answers = Vec::new();
    for bad in [
        revoked.as_str(),
        &expired,
        &wrong,
        &unknown,
        "garbage",
        &feed,
    ] {
        let r = send(
            &f.app,
            get_with("/test/torznab", &[("x-api-key", bad)]),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad}");
        assert!(!r.text.contains(bad), "the secret isn't echoed");
        answers.push(r.text);
    }
    assert!(answers.windows(2).all(|w| w[0] == w[1]), "{answers:?}");
    assert!(answers[0].contains("bad_api_key"));
}

#[tokio::test]
async fn scopes_are_enforced() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let (_, search_only) = make_key(&f, &cookie, "api_key", json!(["search"])).await;
    let r = send(
        &f.app,
        get_with("/test/search", &[("x-api-key", &search_only)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &search_only)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["error"]["code"], "key_scope");
    // A refused request isn't recorded as a use.
    let (refused_id, refused) = make_key(&f, &cookie, "api_key", json!(["search"])).await;
    send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &refused)]),
        fresh_ip(),
    )
    .await;
    let k = f
        .store
        .lock()
        .unwrap()
        .api_key(refused_id)
        .unwrap()
        .unwrap();
    assert_eq!(k.last_used_at, None);
    let r = send(
        &f.app,
        get_with("/test/feeds/1", &[("x-api-key", &search_only)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let (_, feeds) = make_key(&f, &cookie, "api_key", json!(["feeds", "torznab"])).await;
    let r = send(
        &f.app,
        get_with("/test/feeds/1", &[("x-api-key", &feeds)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // `token` is for feed tokens only, and only on feed routes.
    let r = send(
        &f.app,
        get_with(&format!("/test/feeds/1?token={feeds}"), &[]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &f.app,
        get_with(&format!("/test/torznab?token={feeds}"), &[]),
        fresh_ip(),
    )
    .await;
    assert_eq!(
        r.body["error"]["code"], "login_required",
        "ignored off feed routes"
    );
}

#[tokio::test]
async fn a_feed_token_is_limited_to_its_saved_search() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let mine = saved_search(&f, "mine");
    let other = saved_search(&f, "other");
    let r = create(
        &f,
        &cookie,
        json!({ "name": "reader", "kind": "feed_token", "saved_search_id": mine, "password": PASSWORD }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.body["key"]["scopes"], json!(["feeds"]));
    assert_eq!(r.body["key"]["saved_search_id"], json!(mine));
    let token = r.body["secret"].as_str().unwrap().to_string();
    assert!(token.starts_with("lsf_"));

    let at = |id: i64| format!("/test/feeds/{id}?token={token}");
    let r = send(&f.app, get_with(&at(mine), &[]), fresh_ip()).await;
    assert_eq!((r.status, r.text.as_str()), (StatusCode::OK, "<rss/>"));
    let r = send(&f.app, get_with(&at(other), &[]), fresh_ip()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Only as `?token=`, and never on a route that isn't a feed.
    let r = send(
        &f.app,
        get_with(&format!("/test/feeds/{mine}"), &[("x-api-key", &token)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &f.app,
        get_with(&format!("/test/torznab?apikey={token}"), &[]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // An every-feed token reaches both.
    let (_, all) = make_key(&f, &cookie, "feed_token", json!([])).await;
    for id in [mine, other] {
        let r = send(
            &f.app,
            get_with(&format!("/test/feeds/{id}?token={all}"), &[]),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
    }
}

#[tokio::test]
async fn regenerate_kills_the_old_secret() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let (id, old) = make_key(&f, &cookie, "api_key", json!(["torznab", "search"])).await;
    let uri = format!("/api/v1/keys/{id}/regenerate");
    let r = send(
        &f.app,
        req(Method::POST, &uri, Some(&cookie), Some(json!({}))),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    let r = send(
        &f.app,
        req(
            Method::POST,
            &uri,
            Some(&cookie),
            Some(json!({ "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.cache_control.as_deref(), Some("no-store"));
    let new = r.body["secret"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    assert_eq!(r.body["key"]["name"], "Prowlarr");
    assert_eq!(r.body["key"]["scopes"], json!(["search", "torznab"]));
    let new_id = r.body["key"]["id"].as_i64().unwrap();
    assert_ne!(new_id, id);

    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &old)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &new)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // The old one is gone from the list; a gone or revoked key can't be
    // regenerated.
    let keys = f.store.lock().unwrap().api_keys().unwrap();
    assert_eq!(keys.iter().map(|k| k.id).collect::<Vec<_>>(), [new_id]);
    let r = send(
        &f.app,
        req(
            Method::POST,
            &uri,
            Some(&cookie),
            Some(json!({ "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    send(
        &f.app,
        req(
            Method::POST,
            &format!("/api/v1/keys/{new_id}/revoke"),
            Some(&cookie),
            Some(json!({})),
        ),
        fresh_ip(),
    )
    .await;
    let r = send(
        &f.app,
        req(
            Method::POST,
            &format!("/api/v1/keys/{new_id}/regenerate"),
            Some(&cookie),
            Some(json!({})),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"]["code"], "key_inactive");
}

#[tokio::test]
async fn revoke_and_delete_need_no_password() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let (id, key) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let revoke = format!("/api/v1/keys/{id}/revoke");
    for _ in 0..2 {
        let r = send(
            &f.app,
            req(Method::POST, &revoke, Some(&cookie), Some(json!({}))),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "idempotent");
        assert_eq!(r.body["status"], "revoked");
    }
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &key)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let del = format!("/api/v1/keys/{id}");
    let r = send(
        &f.app,
        req(Method::DELETE, &del, Some(&cookie), Some(json!({}))),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, json!({ "ok": true }));
    let r = send(
        &f.app,
        req(Method::DELETE, &del, Some(&cookie), Some(json!({}))),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.body["error"]["code"], "key_not_found");
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/keys/999/revoke",
            Some(&cookie),
            Some(json!({})),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Cross-site writes are refused like every other.
    let mut x = req(Method::DELETE, &del, Some(&cookie), Some(json!({})));
    x.headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    x.headers_mut()
        .insert(header::HOST, "lodestar.local".parse().unwrap());
    assert_eq!(
        send(&f.app, x, fresh_ip()).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn failed_key_checks_are_limited_per_ip() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let (_, good) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let attacker = fresh_ip();
    let bad = keys::mint(KeyKind::ApiKey)
        .unwrap()
        .secret
        .expose()
        .to_string();
    for i in 0..auth::KEY_FAIL_BURST as usize {
        let r = send(
            &f.app,
            get_with("/test/torznab", &[("x-api-key", &bad)]),
            attacker,
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "attempt {i}");
    }
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &bad)]),
        attacker,
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"]["code"], "rate_limited");
    let wait: u64 = r.retry_after.unwrap().parse().unwrap();
    assert!((1..=6).contains(&wait), "{wait}");
    // Even the right key waits from there; another address doesn't.
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &good)]),
        attacker,
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &good)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    // The login bucket is its own.
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/login",
            None,
            Some(json!({ "password": PASSWORD })),
        ),
        attacker,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn each_key_has_a_request_bucket() {
    let f = fixture_with(true);
    let cookie = signed_in(&f).await;
    let (_, key) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let (_, other) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    for i in 0..auth::KEY_BURST as usize {
        let r = send(
            &f.app,
            get_with("/test/torznab", &[("x-api-key", &key)]),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "request {i}");
    }
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &key)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"]["code"], "rate_limited");
    // With no refill the wait is the cap; `limit.rs` tests the arithmetic.
    assert_eq!(r.retry_after.as_deref(), Some("3600"));
    let r = send(
        &f.app,
        get_with("/test/torznab", &[("x-api-key", &other)]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "keys don't share a bucket");
}

#[tokio::test]
async fn the_fresh_password_is_asked_every_time_and_limited_like_a_login() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let from = fresh_ip();
    let revoke = |body: Value| {
        req(
            Method::POST,
            "/api/v1/sessions/revoke-others",
            Some(&cookie),
            Some(body),
        )
    };
    // Success, then nothing is remembered: the next call asks again.
    let r = send(&f.app, revoke(json!({ "password": PASSWORD })), from).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let new_cookie = r.cookie.unwrap();
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/sessions/revoke-others",
            Some(&new_cookie),
            Some(json!({})),
        ),
        from,
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(r.body["error"]["field"], "password");

    // Setup took one attempt from another address; this one has five.
    let other = fresh_ip();
    let wrong = || {
        req(
            Method::POST,
            "/api/v1/sessions/revoke-others",
            Some(&new_cookie),
            Some(json!({ "password": "nope nope nope" })),
        )
    };
    for _ in 0..5 {
        let r = send(&f.app, wrong(), other).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.body["error"]["code"], "bad_password");
    }
    let r = send(&f.app, wrong(), other).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.body["error"]["code"], "rate_limited");
    // A missing password spends nothing and still says 428.
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/sessions/revoke-others",
            Some(&new_cookie),
            Some(json!({})),
        ),
        other,
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    // The login bucket is the same one.
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/login",
            None,
            Some(json!({ "password": PASSWORD })),
        ),
        other,
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn revoking_other_sessions_keeps_this_one() {
    let f = fixture();
    let first = signed_in(&f).await;
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/login",
            None,
            Some(json!({ "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    let second = r.cookie.unwrap();
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/sessions/revoke-others",
            Some(&first),
            Some(json!({ "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, json!({ "ok": true }));
    let mine = r.cookie.unwrap();
    for (c, ok) in [(&first, false), (&second, false), (&mine, true)] {
        let r = send(
            &f.app,
            req(Method::GET, "/api/v1/keys", Some(c), None),
            fresh_ip(),
        )
        .await;
        assert_eq!(r.status == StatusCode::OK, ok, "{c}");
    }
}

#[tokio::test]
async fn without_user_data_keys_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let state = ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(
            Store::open_reader(&db, StoreConfig::default()).unwrap(),
        )),
        filters: None,
        settings: None,
        ops: None,
        user_data: None,
        adult: Default::default(),
        insights: None,
        hot: None,
        events: None,
    };
    let app = Router::new().nest("/api/v1", router(state));
    let token = auth.setup_token().unwrap();
    let r = send(
        &app,
        req(
            Method::POST,
            "/api/v1/setup",
            None,
            Some(json!({ "token": token, "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    let cookie = r.cookie.unwrap();
    let r = send(
        &app,
        req(Method::GET, "/api/v1/keys", Some(&cookie), None),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.body["error"]["code"], "keys_unavailable");
}

/// Every log line and span field written while the test runs, from any
/// thread (the blocking pool included).
mod capture {
    use std::fmt::Write as _;
    use std::sync::{Mutex, OnceLock};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    static LINES: Mutex<String> = Mutex::new(String::new());

    struct Capture;

    struct Fields(String);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, " {}={:?}", field.name(), value);
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let _ = write!(self.0, " {}={}", field.name(), value);
        }
    }

    fn keep(meta: &Metadata<'_>, f: Fields) {
        let mut lines = LINES.lock().unwrap();
        let _ = writeln!(lines, "{} {}{}", meta.level(), meta.target(), f.0);
    }

    impl Subscriber for Capture {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            let mut f = Fields(format!(" span={}", span.metadata().name()));
            span.record(&mut f);
            keep(span.metadata(), f);
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, values: &Record<'_>) {
            let mut f = Fields(String::new());
            values.record(&mut f);
            let mut lines = LINES.lock().unwrap();
            let _ = writeln!(lines, "record{}", f.0);
        }

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut f = Fields(String::new());
            event.record(&mut f);
            keep(event.metadata(), f);
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    /// Starts capturing (once per test binary).
    pub fn start() {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| {
            tracing::subscriber::set_global_default(Capture).expect("no other subscriber");
        });
    }

    pub fn lines() -> String {
        LINES.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn no_secret_key_token_or_proxy_user_reaches_a_log() {
    capture::start();
    let f = fixture();
    let cookie = signed_in(&f).await;
    let search = saved_search(&f, "logged");
    // A key, used every way, regenerated, then failed with.
    let (id, key) = make_key(&f, &cookie, "api_key", json!(["torznab"])).await;
    let bearer = format!("Bearer {key}");
    for r in [
        get_with("/test/torznab", &[("authorization", &bearer)]),
        get_with("/test/torznab", &[("x-api-key", &key)]),
        get_with(&format!("/test/torznab?apikey={key}"), &[]),
    ] {
        assert_eq!(send(&f.app, r, fresh_ip()).await.status, StatusCode::OK);
    }
    let r = send(
        &f.app,
        req(
            Method::POST,
            &format!("/api/v1/keys/{id}/regenerate"),
            Some(&cookie),
            Some(json!({ "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    let new = r.body["secret"].as_str().unwrap().to_string();
    for bad in [&key, &new[..new.len() - 1]] {
        send(
            &f.app,
            get_with("/test/torznab", &[("x-api-key", bad)]),
            fresh_ip(),
        )
        .await;
    }
    // A feed token.
    let r = create(
        &f,
        &cookie,
        json!({ "name": "rss", "kind": "feed_token", "saved_search_id": search, "password": PASSWORD }),
    )
    .await;
    let token = r.body["secret"].as_str().unwrap().to_string();
    send(
        &f.app,
        get_with(&format!("/test/feeds/{search}?token={token}"), &[]),
        fresh_ip(),
    )
    .await;
    // Wrong passwords, proxy auth and its user header.
    let wrong = "wrong-password-canary";
    send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/login",
            None,
            Some(json!({ "password": wrong })),
        ),
        fresh_ip(),
    )
    .await;
    let r = send(
        &f.app,
        req(
            Method::PUT,
            "/api/v1/auth/proxy",
            Some(&cookie),
            Some(json!({ "enabled": true, "trusted": ["10.50.0.0/16"], "allowed_users": ["proxy-user-canary"], "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = send(
        &f.app,
        get_with("/api/v1/keys", &[("remote-user", "proxy-user-canary")]),
        fresh_ip(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "signed in by the header");
    send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/sessions/revoke-others",
            Some(&cookie),
            Some(json!({ "password": PASSWORD })),
        ),
        fresh_ip(),
    )
    .await;

    let lines = capture::lines();
    assert!(
        lines.contains("setup complete"),
        "the capture sees the log: {lines}"
    );
    let session = cookie.split_once('=').unwrap().1;
    for (what, secret) in [
        ("api key", &key[13..]),
        ("regenerated key", &new[13..]),
        ("feed token", &token[13..]),
        ("password", PASSWORD),
        ("wrong password", wrong),
        ("proxy user", "proxy-user-canary"),
        ("session cookie", session),
    ] {
        assert!(
            !lines.contains(secret),
            "the {what} reached a log:\n{lines}"
        );
    }
}
