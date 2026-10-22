//! `/crawler`, `/stats`, `/sources` and `/network/check` with the real
//! registry, pipeline and DHT node behind the real router.

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use lodestar_api::{ApiState, Auth, router};
use lodestar_dht::node::Intensity;
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::{Control, DhtSource, Pipeline, PipelineConfig, event};
use lodestar_sources::{
    BoxFuture, Consent, Fetcher, Found, Kind, Manifest, Network, PluginError, PoolConfig, Priority,
    Risk, SourceCtx, SourcePlugin,
};
use lodestar_store::{Store, StoreConfig};
use serde_json::{Value, json};
use tokio::net::UdpSocket;
use tower::ServiceExt;

use super::*;
use crate::config::Config;
use crate::settings::{self, Registry};

static FEED: Manifest = Manifest {
    id: "feed",
    name: "Test feed",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    endpoints: &[],
    risk: Risk::Low,
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: true,
};

/// Reports a new key with a peer every 10 ms.
struct Feed;

impl SourcePlugin for Feed {
    fn manifest(&self) -> &Manifest {
        &FEED
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let mut n = 0u32;
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {
                        n += 1;
                        let mut key = [7u8; 20];
                        key[..4].copy_from_slice(&n.to_be_bytes());
                        let peer = SocketAddr::from(([10, 0, (n >> 8) as u8, n as u8], 1));
                        ctx.emitter.try_emit(Found::key(InfoHash(key), 0).with_peers(vec![peer]));
                    }
                }
            }
        })
    }
}

/// Counts fetch starts; every peer refuses.
#[derive(Default)]
struct Counting(Arc<AtomicUsize>);

impl Fetcher for Counting {
    fn fetch(
        &self,
        _peer: SocketAddr,
        _key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Err(FailureKind::Refused)
        })
    }
}

struct Fixture {
    app: Router,
    auth: Arc<Auth>,
    registry: Arc<Registry>,
    control_store: Arc<Mutex<Store>>,
    starts: Arc<AtomicUsize>,
    dht: Arc<DhtSource>,
    dht_addr: SocketAddr,
    pipeline: Option<Pipeline>,
    ops: Arc<Ops>,
    dir: tempfile::TempDir,
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::load(None, |_| None).unwrap();
    let db = dir.path().join("t.db");
    let control_store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let registry = Arc::new(Registry::load(&config, control_store.clone()).unwrap());
    let crawler = registry.get::<settings::crawler::Crawler>();
    let dht_addr = SocketAddr::from(([127, 0, 0, 1], free_udp_port()));
    let dht = Arc::new(DhtSource::new(lodestar_dht::node::Config {
        bind: vec![dht_addr],
        bootstrap: Vec::new(),
        tick: Duration::from_millis(50),
        paused: crawler.paused,
        ..lodestar_dht::node::Config::default()
    }));
    settings::crawler::follow_intensity(registry.subscribe(), dht.node_handle());
    let starts = Arc::new(AtomicUsize::new(0));
    let pipeline = Pipeline::start_with(
        PipelineConfig {
            pool: PoolConfig {
                paused: crawler.paused,
                ..PoolConfig::default()
            },
            flush_every: Duration::from_millis(100),
            ..PipelineConfig::default()
        },
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![dht.clone(), Arc::new(Feed)],
        Arc::new(Counting(starts.clone())),
    )
    .await
    .unwrap();
    let control = Control::start(
        pipeline.monitor(),
        Some(dht.clone()),
        control_store.clone(),
        crawler.paused,
        crawler.intensity.into(),
    )
    .await
    .unwrap();
    settings::crawler::follow_control(registry.subscribe(), control.clone());
    let ops = Arc::new(Ops::new(
        control,
        registry.subscribe(),
        pipeline.monitor(),
        None,
        dir.path().to_path_buf(),
    ));
    let auth = Arc::new(Auth::load(&dir.path().join("auth.json"), false).unwrap());
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let app = Router::new().nest(
        "/api/v1",
        router(ApiState {
            auth: auth.clone(),
            index: None,
            reader: Arc::new(Mutex::new(reader)),
            filters: None,
            settings: Some(registry.clone()),
            ops: Some(ops.clone()),
            adult: Default::default(),
        }),
    );
    Fixture {
        app,
        auth,
        registry,
        control_store,
        starts,
        dht,
        dht_addr,
        pipeline: Some(pipeline),
        ops,
        dir,
    }
}

impl Fixture {
    async fn send(&self, mut req: Request<Body>) -> (u16, Value) {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn session(&self) -> String {
        let token = self.auth.setup_token().unwrap();
        let mut req = Request::post("/api/v1/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "token": token, "password": "correct horse battery" }).to_string(),
            ))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 2], 1))));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    fn events(&self, kind: u32) -> Vec<String> {
        self.control_store
            .lock()
            .unwrap()
            .events(0, i64::MAX)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.detail)
            .collect()
    }

    async fn stop(&mut self) {
        if let Some(p) = self.pipeline.take() {
            p.shutdown().await.unwrap();
        }
    }
}

fn req(method: &str, uri: &str, cookie: &str, body: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b.body(Body::from(body.unwrap_or("").to_string())).unwrap()
}

/// Fetch starts over `ms`.
async fn starts_during(f: &Fixture, ms: u64) -> usize {
    let before = f.starts.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    f.starts.load(Ordering::SeqCst) - before
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out");
}

async fn node_paused(dht: &DhtSource) -> Option<bool> {
    let handle = dht.node_handle().lock().unwrap().clone()?;
    Some(handle.snapshot().await?.paused)
}

/// Whether the node answers a ping.
async fn answers(addr: SocketAddr) -> bool {
    use lodestar_dht::krpc::{self, Body as Krpc, Message, Method, Query};
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let msg = Message {
        transaction_id: b"pg",
        version: None,
        ip: None,
        read_only: true,
        body: Krpc::Query(Query {
            id: lodestar_dht::NodeId([1; 20]),
            method: Method::Ping,
        }),
    };
    socket.send_to(&msg.encode(), addr).await.unwrap();
    let mut buf = [0u8; 1500];
    match tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => krpc::parse(&buf[..n]).is_ok_and(|m| matches!(m.body, Krpc::Response(_))),
        _ => false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_resume_and_intensity_through_the_api() {
    let mut f = fixture().await;
    // Without a session, a 401; cross-origin, a 403.
    let (status, _) = f
        .send(
            Request::post("/api/v1/crawler/pause")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await;
    assert_eq!(status, 401);
    let cookie = f.session().await;
    let c = cookie.as_str();
    let mut cross = req("POST", "/api/v1/crawler/pause", c, Some("{}"));
    cross
        .headers_mut()
        .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
    assert_eq!(f.send(cross).await.0, 403);
    assert!(f.events(event::PAUSE).is_empty());

    let (status, running) = f.send(req("GET", "/api/v1/crawler", c, None)).await;
    assert_eq!(status, 200, "{running}");
    assert_eq!(running["state"], json!("running"));
    assert_eq!(running["intensity"], json!("balanced"));
    assert_eq!(running["intensity_env"], Value::Null);
    eventually(|| f.starts.load(Ordering::SeqCst) > 10).await;
    eventually(|| f.dht.stats().is_some()).await;

    // Pause: no new fetch starts within a bound, the node holds its
    // crawl but still answers, and one event is recorded.
    let (status, paused) = f
        .send(req("POST", "/api/v1/crawler/pause", c, Some("{}")))
        .await;
    assert_eq!(status, 200, "{paused}");
    assert_eq!(
        (paused["state"].clone(), paused["paused"].clone()),
        (json!("paused"), json!(true))
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        starts_during(&f, 400).await,
        0,
        "a paused pool starts nothing"
    );
    assert_eq!(node_paused(&f.dht).await, Some(true));
    assert!(answers(f.dht_addr).await, "a paused node still answers");
    assert_eq!(f.events(event::PAUSE).len(), 1);
    // Idempotent: a second pause changes and records nothing.
    let (status, again) = f
        .send(req("POST", "/api/v1/crawler/pause", c, Some("{}")))
        .await;
    assert_eq!(
        (status, again["since"].clone()),
        (200, paused["since"].clone())
    );
    assert_eq!(f.events(event::PAUSE).len(), 1);
    let (_, stats) = f.send(req("GET", "/api/v1/stats", c, None)).await;
    assert_eq!(stats["paused"], json!(true));

    // Resume.
    let (status, resumed) = f
        .send(req("POST", "/api/v1/crawler/resume", c, Some("{}")))
        .await;
    assert_eq!((status, resumed["state"].clone()), (200, json!("running")));
    assert!(starts_during(&f, 400).await > 0, "fetching resumes");
    assert_eq!(node_paused(&f.dht).await, Some(false));
    assert_eq!(f.events(event::RESUME).len(), 1);

    // Intensity: /crawler and /settings/crawler are one value.
    let (status, low) = f
        .send(req(
            "PATCH",
            "/api/v1/crawler",
            c,
            Some(r#"{"intensity":"low"}"#),
        ))
        .await;
    assert_eq!((status, low["intensity"].clone()), (200, json!("low")));
    let (_, section) = f
        .send(req("GET", "/api/v1/settings/crawler", c, None))
        .await;
    assert_eq!(section["fields"]["intensity"]["value"], json!("low"));
    assert_eq!(f.events(event::INTENSITY), vec!["low"]);
    let (status, _) = f
        .send(req(
            "PATCH",
            "/api/v1/settings/crawler",
            c,
            Some(r#"{"intensity":"aggressive"}"#),
        ))
        .await;
    assert_eq!(status, 200);
    let (_, crawler) = f.send(req("GET", "/api/v1/crawler", c, None)).await;
    assert_eq!(crawler["intensity"], json!("aggressive"));
    eventually(|| f.events(event::INTENSITY) == ["low", "aggressive"]).await;
    let (status, bad) = f
        .send(req(
            "PATCH",
            "/api/v1/crawler",
            c,
            Some(r#"{"intensity":"max"}"#),
        ))
        .await;
    assert_eq!(
        (status, bad["error"]["code"].clone()),
        (422, json!("bad_body"))
    );

    // A pause made through /settings/crawler takes effect and is
    // recorded too.
    f.send(req(
        "PATCH",
        "/api/v1/settings/crawler",
        c,
        Some(r#"{"paused":true}"#),
    ))
    .await;
    eventually(|| f.events(event::PAUSE).len() == 2).await;
    assert_eq!(node_paused(&f.dht).await, Some(true));
    assert!(f.ops.control.status().paused);
    f.stop().await;
}

/// `/stats` reads the counters `/metrics` shows; what can't be measured
/// is null.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_match_the_metrics() {
    let mut f = fixture().await;
    let cookie = f.session().await;
    let c = cookie.as_str();
    eventually(|| f.starts.load(Ordering::SeqCst) > 5).await;
    // Pausing holds the counters still, so the two reads agree.
    f.send(req("POST", "/api/v1/crawler/pause", c, Some("{}")))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, stats) = f.send(req("GET", "/api/v1/stats", c, None)).await;
    assert_eq!(status, 200, "{stats}");
    let monitor = f.ops.monitor.clone();
    let metrics = crate::metrics::render(&monitor, f.dht.stats().as_ref(), &Default::default());
    let metric = |name: &str| -> u64 {
        metrics
            .lines()
            .find_map(|l| l.strip_prefix(&format!("lodestar_{name} ")))
            .unwrap_or_else(|| panic!("no {name} in /metrics"))
            .parse()
            .unwrap()
    };
    assert_eq!(stats["stored_torrents"], json!(metric("torrents")));
    assert_eq!(stats["queue_depth"], json!(metric("fetch_pending")));
    assert!(stats["queue_depth"].as_u64().unwrap() > 0);
    assert_eq!(
        stats["uptime_s"].as_i64().unwrap(),
        stats["updated_at"].as_i64().unwrap() - metric("start_time_seconds") as i64
    );
    // Not measurable here: no index, under a minute of samples, and no
    // free-space call in this build.
    for null in [
        "searchable_torrents",
        "index_lag_s",
        "harvested_per_hour",
        "fetched_per_hour",
        "disk_free_bytes",
    ] {
        assert_eq!(stats[null], Value::Null, "{null}");
    }
    assert!(
        stats["data_bytes"].as_u64().unwrap() > 0,
        "the database is there"
    );
    f.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sources_and_the_network_check() {
    let mut f = fixture().await;
    let cookie = f.session().await;
    let c = cookie.as_str();
    let (status, list) = f.send(req("GET", "/api/v1/sources", c, None)).await;
    assert_eq!(status, 200, "{list}");
    let sources = list["sources"].as_array().unwrap();
    let plugins: Vec<&str> = sources
        .iter()
        .map(|s| s["plugin"].as_str().unwrap())
        .collect();
    assert_eq!(plugins, ["dht", "feed"]);
    let dht = &sources[0];
    assert_eq!(dht["consent"], json!("none"));
    assert_eq!(dht["kinds"], json!(["discovery", "peer_finder"]));
    assert_eq!(dht["state"], json!("enabled"));
    assert_eq!(dht["restart_required"], json!(false));
    assert_eq!(dht["health"]["running"], json!(true));
    let feed = sources[1]["id"].as_u64().unwrap();

    // Priority applies and is saved; enabling state waits for a restart.
    let uri = format!("/api/v1/sources/{feed}");
    let (status, high) = f
        .send(req("PATCH", &uri, c, Some(r#"{"priority":"high"}"#)))
        .await;
    assert_eq!(
        (status, high["priority"].clone()),
        (200, json!("high")),
        "{high}"
    );
    let saved = f
        .control_store
        .lock()
        .unwrap()
        .setting_text("sources.priority.feed")
        .unwrap();
    assert_eq!(saved.as_deref(), Some("\"high\""));
    let (status, off) = f
        .send(req("PATCH", &uri, c, Some(r#"{"enabled":false}"#)))
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (off["state"].clone(), off["restart_required"].clone()),
        (json!("disabled"), json!(true))
    );
    assert_eq!(
        off["health"]["running"],
        json!(true),
        "it stops at the next start"
    );
    let (status, e) = f.send(req("PATCH", &uri, c, Some(r#"{"agents":4}"#))).await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (409, json!("not_editable"))
    );
    assert_eq!(e["error"]["field"], json!("agents"));
    let (status, e) = f
        .send(req(
            "PATCH",
            "/api/v1/sources/63",
            c,
            Some(r#"{"enabled":true}"#),
        ))
        .await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (404, json!("unknown_source"))
    );
    let (status, _) = f
        .send(req("PATCH", "/api/v1/sources/200", c, Some("{}")))
        .await;
    assert_eq!(status, 404);

    // The node has nobody to bootstrap from: unknown, with the reason.
    let (status, check) = f.send(req("GET", "/api/v1/network/check", c, None)).await;
    assert_eq!(status, 200, "{check}");
    assert_eq!(check["reachable"], Value::Null);
    assert_eq!(check["reason"], json!("bootstrapping"));
    assert_eq!(check["mapping"], json!("unavailable"));
    assert_eq!(check["external"]["ip"], Value::Null);
    let (status, _) = f
        .send(req("POST", "/api/v1/network/check", c, Some("{}")))
        .await;
    assert_eq!(status, 200);
    let (status, e) = f
        .send(req("POST", "/api/v1/network/check", c, Some("{}")))
        .await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (429, json!("rate_limited"))
    );
    f.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_toggles_leave_the_process_as_the_registry_says() {
    let f = fixture().await;
    let cookie = f.session().await;
    for round in 0..5 {
        let mut calls = tokio::task::JoinSet::new();
        for i in 0..16 {
            let path = if (i + round) % 2 == 0 {
                "pause"
            } else {
                "resume"
            };
            let mut r = req(
                "POST",
                &format!("/api/v1/crawler/{path}"),
                &cookie,
                Some("{}"),
            );
            r.extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 1], 1))));
            let app = f.app.clone();
            calls.spawn(async move { app.oneshot(r).await.unwrap().status().as_u16() });
        }
        while let Some(status) = calls.join_next().await {
            assert_eq!(status.unwrap(), 200);
        }
        // Whatever order they finished in, the process is as saved.
        let saved = f.registry.get::<settings::crawler::Crawler>().paused;
        assert_eq!(f.ops.control.status().paused, saved, "round {round}");
        // And so is the DHT node, which applies it in its own task.
        for _ in 0..200 {
            if node_paused(&f.dht).await == Some(saved) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(node_paused(&f.dht).await, Some(saved), "round {round}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dht_disabled_in_the_store_reads_as_off() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let dht = Arc::new(DhtSource::new(lodestar_dht::node::Config {
        bind: vec![SocketAddr::from(([127, 0, 0, 1], free_udp_port()))],
        bootstrap: Vec::new(),
        ..lodestar_dht::node::Config::default()
    }));
    {
        let m = dht.manifest();
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let id = store.register_source(m.id, m.version, 0).unwrap();
        store
            .set_source_state(id, lodestar_store::SourceState::Disabled)
            .unwrap();
    }
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![dht.clone(), Arc::new(Feed)],
        Arc::new(Counting(Arc::default())),
    )
    .await
    .unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let control = Control::start(
        pipeline.monitor(),
        Some(dht),
        store,
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
    // Not `bootstrapping` forever: off, and nothing to show.
    let check = control.network();
    assert_eq!(check.reach.reason, control::ReachReason::DhtOff);
    assert_eq!(check.reach.reachable, None);
    assert_eq!(control.status().dht, None);
    pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pause_survives_a_restart() {
    let mut f = fixture().await;
    let cookie = f.session().await;
    f.send(req("POST", "/api/v1/crawler/pause", &cookie, Some("{}")))
        .await;
    let at = f.ops.control.status().since;
    f.stop().await;
    let db = f.dir.path().join("t.db");
    drop(f.registry);

    // The store reopened: the registry has the pause, and a control
    // started from it dates the pause from the event.
    let store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let config = Config::load(None, |_| None).unwrap();
    let registry = Registry::load(&config, store.clone()).unwrap();
    let crawler = registry.get::<settings::crawler::Crawler>();
    assert!(crawler.paused);
    let pipeline = Pipeline::start_with(
        PipelineConfig {
            pool: PoolConfig {
                paused: crawler.paused,
                ..PoolConfig::default()
            },
            ..PipelineConfig::default()
        },
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![Arc::new(Feed)],
        Arc::new(Counting(f.starts.clone())),
    )
    .await
    .unwrap();
    let control = Control::start(pipeline.monitor(), None, store, true, Intensity::Balanced)
        .await
        .unwrap();
    let status = control.status();
    assert!(status.paused);
    assert_eq!(status.since, at);
    // Started paused: nothing is fetched.
    let before = f.starts.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(f.starts.load(Ordering::SeqCst), before);
    pipeline.shutdown().await.unwrap();
}

#[test]
fn the_first_run_log_names_a_reachable_address() {
    assert_eq!(
        ui_address("0.0.0.0:8090".parse().unwrap()),
        "http://<this host's address>:8090/"
    );
    assert_eq!(
        ui_address("[::]:8090".parse().unwrap()),
        "http://<this host's address>:8090/"
    );
    assert_eq!(
        ui_address("192.168.1.20:8090".parse().unwrap()),
        "http://192.168.1.20:8090/"
    );
    assert_eq!(ui_address("[::1]:80".parse().unwrap()), "http://[::1]:80/");
}

#[test]
fn index_lag_is_zero_when_level() {
    // Nothing new stored: the index has nothing to catch up with.
    assert_eq!(index_lag(0, 0), Some(0));
    assert_eq!(index_lag(0, 100), Some(0));
    // New torrents, no index commit yet: unknown.
    assert_eq!(index_lag(100, 0), None);
    assert_eq!(index_lag(105, 100), Some(5));
    // The index committed after the last new torrent: level, however
    // long ago that was (rediscoveries don't move `last_stored`).
    assert_eq!(index_lag(100, 105), Some(0));
}

#[test]
fn dir_size_counts_nested_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), [0u8; 10]).unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/b"), [0u8; 5]).unwrap();
    assert_eq!(dir_size(dir.path()), Some(15));
    assert_eq!(dir_size(&dir.path().join("missing")), None);
}
