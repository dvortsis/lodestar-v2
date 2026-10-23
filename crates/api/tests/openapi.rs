//! `/openapi.json`: a valid OpenAPI 3.1 document that describes exactly
//! the routes the router serves, with the session scheme and the error
//! envelope, and field nullability as `docs/metrics.md` §7 says.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use lodestar_api::{ApiState, Auth, openapi, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::Value;
use tower::ServiceExt;

/// Every route `api::router` serves. A new route goes here, in its
/// module's `ApiDoc`, and in `routes()`.
const ROUTES: &[(&str, &str)] = &[
    ("GET", "/auth"),
    ("POST", "/setup"),
    ("POST", "/login"),
    ("POST", "/logout"),
    ("POST", "/password"),
    ("GET", "/parse"),
    ("GET", "/search"),
    ("POST", "/search"),
    ("GET", "/facets"),
    ("POST", "/facets"),
    ("GET", "/suggest"),
    ("GET", "/torrents/{hash}"),
    ("GET", "/torrents/{hash}/files"),
    ("GET", "/torrents/{hash}/siblings"),
    ("POST", "/compare"),
    ("GET", "/discover"),
    ("GET", "/filters"),
    ("GET", "/filters/queue"),
    ("GET", "/filters/tombstones"),
    ("GET", "/filters/fixtures"),
    ("POST", "/filters/test"),
    ("POST", "/filters/strictness"),
    ("GET", "/classify"),
    ("POST", "/classify/correct"),
    ("POST", "/classify/rules"),
    ("POST", "/classify/rules/remove"),
    ("POST", "/filters/preview"),
    ("POST", "/filters/preview/jobs"),
    ("GET", "/filters/preview/jobs/{id}"),
    ("POST", "/filters/preview/jobs/{id}/cancel"),
    ("POST", "/filters/apply"),
    ("GET", "/settings"),
    ("GET", "/settings/{section}"),
    ("PATCH", "/settings/{section}"),
    ("GET", "/events"),
    ("GET", "/openapi.json"),
];
const OPEN: [&str; 5] = ["/auth", "/setup", "/login", "/logout", "/openapi.json"];
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

fn doc() -> Value {
    serde_json::from_str(&openapi::json()).unwrap()
}

fn operations(doc: &Value) -> Vec<(String, String, &Value)> {
    let mut out = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for m in METHODS {
            if let Some(op) = item.get(m) {
                out.push((m.to_uppercase(), path.clone(), op));
            }
        }
    }
    out
}

/// Resolves a local `$ref` (`#/components/schemas/X`).
fn resolve<'a>(doc: &'a Value, r: &str) -> Option<&'a Value> {
    let mut v = doc;
    for part in r.strip_prefix("#/")?.split('/') {
        v = v.get(part)?;
    }
    Some(v)
}

fn refs<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                out.push(r);
            }
            m.values().for_each(|v| refs(v, out));
        }
        Value::Array(a) => a.iter().for_each(|v| refs(v, out)),
        _ => {}
    }
}

#[test]
fn the_document_is_well_formed() {
    let doc = doc();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["info"]["title"], "Lodestar API");
    assert!(doc["info"]["version"].is_string());
    assert_eq!(doc["servers"][0]["url"], openapi::PREFIX);

    // Every reference resolves.
    let mut all = Vec::new();
    refs(&doc, &mut all);
    assert!(!all.is_empty());
    for r in all {
        assert!(resolve(&doc, r).is_some(), "dangling $ref {r}");
    }

    // Operation ids are unique; path parameters are declared; every
    // operation has a success response and the error envelope.
    let mut ids = BTreeSet::new();
    for (method, path, op) in operations(&doc) {
        let id = op["operationId"].as_str().unwrap();
        assert!(ids.insert(id.to_string()), "duplicate operationId {id}");
        assert!(op["summary"].is_string(), "{method} {path} has no summary");
        let declared: BTreeSet<&str> = op["parameters"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .filter(|p| p["in"] == "path")
                    .map(|p| {
                        assert_eq!(p["required"], true, "{method} {path}");
                        p["name"].as_str().unwrap()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let in_path: BTreeSet<&str> = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .collect();
        assert_eq!(declared, in_path, "{method} {path} path parameters");
        let responses = op["responses"].as_object().unwrap();
        assert!(
            responses.keys().any(|k| k.starts_with('2')),
            "{method} {path} has no success response"
        );
        assert_eq!(
            responses["default"]["$ref"], "#/components/responses/Error",
            "{method} {path}"
        );
        // The session, except on the open paths.
        let security = op["security"].as_array().unwrap();
        if OPEN.contains(&path.as_str()) {
            assert!(security.is_empty(), "{method} {path} should be open");
        } else {
            assert_eq!(
                security,
                &vec![serde_json::json!({ "session": [] })],
                "{method} {path}"
            );
        }
    }

    let scheme = &doc["components"]["securitySchemes"]["session"];
    assert_eq!(scheme["type"], "apiKey");
    assert_eq!(scheme["in"], "cookie");
    assert_eq!(scheme["name"], lodestar_api::auth::COOKIE);
    let error = &doc["components"]["responses"]["Error"]["content"]["application/json"]["schema"];
    assert_eq!(error["$ref"], "#/components/schemas/ErrorEnvelope");
}

#[test]
fn it_lists_exactly_the_served_routes() {
    let doc = doc();
    let documented: BTreeSet<(String, String)> = operations(&doc)
        .into_iter()
        .map(|(m, p, _)| (m, p))
        .collect();
    let served: BTreeSet<(String, String)> = ROUTES
        .iter()
        .map(|(m, p)| (m.to_string(), p.to_string()))
        .collect();
    assert_eq!(documented, served);
}

fn app() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth,
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            adult: Default::default(),
        }),
    );
    (dir, app)
}

#[tokio::test]
async fn every_documented_route_is_served() {
    let (_dir, app) = app();
    for (method, path) in ROUTES {
        let uri = format!(
            "/api/v1{}",
            path.replace("{hash}", &"ab".repeat(20))
                .replace("{id}", "1")
                .replace("{section}", "crawler")
        );
        let req = Request::builder()
            .method(Method::from_bytes(method.as_bytes()).unwrap())
            .uri(&uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = body["error"]["code"].as_str().unwrap_or("");
        assert!(
            code != "no_route" && code != "method_not_allowed",
            "{method} {path}: {status} {body}"
        );
        // Without a session, only the open paths answer.
        let open = OPEN.contains(path);
        assert_eq!(
            status == StatusCode::UNAUTHORIZED && code == "login_required",
            !open,
            "{method} {path}: {status} {body}"
        );
    }
}

#[tokio::test]
async fn it_is_served_open_as_generated() {
    let (_dir, app) = app();
    let resp = app
        .oneshot(
            Request::get("/api/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), openapi::json());
}

#[test]
fn generation_is_stable() {
    assert_eq!(openapi::json(), openapi::json());
    assert!(openapi::json().ends_with("}\n"));
}

/// `docs/metrics.md` §7: nullable fields are present and may be null;
/// `matched_files` may be absent; the rest are never null.
#[test]
fn nullability_follows_the_field_contract() {
    let doc = doc();
    let schema = |name: &str| doc["components"]["schemas"][name].clone();
    let required = |s: &Value| -> BTreeSet<String> {
        s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let nullable = |s: &Value, f: &str| {
        let t = &s["properties"][f]["type"];
        t.as_array()
            .is_some_and(|a| a.contains(&Value::from("null")))
            || s["properties"][f]["oneOf"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v["type"] == "null"))
    };
    let swarm = [
        "seeders",
        "leechers",
        "completed",
        "trackers_reporting",
        "trackers_checked",
        "last_scrape_at",
    ];
    for name in ["SearchHit", "TorrentDetail"] {
        let s = schema(name);
        let req = required(&s);
        for f in swarm.iter().chain(&["hash_v2"]) {
            assert!(req.contains(*f), "{name}.{f} is always present");
            assert!(nullable(&s, f), "{name}.{f} is nullable");
        }
        for f in ["id", "hash", "name", "size", "files", "piece_len", "flags"] {
            assert!(req.contains(f), "{name}.{f}");
            assert!(!nullable(&s, f), "{name}.{f} is never null");
        }
    }
    let hit = schema("SearchHit");
    assert!(!required(&hit).contains("matched_files"));
    assert!(!nullable(&hit, "matched_files"));
    let detail = schema("TorrentDetail");
    for f in ["dht_seeds", "dht_peers", "swarm_trend", "swarm_history"] {
        assert!(required(&detail).contains(f), "TorrentDetail.{f}");
        assert!(nullable(&detail, f), "TorrentDetail.{f}");
    }
    let sample = schema("SwarmSample");
    assert!(nullable(&sample, "seeders") && nullable(&sample, "leechers"));
    assert!(!nullable(&sample, "at"));
    // The envelope's optional keys are absent, never null.
    let body = schema("ErrorBody");
    assert_eq!(
        required(&body),
        ["code", "message"].map(String::from).into()
    );
    for f in ["field", "start", "end"] {
        assert!(!nullable(&body, f), "ErrorBody.{f}");
    }
}
