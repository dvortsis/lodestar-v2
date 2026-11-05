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
use lodestar_sources::consent;
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
    always_adult: false,
    finds_private: false,
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
                    // Paces the source: one key per tick is the load under test.
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
            // Simulates a peer that takes a moment before it refuses.
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
    let ops = Arc::new(
        Ops::new(
            control,
            registry.subscribe(),
            pipeline.monitor(),
            None,
            dir.path().to_path_buf(),
        )
        .with_dht(Some(DhtReport {
            stats: dht.stats_handle(),
            node: dht.node_handle(),
        })),
    );
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
                json!({ "token": token, "password": "correct horse battery", "notice_version": lodestar_api::legal::NOTICE_VERSION }).to_string(),
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

/// Fetch starts over `ms`. The window is the point: it checks that
/// something does or doesn't start within a bound, so it stays a sleep.
async fn starts_during(f: &Fixture, ms: u64) -> usize {
    let before = f.starts.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    f.starts.load(Ordering::SeqCst) - before
}

/// Polls `check` every 25 ms for up to 15 s, then panics saying `what`
/// it was waiting for and what `seen` reports.
async fn eventually(what: &str, seen: impl Fn() -> String, mut check: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after 15 s waiting for {what}; last saw {}",
            seen()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Waits until fetch starts stop changing between two reads 100 ms
/// apart. A pause applies before the API answers, so this settles at
/// once; the 1 s bound is there so a pool that keeps starting fetches
/// after a pause fails here instead of passing.
async fn starts_settled(f: &Fixture) {
    let mut last = usize::MAX;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let now = f.starts.load(Ordering::SeqCst);
        if now == last {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fetch starts kept changing for 1 s, last saw {now}"
        );
        last = now;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
    match tokio::time::timeout(Duration::from_secs(10), socket.recv_from(&mut buf)).await {
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
    eventually(
        "more than 10 fetch starts",
        || f.starts.load(Ordering::SeqCst).to_string(),
        || f.starts.load(Ordering::SeqCst) > 10,
    )
    .await;
    eventually("DHT stats", || "none".into(), || f.dht.stats().is_some()).await;

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
    starts_settled(&f).await;
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
    eventually(
        "the low and aggressive intensity events",
        || format!("{:?}", f.events(event::INTENSITY)),
        || f.events(event::INTENSITY) == ["low", "aggressive"],
    )
    .await;
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
    eventually(
        "two pause events",
        || format!("{:?}", f.events(event::PAUSE)),
        || f.events(event::PAUSE).len() == 2,
    )
    .await;
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
    eventually(
        "more than 5 fetch starts",
        || f.starts.load(Ordering::SeqCst).to_string(),
        || f.starts.load(Ordering::SeqCst) > 5,
    )
    .await;
    // Pausing holds the counters still, so the two reads agree.
    f.send(req("POST", "/api/v1/crawler/pause", c, Some("{}")))
        .await;
    starts_settled(&f).await;
    // The Feed source keeps emitting while paused, so the two reads are only
    // compared once they agree on the fields that move; try until they do.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let (stats, metric) = loop {
        let (status, stats) = f.send(req("GET", "/api/v1/stats", c, None)).await;
        assert_eq!(status, 200, "{stats}");
        let monitor = f.ops.monitor.clone();
        let metrics = crate::metrics::render(&monitor, f.dht.stats().as_ref(), &Default::default());
        let metric = move |name: &str| -> u64 {
            metrics
                .lines()
                .find_map(|l| l.strip_prefix(&format!("lodestar_{name} ")))
                .unwrap_or_else(|| panic!("no {name} in /metrics"))
                .parse()
                .unwrap()
        };
        if stats["stored_torrents"] == json!(metric("torrents"))
            && stats["queue_depth"] == json!(metric("fetch_pending"))
        {
            break (stats, metric);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "after 15 s /stats said {stats} but /metrics said torrents {} fetch_pending {}",
            metric("torrents"),
            metric("fetch_pending")
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
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
    // The running node, with the restart proof off, holds its samples
    // back for the whole suppression and says why.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let blocked = loop {
        let (_, stats) = f.send(req("GET", "/api/v1/stats", c, None)).await;
        if !stats["sampling_blocked"].is_null() {
            break stats["sampling_blocked"].clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no sampling_blocked after 15 s: {stats}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(blocked["reason"], json!("off"), "{blocked}");
    assert!(blocked["remaining_s"].as_u64().unwrap() > 0, "{blocked}");
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
    // Purge waits for SRC-14b.
    let (status, e) = f
        .send(req("PATCH", &uri, c, Some(r#"{"purge":true}"#)))
        .await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (409, json!("not_editable"))
    );
    // The plugin declares one agent: more is refused, and nothing is saved.
    let (status, e) = f.send(req("PATCH", &uri, c, Some(r#"{"agents":4}"#))).await;
    assert_eq!(
        (
            status,
            e["error"]["code"].clone(),
            e["error"]["field"].clone()
        ),
        (400, json!("invalid_agents"), json!("agents"))
    );
    // A schedule is saved and shown, and says it isn't enforced yet.
    let (status, saved) = f
        .send(req(
            "PATCH",
            &uri,
            c,
            Some(r#"{"schedule":["01:00-07:00"]}"#),
        ))
        .await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(
        saved["schedule"],
        json!({ "default": [], "enforced": false, "windows": ["01:00-07:00"] })
    );
    assert_eq!(
        saved["agents"],
        json!({ "count": 1, "enforced": false, "max": 1 })
    );
    // A route change needs the password, and an interface is refused.
    let (status, e) = f
        .send(req(
            "PATCH",
            &uri,
            c,
            Some(r#"{"egress":{"route":"socks5","proxy":"10.8.0.1:1080"}}"#),
        ))
        .await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (428, json!("confirm_required"))
    );
    let (status, e) = f
        .send(req(
            "PATCH",
            &uri,
            c,
            Some(r#"{"egress":{"route":"interface"}}"#),
        ))
        .await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (409, json!("route_unavailable"))
    );
    let (_, list) = f.send(req("GET", "/api/v1/sources", c, None)).await;
    assert!(
        list.to_string().contains(r#""setting":{"route":"direct"}"#),
        "{list}"
    );
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while node_paused(&f.dht).await != Some(saved) && tokio::time::Instant::now() < deadline {
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
    // A negative check: nothing starts over a fixed window, so it stays a sleep.
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
fn index_lag_is_zero_when_level_and_grows_while_stalled() {
    // Everything stored is searchable: level, however long ago the
    // index last committed (or if it never did, on an empty store).
    assert_eq!(index_lag(1000, 50, 50, 10), Some(0));
    assert_eq!(index_lag(1000, 0, 0, 0), Some(0));
    // Behind, and no commit since the start (a backlog after a
    // restart): unknown, not 0.
    assert_eq!(index_lag(1000, 50, 20, 0), None);
    // Behind: the time since the last commit, growing while it stalls.
    assert_eq!(index_lag(1000, 50, 20, 995), Some(5));
    assert_eq!(index_lag(1600, 50, 20, 995), Some(605));
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

#[test]
fn sampling_blocked_follows_the_dht_report() {
    use lodestar_dht::restart::Reason;
    let report = |seconds, reason| StatsSnapshot {
        sample_suppression_seconds: seconds,
        sample_suppression_reason: reason,
        ..StatsSnapshot::default()
    };
    for reason in [Reason::Off, Reason::Dirty, Reason::InvalidateFailed] {
        assert_eq!(
            sampling_blocked(report(21_540, reason)),
            Some(SamplingBlocked {
                reason: reason.as_str().into(),
                remaining_s: 21_540
            })
        );
    }
    // Sampling: a resumed run, or a suppression that has run out.
    assert_eq!(sampling_blocked(report(0, Reason::Resumed)), None);
    assert_eq!(sampling_blocked(report(0, Reason::Dirty)), None);
    // A report outlives its node (it failed to start, or stopped): no
    // longer what the DHT is doing, so nothing is said.
    let stopped = DhtReport {
        stats: Arc::new(Mutex::new(Some(report(21_540, Reason::Off)))),
        node: Arc::default(),
    };
    assert_eq!(stopped.live(), None);
}

/// A source of each class, doing nothing while it runs.
struct Gated(Manifest, Arc<std::sync::atomic::AtomicBool>);

impl SourcePlugin for Gated {
    fn manifest(&self) -> &Manifest {
        &self.0
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        self.1.store(true, Ordering::SeqCst);
        Box::pin(async move {
            ctx.shutdown.requested().await;
            Ok(())
        })
    }
}

const GATED: [(&str, Consent); 5] = [
    ("none", Consent::None),
    ("external", Consent::External),
    ("account", Consent::Account),
    ("restricted", Consent::Restricted),
    ("overlay", Consent::Overlay),
];

fn gated() -> Vec<(Arc<dyn SourcePlugin>, Arc<std::sync::atomic::AtomicBool>)> {
    GATED
        .iter()
        .map(|(id, consent)| {
            let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let m = Manifest {
                id,
                name: id,
                version: "1",
                kinds: &[Kind::Discovery],
                networks: &[Network::Bt],
                endpoints: &[lodestar_sources::Endpoint {
                    protocol: "https",
                    host: "example.org",
                }],
                risk: Risk::High,
                consent: *consent,
                default_priority: Priority::Normal,
                enabled_by_default: false,
                always_adult: false,
                finds_private: false,
            };
            (
                Arc::new(Gated(m, ran.clone())) as Arc<dyn SourcePlugin>,
                ran,
            )
        })
        .collect()
}

struct GatedFixture {
    app: Router,
    auth: Arc<Auth>,
    pipeline: Pipeline,
    dir: tempfile::TempDir,
}

async fn gated_fixture(
    dir: tempfile::TempDir,
    plugins: Vec<Arc<dyn SourcePlugin>>,
) -> GatedFixture {
    let config = Config::load(None, |_| None).unwrap();
    let db = dir.path().join("t.db");
    let control_store = Arc::new(Mutex::new(
        Store::open(&db, StoreConfig::default()).unwrap(),
    ));
    let registry = Arc::new(Registry::load(&config, control_store.clone()).unwrap());
    let pipeline = Pipeline::start_with(
        PipelineConfig::default(),
        Store::open(&db, StoreConfig::default()).unwrap(),
        plugins,
        Arc::new(Counting::default()),
    )
    .await
    .unwrap();
    let control = Control::start(
        pipeline.monitor(),
        None,
        control_store,
        false,
        Intensity::Balanced,
    )
    .await
    .unwrap();
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
            settings: Some(registry),
            ops: Some(ops),
            user_data: None,
            adult: Default::default(),
            insights: None,
            hot: None,
            events: None,
        }),
    );
    GatedFixture {
        app,
        auth,
        pipeline,
        dir,
    }
}

impl GatedFixture {
    async fn send(&self, req: Request<Body>) -> (u16, Value) {
        self.send_from(1, req).await
    }

    /// From client `10.9.0.<host>`: password checks are rate-limited per
    /// address (a burst of 5).
    async fn send_from(&self, host: u8, mut req: Request<Body>) -> (u16, Value) {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, host], 1))));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(!text.contains(SECRET), "a credential left in a response");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::String(text)),
        )
    }

    async fn session(&self) -> String {
        let token = self.auth.setup_token().unwrap();
        let mut req = Request::post("/api/v1/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "token": token, "password": "correct horse battery", "notice_version": lodestar_api::legal::NOTICE_VERSION }).to_string(),
            ))
            .unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([10, 9, 0, 2], 1))));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let cookie = resp.headers()[header::SET_COOKIE].to_str().unwrap();
        cookie.split(';').next().unwrap().to_string()
    }

    async fn source(&self, cookie: &str, plugin: &str) -> Value {
        let (status, list) = self.send(req("GET", "/api/v1/sources", cookie, None)).await;
        assert_eq!(status, 200);
        list["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["plugin"] == json!(plugin))
            .unwrap()
            .clone()
    }

    async fn patch(&self, cookie: &str, plugin: &str, body: Value) -> (u16, Value) {
        self.patch_from(1, cookie, plugin, body).await
    }

    async fn patch_from(&self, host: u8, cookie: &str, plugin: &str, body: Value) -> (u16, Value) {
        let id = self.source(cookie, plugin).await["id"].clone();
        self.send_from(
            host,
            req(
                "PATCH",
                &format!("/api/v1/sources/{id}"),
                cookie,
                Some(&body.to_string()),
            ),
        )
        .await
    }
}

/// A credential value no response, export or file but the key-sealed
/// store may hold.
const SECRET: &str = "s3cret-Credential-Value";

/// No file under `dir` (the database, its WAL, the key file) holds
/// [`SECRET`] in plain text: the database holds it only sealed.
fn assert_no_plaintext(dir: &std::path::Path) {
    let needle = SECRET.as_bytes();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let bytes = std::fs::read(&p).unwrap();
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "{} holds the credential in plain text",
                    p.display()
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_class_can_be_enabled_past_its_condition() {
    let plugins = gated();
    let f = gated_fixture(
        tempfile::tempdir().unwrap(),
        plugins.iter().map(|(p, _)| p.clone()).collect(),
    )
    .await;
    let c = f.session().await;

    // Every class starts off, and says what it needs.
    for (id, need) in [
        ("none", Value::Null),
        ("external", json!("endpoints_ack")),
        ("account", json!("credentials")),
        ("restricted", json!("consent")),
        ("overlay", json!("consent")),
    ] {
        let s = f.source(&c, id).await;
        assert_eq!(
            (s["enabled"].clone(), s["needs"].clone()),
            (json!(false), need),
            "{id}"
        );
    }
    // None: a toggle.
    assert_eq!(f.patch(&c, "none", json!({ "enabled": true })).await.0, 200);

    // A settings import can't enable a source, record consent or plant a
    // credential: the registry has no such field, and source state isn't
    // settings. The forged record uses the real version, so it would
    // count if it landed.
    let overlay_version = f.source(&c, "overlay").await["consent_version"]
        .as_str()
        .unwrap()
        .to_string();
    let forged = format!("overlay|{overlay_version}|1");
    let db = f.dir.path().join("t.db");
    let sources_keys = || {
        Store::open_reader(&db, StoreConfig::default())
            .unwrap()
            .connection()
            .prepare("SELECT key FROM settings WHERE key LIKE 'sources.%' ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let before = sources_keys();
    for toml in [
        "lodestar_settings = 1\n[sources]\noverlay = true\n".to_string(),
        format!("lodestar_settings = 1\n[\"sources.consent\"]\noverlay = \"{forged}\"\n"),
        format!("lodestar_settings = 1\n[sources.consent]\noverlay = \"{forged}\"\n"),
        format!("lodestar_settings = 1\n[crawler]\n\"sources.consent.overlay\" = \"{forged}\"\n"),
        "lodestar_settings = 1\n[sources.secret]\n\"account/api_key\" = \"x\"\n".to_string(),
    ] {
        for dry_run in ["true", "false"] {
            // An unknown section may be refused or ignored; either way
            // nothing below may change.
            let (status, report) = f
                .send(req(
                    "POST",
                    &format!("/api/v1/settings/import?dry_run={dry_run}"),
                    &c,
                    Some(&json!({ "toml": toml }).to_string()),
                ))
                .await;
            if status == 200 {
                assert_eq!(report["changes"], json!([]), "{toml}: {report}");
            }
        }
        let s = f.source(&c, "overlay").await;
        assert_eq!(
            (s["enabled"].clone(), s["needs"].clone()),
            (json!(false), json!("consent")),
            "{toml}"
        );
        assert_eq!(sources_keys(), before, "{toml}");
    }

    // External: refused without the acknowledgement, or with a stale or
    // borrowed one; accepted with its own, no password.
    let ext = f.source(&c, "external").await;
    let version = ext["consent_version"].as_str().unwrap().to_string();
    assert_eq!(ext["endpoints"], json!(["https://example.org"]));
    for body in [
        json!({ "enabled": true }),
        json!({ "enabled": true, "consent": "external" }),
        json!({ "enabled": true, "consent": "external", "consent_version": "0-0" }),
        json!({ "enabled": true, "consent": "none", "consent_version": version }),
        json!({ "enabled": true, "consent": "overlay", "consent_version": version }),
    ] {
        let (status, e) = f.patch(&c, "external", body.clone()).await;
        assert_eq!(
            (status, e["error"]["code"].clone()),
            (409, json!("consent_required")),
            "{body}"
        );
    }
    let (status, s) = f
        .patch(
            &c,
            "external",
            json!({ "enabled": true, "consent": "external", "consent_version": version }),
        )
        .await;
    assert_eq!(
        (status, s["enabled"].clone(), s["needs"].clone()),
        (200, json!(true), Value::Null)
    );

    // Account: refused until credentials are stored, whatever is sent.
    for body in [
        json!({ "enabled": true }),
        json!({ "enabled": true, "consent": "account", "consent_version": "" }),
    ] {
        let (status, e) = f.patch(&c, "account", body).await;
        assert_eq!(
            (status, e["error"]["code"].clone()),
            (409, json!("credentials_required"))
        );
    }
    let id = f.source(&c, "account").await["id"].clone();
    let creds = |body: Value| {
        req(
            "PATCH",
            &format!("/api/v1/sources/{id}/credentials"),
            &c,
            Some(&body.to_string()),
        )
    };
    let (status, s) = f
        .send(creds(json!({ "credentials": { "api_key": SECRET } })))
        .await;
    assert_eq!(
        (status, s["credentials"].clone()),
        (200, json!(["api_key"]))
    );
    let (status, e) = f
        .send(creds(json!({ "credentials": { "Bad/Name": SECRET } })))
        .await;
    assert_eq!(
        (status, e["error"]["code"].clone()),
        (400, json!("invalid_credentials"))
    );
    assert_eq!(
        f.patch(&c, "account", json!({ "enabled": true })).await.0,
        200
    );

    // Restricted and Overlay: the consent screen at the current version,
    // accepted with the admin password, every time it's accepted.
    for (host, id) in [(10, "restricted"), (11, "overlay")] {
        let version = f.source(&c, id).await["consent_version"].clone();
        let (status, e) = f.patch_from(host, &c, id, json!({ "enabled": true })).await;
        assert_eq!(
            (status, e["error"]["code"].clone()),
            (409, json!("consent_required"))
        );
        let ack = json!({ "enabled": true, "consent": id, "consent_version": version });
        let (status, e) = f.patch_from(host, &c, id, ack.clone()).await;
        assert_eq!(
            (status, e["error"]["code"].clone()),
            (428, json!("confirm_required")),
            "{id}"
        );
        let mut wrong = ack.clone();
        wrong["password"] = json!("not the password");
        let (status, _) = f.patch_from(host, &c, id, wrong).await;
        assert_eq!(status, 401, "{id}");
        assert_eq!(f.source(&c, id).await["enabled"], json!(false));
        let mut right = ack;
        right["password"] = json!("correct horse battery");
        let (status, s) = f.patch_from(host, &c, id, right).await;
        assert_eq!((status, s["enabled"].clone()), (200, json!(true)), "{id}");
        // Off and on again: the acknowledgement stands, the password is
        // asked every time.
        assert_eq!(
            f.patch_from(host, &c, id, json!({ "enabled": false }))
                .await
                .0,
            200
        );
        let (status, e) = f.patch_from(host, &c, id, json!({ "enabled": true })).await;
        assert_eq!(
            (status, e["error"]["code"].clone()),
            (428, json!("confirm_required")),
            "{id}"
        );
        let again = json!({ "enabled": true, "password": "correct horse battery" });
        let (status, s) = f.patch_from(host, &c, id, again).await;
        assert_eq!((status, s["enabled"].clone()), (200, json!(true)), "{id}");
    }

    // Settings: an import can't enable a source or record consent, and
    // the export carries no credential.
    let (status, export) = f
        .send(req("GET", "/api/v1/settings/export", &c, None))
        .await;
    assert_eq!(status, 200);
    let text = export.to_string();
    assert!(
        !text.contains(SECRET) && !text.contains("api_key"),
        "{text}"
    );
    // While it's stored, it's stored sealed.
    assert_no_plaintext(f.dir.path());

    // Disabling keeps the acknowledgement; removing the credentials of an
    // enabled Account source switches it off.
    let (status, s) = f
        .send(creds(json!({ "credentials": { "api_key": null } })))
        .await;
    assert_eq!(
        (status, s["enabled"].clone(), s["needs"].clone()),
        (200, json!(false), json!("credentials"))
    );

    // And after it's removed.
    assert_no_plaintext(f.dir.path());
    f.pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_honours_consent_and_a_new_text_asks_again() {
    let plugins = gated();
    let ran: Vec<_> = plugins.iter().map(|(_, r)| r.clone()).collect();
    let f = gated_fixture(
        tempfile::tempdir().unwrap(),
        plugins.iter().map(|(p, _)| p.clone()).collect(),
    )
    .await;
    let c = f.session().await;
    let version = f.source(&c, "restricted").await["consent_version"].clone();
    let (status, _) = f
        .patch(
            &c,
            "restricted",
            json!({ "enabled": true, "consent": "restricted", "consent_version": version,
                    "password": "correct horse battery" }),
        )
        .await;
    assert_eq!(status, 200);
    let ext_version = f.source(&c, "external").await["consent_version"].clone();
    let (status, _) = f
        .patch(
            &c,
            "external",
            json!({ "enabled": true, "consent": "external", "consent_version": ext_version }),
        )
        .await;
    assert_eq!(status, 200);
    let GatedFixture { pipeline, dir, .. } = f;
    pipeline.shutdown().await.unwrap();

    // Same build: the acknowledged source runs, nothing else does.
    let f = gated_fixture(dir, plugins.iter().map(|(p, _)| p.clone()).collect()).await;
    eventually("the acknowledged sources run", String::new, || {
        ran[1].load(Ordering::SeqCst) && ran[3].load(Ordering::SeqCst)
    })
    .await;
    for s in f.pipeline.sources() {
        let acked = s.plugin == "restricted" || s.plugin == "external";
        assert_eq!(s.enabled, acked, "{}", s.plugin);
    }
    let GatedFixture { pipeline, dir, .. } = f;
    pipeline.shutdown().await.unwrap();

    // A plugin update that contacts a new host is a new version: it's
    // off after the restart, and asks again.
    static MOVED: [lodestar_sources::Endpoint; 1] = [lodestar_sources::Endpoint {
        protocol: "https",
        host: "elsewhere.example",
    }];
    // Both an External and a Restricted plugin move.
    let mut plugins = gated();
    let mut moved_ran = Vec::new();
    for (i, before) in [(1, &ext_version), (3, &version)] {
        let r = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut m = plugins[i].0.manifest().clone();
        m.endpoints = &MOVED;
        plugins[i] = (Arc::new(Gated(m, r.clone())), r.clone());
        assert_ne!(&json!(consent::version(plugins[i].0.manifest())), before);
        moved_ran.push(r);
    }
    let f = gated_fixture(dir, plugins.iter().map(|(p, _)| p.clone()).collect()).await;
    let db = f.dir.path().join("t.db");
    let rows = Store::open(&db, StoreConfig::default())
        .unwrap()
        .sources()
        .unwrap();
    for id in ["external", "restricted"] {
        let row = rows.iter().find(|r| r.plugin == id).unwrap();
        assert_eq!(row.state, lodestar_store::SourceState::Disabled, "{id}");
    }
    // Instant checks: the stored state above is what carries the test.
    assert!(f.pipeline.sources().iter().all(|s| !s.running));
    assert!(moved_ran.iter().all(|r| !r.load(Ordering::SeqCst)));
    f.pipeline.shutdown().await.unwrap();
}
