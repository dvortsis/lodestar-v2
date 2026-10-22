//! Swarm health end to end: stored torrents → the scrape scheduler → fake
//! trackers → the writer → SQLite and the hot columns.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::health::HealthConfig;
use lodestar_pipeline::{HealthSetup, Pipeline, PipelineConfig, metric, open_hot, save_hot};
use lodestar_sources::{
    BoxFuture, Consent, Found, Kind, Manifest, Network, PluginError, Priority, Risk, SourceCtx,
    SourcePlugin,
};
use lodestar_store::{
    Col, HotColumns, NEVER, NewTorrent, Store, StoreConfig, SwarmUpdate, WriteBatch,
};
use lodestar_tracker_scrape::testing::{http_tracker, udp_tracker};
use lodestar_tracker_scrape::{Counts, HttpConfig, TrackerUrl, UdpConfig};
use tokio::sync::mpsc;

const SWARM: Counts = Counts {
    seeders: 12,
    completed: 300,
    leechers: 4,
};

fn dict(name: &str, private: bool) -> Vec<u8> {
    let mut d = BTreeMap::new();
    d.insert(b"length".to_vec(), OwnedValue::Int(5));
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(16_384));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20]));
    if private {
        d.insert(b"private".to_vec(), OwnedValue::Int(1));
    }
    to_vec(&OwnedValue::Dict(d))
}

/// Stores torrents named `names` (private when the name says so).
fn seed(path: &std::path::Path, names: &[&str]) -> Vec<(i64, InfoHash)> {
    let mut store = Store::open(path, StoreConfig::default()).unwrap();
    let source = store.register_source("test", "1", 1).unwrap();
    names
        .iter()
        .map(|name| {
            let raw = dict(name, name.starts_with("private"));
            let info = Info::parse(&raw).unwrap();
            let id = store
                .insert(&[NewTorrent {
                    info: &info,
                    raw_info: Some(&raw),
                    discovered_at: 1,
                    metadata_at: 1,
                    extra_flags: 0,
                    source,
                }])
                .unwrap()[0]
                .id();
            (id, info.primary_hash())
        })
        .collect()
}

fn health(path: &std::path::Path, trackers: Vec<String>) -> HealthSetup {
    HealthSetup {
        config: HealthConfig {
            interval: Duration::from_millis(50),
            http_interval: Duration::from_millis(50),
            round_deadline: Duration::from_secs(3),
            refill_every: Duration::from_millis(50),
            dht: false,
            udp: UdpConfig {
                timeout: Duration::from_millis(200),
                retries: 0,
                ..UdpConfig::default()
            },
            ..HealthConfig::default()
        },
        trackers: trackers
            .iter()
            .map(|t| TrackerUrl::parse(t).unwrap())
            .collect(),
        db: path.to_path_buf(),
        store: StoreConfig::default(),
        dht: None,
    }
}

fn config(hot: &Arc<HotColumns>, health: HealthSetup) -> PipelineConfig {
    PipelineConfig {
        flush_every: Duration::from_millis(50),
        batch_wait: Duration::from_millis(10),
        hot: Some(hot.clone()),
        health: Some(health),
        ..PipelineConfig::default()
    }
}

async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A source the test drives through a channel.
struct Scripted {
    manifest: Manifest,
    rx: Mutex<Option<mpsc::Receiver<Found>>>,
}

fn manifest() -> Manifest {
    Manifest {
        id: "scripted",
        name: "scripted",
        version: "1",
        kinds: &[Kind::Discovery],
        networks: &[Network::Bt],
        endpoints: &[],
        risk: Risk::Low,
        consent: Consent::None,
        default_priority: Priority::Normal,
        enabled_by_default: true,
    }
}

impl SourcePlugin for Scripted {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        let mut rx = self.rx.lock().unwrap().take().expect("run once");
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(f) = rx.recv() => {
                        ctx.emitter.emit(f).await.map_err(|_| PluginError("closed".into()))?;
                    }
                }
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrapes_new_torrents_into_the_store_and_hot_columns() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let t = seed(&db, &["alive", "dead", "private one"]);
    let (alive, dead, private) = (t[0], t[1], t[2]);
    let tracker = udp_tracker().await;
    tracker.state.set(alive.1, SWARM);

    let (tx, rx) = mpsc::channel(8);
    let plugin = Arc::new(Scripted {
        manifest: manifest(),
        rx: Mutex::new(Some(rx)),
    });
    let hot = Arc::new(HotColumns::with_epoch_every(Duration::ZERO));
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(
        config(&hot, health(&db, vec![tracker.url(true)])),
        store,
        vec![plugin],
    )
    .await
    .unwrap();
    let id = |t: (i64, InfoHash)| t.0 as u64;
    until("both public torrents scraped", || {
        hot.get(Col::Seeders, id(alive)) == 12 && hot.get(Col::Seeders, id(dead)) == 0
    })
    .await;
    assert_eq!(hot.get(Col::Leechers, id(alive)), 4);
    assert_eq!(hot.get(Col::Trackers, id(alive)), 1);
    assert_eq!(hot.get(Col::Trackers, id(dead)), 0, "zeros don't count");
    assert_eq!(hot.get(Col::Seeders, id(private)), NEVER, "never scraped");

    // A rediscovery moves last-seen at once.
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    tx.send(Found::key(alive.1, 0)).await.unwrap();
    until("last seen moved", || {
        let seen = hot.get(Col::LastSeen, id(alive));
        seen != NEVER && seen >= before
    })
    .await;

    let monitor = pipeline.monitor();
    let h = monitor.health().unwrap();
    // Just scraped: a look at it, or a rediscovery, doesn't scrape again.
    // (A torrent with both hints queued is taken once.)
    let get = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
    let rounds = get(&h.stats().rounds);
    h.interest(alive.0);
    until("fresh hints skipped", || get(&h.stats().fresh_skipped) >= 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(get(&h.stats().rounds), rounds, "no rescrape");
    assert_eq!(h.trackers().len(), 1);
    assert!(h.trackers()[0].rounds[0] >= 1, "{:?}", h.trackers());
    let store = pipeline.shutdown().await.unwrap();

    let row = store.swarm(alive.0).unwrap().unwrap();
    assert_eq!((row.seeders, row.leechers, row.completed), (12, 4, 300));
    assert_eq!((row.trackers_reporting, row.trackers_checked), (1, 1));
    assert!(
        row.next_scrape_at > row.last_scrape_at + 5 * 3600,
        "alive: ~6 h"
    );
    let row = store.swarm(dead.0).unwrap().unwrap();
    assert_eq!((row.trackers_reporting, row.trackers_checked), (0, 1));
    assert!(
        row.next_scrape_at > row.last_scrape_at + 40 * 3600,
        "dead: ~2 d"
    );
    assert!(store.swarm(private.0).unwrap().is_none());
    assert!(store.history(alive.0).unwrap().is_some());
    assert!(store.history(dead.0).unwrap().is_none());
    assert_eq!(store.swarm_trackers(alive.0).unwrap().len(), 1);
    assert_eq!(store.trackers().unwrap().len(), 1);
    let requests = store.rollup(metric::SCRAPE_REQUESTS, 0, i64::MAX).unwrap();
    assert!(
        requests.iter().any(|c| c.key == 0 && c.value >= 1),
        "{requests:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_http_tracker_answers_rounds_spanning_several_requests() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let names: Vec<String> = (0..40).map(|i| format!("multi {i}")).collect();
    let t = seed(&db, &names.iter().map(String::as_str).collect::<Vec<_>>());
    let tracker = http_tracker().await;
    for &(_, hash) in &t {
        tracker.state.set(hash, SWARM);
    }
    let mut setup = health(&db, vec![tracker.url(false)]);
    // Two rounds of four requests each, back to back: the second must
    // wait for the first's last slot rather than be skipped.
    setup.config.round_size = 20;
    setup.config.http = HttpConfig {
        max_hashes: 5,
        ..HttpConfig::default()
    };
    let hot = Arc::new(HotColumns::with_epoch_every(Duration::ZERO));
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(&hot, setup), store, Vec::new())
        .await
        .unwrap();
    until("every torrent scraped", || {
        t.iter()
            .all(|&(id, _)| hot.get(Col::Seeders, id as u64) == 12)
    })
    .await;
    let h = pipeline.monitor().health().unwrap().clone();
    let rounds = h.trackers()[0].rounds;
    assert_eq!(
        rounds,
        [2, 0, 0, 0, 0],
        "both rounds answered, none skipped"
    );
    pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_due_rescrape_keeps_the_dht_estimate_it_didnt_ask_for() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let t = seed(&db, &["due"]);
    {
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let swarm = [SwarmUpdate {
            torrent_id: t[0].0,
            at: 1,
            seeders: 5,
            leechers: 9,
            completed: 0,
            trackers_reporting: 0,
            trackers_checked: 1,
            dht: Some((5, 9)),
            per_tracker: Vec::new(),
            next_scrape_at: 2,
            flags: 0,
            interest: false,
        }];
        store
            .write(&WriteBatch {
                swarm: &swarm,
                ..WriteBatch::default()
            })
            .unwrap();
    }
    let tracker = udp_tracker().await;
    tracker.state.set(t[0].1, SWARM);
    let hot = Arc::new(HotColumns::with_epoch_every(Duration::ZERO));
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(
        config(&hot, health(&db, vec![tracker.url(true)])),
        store,
        Vec::new(),
    )
    .await
    .unwrap();
    until("rescraped", || hot.get(Col::Seeders, t[0].0 as u64) == 12).await;
    let store = pipeline.shutdown().await.unwrap();
    let row = store.swarm(t[0].0).unwrap().unwrap();
    assert_eq!((row.seeders, row.trackers_reporting), (12, 1));
    assert_eq!(row.dht, Some((5, 9)), "carried over");
    assert_eq!(hot.get(Col::Dht, t[0].0 as u64), 14, "seeds + peers");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_tracker_leaves_counts_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let t = seed(&db, &["quiet"]);
    // Nothing listens there.
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let hot = Arc::new(HotColumns::new());
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(
        config(&hot, health(&db, vec![format!("udp://127.0.0.1:{port}")])),
        store,
        Vec::new(),
    )
    .await
    .unwrap();
    let h = pipeline.monitor().health().unwrap().clone();
    until("a round closed", || {
        h.stats()
            .unreached
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let row = store.swarm(t[0].0).unwrap().unwrap();
    assert_eq!(
        (row.trackers_checked, row.dht),
        (0, None),
        "reads as never scraped"
    );
    assert!(
        row.next_scrape_at - row.last_scrape_at < 2 * 3600,
        "retried within ~1 h"
    );
    assert_eq!(hot.get(Col::Seeders, t[0].0 as u64), NEVER);
}

#[test]
fn hot_columns_survive_a_clean_restart_and_rebuild_after_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let hot_dir = dir.path().join("hot");
    let t = seed(&db, &["a"]);
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store
        .write(&lodestar_store::WriteBatch {
            swarm: &[lodestar_store::SwarmUpdate {
                torrent_id: t[0].0,
                at: 10,
                seeders: 7,
                leechers: 1,
                completed: 0,
                trackers_reporting: 1,
                trackers_checked: 1,
                dht: None,
                per_tracker: Vec::new(),
                next_scrape_at: 20,
                flags: 0,
                interest: false,
            }],
            ..lodestar_store::WriteBatch::default()
        })
        .unwrap();
    let (hot, from_snapshot) = open_hot(&mut store, &hot_dir).unwrap();
    assert!(!from_snapshot, "no snapshot yet: rebuilt");
    assert_eq!(hot.get(Col::Seeders, t[0].0 as u64), 7);
    // A value only the snapshot has (as if the scraper wrote it last).
    hot.set(Col::Dht, t[0].0 as u64, 99);
    save_hot(&mut store, &hot, &hot_dir).unwrap();

    let (back, from_snapshot) = open_hot(&mut store, &hot_dir).unwrap();
    assert!(from_snapshot);
    assert_eq!(back.get(Col::Dht, t[0].0 as u64), 99);
    // Opening cleared the stamp: without a new save, the next start
    // rebuilds from SQLite, as after a crash.
    let (again, from_snapshot) = open_hot(&mut store, &hot_dir).unwrap();
    assert!(!from_snapshot);
    assert_eq!(again.get(Col::Dht, t[0].0 as u64), NEVER);
    assert_eq!(again.get(Col::Seeders, t[0].0 as u64), 7);
}
