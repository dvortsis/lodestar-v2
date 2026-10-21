//! End to end: plugins → intake → fetch pool → peer wire (real TCP to a
//! fake seeding peer) → writer → SQLite, then a restart.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_pipeline::{Pipeline, PipelineConfig, event, metric, strategy};
use lodestar_sources::{
    BoxFuture, Consent, Found, Kind, Loss, Manifest, Network, PluginError, PoolConfig, Priority,
    Risk, SourceCtx, SourcePlugin, Stage,
};
use lodestar_store::{Store, StoreConfig, flags};
use tokio::sync::mpsc;

fn info_dict(name: &str) -> Vec<u8> {
    let mut f = BTreeMap::new();
    f.insert(b"length".to_vec(), OwnedValue::Int(1_000_000));
    f.insert(
        b"path".to_vec(),
        OwnedValue::List(vec![OwnedValue::from(format!("{name}.mkv").as_str())]),
    );
    let mut d = BTreeMap::new();
    d.insert(
        b"files".to_vec(),
        OwnedValue::List(vec![OwnedValue::Dict(f)]),
    );
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 80]));
    to_vec(&OwnedValue::Dict(d))
}

/// A source the test drives through a channel.
struct Scripted {
    manifest: Manifest,
    rx: Mutex<Option<mpsc::Receiver<Found>>>,
}

fn manifest(id: &'static str, kinds: &'static [Kind]) -> Manifest {
    Manifest {
        id,
        name: id,
        version: "1",
        kinds,
        networks: &[Network::Bt],
        endpoints: &[],
        risk: Risk::Low,
        consent: Consent::None,
        default_priority: Priority::Normal,
        enabled_by_default: true,
    }
}

fn scripted(id: &'static str) -> (Arc<Scripted>, mpsc::Sender<Found>) {
    let (tx, rx) = mpsc::channel(64);
    let s = Scripted {
        manifest: manifest(id, &[Kind::Discovery]),
        rx: Mutex::new(Some(rx)),
    };
    (Arc::new(s), tx)
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

/// Stands in for the DHT's peer finding: answers lookups from a table.
struct PeerTable {
    manifest: Manifest,
    peers: HashMap<InfoHash, Vec<SocketAddr>>,
}

impl SourcePlugin for PeerTable {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(async move {
            let mut requests = ctx.peer_requests.take().expect("a peer finder");
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(key) = requests.recv() => {
                        if let Some(peers) = self.peers.get(&key) {
                            let replies = ctx.peer_replies.as_ref().expect("a peer finder gets replies");
                            replies.try_emit(Found::key(key, strategy::LOOKUP).with_peers(peers.clone()));
                        }
                    }
                }
            }
        })
    }
}

fn config() -> PipelineConfig {
    PipelineConfig {
        pool: PoolConfig {
            lookup_wait: Duration::from_millis(300),
            retry_backoff: Duration::from_millis(100),
            max_attempts: 2,
            ..PoolConfig::default()
        },
        flush_every: Duration::from_millis(100),
        batch_wait: Duration::from_millis(20),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discoveries_become_stored_torrents_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let seeds: Vec<Seed> = ["Alpha", "Beta", "Blocked"]
        .iter()
        .map(|n| Seed::v1(info_dict(n)))
        .collect();
    let [alpha, beta, blocked] = [0, 1, 2].map(|i| InfoHash(seeds[i].info_hash));
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let nowhere = InfoHash([0xee; 20]);
    let later = InfoHash([0xdd; 20]);

    let (feed, feed_tx) = scripted("feed");
    let (other, other_tx) = scripted("other");
    let finder = Arc::new(PeerTable {
        manifest: manifest("finder", &[Kind::PeerFinder]),
        peers: HashMap::from([(beta, vec![peer.addr()])]),
    });
    let mut cfg = config();
    cfg.blocklist.insert(blocked.0);
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(cfg, store, vec![feed, other, finder])
        .await
        .unwrap();
    let ids: Vec<_> = pipeline.sources().iter().map(|s| s.id).collect();
    let (feed_id, other_id) = (ids[0], ids[1]);

    // Alpha comes with a peer; Beta needs a lookup; one key has no peers
    // anywhere; one is blocklisted.
    feed_tx
        .send(Found::key(alpha, 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    feed_tx.send(Found::key(beta, 0)).await.unwrap();
    feed_tx.send(Found::key(nowhere, 0)).await.unwrap();
    feed_tx
        .send(Found::key(blocked, 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    until("two stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 2
    })
    .await;
    until("blocked", || {
        pipeline.writer_stats().blocked.load(Ordering::Relaxed) == 1
    })
    .await;
    until("no-peers loss", || {
        pipeline.pool_stats().lost_count(Loss::NoPeers) == 1
    })
    .await;

    // Another source sees Alpha: a rediscovery, not a fetch.
    other_tx.send(Found::key(alpha, 7)).await.unwrap();
    until("rediscovery", || {
        pipeline.writer_stats().rediscovered.load(Ordering::Relaxed) == 1
    })
    .await;
    assert_eq!(pipeline.intake_stats().known.load(Ordering::Relaxed), 1);

    // A key still waiting at shutdown is saved with the queue.
    feed_tx.send(Found::key(later, 3)).await.unwrap();
    until("queued", || {
        pipeline.pool_stats().pending.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();

    assert_eq!(store.count().unwrap(), 2);
    let alpha_id = store.find(&alpha).unwrap().unwrap();
    let row = store.torrent(alpha_id).unwrap().unwrap();
    assert_eq!(row.name, "Alpha");
    assert_eq!(row.first_source, Some(feed_id));
    assert_eq!(row.sources, (1 << feed_id.get()) | (1 << other_id.get()));
    assert_ne!(row.flags & flags::BACKFILL, 0, "first run: warm-up window");
    let queue = store.load_fetch_queue().unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!((queue[0].info_hash, queue[0].strategy), (later, 3));

    // The funnel and rollups were flushed with the data.
    let cohorts = store.cohorts(0, i64::MAX).unwrap();
    let count = |stage: Stage, reason: u8| -> i64 {
        cohorts
            .iter()
            .filter(|c| c.source == feed_id && c.stage == stage as u8 && c.reason == reason)
            .map(|c| c.count)
            .sum()
    };
    assert_eq!(count(Stage::Harvested, 0), 5);
    assert_eq!(count(Stage::PeerFound, 0), 3);
    assert_eq!(count(Stage::Fetched, 0), 3);
    assert_eq!(count(Stage::Stored, 0), 2);
    assert_eq!(count(Stage::Lost, Loss::Blocked as u8), 1);
    assert_eq!(count(Stage::Lost, Loss::NoPeers as u8), 1);
    let stored: i64 = store
        .rollup(metric::STORED, 0, i64::MAX)
        .unwrap()
        .iter()
        .map(|c| c.value)
        .sum();
    assert_eq!(stored, 2);
    let rediscovered = store.rollup(metric::REDISCOVERED, 0, i64::MAX).unwrap();
    assert_eq!(rediscovered.len(), 1);
    assert_eq!(
        (rediscovered[0].dim, rediscovered[0].key),
        (u32::from(other_id.get()), 7)
    );
    let kinds: Vec<u32> = store
        .events(0, i64::MAX)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec![event::START, event::STOP]);
    drop(store);

    // Restart: same source ids, the have-filter knows what's stored, and
    // the saved queue is back.
    let (feed, feed_tx) = scripted("feed");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    assert_eq!(pipeline.sources()[0].id, feed_id);
    assert!(pipeline.have().contains(&alpha));
    until("restored queue", || {
        pipeline.pool_stats().pending.load(Ordering::Relaxed) == 1
    })
    .await;
    feed_tx.send(Found::key(beta, 0)).await.unwrap();
    until("known", || {
        pipeline.writer_stats().rediscovered.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(store.count().unwrap(), 2);
}

/// The real DHT plugin starts inside the pipeline, reports stats and saves
/// its state on shutdown. (Harvesting itself is covered by the dht crate's
/// in-process network tests.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_dht_plugin_runs_inside_the_pipeline() {
    use lodestar_dht::node::Config;
    use lodestar_pipeline::DhtSource;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.bin");
    let dht = Arc::new(DhtSource::new(Config {
        bind: vec!["127.0.0.1:0".parse().unwrap()],
        bootstrap: vec![],
        state_path: Some(state.clone()),
        ..Config::default()
    }));
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![dht.clone()])
        .await
        .unwrap();
    until("dht stats", || dht.stats().is_some()).await;
    assert!(pipeline.sources()[0].running);
    assert_eq!(pipeline.sources()[0].plugin, "dht");
    pipeline.shutdown().await.unwrap();
    assert!(state.exists(), "routing state saved");
}

/// An import with an incomplete file list is re-fetched at startup and
/// upgraded in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_imports_are_refetched() {
    use lodestar_store::{NewTorrent, flags};
    let raw = info_dict("Imported.Pack");
    let seed = Seed::v1(raw.clone());
    let key = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let real = lodestar_metainfo::Info::parse(&raw).unwrap();
    let mut partial = real.clone();
    partial.files.clear();
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let bm = store.register_source("bitmagnet", "1", 0).unwrap();
    store
        .insert(&[NewTorrent {
            info: &partial,
            raw_info: None,
            discovered_at: 1_600_000_000,
            metadata_at: 1_600_000_000,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            source: bm,
        }])
        .unwrap();
    let finder = Arc::new(PeerTable {
        manifest: manifest("finder", &[Kind::PeerFinder]),
        peers: HashMap::from([(key, vec![peer.addr()])]),
    });
    let pipeline = Pipeline::start(config(), store, vec![finder])
        .await
        .unwrap();
    until("refetched", || {
        pipeline.pool_stats().fetched.load(Ordering::Relaxed) == 1
    })
    .await;
    until("written", || {
        pipeline.writer_stats().existing.load(Ordering::Relaxed) == 1
    })
    .await;
    let mut store = pipeline.shutdown().await.unwrap();
    let id = store.find(&key).unwrap().unwrap();
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.flags & flags::FILES_PARTIAL, 0);
    assert_ne!(row.flags & flags::IMPORTED, 0);
    assert_eq!(store.files(id).unwrap().unwrap().files.len(), 1);
    assert!(store.partial_hashes(10).unwrap().is_empty());
}
