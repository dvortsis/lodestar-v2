//! The `logging` section and `GET /logs/catalog` through the real router,
//! in the golden style of `api_tests.rs` (`golden/<name>.txt`): the
//! section, its refusals, the aliases (`log`, `LODESTAR_LOG`), the move of
//! an earlier build's `general.log_level`, and the catalogue.
//! `LODESTAR_BLESS=1 cargo test -p lodestar settings::logging_tests`
//! rewrites the files; read the diff before committing it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::settings::{SettingSource, SettingsApi, SettingsError};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::logging::{Logging, Verbosity, alias};
use super::tests::{config, open, patch, registry};
use super::{KEY_PREFIX, Registry};

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    registry: Arc<Registry>,
    dir: tempfile::TempDir,
}

fn fixture(file: Option<&str>, env: &[(&'static str, &'static str)]) -> Fixture {
    let _ = lodestar_log::register(crate::logging::CATALOGUES);
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path(), file, env);
    let db = dir.path().join("t.db");
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let registry = Arc::new(Registry::load(&c, store).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: Some(registry.clone()),
            ops: None,
            adult: Default::default(),
            user_data: None,
        }),
    );
    Fixture {
        app,
        auth,
        registry,
        dir,
    }
}

async fn send(f: &Fixture, mut req: Request<Body>) -> (u16, String) {
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// The response with what differs between runs replaced: the temporary
/// folder, the clock and the counters' rates.
fn stable(f: &Fixture, body: &str) -> String {
    let mut v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return body.to_string(),
    };
    fn walk(v: &mut Value) {
        match v {
            Value::Object(m) => {
                for (k, x) in m.iter_mut() {
                    match k.as_str() {
                        "now" => *x = json!(0),
                        "rate_per_hour" => *x = json!(0),
                        _ => walk(x),
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    let dir = f.dir.path().display().to_string();
    // One path separator on every platform (`data\\logs` on Windows is
    // JSON's escaped backslash).
    v.to_string().replace(&dir, "<data>").replace("\\\\", "/")
}

async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = send(f, req).await;
    let got = format!("{status}\n{}\n", stable(f, &body));
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/settings/golden")
        .join(format!("{name}.txt"));
    if std::env::var_os("LODESTAR_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
    } else {
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e} (bless with LODESTAR_BLESS=1)", path.display()));
        assert_eq!(
            got,
            want.replace("\r\n", "\n"),
            "{name}: the response bytes changed"
        );
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

fn write(method: &str, uri: &str, cookie: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let mut req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({ "token": token, "password": "correct horse battery" }).to_string(),
        ))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 2], 1))));
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

fn now() -> i64 {
    crate::logging::unix_now()
}

#[tokio::test]
async fn the_section_and_its_refusals() {
    let f = fixture(None, &[]);
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let values = golden(&f, "settings_logging", get("/api/v1/settings/logging", c)).await;
    assert_eq!(values["fields"]["verbosity"]["value"], json!("normal"));
    assert_eq!(values["fields"]["data"]["value"], json!("private"));
    assert_eq!(values["fields"]["libraries"]["value"], json!("warn"));
    assert_eq!(values["fields"]["file_size_mib"]["value"], json!(16));
    assert_eq!(values["fields"]["file_count"]["value"], json!(7));
    assert_eq!(values["fields"]["file_max_age_days"]["value"], json!(14));
    assert_eq!(values["fields"]["file_total_mib"]["value"], json!(256));
    assert!(
        values["fields"]["file_folder"]["restart_required"]
            .as_bool()
            .unwrap()
    );

    let refuse = |name: &'static str, body: Value| {
        let f = &f;
        let cookie = cookie.clone();
        async move {
            let req = write("PATCH", "/api/v1/settings/logging", &cookie, body);
            golden(f, name, req).await
        }
    };
    // A preset that identifies torrents needs its duration.
    let e = refuse("settings_logging_needs_duration", json!({ "data": "full" })).await;
    assert_eq!(e["error"]["field"], json!("data"));
    // An opt-in lasts 24 hours at most, and never "until turned off".
    let e = refuse(
        "settings_logging_opt_in_until_off",
        json!({ "peer_activity_until": -1 }),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("invalid_setting"));
    let (status, _) = send(
        &f,
        write(
            "PATCH",
            "/api/v1/settings/logging",
            &cookie,
            json!({ "change_details_until": now() + 25 * 3600 }),
        ),
    )
    .await;
    assert_eq!(status, 422);
    // A required event can't be turned off; the opt-in event follows its
    // opt-in; an unknown one is refused.
    let e = refuse(
        "settings_logging_required_event",
        json!({ "events": { "app.start": false } }),
    )
    .await;
    assert_eq!(e["error"]["field"], json!("events"));
    for events in [
        json!({ "filters.change_detail": true }),
        json!({ "no.such": true }),
    ] {
        let (status, body) = send(
            &f,
            write(
                "PATCH",
                "/api/v1/settings/logging",
                &cookie,
                json!({ "events": events }),
            ),
        )
        .await;
        assert_eq!(status, 422, "{body}");
    }
    // Lodestar's own targets aren't Libraries.
    let (status, _) = send(
        &f,
        write(
            "PATCH",
            "/api/v1/settings/logging",
            &cookie,
            json!({ "libraries": "warn,lodestar_dht=trace" }),
        ),
    )
    .await;
    assert_eq!(status, 422);

    // What the UI sends: Full for an hour, an opt-in for 24 hours, one
    // event turned on.
    let (status, body) = send(
        &f,
        write(
            "PATCH",
            "/api/v1/settings/logging",
            &cookie,
            json!({
                "data": "full",
                "data_until": now() + 3600,
                "peer_activity_until": now() + 24 * 3600,
                "events": { "logging.applied": true },
                "verbosity": "warnings",
            }),
        ),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let l = f.registry.get::<Logging>();
    assert_eq!(l.verbosity, Verbosity::Warnings);
    assert!(l.data_until > now());
}

#[tokio::test]
async fn the_preview_warns_and_estimates() {
    let f = fixture(None, &[]);
    let cookie = session(&f).await;
    let (status, body) = send(
        &f,
        write(
            "POST",
            "/api/v1/settings/preview",
            &cookie,
            json!({ "logging": {
                "data": "full",
                "data_until": now() + 3600,
                "peer_activity_until": now() + 3600,
            } }),
        ),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    let data = &v["sections"]["logging"]["fields"]["data"];
    let codes: Vec<&str> = data["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["identifying_kinds", "peer_activity"]);
    let names: Vec<&str> = data["estimates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "console_mib_per_day",
            "file_mib_per_day",
            "file_days_kept",
            "examples"
        ]
    );
    // Example lines come from sample data only, never a real torrent; the
    // file's carry the sample name now, the console's don't.
    let examples = data["estimates"][3]["value"].as_array().unwrap();
    assert!(!examples.is_empty());
    assert!(examples.iter().all(|e| {
        !e["console"]
            .as_str()
            .unwrap_or("")
            .contains("Example.Open.Source")
    }));
    // Nothing saved.
    assert_eq!(f.registry.get::<Logging>().data_until, 0);
    assert_eq!(f.registry.section("logging").unwrap().revision, 0);
}

#[tokio::test]
async fn the_catalog_lists_every_event_and_kind() {
    let f = fixture(None, &[]);
    let (status, body) = send(&f, get("/api/v1/logs/catalog", None)).await;
    assert_eq!(status, 401, "admin session only: {body}");
    let cookie = session(&f).await;
    let v = golden(
        &f,
        "logs_catalog",
        get("/api/v1/logs/catalog", Some(&cookie)),
    )
    .await;
    let events = v["events"].as_array().unwrap();
    let required: Vec<&str> = events
        .iter()
        .filter(|e| e["required"] == json!(true))
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    for id in [
        "app.start",
        "app.stop",
        "app.crash",
        "app.restart",
        "auth.setup_token",
        "settings.saved",
        "logging.kind_switched",
        "logging.disk_full",
        "logging.rotation_failed",
    ] {
        assert!(required.contains(&id), "{id} is required: {required:?}");
    }
    // Required events carry no torrent data.
    for e in events.iter().filter(|e| e["required"] == json!(true)) {
        for k in e["kinds"].as_array().unwrap() {
            assert!(
                ![
                    "hash",
                    "name",
                    "path",
                    "peer",
                    "peer_activity",
                    "filter_changes"
                ]
                .contains(&k.as_str().unwrap()),
                "{e}"
            );
        }
    }
    let detail = events
        .iter()
        .find(|e| e["id"] == "filters.change_detail")
        .unwrap();
    assert_eq!(detail["opt_in"], json!("filter_changes"));
    assert_eq!(detail["current"], json!(false));
    let kinds = v["kinds"].as_array().unwrap();
    assert_eq!(kinds.len(), 10);
    let on: Vec<&str> = kinds
        .iter()
        .filter(|k| k["outputs"]["file"] == json!(true))
        .map(|k| k["kind"].as_str().unwrap())
        .collect();
    assert_eq!(on, ["client", "size"], "Private by default");
}

#[test]
fn ids_are_unique_across_the_catalogues() {
    lodestar_log::check_ids(crate::logging::CATALOGUES).unwrap();
    for c in crate::logging::CATALOGUES {
        for e in c.events {
            assert!(!e.message.is_empty(), "{}", e.id);
            // Required events carry no torrent data.
            if e.required {
                assert!(
                    e.kinds().iter().all(|k| !k.identifying() && !k.opt_in()),
                    "{}",
                    e.id
                );
            }
        }
    }
}

#[test]
fn the_aliases_map_onto_the_section() {
    let a = alias("debug").unwrap();
    assert_eq!(a.verbosity, Verbosity::Detailed);
    assert!(a.components.is_empty());
    assert_eq!(a.libraries, "warn");
    let a =
        alias("warn,lodestar_dht=trace,lodestar_api::auth=debug,hyper=info,tantivy=error").unwrap();
    assert_eq!(a.verbosity, Verbosity::Warnings);
    assert_eq!(a.components["dht"], "trace");
    assert_eq!(a.components["auth"], "debug");
    assert_eq!(a.libraries, "hyper=info,tantivy=error");
    let a = alias("lodestar=debug").unwrap();
    assert_eq!(a.verbosity, Verbosity::Normal, "no default level: Normal");
    assert_eq!(a.components["system"], "debug");
    assert_eq!(
        alias("lodestar_nope=debug").unwrap().unknown,
        ["lodestar_nope"]
    );
    assert!(alias("info,=[").is_none());
    let a = alias("lodestar_search,hyper").unwrap();
    assert_eq!(
        a.components["search"], "trace",
        "a bare target is all its levels"
    );
    assert_eq!(a.libraries, "hyper");
}

#[test]
fn lodestar_log_and_the_file_key_set_the_three_fields() {
    let dir = tempfile::tempdir().unwrap();
    let field = |r: &Registry, name: &str| {
        let v = r.section("logging").unwrap();
        let f = &v.fields[name];
        (f.value.clone(), f.source, f.read_only)
    };
    // `LODESTAR_LOG=debug`: read-only, naming the variable.
    let r = registry(
        dir.path(),
        &config(dir.path(), None, &[("LODESTAR_LOG", "debug")]),
    );
    assert_eq!(
        field(&r, "verbosity"),
        (json!("detailed"), SettingSource::Env, true)
    );
    assert_eq!(
        field(&r, "libraries"),
        (json!("warn"), SettingSource::Env, true)
    );
    for body in [
        json!({ "verbosity": "normal" }),
        json!({ "components": {} }),
        json!({ "libraries": "warn" }),
    ] {
        assert!(
            matches!(
                r.patch("logging", patch(body)),
                Err(SettingsError::SetByEnv { ref var, .. }) if var == "LODESTAR_LOG"
            ),
            "set by the environment"
        );
    }
    // The other fields stay editable.
    r.patch("logging", patch(json!({ "console_format": "json" })))
        .unwrap();
    drop(r);
    // A filter string in `config.toml`.
    let dir = tempfile::tempdir().unwrap();
    let r = registry(
        dir.path(),
        &config(
            dir.path(),
            Some("log = \"error,lodestar_search=debug,h2=warn\"\n"),
            &[],
        ),
    );
    assert_eq!(
        field(&r, "verbosity"),
        (json!("errors"), SettingSource::File, false)
    );
    assert_eq!(
        field(&r, "components"),
        (json!({ "search": "debug" }), SettingSource::File, false)
    );
    assert_eq!(
        field(&r, "libraries"),
        (json!("h2=warn"), SettingSource::File, false)
    );
    assert_eq!(
        field(&r, "data"),
        (json!("private"), SettingSource::Default, false)
    );
}

fn saved(dir: &Path) -> Vec<(String, String)> {
    open(dir)
        .lock()
        .unwrap()
        .settings_with_prefix(KEY_PREFIX)
        .unwrap()
}

#[test]
fn a_saved_log_level_moves_to_the_logging_section_once() {
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[(
            "settings.general.log_level",
            "\"debug,lodestar_dht=trace,hyper=info\"",
        )])
        .unwrap();
    let c = config(dir.path(), None, &[]);
    let r = registry(dir.path(), &c);
    let l = r.get::<Logging>();
    assert_eq!(l.verbosity, Verbosity::Detailed);
    assert_eq!(l.components["dht"], "trace");
    assert_eq!(l.libraries, "hyper=info");
    let v = r.section("logging").unwrap();
    assert_eq!(v.fields["verbosity"].source, SettingSource::Db);
    drop(r);
    let rows = saved(dir.path());
    assert!(
        !rows.iter().any(|(k, _)| k == "settings.general.log_level"),
        "{rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|(k, _)| k.starts_with("settings.logging."))
            .count(),
        3,
        "{rows:?}"
    );
    // The next start has nothing to move, and keeps the moved values.
    let r = registry(dir.path(), &c);
    assert_eq!(r.get::<Logging>().verbosity, Verbosity::Detailed);
    drop(r);
    assert_eq!(saved(dir.path()), rows);

    // A value saved under the new key already wins over the old one.
    let dir = tempfile::tempdir().unwrap();
    open(dir.path())
        .lock()
        .unwrap()
        .set_settings_text(&[
            ("settings.general.log_level", "\"trace\""),
            ("settings.logging.verbosity", "\"errors\""),
        ])
        .unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    assert_eq!(r.get::<Logging>().verbosity, Verbosity::Errors);
    drop(r);
    assert!(
        !saved(dir.path())
            .iter()
            .any(|(k, _)| k.starts_with("settings.general."))
    );
}

#[test]
fn deadlines_are_checked_against_the_clock() {
    let dir = tempfile::tempdir().unwrap();
    let r = registry(dir.path(), &config(dir.path(), None, &[]));
    let t = now();
    for (body, ok) in [
        (json!({ "temporary_until": t + 1800 }), true),
        (json!({ "temporary_until": -1 }), false),
        (json!({ "temporary_until": t + 30 * 3600 }), false),
        (json!({ "kinds": { "name": t + 3600 } }), true),
        (json!({ "kinds": { "name": -1 } }), true),
        (json!({ "kinds": { "name": t + 48 * 3600 } }), false),
        (json!({ "kinds": { "size": t + 60 } }), false),
        (json!({ "kinds": { "peer_activity": t + 60 } }), false),
        (json!({ "kinds": { "nope": 0 } }), false),
        (json!({ "kinds": { "url": -2 } }), false),
        (json!({ "data": "hashes", "data_until": -1 }), true),
        (json!({ "change_details_until": t + 24 * 3600 }), true),
        (json!({ "components": { "libraries": "debug" } }), false),
        (json!({ "components": { "dht": "loud" } }), false),
        (json!({ "file_folder": " " }), false),
        (json!({ "libraries": "warn,lodestar_api::filters" }), false),
        (json!({ "libraries": "warn,hyper,h2=info" }), true),
        (json!({ "file_total_mib": 16, "file_size_mib": 32 }), false),
    ] {
        assert_eq!(
            r.patch("logging", patch(body.clone())).is_ok(),
            ok,
            "{body}"
        );
    }
}
