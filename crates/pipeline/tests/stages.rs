//! The worker's stages built without a store (WK-7, architecture §19.4):
//! a remote worker's fetch pool, prepare stage and funnel from
//! `WorkerStages`, with a `Preparer` from dictionary bytes, and the
//! byte cap on what's queued for the coordinator (owner §1.6.5).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_cluster::{Token, WorkerId};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, FetchOptions, Fetched};
use lodestar_pipeline::default_filter;
use lodestar_pipeline::gate::GateStats;
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::stages::{
    ByteReceiver, ByteSender, OUTBOUND_BYTES, Queues, Reports, Results, Running, Weigh,
    WorkerLeases, WorkerStages, byte_queue,
};
use lodestar_sources::{
    BoxFuture, Discovery, Fetcher, Found, FunnelCounts, Loss, PoolConfig, Shutdown,
};
use lodestar_store::{Preparer, SourceId, StoreConfig};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// An info dict whose unknown `x-pad` key is `pad` bytes, so its fetched
/// dict and its prepared result each weigh at least that.
fn info(name: &str, pad: usize) -> Vec<u8> {
    to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(1 << 20)),
            (b"name".to_vec(), OwnedValue::from(name)),
            (b"piece length".to_vec(), OwnedValue::Int(1 << 18)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 80])),
            (b"x-pad".to_vec(), OwnedValue::Bytes(vec![b'p'; pad])),
        ]
        .into_iter()
        .collect(),
    ))
}

fn key_of(raw: &[u8]) -> InfoHash {
    InfoHash(lodestar_peer_wire::testing::Seed::v1(raw.to_vec()).info_hash)
}

/// Serves each key's dict from memory, counting fetches.
#[derive(Default)]
struct MapFetch {
    infos: Mutex<HashMap<InfoHash, Vec<u8>>>,
    calls: AtomicUsize,
}

impl Fetcher for MapFetch {
    fn fetch(
        &self,
        _peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let info = self.infos.lock().unwrap().get(&key).cloned();
        Box::pin(async move {
            info.map(|info| Fetched {
                info,
                v2: false,
                encrypted: false,
                pex: Vec::new(),
                client: None,
            })
            .ok_or(FailureKind::Refused)
        })
    }
}

/// What a remote worker's session would be told.
#[derive(Default)]
struct Told(Mutex<Vec<(InfoHash, Token, Loss)>>);

impl Reports for Told {
    fn lost(&self, key: &InfoHash, token: Token, loss: Loss) -> bool {
        self.0.lock().unwrap().push((*key, token, loss));
        true
    }
}

struct Worker {
    running: Running,
    discoveries: mpsc::Sender<Discovery>,
    leases: Arc<WorkerLeases>,
    stop: watch::Sender<bool>,
}

const WORKER: WorkerId = WorkerId(7);

fn source() -> SourceId {
    SourceId::new(1).unwrap()
}

/// A worker's stages: no store anywhere, only a dictionary-less preparer
/// at the store's defaults.
fn worker(
    fetcher: Arc<MapFetch>,
    reports: Arc<Told>,
    filter: Arc<lodestar_filter::Handle>,
    pool: PoolConfig,
    queues: Queues,
    out: ByteSender<Results>,
) -> Worker {
    let defaults = StoreConfig::default();
    let leases = Arc::new(WorkerLeases::default());
    let (tx, rx) = mpsc::channel(1024);
    let (stop, shutdown) = Shutdown::new();
    let running = WorkerStages {
        worker: WORKER,
        pool,
        fetcher,
        peer_finders: Vec::new(),
        weights: HashMap::new(),
        restore: Vec::new(),
        preparer: Preparer::from_dict(None, defaults.compression_level, defaults.archive_raw),
        filter,
        reports,
        leases: leases.clone(),
        funnel: Arc::new(FunnelCounts::default()),
        gate: Arc::new(GateStats::new(0)),
        stats: Arc::default(),
        adult: AdultSources::default(),
        queues,
    }
    .spawn(rx, shutdown, out);
    Worker {
        running,
        discoveries: tx,
        leases,
        stop,
    }
}

impl Worker {
    /// Claims (here: a lease the test grants) and hands the key to the pool
    /// with a peer that has it.
    async fn found(&self, key: InfoHash, token: u64) {
        self.leases.insert(key, Token(token));
        let mut found = Found::key(key, 0);
        found.peers = vec!["192.0.2.1:6881".parse().unwrap()];
        self.discoveries
            .send(Discovery {
                source: source(),
                found,
                at: 1_700_000_000,
                reply: false,
            })
            .await
            .unwrap();
    }
}

/// Waits for `done` for at most 10 s, polling, and fails with what it saw.
async fn until(what: &str, mut done: impl FnMut() -> bool, saw: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out: {what}; saw {}",
            saw()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn recv(rx: &mut ByteReceiver<Results>) -> Results {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("a result within 10 s")
        .expect("open")
        .into_inner()
}

/// A worker fetches, prepares and judges with no store: a usable dict
/// becomes its result under the lease's token and the worker's id, a hash
/// on the floor's list and unusable metainfo are reported lost under
/// their tokens, and no lease is left behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worker_prepares_without_a_store() {
    let fetcher = Arc::new(MapFetch::default());
    let told = Arc::new(Told::default());
    let filter = Arc::new(default_filter());
    let raws = [info("Fine", 16), info("Floor", 16), b"de".to_vec()];
    let keys: Vec<InfoHash> = raws.iter().map(|r| key_of(r)).collect();
    for (k, r) in keys.iter().zip(&raws) {
        fetcher.infos.lock().unwrap().insert(*k, r.clone());
    }
    filter.update(|core| {
        core.cs
            .add_hash(keys[1].0, lodestar_filter::Origin::Imported)
    });
    let (out, mut rx) = byte_queue(OUTBOUND_BYTES, 64);
    let w = worker(
        fetcher.clone(),
        told.clone(),
        filter,
        PoolConfig::default(),
        Queues::worker(),
        out,
    );
    for (n, k) in keys.iter().enumerate() {
        w.found(*k, 100 + n as u64).await;
    }
    let r = recv(&mut rx).await;
    assert_eq!(r.key, keys[0]);
    assert_eq!(r.token, Token(100));
    assert_eq!(r.worker, WORKER);
    assert_eq!(r.torrent.name, "Fine");
    assert_eq!(r.torrent.files.dict_id, None);
    assert!(r.weigh() > 16, "a result weighs its heap");
    until(
        "both losses reported",
        || told.0.lock().unwrap().len() == 2,
        || format!("{:?}", told.0.lock().unwrap()),
    )
    .await;
    let mut lost = told.0.lock().unwrap().clone();
    lost.sort_by_key(|(_, t, _)| t.0);
    assert_eq!(
        lost,
        vec![
            (keys[1], Token(101), Loss::Blocked),
            (keys[2], Token(102), Loss::Invalid),
        ]
    );
    assert!(w.leases.is_empty());
    // Stopped, the pool ends and the prepare stage after it.
    w.stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), w.running.pool_task)
        .await
        .expect("the pool stops")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), w.running.prepare)
        .await
        .expect("the prepare stage ends")
        .unwrap();
}

/// A stalled link pauses fetching: with nobody reading what's queued for
/// the coordinator, the queue stays within its cap and the pool stops at
/// what the two capped queues and its sockets can hold, far short of what
/// was found. Once the link moves again every key arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_link_pauses_fetching() {
    const KEYS: usize = 400;
    const PAD: usize = 4096;
    const CAP: usize = 64 << 10;
    const SOCKETS: usize = 4;
    let fetcher = Arc::new(MapFetch::default());
    let told = Arc::new(Told::default());
    let keys: Vec<InfoHash> = (0..KEYS)
        .map(|i| {
            let raw = info(&format!("Torrent {i}"), PAD);
            let k = key_of(&raw);
            fetcher.infos.lock().unwrap().insert(k, raw);
            k
        })
        .collect();
    let (out, mut rx) = byte_queue(CAP, 1024);
    let queues = Queues {
        fetched_bytes: Some(CAP),
        ..Queues::worker()
    };
    let pool = PoolConfig {
        max_sockets: SOCKETS,
        ..PoolConfig::default()
    };
    let w = worker(
        fetcher.clone(),
        told.clone(),
        Arc::new(default_filter()),
        pool,
        queues,
        out.clone(),
    );
    for (n, k) in keys.iter().enumerate() {
        w.found(*k, n as u64 + 1).await;
    }
    // Every item weighs more than PAD, so each capped queue holds at most
    // CAP / PAD of them, plus one in the pool's channel, one in the
    // forwarder's hand and one per socket.
    let bound = 2 * (CAP / PAD + 1) + 2 + SOCKETS;
    until(
        "the outbound queue to fill",
        || out.queued_bytes() + 2 * PAD > CAP,
        || format!("{} bytes", out.queued_bytes()),
    )
    .await;
    // Let the pool run on as far as it can.
    for _ in 0..200 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(out.queued_bytes() <= CAP, "{} > {CAP}", out.queued_bytes());
    }
    let calls = fetcher.calls.load(Ordering::SeqCst);
    assert!(
        calls <= bound,
        "{calls} fetches with the link stalled, bound {bound}"
    );
    assert!(told.0.lock().unwrap().is_empty(), "nothing lost");

    // The link moves: everything arrives, once each, within the cap.
    let mut seen = std::collections::HashSet::new();
    while seen.len() < KEYS {
        assert!(out.queued_bytes() <= CAP);
        let r = recv(&mut rx).await;
        assert_eq!(
            r.token.0,
            keys.iter().position(|k| *k == r.key).unwrap() as u64 + 1
        );
        assert!(seen.insert(r.key), "once each");
    }
    assert_eq!(fetcher.calls.load(Ordering::SeqCst), KEYS);
    assert!(w.leases.is_empty());
}

/// What the stages hold on a remote worker at defaults, in the worst case:
/// one info dict in the pool's channel and one in the forwarder's hand,
/// the fetched queue (whatever is being prepared included) and what its
/// prepared rows hold, and the outbound queue. A queue holds its cap, or
/// one item larger than it alone. Beside the plan's Q5 estimates for the
/// rest (pool ≤ 46, have-filter ≤ 31 during a swap, DHT ≤ 60, runtime ≤
/// 30, inbound events 8 MiB) it stays inside the 256 MB budget (owner
/// §1.6.5).
#[test]
fn the_worker_queues_fit_the_budget() {
    const MIB: usize = 1 << 20;
    let q = Queues::worker();
    let fetched = q.fetched_bytes.expect("a worker caps fetched dicts");
    assert_eq!(fetched, 8 * MIB);
    assert_eq!(OUTBOUND_BYTES, 8 * MIB);
    let largest = FetchOptions::default().max_metadata;
    let typical = 2 * fetched + OUTBOUND_BYTES;
    let worst = 2 * largest + 2 * fetched.max(largest) + OUTBOUND_BYTES.max(largest);
    let others = (46 + 31 + 60 + 30 + 8) * MIB;
    assert!(
        typical + others <= 256 * 1_000_000,
        "{}",
        (typical + others) / MIB
    );
    assert!(
        worst + others <= 256 * MIB,
        "{} MiB",
        (worst + others) / MIB
    );
}
