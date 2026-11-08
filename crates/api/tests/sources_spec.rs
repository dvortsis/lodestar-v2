//! X-sources-api-spec through the router: `GET /sources` lists each
//! source's declared credentials (`credential_spec`) and settings schema
//! (`config_schema`) and never a credential's value, and
//! `GET /sources/{id}/purge/preview` counts what a purge removes, changes
//! nothing, needs a session (an API key won't do) and asks no password.
//!
//! The running process is a fake over a real store: the preview and the
//! purge are the store's own `purge_preview` and `purge_source`, so the
//! counts the routes report are compared with what the purge job then
//! does. The binary's `Ops` (how a plugin's `Policy` becomes these
//! fields) is tested in `crates/lodestar/src/ops.rs`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::dto::jobs::PurgePreview;
use lodestar_api::ops::{
    BoxFuture, ConsentClass, CrawlerRuntime, CredentialField, CredentialKind, EgressCounts,
    EgressLive, EgressRoute, EgressSetting, EgressState, ExternalAddress, FetchStatus,
    NetworkCheck, OpsApi, OpsError, PortMapping, ReachReason, RiskLevel, Secret, Source,
    SourceAgents, SourceEgress, SourceHealth, SourceKind, SourceNeed, SourcePatch, SourcePriority,
    SourceSchedule, SourceState, StatsResponse,
};
use lodestar_api::{ApiState, Auth, router};
use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::claims::Claims;
use lodestar_store::unverified::{Network, NewUnverified, PurgeCursor, PurgeStep};
use lodestar_store::{PreparedTorrent, SourceId, Store, StoreConfig, WriteBatch};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
/// A credential value no response may ever hold.
const VALUE: &str = "SECRET-cafebabe-0123456789";
const T: i64 = 1_700_000_000;

/// The pipeline, as far as these routes ask, over a real store.
#[derive(Debug)]
struct StoreOps {
    store: Arc<Mutex<Store>>,
    /// The credential names stored per source id: never a value.
    stored: Mutex<Vec<(u8, String)>>,
    /// Every value the fake was given, to check none comes back.
    given: Mutex<Vec<String>>,
    /// The purge's steps, summed as the job ran them.
    purged: Mutex<PurgeStep>,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

impl StoreOps {
    fn source(&self, id: u8) -> Source {
        let rows = self.store.lock().unwrap().sources().unwrap();
        let row = rows.iter().find(|r| r.id.get() == id).unwrap();
        let state = match row.state {
            lodestar_store::SourceState::Enabled => SourceState::Enabled,
            lodestar_store::SourceState::Paused => SourceState::Paused,
            lodestar_store::SourceState::Disabled => SourceState::Disabled,
            lodestar_store::SourceState::Purged => SourceState::Purged,
        };
        let names: Vec<String> = self
            .stored
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| *s == id)
            .map(|(_, n)| n.clone())
            .collect();
        let account = row.plugin == "prowlarr";
        Source {
            agents: SourceAgents {
                count: 1,
                enforced: false,
                max: 1,
            },
            config_schema: if account {
                schema(
                    json!({ "url": { "type": "string", "format": "uri", "title": "Address" } }),
                    &["url"],
                )
            } else {
                schema(json!({}), &[])
            },
            consent: if account {
                ConsentClass::Account
            } else {
                ConsentClass::External
            },
            consent_required: !account,
            consent_version: String::new(),
            credential_spec: if account {
                vec![CredentialField {
                    kind: CredentialKind::Secret,
                    label: "API key".into(),
                    name: "api_key".into(),
                    required: true,
                }]
            } else {
                Vec::new()
            },
            companion_need: None,
            needs: (account && names.is_empty()).then_some(SourceNeed::Credentials),
            credentials: names,
            editable: Vec::new(),
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
            enabled: state == SourceState::Enabled,
            endpoints: Vec::new(),
            finds_private: account,
            health: SourceHealth {
                harvested_24h: 0,
                last_error: None,
                last_run: None,
                running: false,
                stored_24h: 0,
            },
            id,
            kinds: vec![SourceKind::Discovery],
            name: row.plugin.clone(),
            networks: vec!["bt".into()],
            not_yet_editable: vec!["purge".into()],
            password_required: false,
            plugin: row.plugin.clone(),
            priority: SourcePriority::Normal,
            purge_pending: false,
            restart_required: false,
            risk: RiskLevel::Low,
            schedule: SourceSchedule {
                default: Vec::new(),
                enforced: false,
                windows: Vec::new(),
            },
            state,
            version: "1".into(),
        }
    }

    fn id(id: u8) -> Result<SourceId, OpsError> {
        SourceId::new(id).ok_or(OpsError::UnknownSource)
    }
}

impl OpsApi for StoreOps {
    fn crawler(&self) -> CrawlerRuntime {
        CrawlerRuntime {
            since: 0,
            dht: None,
            fetch: FetchStatus {
                in_flight: 0,
                lookups: 0,
                queued: 0,
            },
        }
    }

    fn sync_crawler(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn stats(&self) -> BoxFuture<'_, StatsResponse> {
        unreachable!("not these routes")
    }

    fn sources(&self) -> BoxFuture<'_, Result<Vec<Source>, OpsError>> {
        Box::pin(async {
            let ids: Vec<u8> = self
                .store
                .lock()
                .unwrap()
                .sources()
                .unwrap()
                .iter()
                .map(|r| r.id.get())
                .collect();
            Ok(ids.into_iter().map(|id| self.source(id)).collect())
        })
    }

    fn patch_source(
        &self,
        _: u8,
        _: SourcePatch,
        _: bool,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        unreachable!("not these routes")
    }

    fn patch_source_credentials(
        &self,
        id: u8,
        changes: Vec<(String, Option<Secret>)>,
    ) -> BoxFuture<'_, Result<Source, OpsError>> {
        Box::pin(async move {
            for (name, value) in changes {
                let mut stored = self.stored.lock().unwrap();
                stored.retain(|(s, n)| !(*s == id && *n == name));
                if let Some(v) = value {
                    self.given.lock().unwrap().push(v.expose().to_string());
                    stored.push((id, name));
                }
            }
            Ok(self.source(id))
        })
    }

    fn begin_purge(
        &self,
        id: u8,
        confirmed: bool,
    ) -> BoxFuture<'_, Result<(String, u64), OpsError>> {
        Box::pin(async move {
            assert!(confirmed);
            let source = Self::id(id)?;
            self.store
                .lock()
                .unwrap()
                .begin_source_purge(source, T)
                .map_err(|_| OpsError::UnknownSource)?;
            Ok((self.source(id).plugin, 0))
        })
    }

    fn purge_step(&self, id: u8, from: PurgeCursor) -> BoxFuture<'_, Result<PurgeStep, OpsError>> {
        Box::pin(async move {
            let step = self
                .store
                .lock()
                .unwrap()
                .purge_source(Self::id(id)?, from, 2)
                .map_err(|e| OpsError::Internal(e.to_string()))?;
            let mut sum = self.purged.lock().unwrap();
            sum.unverified_deleted += step.unverified_deleted;
            sum.unverified_kept += step.unverified_kept;
            sum.claims_deleted += step.claims_deleted;
            sum.torrents_deleted.extend(&step.torrents_deleted);
            sum.torrents_kept += step.torrents_kept;
            sum.state_deleted += step.state_deleted;
            sum.queue_deleted += step.queue_deleted;
            Ok(step)
        })
    }

    fn purge_preview(&self, id: u8) -> BoxFuture<'_, Result<PurgePreview, OpsError>> {
        Box::pin(async move {
            let source = Self::id(id)?;
            let p = self
                .store
                .lock()
                .unwrap()
                .purge_preview(source)
                .map_err(|_| OpsError::UnknownSource)?;
            let s = self.source(id);
            Ok(PurgePreview {
                claims_deleted: p.claims_deleted,
                kept: p.torrents_kept,
                plugin: s.plugin,
                queue_deleted: p.queue_deleted,
                settled: s.state != SourceState::Enabled,
                source_id: id,
                state_deleted: p.state_deleted,
                torrents_deleted: p.torrents_deleted,
                unverified_deleted: p.unverified_deleted,
                unverified_kept: p.unverified_kept,
            })
        })
    }

    fn finish_purge(&self, id: u8) -> BoxFuture<'_, Result<(u64, u64), OpsError>> {
        Box::pin(async move {
            self.store
                .lock()
                .unwrap()
                .finish_source_purge(Self::id(id)?, T)
                .map_err(|e| OpsError::Internal(e.to_string()))?;
            Ok((0, 0))
        })
    }

    fn network(&self) -> NetworkCheck {
        NetworkCheck {
            checked_at: 0,
            external: ExternalAddress {
                agreeing: 0,
                ip: None,
                port: None,
                voters: 0,
            },
            inbound_queries: None,
            mapping: PortMapping::Unavailable,
            measured_s: 0,
            min_window_s: 0,
            reachable: None,
            reason: ReachReason::DhtOff,
            unsolicited_queries: None,
            unsolicited_sources: None,
            window_s: 0,
            window_started_at: None,
        }
    }

    fn restart_network_check(&self) -> Result<NetworkCheck, OpsError> {
        Ok(self.network())
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    ops: Arc<StoreOps>,
    _dir: tempfile::TempDir,
}

fn info_bytes(name: &str) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn put_torrent(store: &mut Store, name: &str, source: SourceId) -> i64 {
    let p = store
        .preparer()
        .prepare(&Info::parse(&info_bytes(name)).unwrap(), None)
        .unwrap();
    store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &p,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap()
        .prepared[0]
        .id()
}

/// Nyaa (1, off) found three unverified keys, one of them with Prowlarr
/// (2); three torrents, one with the DHT (0); and has a claim on the
/// shared one and a state entry.
fn seeded() -> Store {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", T).unwrap();
    let nyaa = s.register_source("nyaa", "1", T).unwrap();
    let prowlarr = s.register_source("prowlarr", "1", T).unwrap();
    let keys: Vec<[u8; 20]> = (1..=3u8).map(|i| [0x50 | i; 20]).collect();
    // A key with no claims isn't stored (`Put::Bare`).
    let named = Claims {
        name: Some("a name".into()),
        ..Claims::default()
    };
    fn item<'a>(k: &'a [u8; 20], source: SourceId, claims: &'a Claims) -> NewUnverified<'a> {
        NewUnverified {
            network: Network::Bt,
            key: k,
            source,
            private_source: false,
            claims: Some(claims),
            at: T,
        }
    }
    s.put_unverified(&[
        item(&keys[0], nyaa, &named),
        item(&keys[1], nyaa, &named),
        item(&keys[2], nyaa, &named),
    ])
    .unwrap();
    s.put_unverified(&[item(&keys[2], prowlarr, &named)])
        .unwrap();
    put_torrent(&mut s, "only nyaa a", nyaa);
    put_torrent(&mut s, "only nyaa b", nyaa);
    let shared = put_torrent(&mut s, "shared", dht);
    put_torrent(&mut s, "shared", nyaa);
    let claims = Claims {
        name: Some("shared".into()),
        ..Claims::default()
    };
    s.put_claims(shared, nyaa, &claims, false).unwrap();
    s.set_plugin_state(nyaa, "cursor", Some(b"9")).unwrap();
    // Off, as a source is before its purge: nothing more can arrive.
    s.set_source_state(nyaa, lodestar_store::SourceState::Disabled)
        .unwrap();
    s
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let users = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let ops = Arc::new(StoreOps {
        store: Arc::new(Mutex::new(seeded())),
        stored: Mutex::default(),
        given: Mutex::default(),
        purged: Mutex::default(),
    });
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: None,
            ops: Some(ops.clone() as Arc<dyn OpsApi>),
            user_data: Some(users),
            adult: Default::default(),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    Fixture {
        app,
        auth,
        ops,
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

async fn session(f: &Fixture) -> String {
    let token = f.auth.setup_token().unwrap();
    let body = json!({
        "token": token,
        "password": PASSWORD,
        "notice_version": lodestar_api::legal::NOTICE_VERSION
    });
    let mut req = Request::post("/api/v1/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
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

fn with_password() -> String {
    json!({ "password": PASSWORD }).to_string()
}

/// Every row of the tables a purge touches, to show a preview changed none.
fn tables(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    for t in [
        "sources",
        "unverified_items",
        "torrents",
        "torrent_claims",
        "source_state",
        "fetch_queue",
        "settings",
    ] {
        let conn = store.connection();
        let cols: Vec<String> = conn
            .prepare(&format!("SELECT name FROM pragma_table_info('{t}')"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let row = cols
            .iter()
            .map(|c| format!("quote(\"{c}\")"))
            .collect::<Vec<_>>()
            .join(" || ',' || ");
        let mut stmt = conn
            .prepare(&format!("SELECT '{t}: ' || {row} FROM {t} ORDER BY 1"))
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out
}

#[tokio::test]
async fn each_source_lists_its_credential_spec_and_settings_schema() {
    let f = fixture();
    let cookie = session(&f).await;
    let (status, body) = send(&f, req("GET", "/api/v1/sources", Some(&cookie), None)).await;
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    let sources = list["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 3);
    for s in sources {
        assert!(s["credential_spec"].is_array(), "{s}");
        assert_eq!(s["config_schema"]["type"], "object", "{s}");
        assert!(s["config_schema"]["properties"].is_object(), "{s}");
    }
    let prowlarr = sources.iter().find(|s| s["plugin"] == "prowlarr").unwrap();
    assert_eq!(
        prowlarr["credential_spec"],
        json!([{ "kind": "secret", "label": "API key", "name": "api_key", "required": true }])
    );
    assert_eq!(prowlarr["needs"], "credentials");
    assert_eq!(prowlarr["config_schema"]["required"], json!(["url"]));
    // `GET /sources/{id}` is the same row.
    let (_, one) = send(&f, req("GET", "/api/v1/sources/2", Some(&cookie), None)).await;
    let one: Value = serde_json::from_str(&one).unwrap();
    assert_eq!(&one, prowlarr);

    // The OpenAPI document has both as required fields of `Source`.
    let doc: Value = serde_json::from_str(&lodestar_api::openapi::json()).unwrap();
    let src = &doc["components"]["schemas"]["Source"];
    let required: Vec<&str> = src["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"credential_spec") && required.contains(&"config_schema"));
}

#[tokio::test]
async fn no_credential_value_is_ever_returned() {
    let f = fixture();
    let cookie = session(&f).await;
    let patch = json!({ "credentials": { "api_key": VALUE } }).to_string();
    let (status, body) = send(
        &f,
        req(
            "PATCH",
            "/api/v1/sources/2/credentials",
            Some(&cookie),
            Some(&patch),
        ),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(f.ops.given.lock().unwrap().as_slice(), [VALUE]);
    let mut seen = vec![body];
    for uri in ["/api/v1/sources", "/api/v1/sources/2"] {
        seen.push(send(&f, req("GET", uri, Some(&cookie), None)).await.1);
    }
    seen.push(
        send(
            &f,
            req(
                "GET",
                "/api/v1/sources/2/purge/preview",
                Some(&cookie),
                None,
            ),
        )
        .await
        .1,
    );
    // A malformed write is refused without quoting it.
    let bad = format!(r#"{{"credentials": {{"api_key": ["{VALUE}"]}}}}"#);
    let (status, refused) = send(
        &f,
        req(
            "PATCH",
            "/api/v1/sources/2/credentials",
            Some(&cookie),
            Some(&bad),
        ),
    )
    .await;
    assert_eq!(status, 400);
    seen.push(refused);
    for body in &seen {
        assert!(!body.contains(VALUE), "{body}");
        assert!(!body.contains("cafebabe"), "{body}");
    }
    // The name is listed, and the spec still holds no value.
    let row: Value = serde_json::from_str(&seen[2]).unwrap();
    assert_eq!(row["credentials"], json!(["api_key"]));
    assert_eq!(row["needs"], Value::Null);
}

async fn preview(f: &Fixture, cookie: &str) -> (u16, String) {
    let r = req("GET", "/api/v1/sources/1/purge/preview", Some(cookie), None);
    send(f, r).await
}

#[tokio::test]
async fn the_preview_counts_what_the_purge_removes_and_changes_nothing() {
    let f = fixture();
    let cookie = session(&f).await;
    let before = tables(&f.ops.store.lock().unwrap());
    let body = with_password();
    let (status, text) = preview(&f, &cookie).await;
    assert_eq!(status, 200, "{text}");
    let got: PurgePreview = serde_json::from_str(&text).unwrap();
    assert_eq!(
        got,
        PurgePreview {
            claims_deleted: 1,
            kept: 1,
            plugin: "nyaa".into(),
            queue_deleted: 0,
            settled: true,
            source_id: 1,
            state_deleted: 1,
            torrents_deleted: 2,
            unverified_deleted: 2,
            unverified_kept: 1,
        }
    );
    // Nothing changed: not the data, its state or its settings, and no
    // job was started.
    assert_eq!(tables(&f.ops.store.lock().unwrap()), before);
    assert_eq!(preview(&f, &cookie).await.1, text, "the same again");
    let (_, jobs) = send(&f, req("GET", "/api/v1/jobs", Some(&cookie), None)).await;
    assert_eq!(
        serde_json::from_str::<Value>(&jobs).unwrap()["jobs"],
        json!([])
    );

    // The purge then removes exactly that.
    let (status, job) = send(
        &f,
        req(
            "POST",
            "/api/v1/sources/1/purge",
            Some(&cookie),
            Some(&body),
        ),
    )
    .await;
    assert_eq!(status, 202, "{job}");
    let id = serde_json::from_str::<Value>(&job).unwrap()["id"]
        .as_u64()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let job = loop {
        let (_, j) = send(
            &f,
            req("GET", &format!("/api/v1/jobs/{id}"), Some(&cookie), None),
        )
        .await;
        let j: Value = serde_json::from_str(&j).unwrap();
        if j["state"] != "running" {
            break j;
        }
        assert!(std::time::Instant::now() < deadline, "{j}");
        // Polled until the job leaves `running`, within the deadline.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["purge"]["torrents_deleted"], got.torrents_deleted);
    assert_eq!(job["purge"]["kept"], got.kept);
    assert_eq!(job["purge"]["unverified_deleted"], got.unverified_deleted);
    let sum = f.ops.purged.lock().unwrap().clone();
    assert_eq!(sum.claims_deleted as u64, got.claims_deleted);
    assert_eq!(sum.unverified_kept as u64, got.unverified_kept);
    assert_eq!(sum.state_deleted as u64, got.state_deleted);
    assert_eq!(sum.queue_deleted as u64, got.queue_deleted);
    // After it, there's nothing left to remove.
    let after: PurgePreview = serde_json::from_str(&preview(&f, &cookie).await.1).unwrap();
    assert_eq!(
        (
            after.torrents_deleted,
            after.unverified_deleted,
            after.claims_deleted
        ),
        (0, 0, 0)
    );
}

#[tokio::test]
async fn the_preview_needs_a_session_and_no_password() {
    let f = fixture();
    let cookie = session(&f).await;
    let uri = "/api/v1/sources/1/purge/preview";
    // No session.
    assert_eq!(send(&f, req("GET", uri, None, None)).await.0, 401);
    // An API key is no session, however it's presented.
    let make = json!({
        "name": "a key", "kind": "api_key", "scopes": ["torznab"], "password": PASSWORD
    })
    .to_string();
    let (status, made) = send(&f, req("POST", "/api/v1/keys", Some(&cookie), Some(&make))).await;
    assert_eq!(status, 201, "{made}");
    let key = serde_json::from_str::<Value>(&made).unwrap()["secret"]
        .as_str()
        .unwrap()
        .to_string();
    for (name, value) in [
        ("authorization", format!("Bearer {key}")),
        ("x-api-key", key.clone()),
    ] {
        let mut r = req("GET", uri, None, None);
        r.headers_mut().insert(name, value.parse().unwrap());
        let (status, text) = send(&f, r).await;
        assert_eq!(status, 401, "{name}: {text}");
        assert!(!text.contains("torrents_deleted"), "{text}");
    }
    let r = req("GET", &format!("{uri}?apikey={key}"), None, None);
    assert_eq!(send(&f, r).await.0, 401, "apikey query");
    // A session is enough: the password is the purge's to ask.
    let (status, text) = send(&f, req("GET", uri, Some(&cookie), None)).await;
    assert_eq!(status, 200, "{text}");
    // An unknown source.
    let (status, text) = send(
        &f,
        req(
            "GET",
            "/api/v1/sources/9/purge/preview",
            Some(&cookie),
            None,
        ),
    )
    .await;
    assert_eq!(status, 404, "{text}");
    assert!(text.contains("unknown_source"), "{text}");
    // Only GET: it writes nothing.
    let body = with_password();
    assert_eq!(
        send(&f, req("POST", uri, Some(&cookie), Some(&body)))
            .await
            .0,
        405
    );
    // Still nothing ran.
    let (_, jobs) = send(&f, req("GET", "/api/v1/jobs", Some(&cookie), None)).await;
    assert_eq!(
        serde_json::from_str::<Value>(&jobs).unwrap()["jobs"],
        json!([])
    );
}
