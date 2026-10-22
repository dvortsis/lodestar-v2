//! `/stats`, `/crawler`, `/sources` and `/network/check` through the
//! router, over fakes of the settings registry and the running process:
//! the session guard, the same-origin check, the error envelope, and
//! each response's status and bytes against `golden/ops_<name>.txt`.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-api --test ops` rewrites the
//! files; read the diff before committing it.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::ops::{
    BoxFuture, ConsentClass, CrawlerIntensity, CrawlerRuntime, DhtStatus, ExternalAddress,
    FetchStatus, NetworkCheck, OpsApi, OpsError, PortMapping, ReachReason, RiskLevel, Source,
    SourceHealth, SourceKind, SourcePatch, SourcePriority, SourceState, StatsResponse,
};
use lodestar_api::settings::{
    SectionValues, SettingSource, SettingValue, SettingsApi, SettingsError, SettingsIndex,
    SettingsPatch,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The registry's crawler section, as far as these routes read it.
#[derive(Debug)]
struct FakeSettings {
    values: Mutex<BTreeMap<String, Value>>,
    intensity_env: Option<String>,
}

impl SettingsApi for FakeSettings {
    fn index(&self) -> SettingsIndex {
        SettingsIndex {
            sections: Vec::new(),
        }
    }

    fn section(&self, section: &str) -> Option<SectionValues> {
        (section == "crawler").then(|| SectionValues {
            fields: self
                .values
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| {
                    let env = (k == "intensity")
                        .then(|| self.intensity_env.clone())
                        .flatten();
                    let value = SettingValue {
                        read_only: env.is_some(),
                        env,
                        restart_required: false,
                        source: SettingSource::Default,
                        value: v.clone(),
                    };
                    (k.clone(), value)
                })
                .collect(),
            restart_required: Vec::new(),
            section: "crawler".into(),
        })
    }

    fn patch(&self, section: &str, patch: SettingsPatch) -> Result<SectionValues, SettingsError> {
        if section != "crawler" {
            return Err(SettingsError::UnknownSection);
        }
        for (k, v) in patch.0 {
            if k == "intensity"
                && let Some(var) = &self.intensity_env
            {
                return Err(SettingsError::SetByEnv {
                    field: k,
                    var: var.clone(),
                });
            }
            self.values.lock().unwrap().insert(k, v);
        }
        Ok(self.section("crawler").unwrap())
    }
}

#[derive(Debug, Default)]
struct FakeOps {
    paused: Mutex<bool>,
    calls: Mutex<Vec<String>>,
    sources: Mutex<Vec<Source>>,
    restarted: Mutex<bool>,
}

fn dht_source() -> Source {
    Source {
        consent: ConsentClass::None,
        consent_required: false,
        editable: vec!["enabled".into(), "priority".into()],
        enabled: true,
        health: SourceHealth {
            harvested_24h: 1200,
            last_error: None,
            last_run: Some(1_700_000_000),
            running: true,
            stored_24h: 31,
        },
        id: 0,
        kinds: vec![SourceKind::Discovery, SourceKind::PeerFinder],
        name: "Mainline DHT".into(),
        not_yet_editable: ["agents", "egress", "purge", "schedule"]
            .map(String::from)
            .to_vec(),
        plugin: "dht".into(),
        priority: SourcePriority::Normal,
        restart_required: false,
        risk: RiskLevel::Low,
        state: SourceState::Enabled,
        version: "0.0.0".into(),
    }
}

fn restricted_source() -> Source {
    Source {
        consent: ConsentClass::Restricted,
        consent_required: true,
        enabled: false,
        health: SourceHealth {
            harvested_24h: 0,
            last_error: None,
            last_run: None,
            running: false,
            stored_24h: 0,
        },
        id: 1,
        kinds: vec![SourceKind::Importer],
        name: "Shadow library".into(),
        plugin: "shadow".into(),
        priority: SourcePriority::Low,
        risk: RiskLevel::High,
        state: SourceState::Disabled,
        ..dht_source()
    }
}

impl OpsApi for FakeOps {
    fn crawler(&self) -> CrawlerRuntime {
        CrawlerRuntime {
            since: 1_700_000_000,
            dht: Some(DhtStatus {
                bootstrapped: true,
                outstanding: 12,
                routing_nodes: 640,
            }),
            fetch: FetchStatus {
                in_flight: 3,
                lookups: 7,
                queued: 90,
            },
        }
    }

    fn apply_paused(&self, paused: bool) -> BoxFuture<'_, Result<(), OpsError>> {
        Box::pin(async move {
            *self.paused.lock().unwrap() = paused;
            self.calls.lock().unwrap().push(format!("paused={paused}"));
            Ok(())
        })
    }

    fn note_intensity(&self, i: CrawlerIntensity) -> BoxFuture<'_, Result<(), OpsError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("intensity={i:?}"));
            Ok(())
        })
    }

    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        Box::pin(async move {
            StatsResponse {
                data_bytes: Some(1 << 30),
                disk_free_bytes: None,
                fetched_per_hour: Some(1260.0),
                harvested_per_hour: None,
                index_lag_s: Some(0),
                paused: *self.paused.lock().unwrap(),
                queue_depth: 93,
                searchable_torrents: None,
                stored_torrents: 2_410_391,
                updated_at: 1_700_000_100,
                uptime_s: 100,
            }
        })
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async move { Ok(self.sources.lock().unwrap().clone()) })
    }

    fn patch_source(&self, id: u8, patch: SourcePatch) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            let mut sources = self.sources.lock().unwrap();
            let s = sources
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or(OpsError::UnknownSource)?;
            if patch.enabled == Some(true)
                && !s.enabled
                && s.consent_required
                && patch.consent != Some(s.consent)
            {
                return Err(OpsError::ConsentRequired(s.consent));
            }
            if let Some(on) = patch.enabled {
                s.enabled = on;
                s.state = if on {
                    SourceState::Enabled
                } else {
                    SourceState::Disabled
                };
                s.restart_required = on != s.health.running;
            }
            if let Some(p) = patch.priority {
                s.priority = p;
            }
            Ok(s.clone())
        })
    }

    fn network(&self) -> NetworkCheck {
        NetworkCheck {
            checked_at: 1_700_000_100,
            external: ExternalAddress {
                agreeing: 6,
                ip: Some("203.0.113.9".into()),
                port: Some(6881),
                voters: 7,
            },
            inbound_queries: Some(40),
            mapping: PortMapping::Unavailable,
            measured_s: 130,
            min_window_s: 120,
            reachable: Some(true),
            reason: ReachReason::UnsolicitedQueries,
            unsolicited_queries: Some(3),
            window_s: 600,
            window_started_at: Some(1_699_999_970),
        }
    }

    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        let mut r = self.restarted.lock().unwrap();
        if *r {
            return Err(OpsError::RateLimited {
                retry_after_secs: 30,
            });
        }
        *r = true;
        Ok(NetworkCheck {
            inbound_queries: Some(0),
            measured_s: 0,
            reachable: None,
            reason: ReachReason::Measuring,
            unsolicited_queries: Some(0),
            window_started_at: Some(1_700_000_100),
            ..self.network()
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    ops: Arc<FakeOps>,
    _dir: tempfile::TempDir,
}

fn fixture_with(ops: bool, intensity_env: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let fake = Arc::new(FakeOps::default());
    *fake.sources.lock().unwrap() = vec![dht_source(), restricted_source()];
    let settings = FakeSettings {
        values: Mutex::new(
            [
                ("intensity".to_string(), json!("balanced")),
                ("paused".to_string(), json!(false)),
            ]
            .into(),
        ),
        intensity_env: intensity_env.map(String::from),
    };
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: Some(Arc::new(settings)),
            ops: ops.then(|| fake.clone() as Arc<dyn OpsApi>),
        }),
    );
    Fixture {
        app,
        auth,
        ops: fake,
        _dir: dir,
    }
}

fn fixture() -> Fixture {
    fixture_with(true, None)
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

/// Sends `req` and checks the status and body against
/// `golden/ops_{name}.txt`.
async fn golden(f: &Fixture, name: &str, req: Request<Body>) -> Value {
    let (status, body) = send(f, req).await;
    let got = format!("{status}\n{body}\n");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("ops_{name}.txt"));
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

fn req(method: &str, uri: &str, cookie: Option<&str>, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
}

/// Every route this module serves, with a body for the writes.
const ROUTES: [(&str, &str, Option<&str>); 9] = [
    ("GET", "/api/v1/stats", None),
    ("GET", "/api/v1/crawler", None),
    ("PATCH", "/api/v1/crawler", Some(r#"{"intensity":"low"}"#)),
    ("POST", "/api/v1/crawler/pause", Some("{}")),
    ("POST", "/api/v1/crawler/resume", Some("{}")),
    ("GET", "/api/v1/sources", None),
    ("PATCH", "/api/v1/sources/0", Some(r#"{"priority":"high"}"#)),
    ("GET", "/api/v1/network/check", None),
    ("POST", "/api/v1/network/check", Some("{}")),
];

#[tokio::test]
async fn every_route_needs_a_session() {
    let f = fixture();
    for (method, uri, body) in ROUTES {
        let (status, text) = send(&f, req(method, uri, None, body)).await;
        assert_eq!(status, 401, "{method} {uri}: {text}");
        assert!(text.contains("login_required"), "{method} {uri}: {text}");
    }
    assert!(f.ops.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn writes_are_same_origin_json() {
    let f = fixture();
    let cookie = session(&f).await;
    for (method, uri, body) in ROUTES.iter().filter(|r| r.2.is_some()) {
        let mut cross = req(method, uri, Some(&cookie), *body);
        cross
            .headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let (status, text) = send(&f, cross).await;
        assert_eq!(status, 403, "{method} {uri}: {text}");
        assert!(text.contains("cross_origin"));
        // A form post (no JSON content type) is refused too.
        let form = Request::builder()
            .method(*method)
            .uri(*uri)
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("a=1"))
            .unwrap();
        assert_eq!(send(&f, form).await.0, 415, "{method} {uri}");
    }
    assert!(
        f.ops.calls.lock().unwrap().is_empty(),
        "nothing was applied"
    );
    assert!(!*f.ops.paused.lock().unwrap());
}

#[tokio::test]
async fn crawler_controls() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let running = golden(&f, "crawler", req("GET", "/api/v1/crawler", c, None)).await;
    assert_eq!(running["state"], json!("running"));
    let paused = golden(
        &f,
        "crawler_pause",
        req("POST", "/api/v1/crawler/pause", c, Some("{}")),
    )
    .await;
    assert_eq!(
        (paused["paused"].clone(), paused["state"].clone()),
        (json!(true), json!("paused"))
    );
    // Again: the same answer.
    let again = golden(
        &f,
        "crawler_pause",
        req("POST", "/api/v1/crawler/pause", c, Some("{}")),
    )
    .await;
    assert_eq!(again, paused);
    let stats = golden(&f, "stats", req("GET", "/api/v1/stats", c, None)).await;
    assert_eq!(stats["paused"], json!(true));
    golden(
        &f,
        "crawler_resume",
        req("POST", "/api/v1/crawler/resume", c, Some("{}")),
    )
    .await;
    let low = golden(
        &f,
        "crawler_intensity",
        req(
            "PATCH",
            "/api/v1/crawler",
            c,
            Some(r#"{"intensity":"low"}"#),
        ),
    )
    .await;
    assert_eq!(low["intensity"], json!("low"));
    golden(
        &f,
        "crawler_bad_intensity",
        req(
            "PATCH",
            "/api/v1/crawler",
            c,
            Some(r#"{"intensity":"turbo"}"#),
        ),
    )
    .await;
    golden(
        &f,
        "crawler_unknown_field",
        req("PATCH", "/api/v1/crawler", c, Some(r#"{"paused":true}"#)),
    )
    .await;
    assert_eq!(
        *f.ops.calls.lock().unwrap(),
        [
            "paused=true",
            "paused=true",
            "paused=false",
            "intensity=Low"
        ]
    );
}

#[tokio::test]
async fn an_intensity_the_environment_sets_is_refused() {
    let f = fixture_with(true, Some("LODESTAR_INTENSITY"));
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let status = golden(&f, "crawler_env", req("GET", "/api/v1/crawler", c, None)).await;
    assert_eq!(status["intensity_env"], json!("LODESTAR_INTENSITY"));
    let e = golden(
        &f,
        "crawler_intensity_env",
        req(
            "PATCH",
            "/api/v1/crawler",
            c,
            Some(r#"{"intensity":"low"}"#),
        ),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("set_by_environment"));
    assert!(f.ops.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sources() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let list = golden(&f, "sources", req("GET", "/api/v1/sources", c, None)).await;
    assert_eq!(list["sources"].as_array().unwrap().len(), 2);
    golden(
        &f,
        "source_priority",
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(r#"{"priority":"high"}"#),
        ),
    )
    .await;
    let e = golden(
        &f,
        "source_consent_required",
        req("PATCH", "/api/v1/sources/1", c, Some(r#"{"enabled":true}"#)),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("consent_required"));
    golden(
        &f,
        "source_consent_given",
        req(
            "PATCH",
            "/api/v1/sources/1",
            c,
            Some(r#"{"enabled":true,"consent":"restricted"}"#),
        ),
    )
    .await;
    golden(
        &f,
        "source_not_editable",
        req("PATCH", "/api/v1/sources/0", c, Some(r#"{"egress":"vpn"}"#)),
    )
    .await;
    golden(
        &f,
        "source_unknown",
        req(
            "PATCH",
            "/api/v1/sources/9",
            c,
            Some(r#"{"enabled":false}"#),
        ),
    )
    .await;
    golden(
        &f,
        "source_bad_id",
        req(
            "PATCH",
            "/api/v1/sources/x",
            c,
            Some(r#"{"enabled":false}"#),
        ),
    )
    .await;
    golden(
        &f,
        "source_unknown_field",
        req("PATCH", "/api/v1/sources/0", c, Some(r#"{"weight":7}"#)),
    )
    .await;
}

#[tokio::test]
async fn network_check() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    golden(&f, "network", req("GET", "/api/v1/network/check", c, None)).await;
    let restarted = golden(
        &f,
        "network_restart",
        req("POST", "/api/v1/network/check", c, Some("{}")),
    )
    .await;
    assert_eq!(restarted["reachable"], Value::Null);
    let e = golden(
        &f,
        "network_rate_limited",
        req("POST", "/api/v1/network/check", c, Some("{}")),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("rate_limited"));
}

#[tokio::test]
async fn without_a_running_crawler_the_routes_say_so() {
    let f = fixture_with(false, None);
    let cookie = session(&f).await;
    for (method, uri, body) in ROUTES {
        let (status, text) = send(&f, req(method, uri, Some(&cookie), body)).await;
        assert_eq!(status, 503, "{method} {uri}: {text}");
        assert!(text.contains("ops_unavailable"), "{text}");
    }
}
