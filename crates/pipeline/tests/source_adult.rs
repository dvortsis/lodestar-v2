//! Always-adult sources (X-source-adult, owner Q7;
//! `docs/decisions/2026-11-05-X-source-adult.md`): every item such a
//! source finds is classified Adult, as an Is adult correction makes it,
//! whatever its name or claims say. That holds at every point it's
//! classified: prepare, promotion, a stored torrent the source finds again,
//! and the background enrichment. The floor still comes first, the
//! Adult-only core checks the adult reading, and a non-adult source is
//! unaffected.
//!
//! Search, feeds and exports hide a torrent exactly when its stored
//! content type is Adult and "Show adult content" is off
//! (`crates/api/tests/adult_visibility.rs`, `export.rs`, `digest.rs`), so
//! these tests pin the stored type at each point.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::ContentType;
use lodestar_filter::{Core, CsList, Engine, Handle, Lists, Origin, Sealer, Switches, TermSet};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_pipeline::enrich::{Backfill, enrich_stored_with};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Claims, Consent, Found, Kind, Manifest, Network, PluginError, PoolConfig, Priority,
    Risk, SourceCtx, SourcePlugin,
};
use lodestar_store::{ClassCorrection, Store, StoreConfig};
use tokio::sync::mpsc;

/// A child-role and a sexual stand-in ten words apart: only the
/// Adult-only CS-5a (window 12) catches them, and only on an adult item.
const SPREAD: &str = "Studiox Rolea one two three four five six seven eight nine Lexa";

fn info_dict(name: &str) -> Vec<u8> {
    let file = format!("{}.mp4", name.replace(' ', "."));
    let mut f = BTreeMap::new();
    f.insert(b"length".to_vec(), OwnedValue::Int(900 << 20));
    f.insert(
        b"path".to_vec(),
        OwnedValue::List(vec![OwnedValue::from(file.as_str())]),
    );
    let mut d = BTreeMap::new();
    d.insert(
        b"files".to_vec(),
        OwnedValue::List(vec![OwnedValue::Dict(f)]),
    );
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20 * 3600]));
    to_vec(&OwnedValue::Dict(d))
}

/// A source the test drives through a channel.
struct Scripted {
    manifest: Manifest,
    rx: Mutex<Option<mpsc::Receiver<Found>>>,
}

fn scripted(id: &'static str, always_adult: bool) -> (Arc<Scripted>, mpsc::Sender<Found>) {
    let (tx, rx) = mpsc::channel(64);
    let s = Scripted {
        manifest: Manifest {
            id,
            name: id,
            version: "1",
            kinds: &[Kind::Discovery],
            networks: &[Network::Bt],
            endpoints: &[],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: true,
            always_adult,
            finds_private: false,
        },
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
        filter: stand_in_filter(),
        sealer: Some(Arc::new(Sealer::new(&Sealer::generate_key().unwrap()))),
        ..PipelineConfig::default()
    }
}

/// Polls for 10 s (standards §2), so the slowest runner passes.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn claims(name: &str) -> Claims {
    Claims {
        name: Some(name.into()),
        ..Claims::default()
    }
}

fn kind(store: &Store, k: &InfoHash) -> Option<ContentType> {
    let id = store.find(k).unwrap()?;
    ContentType::from_code(store.enrichment(id).unwrap()?.content_type)
}

fn content_type(store: &Store, k: &InfoHash) -> Option<u8> {
    store.content_type(store.find(k).unwrap()?).unwrap()
}

/// What the background pass makes of every stored torrent now, as after
/// a parser upgrade.
fn reenrich(store: &mut Store, filter: &Handle, adult: AdultSources) {
    store.mark_unenriched_after(0).unwrap();
    let ctx = Backfill {
        filter: Some(filter),
        adult,
        ..Backfill::default()
    };
    // One batch covers every test's torrents; the next finds none left.
    for _ in 0..4 {
        if enrich_stored_with(store, 0, 64, &ctx).unwrap().is_none() {
            return;
        }
    }
    panic!("the backfill never finished");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clean_named_find_from_an_always_adult_source_is_stored_adult() {
    let names = [
        "Holiday Clips 2023",
        "Holiday Clips 2024",
        SPREAD,
        "Floor Held",
    ];
    let seeds: Vec<Seed> = names.iter().map(|n| Seed::v1(info_dict(n))).collect();
    let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let cfg = config();
    // The floor comes first: a hash on it is refused before anything else.
    cfg.filter
        .update(|core| core.cs.add_hash(keys[3].0, Origin::Imported));
    let filter = cfg.filter.clone();
    let (adult, adult_tx) = scripted("adult", true);
    let (plain, plain_tx) = scripted("plain", false);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(cfg, store, vec![adult, plain])
        .await
        .unwrap();
    let mask = AdultSources(1 << pipeline.sources()[0].id.get());
    // Key-only finds with a peer are fetched at once (claims alone wait
    // for verification or another finder: `promotion_keeps_…` below), so
    // the worker sees the adult source among the fetch's sources.
    let with = |k: InfoHash| Found::key(k, 0).with_peers(vec![peer.addr()]);
    // Clean names: one from each source.
    adult_tx.send(with(keys[0])).await.unwrap();
    plain_tx.send(with(keys[1])).await.unwrap();
    // CS-5a checks only adult items: from the adult source it holds this.
    adult_tx.send(with(keys[2])).await.unwrap();
    adult_tx.send(with(keys[3])).await.unwrap();
    until("judged", || {
        let w = pipeline.writer_stats();
        w.stored.load(Ordering::Relaxed) == 2 && w.quarantined.load(Ordering::Relaxed) == 1
    })
    .await;
    until("refused", || {
        pipeline.intake_stats().refused.load(Ordering::Relaxed) == 1
    })
    .await;
    let mut store = pipeline.shutdown().await.unwrap();
    assert_eq!(kind(&store, &keys[0]), Some(ContentType::Adult));
    assert_eq!(
        content_type(&store, &keys[0]),
        Some(ContentType::Adult.code())
    );
    assert_ne!(
        kind(&store, &keys[1]),
        Some(ContentType::Adult),
        "unaffected"
    );
    let q = store.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].hash, q[0].rule.as_str()), (keys[2].0, "CS-5a"));
    // Relabelled adult only by its source: a tombstone when it ends.
    assert!(store.find(&keys[2]).unwrap().is_none());
    assert!(store.find(&keys[3]).unwrap().is_none(), "the floor's");

    // A newer parser doesn't undo it: the background pass keeps it adult
    // from its sources, and leaves the plain one as detection says.
    reenrich(&mut store, &filter, mask);
    assert_eq!(kind(&store, &keys[0]), Some(ContentType::Adult));
    assert_ne!(kind(&store, &keys[1]), Some(ContentType::Adult));
    // Without the source registered it would be detection's again, so
    // it's the source that keeps it adult.
    reenrich(&mut store, &filter, AdultSources::default());
    assert_ne!(kind(&store, &keys[0]), Some(ContentType::Adult));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promotion_keeps_an_unverified_find_adult() {
    // The adult source only claims it (no peers): an unverified item.
    // The plain source then finds it with a peer, and the fetch promotes
    // the item: stored adult, though its fetcher's source isn't.
    let name = "Garden Tour 2022";
    let seeds = vec![Seed::v1(info_dict(name)), Seed::v1(info_dict(SPREAD))];
    let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let (adult, adult_tx) = scripted("adult", true);
    let (plain, plain_tx) = scripted("plain", false);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![adult, plain])
        .await
        .unwrap();
    for k in &keys {
        adult_tx
            .send(Found::key(*k, 0).with_claims(claims("Clips")))
            .await
            .unwrap();
    }
    until("unverified", || {
        pipeline
            .writer_stats()
            .unverified
            .stored
            .load(Ordering::Relaxed)
            == 2
    })
    .await;
    for k in &keys {
        plain_tx
            .send(Found::key(*k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("promoted and held", || {
        let w = pipeline.writer_stats();
        w.unverified.promoted.load(Ordering::Relaxed) == 1
            && w.quarantined.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(kind(&store, &keys[0]), Some(ContentType::Adult));
    // The Adult-only core judged the promoted one as adult too.
    let q = store.quarantined(10).unwrap();
    assert_eq!((q.len(), q[0].hash), (1, keys[1].0));
    assert_eq!(q[0].rule, "CS-5a");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_torrent_an_always_adult_source_finds_becomes_adult_unless_you_said_not() {
    let names = ["Lake Walk 2021", "Lake Walk 2020", "Lake Walk 2019"];
    let seeds: Vec<Seed> = names.iter().map(|n| Seed::v1(info_dict(n))).collect();
    let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let filter = stand_in_filter();
    let mut cfg = config();
    cfg.filter = filter.clone();
    let (adult, adult_tx) = scripted("adult", true);
    let (plain, plain_tx) = scripted("plain", false);
    let store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(cfg, store, vec![adult, plain])
        .await
        .unwrap();
    let mask = AdultSources(1 << pipeline.sources()[0].id.get());
    for k in &keys {
        plain_tx
            .send(Found::key(*k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 3
    })
    .await;
    let mut store = pipeline.shutdown().await.unwrap();
    assert_ne!(kind(&store, &keys[0]), Some(ContentType::Adult));
    // Your own Not adult on the second: your explicit act wins.
    let second = store.find(&keys[1]).unwrap().unwrap();
    let not_adult = ClassCorrection {
        adult: Some(false),
        not_junk: false,
    };
    assert!(store.set_correction(second, not_adult).unwrap());

    let (adult, adult_tx2) = scripted("adult", true);
    let (plain, _plain_tx) = scripted("plain", false);
    drop(adult_tx);
    let mut cfg = config();
    cfg.filter = filter.clone();
    let pipeline = Pipeline::start(cfg, store, vec![adult, plain])
        .await
        .unwrap();
    // Claims about the first two, and only a bare sighting of the third:
    // each is the adult source finding a stored torrent.
    for (k, n) in keys.iter().zip(names).take(2) {
        adult_tx2
            .send(Found::key(*k, 0).with_claims(claims(n)))
            .await
            .unwrap();
    }
    adult_tx2.send(Found::key(keys[2], 0)).await.unwrap();
    until("seen again", || {
        let w = pipeline.writer_stats();
        w.unverified.on_torrent.load(Ordering::Relaxed) == 2
            && w.rediscovered.load(Ordering::Relaxed) >= 3
    })
    .await;
    let mut store = pipeline.shutdown().await.unwrap();
    assert_eq!(kind(&store, &keys[0]), Some(ContentType::Adult));
    assert_eq!(
        content_type(&store, &keys[0]),
        Some(ContentType::Adult.code())
    );
    assert_ne!(kind(&store, &keys[1]), Some(ContentType::Adult), "yours");
    assert_eq!(kind(&store, &keys[2]), Some(ContentType::Adult), "bare");
    // The background pass agrees on each.
    reenrich(&mut store, &filter, mask);
    assert_eq!(kind(&store, &keys[0]), Some(ContentType::Adult));
    assert_ne!(kind(&store, &keys[1]), Some(ContentType::Adult));
    assert_eq!(kind(&store, &keys[2]), Some(ContentType::Adult));
}

/// Claims an always-adult source makes are judged as adult before they're
/// stored, and again by the refile sweep: the Adult-only core holds a
/// claimed name only it catches, which the same claims from another
/// source don't meet.
#[test]
fn an_always_adult_sources_claims_meet_the_adult_only_core() {
    use lodestar_pipeline::unverified::{self, Ctx, Phase, UnverifiedStats};
    use lodestar_sources::Discovery;
    use lodestar_store::unverified::{Network as UNet, NewUnverified};

    const T: i64 = 1_700_000_000;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let plain = store.register_source("plain", "1", T).unwrap();
    let adult = store.register_source("adult", "1", T).unwrap();
    let mask = AdultSources(1 << adult.get());
    let filter = stand_in_filter();
    let ctx = Ctx {
        quarantine_secs: 86_400,
        adult: mask,
        ..Ctx::default()
    };
    let found = |k: u8, source| Discovery {
        source,
        found: Found::key(InfoHash([k; 20]), 0).with_claims(claims(SPREAD)),
        at: T,
        reply: false,
    };
    let stats = UnverifiedStats::default();
    let batch = [found(1, plain), found(2, adult)];
    unverified::ingest(
        &mut store,
        &filter.current(),
        ctx,
        &batch,
        &stats,
        T,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(stats.stored.load(Ordering::Relaxed), 1, "the plain one");
    assert_eq!(stats.held.load(Ordering::Relaxed), 1);
    assert!(store.unverified(UNet::Bt, &[1; 20]).unwrap().is_some());
    assert!(store.unverified(UNet::Bt, &[2; 20]).unwrap().is_none());
    assert_eq!(
        store.held_among(&[[1; 20], [2; 20]]).unwrap(),
        [false, true]
    );
    let q = store.quarantined(10).unwrap();
    assert_eq!(
        (q.len(), q[0].hash, q[0].rule.as_str()),
        (1, [2; 20], "CS-5a")
    );

    // Stored before the source was known as always adult (an older
    // build): the sweep judges it as adult now and holds it.
    let c = lodestar_store::claims::Claims {
        name: Some(SPREAD.into()),
        ..Default::default()
    };
    store
        .put_unverified(&[NewUnverified {
            network: UNet::Bt,
            key: &[3; 20],
            source: adult,
            private_source: false,
            claims: Some(&c),
            at: T,
        }])
        .unwrap();
    let version = filter.current().version;
    let s = unverified::refile_step(
        &mut store,
        &filter,
        version,
        Phase::Items,
        0,
        (0, 0),
        ctx,
        T,
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(s.held, [[3; 20]], "only the adult source's item");
    assert!(store.unverified(UNet::Bt, &[1; 20]).unwrap().is_some());
    assert!(store.unverified(UNet::Bt, &[3; 20]).unwrap().is_none());
}
