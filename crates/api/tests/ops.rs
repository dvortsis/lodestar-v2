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
    BoxFuture, ConsentClass, CrawlerRuntime, DhtStatus, ExternalAddress, FetchStatus, NetworkCheck,
    OpsApi, OpsError, PortMapping, ReachReason, RiskLevel, SamplingBlocked, Source, SourceAgents,
    SourceEgress, SourceHealth, SourceKind, SourceNeed, SourcePatch, SourcePriority,
    SourceSchedule, SourceState, StatsResponse,
};
use lodestar_api::ops::{EgressCounts, EgressLive, EgressRoute, EgressSetting, EgressState};
use lodestar_api::settings::{
    Imported, Saved, SectionValues, SettingSource, SettingValue, SettingsApi, SettingsBatch,
    SettingsError, SettingsFailure, SettingsImport, SettingsIndex, SettingsPreview,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_store::unverified::{PurgeCursor, PurgeStep};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The registry's crawler section, as far as these routes read it.
#[derive(Debug)]
struct FakeSettings {
    values: Arc<Mutex<BTreeMap<String, Value>>>,
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
                        secrets_set: Vec::new(),
                        source: SettingSource::Default,
                        value: v.clone(),
                    };
                    (k.clone(), value)
                })
                .collect(),
            restart_required: Vec::new(),
            revision: 0,
            section: "crawler".into(),
        })
    }

    fn save(&self, batch: SettingsBatch) -> Result<Saved, SettingsFailure> {
        let mut sections = Vec::new();
        for (section, patch) in batch.0 {
            if section != "crawler" {
                return Err(SettingsFailure::new(section, SettingsError::UnknownSection));
            }
            for (k, v) in patch.0 {
                if k == "intensity"
                    && let Some(var) = &self.intensity_env
                {
                    return Err(SettingsFailure::new(
                        section,
                        SettingsError::SetByEnv {
                            field: k,
                            var: var.clone(),
                        },
                    ));
                }
                self.values.lock().unwrap().insert(k, v);
            }
            sections.push(self.section("crawler").unwrap());
        }
        Ok(Saved {
            sections,
            changed: Vec::new(),
        })
    }

    fn preview(&self, _: SettingsBatch) -> Result<SettingsPreview, SettingsFailure> {
        unreachable!("these routes don't preview")
    }

    fn export(&self, _: bool) -> String {
        unreachable!("these routes don't export")
    }

    fn import(&self, _: SettingsImport, _: bool) -> Result<Imported, SettingsFailure> {
        unreachable!("these routes don't import")
    }
}

#[derive(Debug, Default)]
struct FakeOps {
    /// The settings' values, which `sync_crawler` applies.
    values: Arc<Mutex<BTreeMap<String, Value>>>,
    paused: Mutex<bool>,
    calls: Mutex<Vec<String>>,
    sources: Mutex<Vec<Source>>,
    restarted: Mutex<bool>,
    /// Purge steps the test lets run, and those that ran.
    purge_permits: Mutex<u32>,
    purge_steps: Mutex<u32>,
    purge_wake: tokio::sync::Notify,
}

fn dht_source() -> Source {
    Source {
        agents: SourceAgents {
            count: 1,
            enforced: false,
            max: 4,
        },
        consent: ConsentClass::None,
        consent_required: false,
        consent_version: String::new(),
        credentials: Vec::new(),
        editable: ["agents", "egress", "enabled", "priority", "schedule"]
            .map(String::from)
            .to_vec(),
        egress: SourceEgress {
            live: Some(EgressLive {
                counts: EgressCounts::default(),
                notice: None,
                reason: None,
                state: EgressState::Direct,
            }),
            setting: Some(EgressSetting {
                route: EgressRoute::Direct,
                proxy: None,
                dns: None,
            }),
            warnings: Vec::new(),
        },
        enabled: true,
        endpoints: Vec::new(),
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
        needs: None,
        networks: vec!["bt".into()],
        not_yet_editable: vec!["purge".into()],
        password_required: false,
        plugin: "dht".into(),
        purge_pending: false,
        priority: SourcePriority::Normal,
        restart_required: false,
        risk: RiskLevel::Low,
        schedule: SourceSchedule {
            default: Vec::new(),
            enforced: false,
            windows: Vec::new(),
        },
        state: SourceState::Enabled,
        version: "0.0.0".into(),
    }
}

fn restricted_source() -> Source {
    Source {
        consent: ConsentClass::Restricted,
        consent_required: true,
        consent_version: "1-00000000000000ff".into(),
        enabled: false,
        endpoints: vec!["https://shadow.example".into()],
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
        needs: Some(SourceNeed::Consent),
        password_required: true,
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

    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let values = self.values.lock().unwrap().clone();
            let paused = values["paused"] == json!(true);
            *self.paused.lock().unwrap() = paused;
            self.calls
                .lock()
                .unwrap()
                .push(format!("paused={paused} intensity={}", values["intensity"]));
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
                sampling_blocked: Some(SamplingBlocked {
                    reason: "dirty".into(),
                    remaining_s: 21_540,
                }),
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

    fn patch_source(
        &self,
        id: u8,
        patch: SourcePatch,
        confirmed: bool,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            let mut sources = self.sources.lock().unwrap();
            let s = sources
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or(OpsError::UnknownSource)?;
            if patch.enabled == Some(true)
                && !s.enabled
                && s.consent_required
                && (patch.consent != Some(s.consent)
                    || patch.consent_version.as_deref() != Some(s.consent_version.as_str()))
            {
                return Err(OpsError::ConsentRequired {
                    class: s.consent,
                    version: s.consent_version.clone(),
                });
            }
            if patch.enabled == Some(true) && !s.enabled && s.password_required && !confirmed {
                // Enabling Restricted or Overlay needs it every time.
                return Err(OpsError::PasswordRequired);
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
            if patch.egress.is_some() && !confirmed {
                return Err(OpsError::PasswordRequired);
            }
            if let Some(n) = patch.agents {
                if n > s.agents.max {
                    return Err(OpsError::Invalid {
                        code: "invalid_setting",
                        field: "agents",
                        message: "The setting wasn't saved: too many agents.".into(),
                    });
                }
                s.agents.count = n;
            }
            if let Some(e) = patch.egress {
                s.egress.setting = Some(e);
            }
            if let Some(w) = patch.schedule {
                s.schedule.windows = w;
            }
            if let Some(p) = patch.priority {
                s.priority = p;
            }
            Ok(s.clone())
        })
    }

    fn patch_source_credentials(
        &self,
        id: u8,
        changes: Vec<(String, Option<lodestar_api::ops::Secret>)>,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            let mut sources = self.sources.lock().unwrap();
            let s = sources
                .iter_mut()
                .find(|s| s.id == id)
                .ok_or(OpsError::UnknownSource)?;
            for (name, value) in changes {
                s.credentials.retain(|n| *n != name);
                if value.is_some() {
                    s.credentials.push(name);
                }
            }
            s.credentials.sort();
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
            unsolicited_sources: Some(3),
            window_s: 600,
            window_started_at: Some(1_699_999_970),
        }
    }

    fn begin_purge(
        &self,
        id: u8,
        confirmed: bool,
    ) -> BoxFuture<'_, Result<(String, u64), OpsError>> {
        Box::pin(async move {
            assert!(confirmed, "the route checked the password");
            self.calls.lock().unwrap().push(format!("begin_purge {id}"));
            Ok(("dht".into(), 5))
        })
    }

    /// Three steps of one deleted torrent each, each waiting for a permit.
    fn purge_step(
        &self,
        _id: u8,
        _from: PurgeCursor,
    ) -> BoxFuture<'_, Result<PurgeStep, OpsError>> {
        Box::pin(async move {
            loop {
                let woken = self.purge_wake.notified();
                {
                    let mut p = self.purge_permits.lock().unwrap();
                    if *p > 0 {
                        *p -= 1;
                        break;
                    }
                }
                woken.await;
            }
            let mut n = self.purge_steps.lock().unwrap();
            *n += 1;
            Ok(PurgeStep {
                next: (!(*n).is_multiple_of(3)).then_some(PurgeCursor::START),
                torrents_deleted: vec![i64::from(*n)],
                torrents_kept: 2,
                ..PurgeStep::default()
            })
        })
    }

    fn finish_purge(&self, id: u8) -> BoxFuture<'_, Result<(u64, u64), OpsError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("finish_purge {id}"));
            Ok((2, 0))
        })
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
            unsolicited_sources: Some(0),
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
    *fake.values.lock().unwrap() = [
        ("intensity".to_string(), json!("balanced")),
        ("paused".to_string(), json!(false)),
    ]
    .into();
    let settings = FakeSettings {
        values: fake.values.clone(),
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
            user_data: None,
            adult: Default::default(),
            insights: None,
            hot: None,
            events: None,
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
const ROUTES: [(&str, &str, Option<&str>); 10] = [
    ("GET", "/api/v1/stats", None),
    ("GET", "/api/v1/crawler", None),
    ("PATCH", "/api/v1/crawler", Some(r#"{"intensity":"low"}"#)),
    ("POST", "/api/v1/crawler/pause", Some("{}")),
    ("POST", "/api/v1/crawler/resume", Some("{}")),
    ("GET", "/api/v1/sources", None),
    ("GET", "/api/v1/sources/0", None),
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
            r#"paused=true intensity="balanced""#,
            r#"paused=true intensity="balanced""#,
            r#"paused=false intensity="balanced""#,
            r#"paused=false intensity="low""#,
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
    // The right class at an old version asks again.
    let e = golden(
        &f,
        "source_consent_stale",
        req(
            "PATCH",
            "/api/v1/sources/1",
            c,
            Some(
                r#"{"enabled":true,"consent":"restricted","consent_version":"0-00000000000000ff"}"#,
            ),
        ),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("consent_required"));
    // Accepting Restricted consent needs the password, and a wrong one is
    // refused before the source is asked.
    let e = golden(
        &f,
        "source_consent_password_required",
        req(
            "PATCH",
            "/api/v1/sources/1",
            c,
            Some(
                r#"{"enabled":true,"consent":"restricted","consent_version":"1-00000000000000ff"}"#,
            ),
        ),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("confirm_required"));
    let e = send(
        &f,
        req(
            "PATCH",
            "/api/v1/sources/1",
            c,
            Some(r#"{"enabled":true,"consent":"restricted","consent_version":"1-00000000000000ff","password":"wrong"}"#),
        ),
    )
    .await;
    assert_ne!(e.0, 200, "{e:?}");
    golden(
        &f,
        "source_consent_given",
        req(
            "PATCH",
            "/api/v1/sources/1",
            c,
            Some(r#"{"enabled":true,"consent":"restricted","consent_version":"1-00000000000000ff","password":"correct horse battery"}"#),
        ),
    )
    .await;
    // Credentials go in and only their names come back.
    let saved = golden(
        &f,
        "source_credentials",
        req(
            "PATCH",
            "/api/v1/sources/1/credentials",
            c,
            Some(r#"{"credentials":{"api_key":"hunter2-value","old":null}}"#),
        ),
    )
    .await;
    assert_eq!(saved["credentials"], json!(["api_key"]));
    assert!(!saved.to_string().contains("hunter2"));
    let e = golden(
        &f,
        "source_credentials_malformed",
        req(
            "PATCH",
            "/api/v1/sources/1/credentials",
            c,
            Some(r#"{"credentials":{"api_key":12345}}"#),
        ),
    )
    .await;
    assert!(!e.to_string().contains("12345"));
    golden(
        &f,
        "source_not_editable",
        req("PATCH", "/api/v1/sources/0", c, Some(r#"{"purge":true}"#)),
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
async fn source_settings() {
    let f = fixture();
    let cookie = session(&f).await;
    let c = Some(cookie.as_str());
    let one = golden(&f, "source_get", req("GET", "/api/v1/sources/0", c, None)).await;
    assert_eq!(one["schedule"]["enforced"], json!(false));
    assert_eq!(one["agents"]["enforced"], json!(false));
    let (status, _) = send(&f, req("GET", "/api/v1/sources/9", c, None)).await;
    assert_eq!(status, 404);
    // Schedule and agents need no password; windows come back canonical.
    let saved = send(
        &f,
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(r#"{"schedule":[" 01:00-07:00 ","23:30-00:15"],"agents":3}"#),
        ),
    )
    .await;
    assert_eq!(saved.0, 200, "{saved:?}");
    let saved: Value = serde_json::from_str(&saved.1).unwrap();
    assert_eq!(
        saved["schedule"]["windows"],
        json!(["01:00-07:00", "23:30-00:15"])
    );
    assert_eq!(saved["agents"]["count"], json!(3));
    golden(
        &f,
        "source_schedule_invalid",
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(r#"{"schedule":["1am-7am"]}"#),
        ),
    )
    .await;
    golden(
        &f,
        "source_agents_zero",
        req("PATCH", "/api/v1/sources/0", c, Some(r#"{"agents":0}"#)),
    )
    .await;
    // The interface and bind-address routes aren't offered (their DNS
    // would leave outside them), password or not.
    for route in ["interface", "bind_address"] {
        let body =
            format!(r#"{{"egress":{{"route":"{route}"}},"password":"correct horse battery"}}"#);
        let e = golden(
            &f,
            &format!("source_egress_{route}"),
            req("PATCH", "/api/v1/sources/0", c, Some(&body)),
        )
        .await;
        assert_eq!(e["error"]["code"], json!("route_unavailable"));
    }
    // An egress change needs the password, and a wrong one is refused
    // before the source is asked.
    let socks = r#""egress":{"route":"socks5","proxy":"10.8.0.1:1080"}"#;
    let e = golden(
        &f,
        "source_egress_password_required",
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(&format!("{{{socks}}}")),
        ),
    )
    .await;
    assert_eq!(e["error"]["code"], json!("confirm_required"));
    let (status, _) = send(
        &f,
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(&format!(r#"{{{socks},"password":"wrong"}}"#)),
        ),
    )
    .await;
    assert_ne!(status, 200);
    assert_eq!(
        f.ops.sources.lock().unwrap()[0]
            .egress
            .setting
            .as_ref()
            .map(|s| s.route),
        Some(EgressRoute::Direct)
    );
    for unusable in ["0.0.0.0:1080", "[ff02::1]:1080"] {
        let body = format!(
            r#"{{"egress":{{"route":"socks5","proxy":"{unusable}"}},"password":"correct horse battery"}}"#
        );
        let (status, e) = send(&f, req("PATCH", "/api/v1/sources/0", c, Some(&body))).await;
        assert_eq!(status, 400, "{unusable}: {e}");
        assert!(e.contains("invalid_egress"), "{e}");
    }
    golden(
        &f,
        "source_egress_proxy_invalid",
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(r#"{"egress":{"route":"socks5","proxy":"user:pass@10.8.0.1:1080"},"password":"correct horse battery"}"#),
        ),
    )
    .await;
    let saved = golden(
        &f,
        "source_egress_socks5",
        req(
            "PATCH",
            "/api/v1/sources/0",
            c,
            Some(&format!(
                r#"{{{socks},"password":"correct horse battery"}}"#
            )),
        ),
    )
    .await;
    assert_eq!(saved["egress"]["setting"]["route"], json!("socks5"));
    assert!(!saved.to_string().contains("correct horse"));
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

impl FakeOps {
    fn permit(&self, n: u32) {
        *self.purge_permits.lock().unwrap() += n;
        self.purge_wake.notify_waiters();
    }
}

/// Polls `GET /jobs/{id}` until `done` holds, for at most 10 s.
async fn job_until(f: &Fixture, cookie: &str, id: u64, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, body) = send(
            f,
            req("GET", &format!("/api/v1/jobs/{id}"), Some(cookie), None),
        )
        .await;
        let job: Value = serde_json::from_str(&body).unwrap();
        if done(&job) {
            return job;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the job didn't get there: {job}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// A purge runs as a job behind the password, reports only what it
/// committed when it's cancelled, and finishes when it's run again
/// (SRC-14b).
#[tokio::test]
async fn purging_a_source_is_a_job() {
    let f = fixture();
    let cookie = session(&f).await;
    let purge = |id: u8, body: &str| {
        req(
            "POST",
            &format!("/api/v1/sources/{id}/purge"),
            Some(&cookie),
            Some(body),
        )
    };
    // The password, every time; and a source that exists.
    let (status, body) = send(&f, purge(0, "{}")).await;
    assert_eq!(status, 428, "{body}");
    let (status, _) = send(&f, purge(0, r#"{"password":"wrong"}"#)).await;
    assert_eq!(status, 401);
    let ok = r#"{"password":"correct horse battery"}"#;
    let (status, _) = send(&f, purge(9, ok)).await;
    assert_eq!(status, 404);

    let (status, body) = send(&f, purge(0, ok)).await;
    assert_eq!(status, 202, "{body}");
    let job: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(job["kind"], json!("purge"));
    assert_eq!(job["purge"]["source_id"], json!(0));
    let id = job["id"].as_u64().unwrap();
    // One at a time per source.
    let (status, body) = send(&f, purge(0, ok)).await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("purge_running"));

    // One step commits, then it's cancelled. A step already waiting
    // still commits; nothing after the cancel starts. The job reports
    // exactly the steps that committed.
    f.ops.permit(1);
    job_until(&f, &cookie, id, |j| {
        j["purge"]["torrents_deleted"] == json!(1)
    })
    .await;
    let (status, _) = send(
        &f,
        req(
            "POST",
            &format!("/api/v1/jobs/{id}/cancel"),
            Some(&cookie),
            Some("{}"),
        ),
    )
    .await;
    assert_eq!(status, 200);
    f.ops.permit(1);
    let job = job_until(&f, &cookie, id, |j| j["state"] == json!("cancelled")).await;
    let ran = u64::from(*f.ops.purge_steps.lock().unwrap());
    assert!((1..=2).contains(&ran), "{ran}");
    assert_eq!(
        job["purge"]["torrents_deleted"],
        json!(ran),
        "the committed steps"
    );
    assert_eq!(job["purge"]["kept"], json!(2 * ran));
    assert_eq!(job["purge"]["keys_dropped"], json!(5));
    assert_eq!(job["purge"]["stage"], json!("removing"));
    assert!(
        !f.ops
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("finish_purge"))
    );

    // Run again: it starts over and finishes.
    let (status, body) = send(&f, purge(0, ok)).await;
    assert_eq!(status, 202, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    f.ops.permit(10);
    let job = job_until(&f, &cookie, id, |j| j["state"] == json!("done")).await;
    assert_eq!(job["purge"]["stage"], json!("purged"));
    assert_eq!(job["purge"]["snapshots_scrubbed"], json!(2));
    let calls = f.ops.calls.lock().unwrap().clone();
    assert_eq!(calls.iter().filter(|c| *c == "begin_purge 0").count(), 2);
    assert!(calls.contains(&"finish_purge 0".to_string()));
}
