//! `POST /api/v1/settings/test` runs at most `MAX_TESTS` tests at once:
//! past that, `429 tests_busy` at once, never a queue. Its own test
//! binary, since the cap is process-wide.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::settings::MAX_TESTS;
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

fn request(cookie: &str, url: &str) -> Request<Body> {
    let body = json!({ "kind": "download_client", "client": "aria2", "url": url });
    let mut req = Request::post("/api/v1/settings/test")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    req
}

#[tokio::test]
async fn tests_past_the_cap_are_refused_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let token = auth.setup_token().unwrap();
    let cookie = auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth,
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: None,
            user_data: None,
            adult: Default::default(),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    // A server that accepts and never answers: each test holds its slot.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let held = {
        let accepted = accepted.clone();
        tokio::spawn(async move {
            let mut open = Vec::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                open.push(socket);
                accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        })
    };
    let url = format!("http://{addr}/jsonrpc");
    let running: Vec<_> = (0..MAX_TESTS)
        .map(|_| tokio::spawn(app.clone().oneshot(request(&cookie, &url))))
        .collect();
    // Wait until each has connected to the server, so each holds a slot.
    let start = std::time::Instant::now();
    while accepted.load(std::sync::atomic::Ordering::SeqCst) < MAX_TESTS {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{} of {MAX_TESTS} connected",
            accepted.load(std::sync::atomic::Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(running.iter().all(|r| !r.is_finished()));
    // One more is refused at once.
    let refused = tokio::time::timeout(
        Duration::from_secs(1),
        app.clone().oneshot(request(&cookie, &url)),
    )
    .await
    .expect("refused at once, not queued")
    .unwrap();
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    let bytes = axum::body::to_bytes(refused.into_body(), 1 << 16)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], json!("tests_busy"));
    // Dropping the running tests frees their slots.
    for r in &running {
        r.abort();
    }
    for r in running {
        let _ = r.await;
    }
    held.abort();
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let resp = app
        .clone()
        .oneshot(request(&cookie, &format!("http://{closed}/jsonrpc")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
