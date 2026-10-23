//! Trusted reverse-proxy header auth (§12) end to end: `/auth/proxy`, the
//! header honoured only from a trusted peer, `X-Forwarded-For` for
//! per-client limits, and the `LODESTAR_PROXY_AUTH=off` override.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const PROXY: [u8; 4] = [172, 17, 0, 2];

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    dir: tempfile::TempDir,
}

fn app_for(dir: &tempfile::TempDir, auth: Arc<Auth>) -> Router {
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth,
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: Some(Arc::new(Mutex::new(store))),
            adult: Default::default(),
        }),
    )
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    Fixture {
        app: app_for(&dir, auth.clone()),
        auth,
        dir,
    }
}

struct Reply {
    status: StatusCode,
    cookie: Option<String>,
    body: Value,
}

async fn send(app: &Router, mut req: Request<Body>, from: impl Into<SocketAddr>) -> Reply {
    req.extensions_mut().insert(ConnectInfo(from.into()));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    Reply {
        status,
        cookie,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn req(method: Method, uri: &str, headers: &[(&str, &str)], body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn at(ip: [u8; 4]) -> SocketAddr {
    SocketAddr::from((ip, 50000))
}

async fn signed_in(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/setup",
            &[],
            Some(json!({ "token": token, "password": PASSWORD })),
        ),
        at([127, 0, 0, 1]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    r.cookie.unwrap()
}

/// Turns proxy auth on, trusting `172.17.0.0/16`.
async fn enable(f: &Fixture, cookie: &str, users: Value) -> Reply {
    send(
        &f.app,
        req(
            Method::PUT,
            "/api/v1/auth/proxy",
            &[("cookie", cookie)],
            Some(json!({
                "enabled": true,
                "trusted": ["172.17.0.0/16"],
                "allowed_users": users,
                "password": PASSWORD,
            })),
        ),
        at([127, 0, 0, 1]),
    )
    .await
}

/// A protected route: 200 with a session, 401 without.
async fn keys_as(f: &Fixture, from: [u8; 4], headers: &[(&str, &str)]) -> StatusCode {
    send(
        &f.app,
        req(Method::GET, "/api/v1/keys", headers, None),
        at(from),
    )
    .await
    .status
}

#[tokio::test]
async fn it_is_off_by_default_and_changing_it_needs_the_password() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    let c = [("cookie", cookie.as_str())];
    let r = send(
        &f.app,
        req(Method::GET, "/api/v1/auth/proxy", &c, None),
        at([127, 0, 0, 1]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body,
        json!({ "allowed_users": [], "enabled": false, "forced_off": false, "header": "Remote-User", "trusted": [] })
    );
    // The header does nothing while it's off, even from a would-be proxy.
    assert_eq!(
        keys_as(&f, PROXY, &[("remote-user", "ann")]).await,
        StatusCode::UNAUTHORIZED
    );

    let put = |body: Value| req(Method::PUT, "/api/v1/auth/proxy", &c, Some(body));
    // Bad values are refused before the password is asked for.
    for (body, field) in [
        (json!({ "enabled": true, "trusted": [] }), "trusted"),
        (
            json!({ "enabled": true, "trusted": ["0.0.0.0/0"] }),
            "trusted",
        ),
        (
            json!({ "enabled": true, "trusted": ["not an ip"] }),
            "trusted",
        ),
        (
            json!({ "enabled": true, "trusted": ["10.0.0.1"], "header": "Cookie" }),
            "header",
        ),
        (
            json!({ "enabled": true, "trusted": ["10.0.0.1"], "allowed_users": [""] }),
            "allowed_users",
        ),
    ] {
        let r = send(&f.app, put(body.clone()), at([127, 0, 0, 1])).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            (&r.body["error"]["code"], &r.body["error"]["field"]),
            (&json!("bad_proxy_settings"), &json!(field)),
            "{body}"
        );
    }
    let ok = json!({ "enabled": true, "trusted": ["172.17.1.9/16"] });
    let r = send(&f.app, put(ok.clone()), at([127, 0, 0, 1])).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(r.body["error"]["field"], "password");
    let mut wrong = ok.clone();
    wrong["password"] = json!("nope nope nope");
    let r = send(&f.app, put(wrong), at([127, 0, 0, 1])).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(f.auth.proxy().enabled, false, "nothing changed");

    let r = enable(&f, &cookie, json!([])).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body,
        json!({ "allowed_users": [], "enabled": true, "forced_off": false, "header": "Remote-User", "trusted": ["172.17.0.0/16"] })
    );

    // Saved in auth.json, so a restart keeps it.
    let again = Auth::load(&f.dir.path().join("auth.json"), false).unwrap();
    assert_eq!(again.proxy(), f.auth.proxy());
}

#[tokio::test]
async fn the_header_counts_only_from_a_trusted_peer() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    assert_eq!(enable(&f, &cookie, json!([])).await.status, StatusCode::OK);
    let user = [("remote-user", "ann")];

    assert_eq!(keys_as(&f, PROXY, &user).await, StatusCode::OK);
    let r = send(
        &f.app,
        req(Method::GET, "/api/v1/auth", &user, None),
        at(PROXY),
    )
    .await;
    assert_eq!(r.body["logged_in"], true);
    // Straight to Lodestar, bypassing the proxy: ignored.
    assert_eq!(
        keys_as(&f, [192, 168, 1, 50], &user).await,
        StatusCode::UNAUTHORIZED
    );
    // A spoofed X-Forwarded-For doesn't make an untrusted peer trusted.
    assert_eq!(
        keys_as(
            &f,
            [192, 168, 1, 50],
            &[("remote-user", "ann"), ("x-forwarded-for", "172.17.0.2")]
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    // The proxy without the header: no session.
    assert_eq!(keys_as(&f, PROXY, &[]).await, StatusCode::UNAUTHORIZED);
    // Cookies keep working, from anywhere.
    assert_eq!(
        keys_as(&f, [192, 168, 1, 50], &[("cookie", &cookie)]).await,
        StatusCode::OK
    );
    // A header session doesn't skip the fresh password.
    let r = send(
        &f.app,
        req(
            Method::POST,
            "/api/v1/keys",
            &user,
            Some(json!({ "name": "x", "kind": "api_key", "scopes": ["torznab"] })),
        ),
        at(PROXY),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED);
}

#[tokio::test]
async fn allowed_users_narrow_who_the_header_signs_in() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    assert_eq!(
        enable(&f, &cookie, json!(["ann"])).await.status,
        StatusCode::OK
    );
    assert_eq!(
        keys_as(&f, PROXY, &[("remote-user", "ann")]).await,
        StatusCode::OK
    );
    assert_eq!(
        keys_as(&f, PROXY, &[("remote-user", "eve")]).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn forwarded_clients_get_their_own_login_bucket() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    assert_eq!(enable(&f, &cookie, json!([])).await.status, StatusCode::OK);
    let login = |xff: &str| {
        req(
            Method::POST,
            "/api/v1/login",
            &[("x-forwarded-for", xff)],
            Some(json!({ "password": "nope nope nope" })),
        )
    };
    // One client behind the proxy uses up its five attempts...
    for _ in 0..5 {
        let r = send(&f.app, login("203.0.113.7"), at(PROXY)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = send(&f.app, login("6.6.6.6, 203.0.113.7"), at(PROXY)).await;
    assert_eq!(
        r.status,
        StatusCode::TOO_MANY_REQUESTS,
        "a spoofed left entry doesn't escape the bucket"
    );
    // ...and another client behind the same proxy still has its own.
    let r = send(&f.app, login("203.0.113.8"), at(PROXY)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // From an untrusted peer, X-Forwarded-For is ignored: varying it
    // doesn't buy more attempts.
    let direct = [192, 168, 1, 77];
    for i in 0..5 {
        let r = send(&f.app, login(&format!("198.51.100.{i}")), at(direct)).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = send(&f.app, login("198.51.100.99"), at(direct)).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_environment_override_turns_it_off() {
    let f = fixture();
    let cookie = signed_in(&f).await;
    assert_eq!(enable(&f, &cookie, json!([])).await.status, StatusCode::OK);
    let user = [("remote-user", "ann")];
    assert_eq!(keys_as(&f, PROXY, &user).await, StatusCode::OK);

    // A restart with LODESTAR_PROXY_AUTH=off, as services.rs applies it.
    let auth = Arc::new(Auth::load(&f.dir.path().join("auth.json"), false).unwrap());
    assert!(!auth.apply_proxy_override(None));
    assert!(!auth.apply_proxy_override(Some("on")));
    assert!(auth.apply_proxy_override(Some(" OFF ")));
    let app = app_for(&f.dir, auth.clone());
    let r = send(
        &app,
        req(Method::GET, "/api/v1/keys", &user, None),
        at(PROXY),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "the header is ignored");
    // Forwarded addresses are too: the proxy's own address is the client.
    let login = || {
        req(
            Method::POST,
            "/api/v1/login",
            &[("x-forwarded-for", "203.0.113.7")],
            Some(json!({ "password": PASSWORD })),
        )
    };
    let r = send(&app, login(), at(PROXY)).await;
    assert_eq!(r.status, StatusCode::OK, "the password still works");
    let c = r.cookie.unwrap();
    let r = send(
        &app,
        req(Method::GET, "/api/v1/auth/proxy", &[("cookie", &c)], None),
        at(PROXY),
    )
    .await;
    assert_eq!(r.body["forced_off"], true);
    assert_eq!(r.body["enabled"], true, "the saved settings are kept");
    assert_eq!(
        auth.client_ip(at(PROXY).ip(), &Default::default()),
        at(PROXY).ip()
    );
}
