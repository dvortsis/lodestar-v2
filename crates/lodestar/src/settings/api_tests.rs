//! `/api/v1/settings` with the real registry behind the real router, in
//! the API's golden style (`crates/api/tests/golden.rs`): each response's
//! status and bytes against `golden/<name>.txt`.
//! `LODESTAR_BLESS=1 cargo test -p lodestar settings::api_tests` rewrites
//! the files; read the diff before committing it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::Registry;
use crate::config::Config;

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
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
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: Some(registry),
            ops: None,
            user_data: None,
            adult: Default::default(),
        }),
    );
    Fixture {
        app,
        auth,
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
    let mut b = Request::patch(uri).header(header::CONTENT_TYPE, "application/json");
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
    assert_eq!(index["sections"].as_array().unwrap().len(), 7);
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
