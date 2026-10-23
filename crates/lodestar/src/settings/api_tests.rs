//! `/api/v1/settings` with the real registry behind the real router, in
//! the API's golden style (`crates/api/tests/golden.rs`): each response's
//! status and bytes against `golden/<name>.txt`. The router is mounted as
//! `lodestar serve` mounts it, with the `/events` hub, so a save's
//! `settings` event is seen end to end.
//! `LODESTAR_BLESS=1 cargo test -p lodestar settings::api_tests` rewrites
//! the files; read the diff before committing it.

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, HttpBody};
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::events::{EventHub, EventSources, EventsConfig};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::Registry;
use crate::config::Config;

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    hub: Arc<EventHub>,
    registry: Arc<Registry>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[fetch]\ntimeout_secs = 30\n").unwrap();
    let env = |k: &str| (k == "LODESTAR_DHT_PORT").then(|| "7000".to_string());
    let config = Config::load(Some(&path), env).unwrap();
    let db = dir.path().join("t.db");
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let registry = Arc::new(Registry::load(&config, store).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let hub = EventHub::start(EventsConfig::default(), EventSources::default());
    let app = Router::new()
        .nest(
            "/api/v1",
            router(ApiState {
                auth: auth.clone(),
                index: None,
                reader: Arc::new(Mutex::new(reader)),
                filters: None,
                settings: Some(registry.clone()),
                ops: None,
                user_data: None,
                adult: Default::default(),
                insights: None,
            }),
        )
        .layer(Extension(hub.clone()));
    Fixture {
        app,
        auth,
        hub,
        registry,
        _dir: dir,
    }
}

async fn send(f: &Fixture, mut req: Request<Body>) -> (u16, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// Sends `req` and checks the status and body against `golden/{name}.txt`.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = send(f, req).await;
    let got = format!("{status}\n{body}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/settings/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        let want = want.replace("\r\n", "\n");
        assert_eq!(got, want, "{name}: the response bytes changed");
    }
    serde_json::from_str(&body).unwrap_or(Value::Null)
}

fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::get(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::empty()).unwrap()
}

fn patch(uri: &str, cookie: Option<&str>, body: &str) -> Request<Body> {
    write("PATCH", uri, cookie, body)
}

fn post(uri: &str, cookie: Option<&str>, body: &str) -> Request<Body> {
    write("POST", uri, cookie, body)
}

fn write(method: &str, uri: &str, cookie: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "token": token, "password": "correct horse battery" }).to_string(),
        ))
        .unwrap();
    let mut req = req;
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 2], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn settings_endpoints() {
    let f = fixture();
    // Without a session, every settings route is a 401.
    for req in [
        get("/api/v1/settings", None),
        get("/api/v1/settings/crawler", None),
        patch("/api/v1/settings/crawler", None, r#"{"intensity":"low"}"#),
    ] {
        let (status, body) = send(&f, req).await;
        assert_eq!(status, 401, "{body}");
    }
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());

    let index = golden(&f, "settings_index", get("/api/v1/settings", c)).await;
    assert_eq!(index["sections"].as_array().unwrap().len(), 9);
    let crawler = golden(&f, "settings_crawler", get("/api/v1/settings/crawler", c)).await;
    assert_eq!(crawler["fields"]["timeout_secs"]["source"], json!("file"));
    let network = golden(&f, "settings_network", get("/api/v1/settings/network", c)).await;
    assert_eq!(
        network["fields"]["dht_port"],
        json!({ "env": "LODESTAR_DHT_PORT", "read_only": true, "restart_required": true, "source": "env", "value": 7000 })
    );
    golden(&f, "settings_general", get("/api/v1/settings/general", c)).await;

    // A partial patch: the live field and a restart-only one.
    let out = golden(
        &f,
        "settings_patch_crawler",
        patch(
            "/api/v1/settings/crawler",
            c,
            r#"{"intensity":"low","timeout_secs":12}"#,
        ),
    )
    .await;
    assert_eq!(out["restart_required"], json!(["timeout_secs"]));
    // And it stays.
    let again = golden(
        &f,
        "settings_crawler_after",
        get("/api/v1/settings/crawler", c),
    )
    .await;
    assert_eq!(again, out);
    golden(
        &f,
        "settings_patch_network",
        patch(
            "/api/v1/settings/network",
            c,
            r#"{"http_bind":"127.0.0.1:9000"}"#,
        ),
    )
    .await;
    golden(
        &f,
        "settings_patch_empty",
        patch("/api/v1/settings/storage", c, "{}"),
    )
    .await;

    // Refusals, each in the envelope with its field.
    let cases: &[(&str, &str, &str, u16, &str)] = &[
        (
            "settings_unknown_section",
            "GET",
            "/api/v1/settings/filters",
            404,
            "",
        ),
        (
            "settings_patch_unknown_section",
            "PATCH",
            "/api/v1/settings/subscriptions",
            404,
            r#"{"catalog":[]}"#,
        ),
        (
            "settings_patch_unknown_field",
            "PATCH",
            "/api/v1/settings/crawler",
            400,
            r#"{"intensty":"low"}"#,
        ),
        (
            "settings_patch_set_by_env",
            "PATCH",
            "/api/v1/settings/network",
            409,
            r#"{"dht_port":7001}"#,
        ),
        (
            "settings_patch_read_only",
            "PATCH",
            "/api/v1/settings/general",
            409,
            r#"{"data_dir":"/elsewhere"}"#,
        ),
        (
            "settings_patch_out_of_range",
            "PATCH",
            "/api/v1/settings/crawler",
            422,
            r#"{"timeout_secs":0}"#,
        ),
        (
            "settings_patch_bad_enum",
            "PATCH",
            "/api/v1/settings/crawler",
            422,
            r#"{"intensity":"turbo"}"#,
        ),
        (
            "settings_patch_bad_address",
            "PATCH",
            "/api/v1/settings/network",
            422,
            r#"{"http_bind":"localhost:80"}"#,
        ),
        (
            "settings_patch_not_an_object",
            "PATCH",
            "/api/v1/settings/crawler",
            422,
            "[1]",
        ),
        (
            "settings_patch_bad_json",
            "PATCH",
            "/api/v1/settings/crawler",
            400,
            "{",
        ),
    ];
    for (name, method, uri, status, body) in cases {
        let req = if *method == "GET" {
            get(uri, c)
        } else {
            patch(uri, c, body)
        };
        let (got, text) = send(&f, req).await;
        assert_eq!(got, *status, "{name}: {text}");
        let req = if *method == "GET" {
            get(uri, c)
        } else {
            patch(uri, c, body)
        };
        golden(&f, name, req).await;
    }
    // Cross-site and non-JSON writes are refused before anything else.
    let mut req = patch(
        "/api/v1/settings/crawler",
        c,
        r#"{"intensity":"aggressive"}"#,
    );
    req.headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    req.headers_mut()
        .insert(header::HOST, "lodestar.local".parse().unwrap());
    golden(&f, "settings_patch_cross_origin", req).await;
    let req = Request::patch("/api/v1/settings/crawler")
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(r#"{"intensity":"aggressive"}"#))
        .unwrap();
    golden(&f, "settings_patch_not_json", req).await;
    // None of the refusals changed anything.
    let after = golden(
        &f,
        "settings_crawler_unchanged",
        get("/api/v1/settings/crawler", c),
    )
    .await;
    assert_eq!(after, out);
    // `null` resets: the file's timeout and the default intensity again.
    let reset = golden(
        &f,
        "settings_patch_reset",
        patch(
            "/api/v1/settings/crawler",
            c,
            r#"{"intensity":null,"timeout_secs":null}"#,
        ),
    )
    .await;
    assert_eq!(reset["fields"]["timeout_secs"]["source"], json!("file"));
    assert_eq!(reset["fields"]["intensity"]["source"], json!("default"));
    assert_eq!(reset["restart_required"], json!([]));
    // A cookie setting saved over plain HTTP would lock everyone out.
    golden(
        &f,
        "settings_patch_secure_cookies",
        patch("/api/v1/settings/network", c, r#"{"secure_cookies":true}"#),
    )
    .await;
}

#[tokio::test]
async fn showing_adult_content_needs_the_18_plus_confirmation() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let uri = "/api/v1/settings/content";
    let before = golden(&f, "settings_content", get(uri, c)).await;
    assert_eq!(before["fields"]["show_adult"]["value"], json!(false));
    assert_eq!(
        before["fields"]["show_adult"]["restart_required"],
        json!(false)
    );
    // Without the confirmation (or with anything but `true`), refused.
    for body in [
        r#"{"show_adult":true}"#,
        r#"{"show_adult":true,"confirm_adult":false}"#,
        r#"{"show_adult":true,"confirm_adult":1}"#,
    ] {
        let (status, text) = send(&f, patch(uri, c, body)).await;
        assert_eq!(status, 422, "{body}: {text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["error"]["code"], json!("adult_confirmation_required"));
        assert_eq!(v["error"]["field"], json!("show_adult"));
    }
    golden(
        &f,
        "settings_patch_adult_unconfirmed",
        patch(uri, c, r#"{"show_adult":true}"#),
    )
    .await;
    // Nothing was saved.
    let (_, text) = send(&f, get(uri, c)).await;
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), before);
    // Confirmed, it's on, live; the confirmation isn't saved.
    let on = golden(
        &f,
        "settings_patch_adult_confirmed",
        patch(uri, c, r#"{"show_adult":true,"confirm_adult":true}"#),
    )
    .await;
    assert_eq!(on["fields"]["show_adult"]["value"], json!(true));
    assert_eq!(on["fields"]["show_adult"]["source"], json!("db"));
    assert_eq!(on["restart_required"], json!([]));
    assert!(on["fields"].get("confirm_adult").is_none());
    // Turning it off needs nothing.
    let off = golden(
        &f,
        "settings_patch_adult_off",
        patch(uri, c, r#"{"show_adult":false}"#),
    )
    .await;
    assert_eq!(off["fields"]["show_adult"]["value"], json!(false));
}

#[tokio::test]
async fn every_settings_route_needs_a_session_and_a_same_origin_json_write() {
    let f = fixture();
    let writes = [
        (
            "PATCH",
            "/api/v1/settings",
            r#"{"crawler":{"intensity":"low"}}"#,
        ),
        (
            "PATCH",
            "/api/v1/settings/crawler",
            r#"{"intensity":"low"}"#,
        ),
        (
            "POST",
            "/api/v1/settings/preview",
            r#"{"crawler":{"intensity":"low"}}"#,
        ),
        (
            "POST",
            "/api/v1/settings/import",
            r#"{"toml":"[crawler]\nintensity = \"low\"\n"}"#,
        ),
        (
            "POST",
            "/api/v1/settings/test",
            r#"{"kind":"download_client","client":"qbittorrent","url":"http://127.0.0.1:1/"}"#,
        ),
    ];
    // 401 without a session, reads and writes alike.
    for uri in ["/api/v1/settings", "/api/v1/settings/export"] {
        let (status, body) = send(&f, get(uri, None)).await;
        assert_eq!(status, 401, "{uri}: {body}");
    }
    for (method, uri, body) in writes {
        let (status, text) = send(&f, write(method, uri, None, body)).await;
        assert_eq!(status, 401, "{method} {uri}: {text}");
    }
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    for (method, uri, body) in writes {
        // 403 cross-site.
        let mut req = write(method, uri, c, body);
        req.headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        req.headers_mut()
            .insert(header::HOST, "lodestar.local".parse().unwrap());
        let (status, text) = send(&f, req).await;
        assert_eq!(status, 403, "{method} {uri}: {text}");
        // 415 unless it's JSON.
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(body))
            .unwrap();
        let (status, text) = send(&f, req).await;
        assert_eq!(status, 415, "{method} {uri}: {text}");
    }
    // None of it saved anything.
    let (_, text) = send(&f, get("/api/v1/settings/crawler", c)).await;
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (
            v["revision"].clone(),
            v["fields"]["intensity"]["value"].clone()
        ),
        (json!(0), json!("balanced"))
    );
    golden(&f, "settings_import_cross_origin", {
        let mut req = post("/api/v1/settings/import", c, r#"{"toml":""}"#);
        req.headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        req.headers_mut()
            .insert(header::HOST, "lodestar.local".parse().unwrap());
        req
    })
    .await;
}

#[tokio::test]
async fn batch_patch_revisions_and_preview() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let saved = golden(
        &f,
        "settings_batch",
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{"if_revision":0,"intensity":"low"},"transport":{"if_revision":0,"utp":true}}"#,
        ),
    )
    .await;
    assert_eq!(saved["sections"]["crawler"]["revision"], json!(1));
    assert_eq!(
        saved["sections"]["transport"]["restart_required"],
        json!(["utp"])
    );
    // The same revisions again: refused, with the current values.
    let stale = golden(
        &f,
        "settings_batch_stale",
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{"if_revision":0,"intensity":"aggressive"},"transport":{"if_revision":1,"utp":false}}"#,
        ),
    )
    .await;
    assert_eq!(stale["error"]["code"], json!("stale_revision"));
    assert_eq!(
        stale["current"]["crawler"]["fields"]["intensity"]["value"],
        json!("low")
    );
    assert!(stale["current"].get("transport").is_none());
    // One bad field, in another section: nothing saved, section and field
    // named.
    let bad = golden(
        &f,
        "settings_batch_invalid",
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{"intensity":"aggressive"},"storage":{"mmap_mib":-1}}"#,
        ),
    )
    .await;
    assert_eq!(
        (&bad["error"]["section"], &bad["error"]["field"]),
        (&json!("storage"), &json!("mmap_mib"))
    );
    golden(
        &f,
        "settings_batch_adult_unconfirmed",
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{"intensity":"aggressive"},"content":{"show_adult":true}}"#,
        ),
    )
    .await;
    golden(
        &f,
        "settings_batch_unknown_section",
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{},"filters":{"rules":[]}}"#,
        ),
    )
    .await;
    golden(
        &f,
        "settings_batch_not_sections",
        patch("/api/v1/settings", c, r#"{"crawler":"low"}"#),
    )
    .await;
    let (_, text) = send(&f, get("/api/v1/settings/crawler", c)).await;
    let crawler: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (
            crawler["revision"].clone(),
            crawler["fields"]["intensity"]["value"].clone()
        ),
        (json!(1), json!("low"))
    );
    // The single-section route takes `if_revision` too.
    golden(
        &f,
        "settings_patch_stale",
        patch(
            "/api/v1/settings/crawler",
            c,
            r#"{"if_revision":0,"timeout_secs":20}"#,
        ),
    )
    .await;
    // The preview: what it would do, saving nothing.
    let preview = golden(
        &f,
        "settings_preview",
        post(
            "/api/v1/settings/preview",
            c,
            r#"{"crawler":{"if_revision":1,"intensity":"aggressive","max_sockets":0},"network":{"http_bind":"192.168.1.5:8191"},"search":{"enabled":false}}"#,
        ),
    )
    .await;
    assert_eq!(
        preview["sections"]["crawler"]["fields"]["max_sockets"]["estimates"],
        json!([{ "name": "sockets", "value": 1000 }])
    );
    golden(
        &f,
        "settings_preview_invalid",
        post(
            "/api/v1/settings/preview",
            c,
            r#"{"network":{"http_bind":"0.0.0.0:0"}}"#,
        ),
    )
    .await;
    let (_, after) = send(&f, get("/api/v1/settings/crawler", c)).await;
    assert_eq!(serde_json::from_str::<Value>(&after).unwrap(), crawler);
    golden(
        &f,
        "settings_preferences",
        get("/api/v1/settings/preferences", c),
    )
    .await;
}

#[tokio::test]
async fn export_and_import() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    send(
        &f,
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{"intensity":"low"},"preferences":{"theme":"nord"}}"#,
        ),
    )
    .await;
    let mut req = f.app.clone().oneshot(get("/api/v1/settings/export", c));
    let resp = std::future::poll_fn(|cx| Pin::new(&mut req).poll(cx))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE],
        "application/toml; charset=utf-8"
    );
    assert_eq!(
        resp.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"lodestar-settings.toml\""
    );
    golden(&f, "settings_export", get("/api/v1/settings/export", c)).await;
    let (_, with) = send(&f, get("/api/v1/settings/export?preferences=true", c)).await;
    assert!(with.contains("[preferences]\ntheme = \"nord\""), "{with}");
    golden(
        &f,
        "settings_export_bad_query",
        get("/api/v1/settings/export?preferences=maybe", c),
    )
    .await;
    // A dry run reports; the import saves.
    let file = "lodestar_settings = 1\n[crawler]\nintensity = \"aggressive\"\nintensity2 = 1\n[network]\ndht_port = 7001\n";
    let body = json!({ "toml": file }).to_string();
    let dry = golden(
        &f,
        "settings_import_dry_run",
        post("/api/v1/settings/import?dry_run=true", c, &body),
    )
    .await;
    assert_eq!(dry["applied"], json!(false));
    let (_, text) = send(&f, get("/api/v1/settings/crawler", c)).await;
    assert!(text.contains("\"value\":\"low\""), "{text}");
    let revisions = dry["revisions"].clone();
    let applied = golden(
        &f,
        "settings_import",
        post(
            "/api/v1/settings/import",
            c,
            &json!({ "toml": file, "revisions": revisions }).to_string(),
        ),
    )
    .await;
    assert_eq!(applied["applied"], json!(true));
    // Refusals.
    golden(
        &f,
        "settings_import_not_toml",
        post(
            "/api/v1/settings/import",
            c,
            r#"{"toml":"[crawler\nintensity = 1\n"}"#,
        ),
    )
    .await;
    golden(
        &f,
        "settings_import_stale",
        post(
            "/api/v1/settings/import",
            c,
            &json!({ "toml": file, "revisions": { "crawler": 0 } }).to_string(),
        ),
    )
    .await;
    golden(
        &f,
        "settings_import_invalid_value",
        post(
            "/api/v1/settings/import",
            c,
            &json!({ "toml": "[crawler]\ntimeout_secs = 0\n" }).to_string(),
        ),
    )
    .await;
    let big = json!({ "toml": "#".repeat(64 * 1024 + 1) }).to_string();
    golden(
        &f,
        "settings_import_too_large",
        post("/api/v1/settings/import", c, &big),
    )
    .await;
}

/// Reads `body` until `needle` appears, or fails after 5 s.
async fn read_until(body: &mut Body, needle: &str) -> String {
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !text.contains(needle) {
        let frame = tokio::time::timeout_at(
            deadline,
            std::future::poll_fn(|cx| Pin::new(&mut *body).poll_frame(cx)),
        )
        .await
        .unwrap_or_else(|_| panic!("no {needle:?} in {text:?}"))
        .expect("the stream ended")
        .unwrap();
        if let Ok(data) = frame.into_data() {
            text.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
    text
}

#[tokio::test]
async fn every_save_publishes_a_settings_event_without_values() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let stream = f
        .app
        .clone()
        .oneshot(get("/api/v1/events?topics=settings", c))
        .await
        .unwrap();
    assert_eq!(stream.status(), 200);
    let mut body = stream.into_body();
    read_until(&mut body, "retry:").await;
    // A batch: one event per changed section.
    send(
        &f,
        patch(
            "/api/v1/settings",
            c,
            r#"{"crawler":{"intensity":"low"},"transport":{"utp":true},"storage":{}}"#,
        ),
    )
    .await;
    let text = read_until(&mut body, "\"section\":\"transport\"").await;
    let id = f.hub.epoch();
    assert!(
        text.contains(&format!(
            "id: {id}-1\nevent: settings\ndata: {{\"revision\":1,\"section\":\"crawler\"}}\n\n"
        )),
        "{text}"
    );
    assert!(
        text.contains("data: {\"revision\":1,\"section\":\"transport\"}"),
        "{text}"
    );
    assert!(!text.contains("storage") && !text.contains("low"), "{text}");
    // A single-section patch and an import publish too; a refused save
    // and a save that changes nothing don't.
    send(
        &f,
        patch("/api/v1/settings/crawler", c, r#"{"timeout_secs":0}"#),
    )
    .await;
    send(
        &f,
        patch("/api/v1/settings/crawler", c, r#"{"intensity":"low"}"#),
    )
    .await;
    send(
        &f,
        patch(
            "/api/v1/settings/crawler",
            c,
            r#"{"intensity":"aggressive"}"#,
        ),
    )
    .await;
    let text = read_until(&mut body, "\"revision\":2").await;
    assert!(text.contains(&format!("id: {id}-3\n")), "{text}");
    send(
        &f,
        post(
            "/api/v1/settings/import",
            c,
            &json!({ "toml": "[preferences]\ntheme = \"nord\"\n" }).to_string(),
        ),
    )
    .await;
    let text = read_until(&mut body, "\"section\":\"preferences\"").await;
    assert!(text.contains(&format!("id: {id}-4\n")), "{text}");
    // A dry run and a preview don't.
    send(
        &f,
        post(
            "/api/v1/settings/import?dry_run=true",
            c,
            &json!({ "toml": "[preferences]\ntheme = \"ayu\"\n" }).to_string(),
        ),
    )
    .await;
    send(
        &f,
        post(
            "/api/v1/settings/preview",
            c,
            r#"{"crawler":{"intensity":"low"}}"#,
        ),
    )
    .await;
    f.hub.settings_saved("marker", 0);
    let text = read_until(&mut body, "marker").await;
    assert!(text.contains(&format!("id: {id}-5\n")), "{text}");
}

// The lock is held across awaits on purpose: the save waits on it in its
// blocking task, on another thread.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn a_save_still_publishes_when_its_client_goes_away() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let stream = f
        .app
        .clone()
        .oneshot(get("/api/v1/events?topics=settings", c))
        .await
        .unwrap();
    let mut body = stream.into_body();
    read_until(&mut body, "retry:").await;
    // Hold the registry's lock so the save waits inside its blocking task,
    // then drop the request, as a client closing the tab does.
    let held = f.registry.write.lock().unwrap();
    let request = tokio::spawn(f.app.clone().oneshot(patch(
        "/api/v1/settings/crawler",
        c,
        r#"{"intensity":"low"}"#,
    )));
    tokio::time::sleep(Duration::from_millis(200)).await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    drop(held);
    // The save went through, and its event with it.
    read_until(&mut body, "data: {\"revision\":1,\"section\":\"crawler\"}").await;
    let (_, text) = send(&f, get("/api/v1/settings/crawler", c)).await;
    assert!(
        text.contains("\"source\":\"db\",\"value\":\"low\""),
        "{text}"
    );
}

#[test]
fn a_connection_test_logs_no_address_or_password() {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    // One thread, so every event of the request reaches this subscriber.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(async {
            let f = fixture();
            let cookie = session(&f).await;
            // Something that answers, but not as qBittorrent, and nothing.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let open = listener.local_addr().unwrap();
            tokio::spawn(axum::serve(listener, Router::new()).into_future());
            let closed = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap();
            // Credentials in the URL are refused before any request; the
            // others reach the network.
            for url in [
                format!("http://{open}/lspath-marker/"),
                format!("http://{closed}/lspath-marker/"),
                format!("http://lsuser:lspath-secret@{open}/lspath-marker/"),
            ] {
                let body = json!({
                    "kind": "download_client",
                    "client": "qbittorrent",
                    "url": url,
                    "username": "lsuser-marker",
                    "password": "lspass-hunter2",
                })
                .to_string();
                let (status, text) =
                    send(&f, post("/api/v1/settings/test", Some(&cookie), &body)).await;
                assert_eq!(status, 200, "{text}");
                assert!(text.contains("\"ok\":false"), "{text}");
            }
        });
    });
    let logged = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(!logged.is_empty(), "nothing was captured");
    for secret in ["lspath", "lsuser", "lspass", "hunter2"] {
        assert!(!logged.contains(secret), "{secret} in:\n{logged}");
    }
}
