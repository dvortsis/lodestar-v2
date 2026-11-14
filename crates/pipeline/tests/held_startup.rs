//! FILTER-15 at start-up: a partial row left stored under a
//! held hash is deleted once, before anything is queued, and never
//! refetched; a partial row whose refetched metadata is held (here, too
//! large to judge) is held and deleted when the refetch commits.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_peer_wire::testing::{Accept, Behavior, FakePeer, Seed};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{
    BoxFuture, Consent, Kind, Manifest, Network, PluginError, PoolConfig, Priority, Risk,
    SourceCtx, SourcePlugin,
};
use lodestar_store::{NewTorrent, Store, StoreConfig, flags};

/// An info dict of `files` files, each with a path of `words` words.
fn info_dict(name: &str, files: usize, words: usize) -> Vec<u8> {
    let list = (0..files)
        .map(|i| {
            let path: String = (0..words).map(|w| format!("w{i}x{w} ")).collect();
            let mut f = BTreeMap::new();
            f.insert(b"length".to_vec(), OwnedValue::Int(1_000));
            f.insert(
                b"path".to_vec(),
                OwnedValue::List(vec![OwnedValue::from(format!("{path}.mkv").as_str())]),
            );
            OwnedValue::Dict(f)
        })
        .collect();
    let mut d = BTreeMap::new();
    d.insert(b"files".to_vec(), OwnedValue::List(list));
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 18));
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20 * 12]));
    to_vec(&OwnedValue::Dict(d))
}

/// A peer finder that answers every request from one fake peer and
/// records each key it was asked about.
struct Recording {
    manifest: Manifest,
    peer: std::net::SocketAddr,
    asked: Arc<Mutex<Vec<InfoHash>>>,
}

impl SourcePlugin for Recording {
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
                        self.asked.lock().unwrap().push(request.key);
                        let (done, rx) = tokio::sync::oneshot::channel();
                        let _ = request.admit.send(Some(rx));
                        let _ = done.send(vec![self.peer]);
                    }
                }
            }
        })
    }
}

fn finder(peer: std::net::SocketAddr, asked: &Arc<Mutex<Vec<InfoHash>>>) -> Arc<Recording> {
    Arc::new(Recording {
        manifest: Manifest {
            id: "finder",
            name: "finder",
            version: "1",
            kinds: &[Kind::PeerFinder],
            networks: &[Network::Bt],
            endpoints: &[],
            risk: Risk::Low,
            consent: Consent::None,
            default_priority: Priority::Normal,
            enabled_by_default: true,
            always_adult: false,
            finds_private: false,
        },
        peer,
        asked: asked.clone(),
    })
}

fn config() -> PipelineConfig {
    PipelineConfig {
        // The fake peer is on loopback.
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

/// Polls until a 10 s deadline (standards §2), so the slowest runner
/// passes however long each poll takes.
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Stores `raw` as an import with no file list: its id and hash.
fn import_partial(store: &mut Store, raw: &[u8]) -> (i64, InfoHash) {
    let mut info = Info::parse(raw).unwrap();
    info.files.clear();
    let src = store.register_source("importer", "1", 0).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: 1_600_000_000,
            metadata_at: 1_600_000_000,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            source: src,
        }])
        .unwrap()[0]
        .id();
    (id, store.torrent(id).unwrap().unwrap().info_hash)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_partials_are_deleted_at_start_or_on_refetch() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let held_raw = info_dict("Held.Import", 1, 1);
    // Far past the core's work budget, so the refetched list can't be
    // judged whole.
    let large_raw = info_dict("Large.Import", 3_000, 120);
    let large = Info::parse(&large_raw).unwrap();
    let files = lodestar_classify::file_infos(&large.files, large.path_encoding);
    assert!(lodestar_filter::too_large(&lodestar_filter::Item {
        hashes: &[],
        name: &large.name.decode(),
        files: &files,
        content_type: lodestar_classify::ContentType::Other,
    }));
    let peer = FakePeer::spawn(
        vec![Seed::v1(held_raw.clone()), Seed::v1(large_raw.clone())],
        Accept::Both,
        Behavior::Honest,
        vec![],
    )
    .await;
    let asked = Arc::new(Mutex::new(Vec::new()));

    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let (held_id, held_hash) = import_partial(&mut store, &held_raw);
    let (large_id, large_hash) = import_partial(&mut store, &large_raw);
    // Held under its hash, but still stored, as an earlier build left it.
    store
        .connection()
        .execute(
            "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'U-1', 1)",
            [&held_hash.0[..]],
        )
        .unwrap();

    let pipeline = Pipeline::start(config(), store, vec![finder(peer.addr(), &asked)])
        .await
        .unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    until("the large import held on refetch", || {
        reader.torrent(large_id).unwrap().is_none()
            && reader.held_among(&[large_hash.0]).unwrap() == [true]
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();

    // Deleted at start, and never asked for.
    assert!(
        store.torrent(held_id).unwrap().is_none(),
        "deleted at start"
    );
    assert!(store.find(&held_hash).unwrap().is_none());
    let asked = asked.lock().unwrap().clone();
    assert!(!asked.contains(&held_hash), "never refetched");
    // Refetched, held as too large to judge, and deleted with the hold.
    assert!(asked.contains(&large_hash));
    assert!(store.find(&large_hash).unwrap().is_none());
    assert_eq!(
        store.quarantine_rule(&large_hash.0).unwrap().as_deref(),
        Some("CS-0a")
    );
}
