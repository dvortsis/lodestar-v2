//! X-api-repeated-params: every route that reads its query answers a known
//! parameter sent more than once the same way, `400 repeated_parameter`
//! naming it, on GET and HEAD alike; a single one still works, an unknown
//! one is ignored repeated or not, and any `ast` on GET is `ast_needs_post`
//! (docs/decisions/2026-11-21-X-api-repeated-params.md). `/logs` and
//! `/events`, which need their own services, are covered in their own files.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_search::SearchIndex;
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

/// Every route in the decisions file's table that reads its query through
/// the shared extractor or `/facets`' layer: the method, the path, one of its
/// parameters with a value it accepts, and any other parameter it requires.
const ROUTES: &[(&str, &str, &str, &str, &str)] = &[
    ("GET", "/parse", "q", "movie", ""),
    ("GET", "/search", "sort", "size", ""),
    ("GET", "/search", "q", "movie", ""),
    ("GET", "/facets", "q", "movie", ""),
    ("GET", "/digest/preview", "period", "daily", ""),
    ("GET", "/torrents/{hash}/files", "offset", "0", ""),
    ("GET", "/collections", "hidden", "true", ""),
    ("GET", "/collections/1", "limit", "5", ""),
    ("GET", "/saved-searches/1/alerts", "limit", "5", ""),
    ("GET", "/user-items", "limit", "5", ""),
    ("GET", "/user-items/labels", "hidden", "true", ""),
    ("GET", "/export", "format", "json", ""),
    ("GET", "/filters/queue", "limit", "5", ""),
    ("GET", "/filters/tombstones", "limit", "5", ""),
    ("GET", "/discover", "type", "movie", ""),
    ("GET", "/settings/export", "preferences", "true", ""),
    ("GET", "/suggest", "q", "mov", ""),
    ("GET", "/insights/overview", "range", "24h", ""),
    ("GET", "/insights/events", "from", "0", "to=1"),
    ("GET", "/insights/discover", "range", "24h", ""),
    ("GET", "/insights/export.csv", "panel", "trend", ""),
    ("POST", "/settings/import", "dry_run", "true", ""),
];

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: Some(SearchIndex::in_memory().unwrap().into()),
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            adult: Default::default(),
            ops: None,
            insights: None,
            user_data: None,
            hot: None,
            events: None,
        }),
    );
    Fixture {
        app,
        auth,
        _dir: dir,
    }
}

/// The status, the `content-length` header and the body.
async fn call(app: &Router, mut req: Request<Body>) -> (StatusCode, String, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 1))));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let len = resp
        .headers()
        .get(header::CONTENT_LENGTH)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, len, String::from_utf8_lossy(&bytes).into_owned())
}

fn request(method: &str, uri: &str, cookie: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    if method == "POST" {
        b = b
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ORIGIN, "http://localhost");
    }
    b.body(Body::empty()).unwrap()
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({ "token": token, "password": PASSWORD,
                       "notice_version": lodestar_api::legal::NOTICE_VERSION });
    let mut req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn uri(path: &str, query: &str) -> String {
    format!("/api/v1{}?{query}", path.replace("{hash}", HASH))
}

fn error(body: &str) -> Value {
    serde_json::from_str::<Value>(body).unwrap_or(Value::Null)["error"].clone()
}

#[tokio::test]
async fn every_route_names_a_repeated_parameter() {
    let f = fixture();
    let c = session(&f).await;
    for &(method, path, name, value, rest) in ROUTES {
        let twice = uri(path, &format!("{rest}&{name}={value}&{name}={value}"));
        let (status, len, body) = call(&f.app, request(method, &twice, &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {twice}: {body}");
        let e = error(&body);
        assert_eq!(e["code"], "repeated_parameter", "{method} {twice}: {body}");
        assert_eq!(e["field"], name, "{method} {twice}: {body}");
        assert_eq!(
            e["message"],
            format!("{name} was sent more than once: send it once."),
            "{method} {twice}"
        );
        if method == "GET" {
            // HEAD, which `get()` also serves, gives GET's answer without
            // its body: the same status and length.
            let (head, head_len, _) = call(&f.app, request("HEAD", &twice, &c)).await;
            assert_eq!(head, StatusCode::BAD_REQUEST, "HEAD {twice}");
            assert!(!len.is_empty(), "{twice}");
            assert_eq!(head_len, len, "HEAD {twice}");
        }
        // Sent once, it's read as before: whatever the route answers, it
        // isn't about the parameter.
        let once = uri(path, &format!("{rest}&{name}={value}"));
        let (_, _, body) = call(&f.app, request(method, &once, &c)).await;
        let code = error(&body)["code"].clone();
        assert!(
            code != "repeated_parameter" && code != "bad_parameter",
            "{method} {once}: {body}"
        );
        // An unknown parameter is ignored, repeated or not.
        let unknown = uri(path, &format!("{rest}&{name}={value}&zz=1&zz=2"));
        let (_, _, body) = call(&f.app, request(method, &unknown, &c)).await;
        assert_ne!(error(&body)["code"], "repeated_parameter", "{method} {unknown}: {body}");
    }
}

#[tokio::test]
async fn a_repeated_name_is_capped_and_escaped() {
    let f = fixture();
    let c = session(&f).await;
    // `/facets`' prefix family takes any name after `limit.`, so a name a
    // caller made up reaches the check: 100 characters, and a bidi override.
    let long = format!("limit.{}", "z".repeat(100));
    let bidi = "limit.%E2%80%AE";
    for (query, shown) in [
        (format!("{long}=1&{long}=2"), format!("limit.{}…", "z".repeat(58))),
        (format!("{bidi}=1&{bidi}=2"), r"limit.\u{202e}".to_owned()),
    ] {
        let (status, _, body) = call(&f.app, request("GET", &uri("/facets", &query), &c)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let e = error(&body);
        assert_eq!(e["code"], "repeated_parameter", "{body}");
        assert_eq!(e["field"], shown, "{body}");
        assert!(!body.contains('\u{202e}'), "{body}");
    }
}

#[tokio::test]
async fn any_ast_on_get_is_ast_needs_post() {
    let f = fixture();
    let c = session(&f).await;
    for path in ["/search", "/facets"] {
        for query in ["ast=x&ast=y", "ast=x", "q=a&ast"] {
            let u = uri(path, query);
            let (status, _, body) = call(&f.app, request("GET", &u, &c)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{u}: {body}");
            assert_eq!(error(&body)["code"], "ast_needs_post", "{u}: {body}");
            assert_eq!(error(&body)["field"], "ast", "{u}: {body}");
        }
    }
}

#[tokio::test]
async fn signed_out_is_still_a_401_first() {
    let f = fixture();
    for &(method, path, name, value, _) in ROUTES {
        let twice = uri(path, &format!("{name}={value}&{name}={value}"));
        let mut req = Request::builder().method(method).uri(&twice).body(Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 1], 1))));
        let status = f.app.clone().oneshot(req).await.unwrap().status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {twice}");
    }
}
