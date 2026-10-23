//! `POST /api/v1/settings/test` end to end: a download client's
//! connection test on unsaved values, against a stand-in qBittorrent on
//! this machine, through the integrations SSRF guard. The result never
//! repeats the address or a credential.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    app: Router,
    cookie: String,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
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
            adult: Default::default(),
            insights: None,
        }),
    );
    Fixture {
        app,
        cookie: set.split(';').next().unwrap().to_string(),
        _dir: dir,
    }
}

/// A qBittorrent Web API that knows `admin` / `adminadmin`.
async fn qbittorrent() -> SocketAddr {
    let app = Router::new()
        .route(
            "/api/v2/auth/login",
            post(|body: String| async move {
                let form: Vec<(String, String)> = form_urlencoded::parse(body.as_bytes())
                    .into_owned()
                    .collect();
                let user = form
                    .iter()
                    .find(|(k, _)| k == "username")
                    .map(|(_, v)| v.as_str());
                let pass = form
                    .iter()
                    .find(|(k, _)| k == "password")
                    .map(|(_, v)| v.as_str());
                if (user, pass) == (Some("admin"), Some("adminadmin")) {
                    let mut h = HeaderMap::new();
                    h.insert(
                        header::SET_COOKIE,
                        "SID=abc123; HttpOnly; path=/".parse().unwrap(),
                    );
                    (h, "Ok.")
                } else {
                    (HeaderMap::new(), "Fails.")
                }
            }),
        )
        .route("/api/v2/app/version", get(|| async { "v4.6.3" }))
        .route("/api/v2/app/webapiVersion", get(|| async { "2.9.3" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

async fn test(f: &Fixture, body: Value) -> (StatusCode, Value, String) {
    let mut req = Request::post("/api/v1/settings/test")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, &f.cookie)
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    (status, serde_json::from_str(&text).unwrap(), text)
}

fn qbt(url: &str, password: &str) -> Value {
    json!({
        "kind": "download_client",
        "client": "qbittorrent",
        "url": url,
        "username": "admin",
        "password": password,
    })
}

#[tokio::test]
async fn a_reachable_client_reports_its_version() {
    let f = fixture();
    let addr = qbittorrent().await;
    let (status, v, _) = test(&f, qbt(&format!("http://{addr}/"), "adminadmin")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        v,
        json!({ "api_version": "2.9.3", "error": null, "ok": true, "version": "v4.6.3" })
    );
}

#[tokio::test]
async fn failures_say_why_without_the_address_or_password() {
    let f = fixture();
    let addr = qbittorrent().await;
    let url = format!("http://{addr}/");
    let (status, v, text) = test(&f, qbt(&url, "wrong-hunter2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["error"]["code"], json!("bad_credentials"));
    assert!(
        !text.contains("hunter2") && !text.contains(&addr.to_string()),
        "{text}"
    );
    // Nothing listens there.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let (_, v, _) = test(&f, qbt(&format!("http://{closed}/"), "x")).await;
    assert_eq!(v["error"]["code"], json!("unreachable"));
    // The SSRF guard: cloud metadata is never a download client.
    let (_, v, text) = test(&f, qbt("http://169.254.169.254/", "x")).await;
    assert_eq!(v["error"]["code"], json!("blocked"), "{text}");
    assert!(!text.contains("169.254"), "{text}");
    // Not a URL.
    let (_, v, _) = test(&f, qbt("not a url", "x")).await;
    assert_eq!(v["ok"], json!(false));
    // Bounded input.
    let (_, v, _) = test(&f, qbt(&format!("http://{addr}/{}", "a".repeat(3000)), "x")).await;
    assert_eq!(v["error"]["code"], json!("invalid"));
}

#[tokio::test]
async fn only_known_tests_are_run() {
    let f = fixture();
    for body in [
        json!({ "kind": "port_scan", "url": "http://127.0.0.1/" }),
        json!({ "kind": "download_client", "client": "utorrent", "url": "http://127.0.0.1/" }),
        json!({ "kind": "download_client", "client": "aria2", "url": "http://127.0.0.1/", "extra": 1 }),
    ] {
        let (status, v, _) = test(&f, body.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(v["error"]["code"], json!("bad_body"), "{body}");
    }
}

#[tokio::test]
async fn a_refusal_shows_its_status_never_the_body() {
    let f = fixture();
    // Any service on the LAN or this machine: its pages must not be
    // readable through the test.
    let app = Router::new().fallback(|| async {
        (
            StatusCode::NOT_FOUND,
            "internal-marker: the admin page of something else",
        )
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for client in ["rtorrent", "transmission", "aria2", "deluge", "qbittorrent"] {
        let (status, v, text) = test(
            &f,
            json!({ "kind": "download_client", "client": client, "url": format!("http://{addr}/any/path") }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{client}");
        assert_eq!(v["ok"], json!(false), "{client}");
        assert!(
            !text.contains("marker") && !text.contains("admin page"),
            "{client}: {text}"
        );
    }
    let (_, v, _) = test(
        &f,
        json!({ "kind": "download_client", "client": "rtorrent", "url": format!("http://{addr}/RPC2") }),
    )
    .await;
    assert_eq!(v["error"]["code"], json!("rejected"));
    assert!(
        v["error"]["message"].as_str().unwrap().contains("HTTP 404"),
        "{v}"
    );
}
