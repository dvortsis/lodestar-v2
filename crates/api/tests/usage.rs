//! `GET /api/v1/usage` and `POST /api/v1/usage/clear` end to end: the
//! count, and Clear usage data behind its confirmation and the
//! same-origin check.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{QueryLogEntry, QueryLogging, Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    app: Router,
    cookie: String,
    _dir: tempfile::TempDir,
}

/// Two searches logged through the store's own reporting API.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let entry = |q| QueryLogEntry {
        at: 1_700_000_000,
        query: q,
        results: Some(1),
        latency_us: 10,
    };
    store
        .log_queries(QueryLogging::On, &[entry("ubuntu"), entry("debian")])
        .unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let token = auth.setup_token().unwrap();
    let set = auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap();
    let app = Router::new().nest(
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
            insights: None,
            hot: None,
            events: None,
        }),
    );
    Fixture {
        app,
        cookie: set.split(';').next().unwrap().to_string(),
        _dir: dir,
    }
}

async fn call(
    f: &Fixture,
    req: axum::http::request::Builder,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = req.header(header::COOKIE, &f.cookie);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = req
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn count(f: &Fixture) -> Value {
    let (status, v) = call(f, Request::get("/api/v1/usage"), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

fn clear() -> axum::http::request::Builder {
    Request::post("/api/v1/usage/clear")
}

#[tokio::test]
async fn clearing_usage_data_needs_its_confirmation_and_the_same_origin() {
    let f = fixture();
    assert_eq!(count(&f).await, json!({ "queries": 2 }));
    for body in [json!({}), json!({ "confirm": false })] {
        let (status, v) = call(&f, clear(), Some(body.clone())).await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}: {v}");
        assert_eq!(v["error"]["code"], json!("confirmation_required"));
    }
    // From another site, even confirmed: refused.
    let (status, v) = call(
        &f,
        clear().header(header::ORIGIN, "https://evil.example"),
        Some(json!({ "confirm": true })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(count(&f).await["queries"], json!(2), "nothing deleted yet");
    let (status, v) = call(&f, clear(), Some(json!({ "confirm": true }))).await;
    assert_eq!((status, v), (StatusCode::OK, json!({ "queries": 2 })));
    assert_eq!(count(&f).await["queries"], json!(0));
}
