//! Accepting a pack version releases nothing it held before, with real
//! holds behind it (RP-G's leftover, X-commit-rejudge-reuse): torrents an
//! earlier build's MW-2@1 and FK-2@1 hid and flagged are stored through
//! the pipeline, with their filter hits, beside a torrent a rule of yours
//! sent to the review queue. Version 2 of both is accepted, as the API
//! accepts it (`upgrade::accept`, then the engine loaded again), and the
//! pipeline runs again under it: every review row, hit, tombstone and
//! stored flag is as it was, while a new copy of the same torrent is now
//! judged by version 2, which lets it through.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::FileInfo;
use lodestar_filter::corpus::Corpus;
use lodestar_filter::packs::manifest::Manifest;
use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
use lodestar_filter::packs::{Pack, PackRule};
use lodestar_filter::{Action, Engine};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Found, Kind, Manifest as SourceManifest, Network, PluginError, PoolConfig,
    Priority, Risk, SourceCtx, SourcePlugin,
};
use lodestar_store::{Store, StoreConfig, flags};
use tokio::sync::mpsc;

/// An info dict with these files, each path split into its segments.
fn info_dict(name: &str, files: &[FileInfo], salt: u8) -> Vec<u8> {
    let files = files
        .iter()
        .map(|f| {
            let mut d = BTreeMap::new();
            d.insert(b"length".to_vec(), OwnedValue::Int(f.size as i64));
            d.insert(
                b"path".to_vec(),
                OwnedValue::List(
                    f.path
                        .iter()
                        .map(|s| OwnedValue::from(s.as_str()))
                        .collect(),
                ),
            );
            OwnedValue::Dict(d)
        })
        .collect();
    let mut d = BTreeMap::new();
    d.insert(b"files".to_vec(), OwnedValue::List(files));
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    // Another salt, another info hash: a copy of the same torrent.
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![salt; 80]));
    to_vec(&OwnedValue::Dict(d))
}

/// A source the test drives through a channel.
struct Scripted {
    manifest: SourceManifest,
    rx: Mutex<Option<mpsc::Receiver<Found>>>,
}

fn scripted() -> (Arc<Scripted>, mpsc::Sender<Found>) {
    let (tx, rx) = mpsc::channel(64);
    let s = Scripted {
        manifest: SourceManifest {
            id: "feed",
            name: "feed",
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
        },
        rx: Mutex::new(Some(rx)),
    };
    (Arc::new(s), tx)
}

impl SourcePlugin for Scripted {
    fn manifest(&self) -> &SourceManifest {
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

/// Every row that holds or releases (the review queue, the tombstones,
/// the filter hits and your exceptions), and each stored torrent's flags.
fn holds(store: &Store) -> Vec<String> {
    [
        "SELECT hex(hash) || ' ' || rule || ' ' || at || ' ' || expires_at || ' ' || ifnull(hex(sealed), '') AS r FROM quarantine ORDER BY hash",
        "SELECT hex(hash) || ' ' || rule || ' ' || at AS r FROM tombstones ORDER BY hash",
        "SELECT id || ' ' || hex(hash) || ' ' || rule || ' ' || action || ' ' || at || ' ' || hex(evidence) AS r FROM filter_hits ORDER BY id",
        "SELECT id || ' ' || rule || ' ' || kind || ' ' || hex(value) AS r FROM filter_exceptions ORDER BY id",
        "SELECT id || ' ' || flags AS r FROM torrents ORDER BY id",
    ]
    .iter()
    .map(|q| {
        let rows: Option<String> = store
            .connection()
            .query_row(&format!("SELECT group_concat(r, ',') FROM ({q})"), [], |r| {
                r.get(0)
            })
            .unwrap();
        rows.unwrap_or_default()
    })
    .collect()
}

/// The precision item on `line`, which version 1 of `rule` holds and
/// version 2 lets through (`crates/filter/tests/packs_gate.rs`,
/// `KEPT_UNTIL_ACCEPTED`).
fn kept_until_accepted(line: usize) -> (String, Vec<FileInfo>) {
    let corpus = Corpus::builtin().expect("the shipped corpus");
    let p = corpus
        .precision
        .iter()
        .find(|p| p.example.line == line)
        .expect("the precision item");
    assert!(
        p.example.content_type.is_none(),
        "judged as the pipeline classifies it"
    );
    (p.example.name.clone(), p.example.files.clone())
}

/// The engine as the pipeline loads it from `store`.
fn engine(store: &Store) -> Engine {
    let h = lodestar_pipeline::default_filter();
    let from = lodestar_pipeline::StoreFilter::read(store).unwrap();
    assert!(lodestar_pipeline::load_filter(&h, &from).is_empty());
    Engine::clone(&h.current())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepting_a_version_releases_no_stored_hold() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    // An install an earlier build set up, every pack on: MW-2@1 and
    // FK-2@1 apply, version 2 of each pending.
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    for p in Pack::ALL {
        store.set_pack_switch(p.as_str(), true).unwrap();
    }
    upgrade::ensure_seeded(&mut store, &Manifest::builtin(), 0).unwrap();
    // A rule of yours that sends a torrent to the review queue.
    store
        .add_rule("beaconwood", Action::Quarantine.code(), None, None)
        .unwrap();
    let e = engine(&store);
    for r in [PackRule::Mw2, PackRule::Fk2] {
        assert!(e.pack_versions.state(r).pending(), "{} pending", r.id());
    }

    let (mw2_name, mw2_files) = kept_until_accepted(23);
    let (fk2_name, fk2_files) = kept_until_accepted(107);
    let quarantined: Vec<FileInfo> = vec![FileInfo::new(
        vec!["Beaconwood.2024.mkv".into()],
        900_000_000,
    )];
    let seeds = vec![
        Seed::v1(info_dict(&mw2_name, &mw2_files, 1)),
        Seed::v1(info_dict(&fk2_name, &fk2_files, 1)),
        Seed::v1(info_dict("Beaconwood 2024", &quarantined, 1)),
        // A copy of the MW-2 item, found once version 2 applies.
        Seed::v1(info_dict(&mw2_name, &mw2_files, 2)),
    ];
    let keys: Vec<InfoHash> = seeds.iter().map(|s| InfoHash(s.info_hash)).collect();
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;

    // Version 1 holds them: stored hidden and flagged, with their hits,
    // and the review queue holds the third.
    let (feed, feed_tx) = scripted();
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    for k in &keys[..3] {
        feed_tx
            .send(Found::key(*k, 0).with_peers(vec![peer.addr()]))
            .await
            .unwrap();
    }
    until("stored and quarantined", || {
        let w = pipeline.writer_stats();
        w.stored.load(Ordering::Relaxed) == 2 && w.quarantined.load(Ordering::Relaxed) == 1
    })
    .await;
    let mut store = pipeline.shutdown().await.unwrap();
    let hit = |store: &Store, key: &InfoHash| {
        let hits = store.filter_hits(0).unwrap();
        let h = hits.iter().find(|h| h.hash == key.0).cloned();
        h.map(|h| {
            (
                h.rule,
                Action::from_code(h.action & !lodestar_store::SHADOW),
            )
        })
    };
    let stored = |store: &Store, key: &InfoHash| {
        let id = store.find(key).unwrap().expect("stored");
        store.torrent(id).unwrap().unwrap().flags
    };
    assert_eq!(
        hit(&store, &keys[0]),
        Some(("MW-2".into(), Some(Action::Hide)))
    );
    assert_ne!(
        stored(&store, &keys[0]) & flags::HIDDEN,
        0,
        "hidden by MW-2@1"
    );
    assert_eq!(
        hit(&store, &keys[1]),
        Some(("FK-2".into(), Some(Action::Flag)))
    );
    assert_ne!(
        stored(&store, &keys[1]) & flags::FLAGGED,
        0,
        "flagged by FK-2@1"
    );
    let q = store.quarantined(10).unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].hash, keys[2].0);
    assert!(
        store.find(&keys[2]).unwrap().is_none(),
        "held back, not stored"
    );
    let before = holds(&store);

    // Accepted, as the API accepts it, and the pipeline started again
    // under version 2: its sweeps re-judge through the rule-change path.
    upgrade::accept(
        &mut store,
        &Manifest::builtin(),
        &[PackRule::Mw2, PackRule::Fk2],
        10,
    )
    .unwrap();
    let e = engine(&store);
    for r in [PackRule::Mw2, PackRule::Fk2] {
        let st = e.pack_versions.state(r);
        assert!(!st.pending() && st.plain, "{} accepted", r.id());
    }
    assert_eq!(holds(&store), before, "acceptance writes no hold");
    let (feed, feed_tx) = scripted();
    let pipeline = Pipeline::start(config(), store, vec![feed]).await.unwrap();
    // The copy is judged by version 2 and stored as it is; the writer has
    // run under the accepted versions by then.
    feed_tx
        .send(Found::key(keys[3], 0).with_peers(vec![peer.addr()]))
        .await
        .unwrap();
    until("the copy stored", || {
        pipeline.writer_stats().stored.load(Ordering::Relaxed) == 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(
        hit(&store, &keys[3]),
        None,
        "version 2 lets the copy through"
    );
    assert_eq!(
        stored(&store, &keys[3]) & (flags::HIDDEN | flags::FLAGGED),
        0
    );
    // Everything held before is held as it was: the copy's row is the
    // only new one.
    let after = holds(&store);
    let copy = store.find(&keys[3]).unwrap().unwrap();
    let without_copy = |rows: &str| {
        rows.split(',')
            .filter(|r| !r.starts_with(&format!("{copy} ")))
            .collect::<Vec<_>>()
            .join(",")
    };
    assert_eq!(
        after[..4],
        before[..4],
        "no review row, tombstone, hit or exception released"
    );
    assert_eq!(without_copy(&after[4]), before[4], "no stored flag cleared");
    assert_ne!(stored(&store, &keys[0]) & flags::HIDDEN, 0);
    assert_ne!(stored(&store, &keys[1]) & flags::FLAGGED, 0);
    assert_eq!(store.quarantined(10).unwrap().len(), 1);
}
