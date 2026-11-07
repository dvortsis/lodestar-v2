//! X-refetch-private: the start queues imports with partial file lists
//! for refetch through the first enabled peer finder. Only public ones:
//! a key with any `flags::PRIVACY` bit (BEP 27 private, a private source's
//! find, a remote row whose `PRIVATE` isn't verified) never reaches public
//! peer finding (§18.7, BEP 27), on the first start or after a restart.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
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

fn info_dict(name: &str, bep27: bool) -> Vec<u8> {
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
    if bep27 {
        d.insert(b"private".to_vec(), OwnedValue::Int(1));
    }
    to_vec(&OwnedValue::Dict(d))
}

/// A peer finder that answers every request from one fake peer and
/// records each key it was asked about: what public peer finding saw.
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

/// Stores `raw` as an import with no file list and `extra` flags, and
/// checks the row carries every privacy bit it was built with.
fn import_partial(store: &mut Store, raw: &[u8], extra: i64) -> InfoHash {
    let mut info = Info::parse(raw).unwrap();
    let bep27 = info.private;
    info.files.clear();
    let src = store.register_source("bitmagnet", "1", 0).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: 1_600_000_000,
            metadata_at: 1_600_000_000,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL | extra,
            source: src,
        }])
        .unwrap()[0]
        .id();
    let row = store.torrent(id).unwrap().unwrap();
    let want = flags::FILES_PARTIAL | extra | if bep27 { flags::PRIVATE } else { 0 };
    assert_eq!(row.flags & want, want);
    row.info_hash
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_partials_never_reach_public_peer_finding_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let raws = [
        info_dict("Private.Source", false),
        info_dict("Bep27", true),
        info_dict("Remote.Unverified", false),
        info_dict("Public.One", false),
        info_dict("Public.Two", false),
    ];
    let seeds: Vec<Seed> = raws.iter().map(|r| Seed::v1(r.clone())).collect();
    // The peer has every one of them: a private key that reached the
    // finder would be fetched, not just looked up.
    let peer = FakePeer::spawn(seeds, Accept::Both, Behavior::Honest, vec![]).await;
    let asked = Arc::new(Mutex::new(Vec::new()));

    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    // The private ones first: the oldest, so the first under the cap.
    let private = [
        import_partial(&mut store, &raws[0], flags::PRIVATE_SOURCE),
        import_partial(&mut store, &raws[1], 0),
        import_partial(&mut store, &raws[2], flags::REMOTE_UNVERIFIED),
    ];
    let public_one = import_partial(&mut store, &raws[3], 0);

    // First start: only the public import is refetched.
    let pipeline = Pipeline::start(config(), store, vec![finder(peer.addr(), &asked)])
        .await
        .unwrap();
    until("the public import refetched", || {
        pipeline.writer_stats().existing.load(Ordering::Relaxed) >= 1
    })
    .await;
    let mut store = pipeline.shutdown().await.unwrap();
    let row = |s: &Store, k: &InfoHash| s.torrent(s.find(k).unwrap().unwrap()).unwrap().unwrap();
    assert_eq!(row(&store, &public_one).flags & flags::FILES_PARTIAL, 0);
    // A second public import, so the restart has something to show for it.
    let public_two = import_partial(&mut store, &raws[4], 0);
    drop(store);

    // Restart from the same file: the private partials are still partial,
    // and still never queued.
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config(), store, vec![finder(peer.addr(), &asked)])
        .await
        .unwrap();
    until("the second public import refetched", || {
        pipeline.writer_stats().existing.load(Ordering::Relaxed) >= 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();

    let asked = asked.lock().unwrap().clone();
    for key in &private {
        assert!(
            !asked.contains(key),
            "a private key reached the peer finder"
        );
        assert_ne!(
            row(&store, key).flags & flags::FILES_PARTIAL,
            0,
            "a private partial is never fetched"
        );
    }
    assert!(asked.contains(&public_one) && asked.contains(&public_two));
    assert_eq!(row(&store, &public_two).flags & flags::FILES_PARTIAL, 0);
    // Nor is one waiting in the saved queue for the next start.
    let saved = store.load_fetch_queue().unwrap();
    assert!(saved.iter().all(|q| !private.contains(&q.info_hash)));
}
