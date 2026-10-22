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
use lodestar_pipeline::{Pipeline, PipelineConfig, event, metric, strategy};
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
    assert_eq!(stat(|s| &s.granted), 2);
    assert_eq!(stat(|s| &s.held), 2);
    assert_eq!(stat(|s| &s.outstanding), 0, "every lease ended");
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
