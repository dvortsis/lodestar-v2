//! End to end: plugins → intake → fetch pool → peer wire (real TCP to a
//! fake seeding peer) → writer → SQLite, then a restart.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::FileInfo;
use lodestar_filter::{Core, CsList, Engine, Handle, Lists, Origin, Sealer, Switches, TermSet};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_pipeline::{Pipeline, PipelineConfig, event, metric};
use lodestar_sources::{
    BoxFuture, Consent, Found, Kind, Loss, Manifest, Network, PluginError, PoolConfig, Priority,
    Risk, SourceCtx, SourcePlugin, Stage,
};
use lodestar_store::{Store, StoreConfig, flags};
use tokio::sync::mpsc;

fn info_dict(name: &str) -> Vec<u8> {
    info_dict_with(name, &[(&format!("{name}.mkv"), 1_000_000)])
}

fn info_dict_with(name: &str, files: &[(&str, i64)]) -> Vec<u8> {
    let files = files
        .iter()
        .map(|(path, size)| {
            let mut f = BTreeMap::new();
            f.insert(b"length".to_vec(), OwnedValue::Int(*size));
            f.insert(
                b"path".to_vec(),
                OwnedValue::List(vec![OwnedValue::from(*path)]),
            );
            OwnedValue::Dict(f)
        })
        .collect();
    let mut d = BTreeMap::new();
    d.insert(b"files".to_vec(), OwnedValue::List(files));
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
                    Some(request) = requests.recv() => {
                        let (done, rx) = tokio::sync::oneshot::channel();
                        let _ = request.admit.send(Some(rx));
                        let _ = done.send(self.peers.get(&request.key).cloned().unwrap_or_default());
                    }
                }
            }
        })
    }
}

fn config() -> PipelineConfig {
    PipelineConfig {
        fetch: lodestar_peer_wire::FetchOptions {
            allow_private: true,
            ..Default::default()
        },
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
    let cfg = config();
    cfg.filter
        .update(|core| core.cs.add_hash(blocked.0, Origin::Imported));
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
    // A CS-1 key is refused at intake, never fetched.
    until("refused", || {
        pipeline.intake_stats().refused.load(Ordering::Relaxed) == 1
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
    // The CS-1 key was refused before peer finding and fetching.
    assert_eq!(count(Stage::PeerFound, 0), 2);
    assert_eq!(count(Stage::Fetched, 0), 2);
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
async fn a_stored_torrent_is_seen_in_the_hot_columns_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let seeds = vec![Seed::v1(info_dict("Gamma"))];
    let gamma = InfoHash(seeds[0].info_hash);
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let (feed, feed_tx) = scripted("feed");
    let hot = Arc::new(lodestar_store::HotColumns::new());
    let cfg = PipelineConfig {
        hot: Some(hot.clone()),
        ..config()
    };
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(cfg, store, vec![feed]).await.unwrap();
    feed_tx
        .send(Found::key(gamma, 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let id = store.find(&gamma).unwrap().unwrap();
    let row = store.torrent(id).unwrap().unwrap();
    // `seen:`, the seen sort and the freshness prior read this column: a
    // new torrent counts as seen when it was discovered, not never.
    assert_eq!(
        i64::from(hot.get(lodestar_store::Col::LastSeen, id as u64)),
        row.last_seen_at
    );
}

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

/// A key only private sources reported is stored flagged, so it's never
/// scraped publicly; a later public sighting clears the flag (§18.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_source_keys_are_flagged_until_seen_publicly() {
    let seed = Seed::v1(info_dict("Members"));
    let key = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let (private, private_tx) = scripted("private");
    let (public, public_tx) = scripted("public");
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![private, public])
        .await
        .unwrap();
    let mut found = Found::key(key, 0).with_peers(vec![peer.addr()]);
    found.private_source = true;
    private_tx.send(found).await.unwrap();
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    // Wait for the commit to land before the rediscovery can find it.
    until("have", || pipeline.have().contains(&key)).await;
    public_tx.send(Found::key(key, 0)).await.unwrap();
    until("rediscovery", || {
        pipeline.writer_stats().rediscovered.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let id = store.find(&key).unwrap().unwrap();
    assert_eq!(
        store.torrent(id).unwrap().unwrap().flags & flags::PRIVATE_SOURCE,
        0,
        "the public sighting cleared it"
    );
}

/// A private-source key stays flagged while only private sources see it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_source_flag_reaches_the_store() {
    let seed = Seed::v1(info_dict("Members only"));
    let key = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let (private, private_tx) = scripted("private");
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![private])
        .await
        .unwrap();
    let mut found = Found::key(key, 0).with_peers(vec![peer.addr()]);
    found.private_source = true;
    private_tx.send(found.clone()).await.unwrap();
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    until("have", || pipeline.have().contains(&key)).await;
    private_tx.send(found).await.unwrap();
    until("rediscovery", || {
        pipeline.writer_stats().rediscovered.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let id = store.find(&key).unwrap().unwrap();
    assert_ne!(
        store.torrent(id).unwrap().unwrap().flags & flags::PRIVATE_SOURCE,
        0
    );
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

/// The worker boundary (§19.3): a key found by two sources while it's in
/// flight is leased once, harvested once and fetched once, and the second
/// source joins the lease. Keys saved at shutdown are leased again on
/// restart without a new harvest, and every lease ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_key_is_leased_and_counted_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let seed = Seed::v1(info_dict("Gamma"));
    let gamma = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let nowhere = InfoHash([0xcc; 20]);
    let parked = InfoHash([0xbb; 20]);

    let (feed, feed_tx) = scripted("feed");
    let (other, other_tx) = scripted("other");
    let finder = Arc::new(PeerTable {
        manifest: manifest("finder", &[Kind::PeerFinder]),
        peers: HashMap::new(),
    });
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![feed, other, finder])
        .await
        .unwrap();
    let ids: Vec<_> = pipeline.sources().iter().map(|s| s.id).collect();
    let (feed_id, other_id) = (ids[0], ids[1]);

    let leases = pipeline.monitor();
    let stat = |f: fn(&lodestar_pipeline::LeaseStats) -> &std::sync::atomic::AtomicU64| {
        f(leases.lease_stats()).load(Ordering::Relaxed)
    };
    // Gamma first arrives without peers; once it's leased, the second
    // source brings them. A key nobody can serve is found twice too.
    feed_tx.send(Found::key(gamma, 0)).await.unwrap();
    feed_tx.send(Found::key(nowhere, 0)).await.unwrap();
    until("leased", || stat(|s| &s.granted) == 2).await;
    other_tx
        .send(Found::key(gamma, 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    other_tx.send(Found::key(nowhere, 0)).await.unwrap();
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    until("no-peers loss", || {
        pipeline.pool_stats().lost_count(Loss::NoPeers) == 1
    })
    .await;
    // The pool counts a loss just before it ends the lease, so the lease
    // may close a moment after the counter moves.
    until("every lease ended", || stat(|s| &s.outstanding) == 0).await;
    assert_eq!(stat(|s| &s.granted), 2);
    assert_eq!(stat(|s| &s.held), 2);
    assert_eq!(stat(|s| &s.stale), 0);
    assert_eq!(pipeline.pool_stats().fetched.load(Ordering::Relaxed), 1);

    // One key waits at shutdown.
    feed_tx.send(Found::key(parked, 4)).await.unwrap();
    until("parked", || stat(|s| &s.outstanding) == 1).await;
    let store = pipeline.shutdown().await.unwrap();

    let id = store.find(&gamma).unwrap().unwrap();
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.first_source, Some(feed_id));
    assert_eq!(
        row.sources & ((1 << feed_id.get()) | (1 << other_id.get())),
        (1 << feed_id.get()) | (1 << other_id.get()),
        "the second source joined the lease"
    );
    let cohorts = store.cohorts(0, i64::MAX).unwrap();
    let total = |stage: Stage, reason: u8| -> i64 {
        cohorts
            .iter()
            .filter(|c| c.stage == stage as u8 && c.reason == reason)
            .map(|c| c.count)
            .sum()
    };
    // Harvested = stored + lost + still leased (the parked key).
    assert_eq!(total(Stage::Harvested, 0), 3);
    assert_eq!(total(Stage::Stored, 0), 1);
    assert_eq!(total(Stage::Lost, Loss::NoPeers as u8), 1);
    drop(store);

    // Restart: the parked key is leased again, not harvested again.
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![]).await.unwrap();
    let monitor = pipeline.monitor();
    assert_eq!(monitor.lease_stats().outstanding.load(Ordering::Relaxed), 1);
    assert_eq!(monitor.lease_stats().granted.load(Ordering::Relaxed), 0);
    let store = pipeline.shutdown().await.unwrap();
    let harvested: i64 = store
        .cohorts(0, i64::MAX)
        .unwrap()
        .iter()
        .filter(|c| c.stage == Stage::Harvested as u8)
        .map(|c| c.count)
        .sum();
    assert_eq!(harvested, 3);
}

/// Enrichment (§6): fetched torrents are enriched in prepare; torrents
/// stored without it (imports, older rows) are enriched in the background
/// and queued for re-indexing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn torrents_are_enriched_in_prepare_and_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let raw = info_dict("Old.Import.2019.1080p.BluRay.x264-GRP");
    let imported = {
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let info = lodestar_metainfo::Info::parse(&raw).unwrap();
        store
            .insert(&[lodestar_store::NewTorrent {
                info: &info,
                raw_info: None,
                discovered_at: 1,
                metadata_at: 1,
                extra_flags: flags::IMPORTED,
                source: lodestar_store::SourceId::new(0).unwrap(),
            }])
            .unwrap()[0]
            .id()
    };
    let seed = Seed::v1(info_dict("Fresh.Show.S02E03.720p.WEB.h264-NTb"));
    let fresh = InfoHash(seed.info_hash);
    let peer = FakePeer::spawn(vec![seed], Accept::Both, Behavior::Honest, vec![]).await;
    let (feed, feed_tx) = scripted("feed");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    until("backfilled", || {
        pipeline.writer_stats().enriched.load(Ordering::Relaxed) == 1
    })
    .await;
    feed_tx
        .send(Found::key(fresh, 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();

    use lodestar_classify::{ContentType, ENRICHMENT_VERSION, Record};
    let old = store.enrichment(imported).unwrap().expect("backfilled");
    assert_eq!(old.version, ENRICHMENT_VERSION);
    assert_eq!(
        ContentType::from_code(old.content_type),
        Some(ContentType::Movie)
    );
    let r = Record::decode(&old.data).unwrap();
    assert_eq!(
        (r.release("title"), r.release("year")),
        (Some("Old Import"), Some("2019"))
    );
    assert!(
        store
            .reindex_after(0, 10)
            .unwrap()
            .iter()
            .any(|(_, id)| *id == imported),
        "queued for re-indexing"
    );
    let id = store.find(&fresh).unwrap().unwrap();
    let new = store.enrichment(id).unwrap().expect("enriched in prepare");
    let r = Record::decode(&new.data).unwrap();
    assert_eq!(
        ContentType::from_code(new.content_type),
        Some(ContentType::Tv)
    );
    assert_eq!(
        (r.release("season"), r.release("episode")),
        (Some("2"), Some("3"))
    );
    assert!(
        store
            .unenriched(0, ENRICHMENT_VERSION, 10)
            .unwrap()
            .is_empty()
    );
}

/// A filter with neutral stand-in terms (`lexa` for a sexual term), so the
/// pattern rules can be exercised without indicative text.
fn stand_in_filter() -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Arc::new(Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    ))))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filtered_torrents_are_held_and_never_fetched_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    // A pattern hit (CS-2a), a repost of a CS-1 payload (CS-8a), and a
    // clean torrent.
    let repost_files: &[(&str, i64)] = &[("a.mkv", 734_003_201), ("b.srt", 81_233)];
    let seeds = vec![
        Seed::v1(info_dict("Title 15yo lexa")),
        Seed::v1(info_dict_with("Renamed", repost_files)),
        Seed::v1(info_dict("Clean")),
    ];
    let [held, repost, clean] = [0, 1, 2].map(|i| InfoHash(seeds[i].info_hash));
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let key = Sealer::generate_key().unwrap();

    let start = |filter: Arc<Handle>| {
        let mut cfg = config();
        cfg.filter = filter;
        cfg.sealer = Some(Arc::new(Sealer::new(&key)));
        cfg
    };
    let filter = stand_in_filter();
    let sizes: Vec<FileInfo> = repost_files
        .iter()
        .map(|(p, s)| FileInfo::new(vec![(*p).to_owned()], *s as u64))
        .collect();
    let sig = lodestar_filter::payload_sig(&sizes).unwrap();
    assert!(sig.specific);
    filter.update(|core| core.cs.add_sig(sig.sig));

    let (feed, feed_tx) = scripted("feed");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(start(filter), store, vec![feed.clone()])
        .await
        .unwrap();
    for k in [held, repost, clean] {
        feed_tx
            .send(Found::key(k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("filtered", || {
        let w = pipeline.writer_stats();
        w.quarantined.load(Ordering::Relaxed) == 1
            && w.blocked.load(Ordering::Relaxed) == 1
            && w.stored.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(store.count().unwrap(), 1);
    assert!(store.find(&held).unwrap().is_none());
    let q = store.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].hash, q[0].rule.as_str()), (held.0, "CS-2a"));
    let sealed = q[0].sealed.as_deref().expect("a sealed name");
    assert_eq!(Sealer::new(&key).open(sealed).unwrap(), "Title 15yo lexa");
    let state = store.filter_state().unwrap();
    assert!(state.held.iter().any(|(h, _)| *h == repost.0));
    let hits = store.filter_hits(0).unwrap();
    let mut rules: Vec<&str> = hits.iter().map(|h| h.rule.as_str()).collect();
    rules.sort();
    assert_eq!(rules, ["CS-2a", "CS-8a"]);

    // After a restart, both are refused at intake: no fetch loop.
    let (feed, feed_tx) = scripted("feed");
    let pipeline = Pipeline::start(start(stand_in_filter()), store, vec![feed])
        .await
        .unwrap();
    for k in [held, repost] {
        feed_tx
            .send(Found::key(k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("refused", || {
        pipeline.intake_stats().refused.load(Ordering::Relaxed) == 2
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(store.count().unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switched_off_rules_and_the_store_state_load_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let seeds = vec![Seed::v1(info_dict("Title 15yo lexa"))];
    let key = InfoHash(seeds[0].info_hash);
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    // Stored switches outlive the run that set them.
    store.set_core_switch("CS-2a", false).unwrap();
    let mut cfg = config();
    cfg.filter = stand_in_filter();
    let (feed, feed_tx) = scripted("feed");
    let pipeline = Pipeline::start(cfg, store, vec![feed]).await.unwrap();
    feed_tx
        .send(Found::key(key, 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert!(store.find(&key).unwrap().is_some());
    assert!(store.quarantined(10).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn your_rules_mark_stored_torrents_and_shadow_rules_only_log() {
    use lodestar_store::{SHADOW, flags};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let seeds = vec![
        Seed::v1(info_dict("Movie 2023 CAM")),
        Seed::v1(info_dict("Movie 2023 TELESYNC")),
        Seed::v1(info_dict("Movie 2023 WEB")),
    ];
    let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let hide = lodestar_filter::Action::Hide.code();
    store.add_rule("cam", hide, None, None).unwrap();
    store
        .add_rule("telesync", hide, Some(i64::MAX), None)
        .unwrap();
    store.set_pack_switch("fakes", false).unwrap();
    let (feed, feed_tx) = scripted("feed");
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    for k in &keys {
        feed_tx
            .send(Found::key(*k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 3
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let row = |k: &InfoHash| {
        store
            .torrent(store.find(k).unwrap().unwrap())
            .unwrap()
            .unwrap()
    };
    assert_ne!(row(&keys[0]).flags & flags::HIDDEN, 0, "hidden by U-1");
    assert_eq!(
        row(&keys[1]).flags & flags::HIDDEN,
        0,
        "U-2 is in shadow mode"
    );
    assert_eq!(row(&keys[2]).flags & (flags::HIDDEN | flags::FLAGGED), 0);
    let hits = store.filter_hits(0).unwrap();
    let find = |rule: &str| hits.iter().find(|h| h.rule == rule).unwrap();
    assert_eq!(find("U-1").action, hide);
    assert_eq!(find("U-2").action, hide | SHADOW);
    assert!(
        hits.iter().all(|h| !h.rule.starts_with("FK")),
        "fakes switched off"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_is_adult_rule_stores_adult_and_meets_adult_filtering() {
    use lodestar_classify::ContentType;
    use lodestar_store::flags;
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    // A child-role and a sexual stand-in ten words apart: only the
    // Adult-only CS-5a (window 12) can catch them, not CS-4a (window 8).
    let spread = "Studiox Rolea one two three four five six seven eight nine Lexa";
    let file = |name: &str| format!("{}.mp4", name.replace(' ', "."));
    let seeds = vec![
        Seed::v1(info_dict("Holiday Clips 2023 Lexb")),
        Seed::v1(info_dict("Holiday Clips 2023 Other")),
        Seed::v1(info_dict_with(spread, &[(&file(spread), 900 << 20)])),
    ];
    let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    // Your rule hides adult torrents; your classification rules call
    // "lexb" and "studiox" ones adult.
    let hide = lodestar_filter::Action::Hide.code();
    store.add_rule("type:adult", hide, None, None).unwrap();
    store.add_class_rule("lexb", true).unwrap();
    store.add_class_rule("studiox", true).unwrap();
    store.set_pack_switch("fakes", false).unwrap();
    let mut cfg = config();
    cfg.filter = stand_in_filter();
    cfg.sealer = Some(Arc::new(Sealer::new(&Sealer::generate_key().unwrap())));
    let (feed, feed_tx) = scripted("feed");
    let pipeline = Pipeline::start(cfg, store, vec![feed]).await.unwrap();
    for k in &keys {
        feed_tx
            .send(Found::key(*k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("judged", || {
        let w = pipeline.writer_stats();
        w.stored.load(Ordering::Relaxed) == 2 && w.quarantined.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    let id = |k: &InfoHash| store.find(k).unwrap().unwrap();
    let kind = |k: &InfoHash| {
        ContentType::from_code(store.enrichment(id(k)).unwrap().unwrap().content_type)
    };
    assert_eq!(kind(&keys[0]), Some(ContentType::Adult), "stored as adult");
    assert_ne!(kind(&keys[1]), Some(ContentType::Adult));
    // Stored as adult, so judged as adult too: your type:adult rule hides it.
    let flags_of = |k: &InfoHash| store.torrent(id(k)).unwrap().unwrap().flags;
    assert_ne!(flags_of(&keys[0]) & flags::HIDDEN, 0);
    assert_eq!(flags_of(&keys[1]) & flags::HIDDEN, 0);
    // And the Adult-only core held the one only it can catch.
    let q = store.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].hash, q[0].rule.as_str()), (keys[2].0, "CS-5a"));
    assert!(store.find(&keys[2]).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_not_adult_rule_never_takes_a_torrent_out_of_the_adult_only_core() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    // A child-role stand-in and a sexual stand-in ten words apart: beyond
    // the general window (CS-4a, 8 words) but within the Adult-only one
    // (CS-5a, 12 words), so only CS-5a can catch it, and only in Adult.
    let words = "Rolea one two three four five six seven eight nine Lexa";
    let adult = format!("Some.Title.XXX {words}");
    let plain = format!("Some Title {words}");
    let file = |name: &str| format!("{}.mp4", name.replace(' ', "."));
    // Detected adult and matched by the rule, with nothing for the core.
    let relabelled = "Other.Title.XXX Rolea Clip";
    let detected = |name: &str| {
        let files = [FileInfo::new(vec![file(name)], 900 << 20)];
        lodestar_classify::classify(name, &files).content_type
    };
    for name in [adult.as_str(), relabelled] {
        assert_eq!(
            detected(name),
            lodestar_classify::ContentType::Adult,
            "{name}"
        );
    }
    assert_ne!(detected(&plain), lodestar_classify::ContentType::Adult);
    let seeds = vec![
        Seed::v1(info_dict_with(&adult, &[(&file(&adult), 900 << 20)])),
        Seed::v1(info_dict_with(&plain, &[(&file(&plain), 900 << 20)])),
        Seed::v1(info_dict_with(
            relabelled,
            &[(&file(relabelled), 900 << 20)],
        )),
    ];
    let [held, clean, relabelled] = [0, 1, 2].map(|i| InfoHash(seeds[i].info_hash));
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    // Your rule says everything with the role word isn't adult.
    store.add_class_rule("rolea", false).unwrap();
    store.set_pack_switch("fakes", false).unwrap();
    let filter = stand_in_filter();
    let mut cfg = config();
    cfg.filter = filter.clone();
    cfg.sealer = Some(Arc::new(Sealer::new(&Sealer::generate_key().unwrap())));
    let (feed, feed_tx) = scripted("feed");
    let pipeline = Pipeline::start(cfg, store, vec![feed]).await.unwrap();
    assert_eq!(filter.current().class_rules.len(), 1, "the rule is loaded");
    for k in [held, clean, relabelled] {
        feed_tx
            .send(Found::key(k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("judged", || {
        let w = pipeline.writer_stats();
        w.quarantined.load(Ordering::Relaxed) == 1 && w.stored.load(Ordering::Relaxed) == 2
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    // Detection said adult, so CS-5a held it despite the Not adult rule.
    let q = store.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].hash, q[0].rule.as_str()), (held.0, "CS-5a"));
    assert!(store.find(&held).unwrap().is_none());
    // The same words outside Adult are within no window: stored.
    assert!(store.find(&clean).unwrap().is_some());
    // The rule was in force: it stored the other one as not adult.
    let id = store.find(&relabelled).unwrap().unwrap();
    let kind = store.enrichment(id).unwrap().unwrap().content_type;
    assert_ne!(
        lodestar_classify::ContentType::from_code(kind),
        Some(lodestar_classify::ContentType::Adult)
    );
}

/// §20.6 and the owner's decision (§2.8): packs are off until chosen, so
/// a fresh install that never chose stores a torrent a pack would hide.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packs_are_off_on_a_fresh_install() {
    use lodestar_store::flags;
    // MW-1: an executable posing as a video.
    let lure: &[(&str, i64)] = &[("Movie.2023.1080p.mp4.exe", 2_000_000)];
    let run = |chosen: bool| async move {
        let dir = tempfile::tempdir().unwrap();
        let seeds = vec![Seed::v1(info_dict_with("Movie 2023 1080p", lure))];
        let key = InfoHash(seeds[0].info_hash);
        let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
        let mut store =
            Store::open(dir.path().join("lodestar.db"), StoreConfig::default()).unwrap();
        if chosen {
            store.set_pack_switch("malware", true).unwrap();
        }
        let (feed, feed_tx) = scripted("feed");
        let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
        feed_tx
            .send(Found::key(key, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
        until("stored", || {
            pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
        })
        .await;
        let store = pipeline.shutdown().await.unwrap();
        let row = store
            .torrent(store.find(&key).unwrap().unwrap())
            .unwrap()
            .unwrap();
        (
            row.flags & flags::HIDDEN != 0,
            store.filter_hits(0).unwrap(),
        )
    };
    let (hidden, hits) = run(false).await;
    assert!(!hidden, "no pack is on until one is chosen");
    assert!(hits.is_empty(), "{hits:?}");
    // The same torrent once the pack is chosen, so the case above is real.
    let (hidden, hits) = run(true).await;
    assert!(hidden);
    assert!(hits.iter().any(|h| h.rule == "MW-1"), "{hits:?}");
}
