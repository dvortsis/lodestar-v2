//! `/system`, `/system/restart` and `/system/address/*` through the
//! router, over a fake of the running process: the session guard, the
//! same-origin check, the error envelope, the restart answered before it
//! happens, and the response bytes against `golden/system_<name>.txt`.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test system` rewrites
//! the files; read the diff before committing it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::{Extension, Router};
use lodestar_api::ops::BoxFuture;
use lodestar_api::settings::SettingSource;
use lodestar_api::system::{
    AddressChange, AddressOutcome, AddressState, BuildInfo, ChangedSetting, Channel, Connection,
    ConnectionKind, ConnectionState, DataFormats, DataInfo, Deployment, DeploymentKind,
    Diagnostics, HostInfo, KeepOrRevertKind, RestartInfo, RestartMethod, SystemApi, SystemError,
    SystemInfo,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

#[derive(Debug)]
struct FakeSystem {
    method: RestartMethod,
    restarts: AtomicU32,
    /// What keep and revert answer.
    address: Mutex<Option<AddressChange>>,
}

fn pending() -> AddressChange {
    AddressChange {
        deadline: Some(1_792_678_200),
        kind: KeepOrRevertKind::HttpBind,
        previous_url: "http://<this host's address>:8090/".into(),
        state: AddressState::AwaitingConfirmation,
        stored: true,
        url: "http://192.168.1.5:8191/".into(),
    }
}

fn info(method: RestartMethod, address: Option<AddressChange>) -> SystemInfo {
    let available = method == RestartMethod::Reexec;
    let none = |kind, settings: &str| Connection {
        count: None,
        kind,
        last_checked_at: None,
        settings: settings.into(),
        state: None,
    };
    SystemInfo {
        address_change: address,
        build: BuildInfo {
            commit: Some("2e64daaddf4df35b41908529eed3de8102b64e30".into()),
            date: Some(1_792_677_600),
            rustc: Some("rustc 1.94.0 (4a4ef493e 2026-03-02)".into()),
            target: "x86_64-unknown-linux-musl".into(),
            ui_build_id: None,
        },
        channel: Channel::Dev,
        connections: vec![
            Connection {
                count: None,
                kind: ConnectionKind::Dht,
                last_checked_at: None,
                settings: "network".into(),
                state: Some(ConnectionState::Paused),
            },
            Connection {
                count: Some(0),
                kind: ConnectionKind::Sources,
                last_checked_at: None,
                settings: "sources".into(),
                state: Some(ConnectionState::Off),
            },
            Connection {
                count: Some(2),
                kind: ConnectionKind::BlocklistSubscriptions,
                last_checked_at: Some(1_792_670_000),
                settings: "filters".into(),
                state: Some(ConnectionState::On),
            },
            none(ConnectionKind::Integrations, "integrations"),
            none(ConnectionKind::Updates, "updates"),
            none(ConnectionKind::Companions, "companions"),
            none(ConnectionKind::Workers, "workers"),
        ],
        data: DataInfo {
            cold_bytes: None,
            database_bytes: Some(6_442_450_944),
            dir: "/data".into(),
            filesystem_warning: None,
            free_bytes: None,
            index_bytes: Some(2_684_354_560),
            logs_bytes: None,
        },
        data_formats: DataFormats {
            index: "v3-t1".into(),
            rebuild: None,
            store_schema: Some(14),
        },
        deployment: Deployment {
            image_digest: None,
            image_tag: Some("0.5.2".into()),
            kind: DeploymentKind::Docker,
        },
        diagnostics: Diagnostics {
            changed: vec![ChangedSetting {
                name: "network.http_bind".into(),
                source: SettingSource::Db,
            }],
            logging: None,
            recent_errors: None,
        },
        fixture: false,
        host: HostInfo {
            arch: "x86_64".into(),
            cpus: Some(4),
            memory_bytes: None,
            os: "linux".into(),
            time_zone: None,
        },
        repository: "https://github.com/dvortsis/lodestar-v2".into(),
        restart: RestartInfo {
            available,
            estimated_seconds: 4,
            hint: (!available).then(|| "Restart the Lodestar service.".into()),
            method,
        },
        restart_required: vec!["network.http_bind".into()],
        started_at: 1_792_677_600,
        uptime_s: 3600,
        version: "0.0.0".into(),
    }
}

impl SystemApi for FakeSystem {
    fn info(&self) -> BoxFuture<'_, SystemInfo> {
        let address = self.address.lock().unwrap().clone();
        Box::pin(async move { info(self.method, address) })
    }

    fn restart_info(&self) -> RestartInfo {
        info(self.method, None).restart
    }

    fn restart(&self) {
        self.restarts.fetch_add(1, Ordering::SeqCst);
    }

    fn keep_address(&self) -> BoxFuture<'_, Result<AddressOutcome, SystemError>> {
        Box::pin(async move {
            let mut address = self.address.lock().unwrap();
            match &*address {
                Some(a) if a.state == AddressState::AwaitingConfirmation => {
                    *address = None;
                    Ok(AddressOutcome {
                        address_change: None,
                        restarting: false,
                    })
                }
                _ => Err(SystemError::NoAddressChange),
            }
        })
    }

    fn revert_address(&self) -> BoxFuture<'_, Result<AddressOutcome, SystemError>> {
        Box::pin(async move {
            let mut address = self.address.lock().unwrap();
            let Some(a) = address.take() else {
                return Err(SystemError::NoAddressChange);
            };
            let reverted = AddressChange {
                deadline: None,
                state: AddressState::Reverted,
                ..a
            };
            *address = Some(reverted.clone());
            Ok(AddressOutcome {
                address_change: Some(reverted),
                restarting: self.method == RestartMethod::Reexec,
            })
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    system: Option<Arc<FakeSystem>>,
    _dir: tempfile::TempDir,
}

fn fixture_with(method: Option<RestartMethod>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let api = router(ApiState {
        auth: auth.clone(),
        index: None,
        reader: Arc::new(Mutex::new(reader)),
        filters: None,
        settings: None,
        ops: None,
        user_data: None,
        insights: None,
        adult: Default::default(),
        hot: None,
        events: None,
    });
    let system = method.map(|method| {
        Arc::new(FakeSystem {
            method,
            restarts: AtomicU32::new(0),
            address: Mutex::new(None),
        })
    });
    // As `lodestar serve` layers it, beside the hub.
    let mut app = Router::new().nest("/api/v1", api);
    if let Some(s) = &system {
        app = app.layer(Extension(s.clone() as Arc<dyn SystemApi>));
    }
    Fixture {
        app,
        auth,
        system,
        _dir: dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(Some(RestartMethod::Reexec))
}

fn req(method: &str, uri: &str, cookie: Option<&str>, json: bool) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    if json {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = b.body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
    req
}

async fn send(f: &Fixture, req: Request<Body>) -> (u16, String) {
    let resp = f.app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = send(f, req).await;
    let got = format!("{status}\n{body}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("system_{name}.txt"));
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

fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let set = f
        .auth
        .setup([127, 0, 0, 1].into(), &token, "correct horse battery")
        .unwrap();
    set.split(';').next().unwrap().to_string()
}

fn restarts(f: &Fixture) -> u32 {
    f.system.as_ref().unwrap().restarts.load(Ordering::SeqCst)
}

#[tokio::test]
async fn every_route_needs_a_session() {
    let f = fixture();
    for (method, path) in [
        ("GET", "/api/v1/system"),
        ("POST", "/api/v1/system/restart"),
        ("POST", "/api/v1/system/address/keep"),
        ("POST", "/api/v1/system/address/revert"),
    ] {
        let (status, body) = send(&f, req(method, path, None, true)).await;
        assert_eq!(status, 401, "{method} {path}: {body}");
        // Session only: an API key or a proxy header alone isn't one.
        for (name, value) in [
            (
                "x-api-key",
                "lsk_AAAAAAAA_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
            ("remote-user", "admin"),
        ] {
            let mut r = req(method, path, None, true);
            r.headers_mut().insert(name, value.parse().unwrap());
            let (status, body) = send(&f, r).await;
            assert_eq!(status, 401, "{method} {path} with {name}: {body}");
        }
    }
    assert_eq!(restarts(&f), 0);
}

#[tokio::test]
async fn system_golden() {
    let f = fixture();
    let c = session(&f);
    let body = golden(&f, "get", req("GET", "/api/v1/system", Some(&c), false)).await;
    // Unmeasured values are null, never 0.
    assert_eq!(body["host"]["memory_bytes"], Value::Null);
    assert_eq!(body["data"]["free_bytes"], Value::Null);
    assert_eq!(body["connections"][3]["state"], Value::Null);
}

#[tokio::test]
async fn restart_answers_then_restarts() {
    let f = fixture();
    let c = session(&f);
    let resp = f
        .app
        .clone()
        .oneshot(req("POST", "/api/v1/system/restart", Some(&c), true))
        .await
        .unwrap();
    assert_eq!(resp.status(), 202);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
    // Not before the answer has gone out.
    assert_eq!(restarts(&f), 0);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(&bytes).unwrap(),
        r#"{"estimated_seconds":4,"restarting":true}"#
    );
    assert_eq!(restarts(&f), 1);
}

#[tokio::test]
async fn restart_where_it_cant_is_refused_with_the_hint() {
    let f = fixture_with(Some(RestartMethod::Manual));
    let c = session(&f);
    golden(
        &f,
        "restart_unavailable",
        req("POST", "/api/v1/system/restart", Some(&c), true),
    )
    .await;
    assert_eq!(restarts(&f), 0);
}

#[tokio::test]
async fn writes_are_same_origin_json() {
    let f = fixture();
    let c = session(&f);
    for path in [
        "/api/v1/system/restart",
        "/api/v1/system/address/keep",
        "/api/v1/system/address/revert",
    ] {
        let (status, _) = send(&f, req("POST", path, Some(&c), false)).await;
        assert_eq!(status, 415, "{path}");
        let mut cross = req("POST", path, Some(&c), true);
        cross
            .headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        cross
            .headers_mut()
            .insert(header::HOST, "lodestar.lan:8090".parse().unwrap());
        let (status, body) = send(&f, cross).await;
        assert_eq!(status, 403, "{path}: {body}");
    }
    assert_eq!(restarts(&f), 0);
}

#[tokio::test]
async fn keep_and_revert() {
    let f = fixture();
    let c = session(&f);
    golden(
        &f,
        "keep_nothing",
        req("POST", "/api/v1/system/address/keep", Some(&c), true),
    )
    .await;
    let system = f.system.clone().unwrap();
    *system.address.lock().unwrap() = Some(pending());
    let body = golden(&f, "pending", req("GET", "/api/v1/system", Some(&c), false)).await;
    assert_eq!(body["address_change"]["state"], "awaiting_confirmation");
    golden(
        &f,
        "keep",
        req("POST", "/api/v1/system/address/keep", Some(&c), true),
    )
    .await;
    assert_eq!(*system.address.lock().unwrap(), None);
    // A revert awaiting confirmation restarts once it has answered.
    *system.address.lock().unwrap() = Some(pending());
    golden(
        &f,
        "revert",
        req("POST", "/api/v1/system/address/revert", Some(&c), true),
    )
    .await;
    assert_eq!(restarts(&f), 1);
}

#[tokio::test]
async fn without_the_system_extension_it_is_unavailable() {
    let f = fixture_with(None);
    let c = session(&f);
    let (status, body) = send(&f, req("GET", "/api/v1/system", Some(&c), false)).await;
    assert_eq!(status, 503, "{body}");
    assert!(body.contains("system_unavailable"));
}

#[test]
fn the_contract_is_in_the_document() {
    let doc: Value = serde_json::from_str(&lodestar_api::openapi::json()).unwrap();
    let schema = &doc["components"]["schemas"]["SystemInfo"];
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for field in [
        "address_change",
        "build",
        "channel",
        "connections",
        "data",
        "data_formats",
        "deployment",
        "diagnostics",
        "fixture",
        "host",
        "repository",
        "restart",
        "restart_required",
        "started_at",
        "uptime_s",
        "version",
    ] {
        assert!(required.contains(&field), "{field}");
    }
    assert_eq!(
        doc["paths"]["/system/restart"]["post"]["responses"]["202"]["content"]["application/json"]
            ["schema"]["$ref"],
        json!("#/components/schemas/RestartAccepted")
    );
}
