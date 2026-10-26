//! The shared fetch pool against a scripted fetcher, in paused time.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::pool::spawn;
use lodestar_sources::{
    BoxFuture, Discovery, FetchedKey, Fetcher, Found, FunnelCounts, Loss, PoolConfig, PoolHandle,
    PoolIo, Shutdown, Stage,
};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

const T0: i64 = 1_700_000_000;
const COHORT: i64 = T0 / 3600;

fn src(n: u8) -> SourceId {
    SourceId::new(n).unwrap()
}

fn key(n: u32) -> InfoHash {
    let mut h = [0u8; 20];
    h[..4].copy_from_slice(&n.to_be_bytes());
    InfoHash(h)
}

fn peer(ip: u8, port: u16) -> SocketAddr {
    SocketAddr::from(([10, 0, 0, ip], port))
}

fn found(source: u8, n: u32, peers: Vec<SocketAddr>) -> Discovery {
    Discovery {
        source: src(source),
        found: Found::key(key(n), 0).with_peers(peers),
        at: T0,
        reply: false,
    }
}

/// How a scripted peer answers, and after how long.
#[derive(Clone, Copy)]
struct Script {
    delay: Duration,
    result: Result<(), FailureKind>,
}

#[derive(Default)]
struct FakeState {
    scripts: Mutex<HashMap<SocketAddr, Script>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    max_per_ip: AtomicUsize,
    started: Mutex<Vec<(InfoHash, SocketAddr)>>,
}

struct Fake(Arc<FakeState>);

impl Fetcher for Fake {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let s = self.0.clone();
        Box::pin(async move {
            let script = s
                .scripts
                .lock()
                .unwrap()
                .get(&peer)
                .copied()
                .unwrap_or(Script {
                    delay: Duration::from_secs(1),
                    result: Ok(()),
                });
            s.started.lock().unwrap().push((key, peer));
            let now = s.active.fetch_add(1, Ordering::SeqCst) + 1;
            s.max_active.fetch_max(now, Ordering::SeqCst);
            let on_ip = {
                let mut m = s.per_ip.lock().unwrap();
                let n = m.entry(peer.ip()).or_insert(0);
                *n += 1;
                *n
            };
            s.max_per_ip.fetch_max(on_ip, Ordering::SeqCst);
            sleep(script.delay).await;
            s.active.fetch_sub(1, Ordering::SeqCst);
            *s.per_ip.lock().unwrap().get_mut(&peer.ip()).unwrap() -= 1;
            script.result.map(|()| Fetched {
                info: key.0.to_vec(),
                v2: false,
                encrypted: true,
                pex: Vec::new(),
                client: None,
            })
        })
    }
}

struct Harness {
    disc: mpsc::Sender<Discovery>,
    lookups: mpsc::Receiver<InfoHash>,
    out: mpsc::Receiver<FetchedKey>,
    funnel: Arc<FunnelCounts>,
    handle: PoolHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<Vec<QueuedKey>>,
    fake: Arc<FakeState>,
}

impl Harness {
    fn start(config: PoolConfig, weights: &[(u8, u32)], restore: Vec<QueuedKey>) -> Harness {
        let fake = Arc::new(FakeState::default());
        Harness::with_fake(config, weights, restore, fake)
    }

    fn with_fake(
        config: PoolConfig,
        weights: &[(u8, u32)],
        restore: Vec<QueuedKey>,
        fake: Arc<FakeState>,
    ) -> Harness {
        let (disc, discoveries) = mpsc::channel(4096);
        let (finder, mut requests) = mpsc::channel::<lodestar_sources::PeerRequest>(4096);
        let (keys, lookups) = mpsc::channel(4096);
        tokio::spawn(async move {
            let mut held = Vec::<tokio::sync::oneshot::Sender<Vec<SocketAddr>>>::new();
            while let Some(request) = requests.recv().await {
                held.retain(|tx| !tx.is_closed());
                let (done, rx) = tokio::sync::oneshot::channel();
                if request.admit.send(Some(rx)).is_ok() {
                    held.push(done);
                    let _ = keys.send(request.key).await;
                }
            }
        });
        let (out_tx, out) = mpsc::channel(4096);
        let (stop, shutdown) = Shutdown::new();
        let funnel = Arc::new(FunnelCounts::default());
        let (handle, task) = spawn(
            config,
            Arc::new(Fake(fake.clone())),
            PoolIo {
                discoveries,
                peer_finders: vec![finder],
                out: out_tx,
                funnel: funnel.clone(),
                shutdown,
            },
            weights.iter().map(|&(s, w)| (src(s), w)).collect(),
            restore,
        );
        Harness {
            disc,
            lookups,
            out,
            funnel,
            handle,
            stop,
            task,
            fake,
        }
    }

    fn script(&self, peer: SocketAddr, delay_ms: u64, result: Result<(), FailureKind>) {
        self.fake.scripts.lock().unwrap().insert(
            peer,
            Script {
                delay: Duration::from_millis(delay_ms),
                result,
            },
        );
    }

    fn send(&self, d: Discovery) {
        self.disc.try_send(d).unwrap();
    }

    fn drain_lookups(&mut self) -> Vec<InfoHash> {
        std::iter::from_fn(|| self.lookups.try_recv().ok()).collect()
    }

    async fn stop(self) -> Vec<QueuedKey> {
        self.stop.send(true).unwrap();
        self.task.await.unwrap()
    }
}

fn config() -> PoolConfig {
    PoolConfig {
        lookup_wait: Duration::from_secs(10),
        retry_backoff: Duration::from_secs(60),
        max_attempts: 3,
        ..PoolConfig::default()
    }
}

#[tokio::test(start_paused = true)]
async fn admission_rejection_empty_completion_and_cancellation_are_owned() {
    let (disc, discoveries) = mpsc::channel(16);
    let (finder, mut requests) = mpsc::channel::<lodestar_sources::PeerRequest>(1);
    let (out, _results) = mpsc::channel(16);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        config(),
        Arc::new(Fake(Arc::new(FakeState::default()))),
        PoolIo {
            discoveries,
            peer_finders: vec![finder],
            out,
            funnel: Arc::new(FunnelCounts::default()),
            shutdown,
        },
        HashMap::new(),
        vec![],
    );
    disc.send(found(0, 55, vec![])).await.unwrap();
    let request = requests.recv().await.unwrap();
    request.admit.send(None).unwrap();
    async fn drained(handle: &PoolHandle) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while handle.stats().lookups.load(Ordering::Relaxed) != 0 {
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
    }
    drained(&handle).await;
    assert_eq!(
        handle.snapshot().await[0].attempts,
        0,
        "rejected work is not attempted"
    );
    let request = requests.recv().await.unwrap();
    let (done, rx) = tokio::sync::oneshot::channel();
    request.admit.send(Some(rx)).unwrap();
    done.send(vec![]).unwrap();
    drained(&handle).await;
    assert_eq!(
        handle.snapshot().await[0].attempts,
        1,
        "empty accepted result counts once"
    );
    let request = requests.recv().await.unwrap();
    let (mut done, rx) = tokio::sync::oneshot::channel();
    request.admit.send(Some(rx)).unwrap();
    // An alternate peer cancels the still-owned lookup.
    disc.send(found(0, 55, vec![peer(1, 1)])).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), done.closed())
        .await
        .unwrap();
    assert!(
        done.send(vec![peer(2, 2)]).is_err(),
        "old completions cannot reach a new generation"
    );
    disc.send(found(0, 56, vec![])).await.unwrap();
    let request = requests.recv().await.unwrap();
    assert_eq!(request.key, key(56));
    let (mut done, rx) = tokio::sync::oneshot::channel();
    request.admit.send(Some(rx)).unwrap();
    tokio::time::timeout(Duration::from_secs(30), done.closed())
        .await
        .unwrap();
    assert_eq!(
        handle
            .snapshot()
            .await
            .iter()
            .find(|q| q.info_hash == key(56))
            .unwrap()
            .attempts,
        1
    );
    disc.send(found(0, 57, vec![])).await.unwrap();
    let request = requests.recv().await.unwrap();
    assert_eq!(request.key, key(57));
    let (mut done, rx) = tokio::sync::oneshot::channel();
    request.admit.send(Some(rx)).unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
    assert!(
        done.is_closed(),
        "shutdown cancels owned completion receivers"
    );
    done.closed().await;
}

#[tokio::test(start_paused = true)]
async fn multiple_finders_share_one_attempt_and_all_cancel_on_timeout() {
    let (disc, discoveries) = mpsc::channel(16);
    let (finder_a, mut requests_a) = mpsc::channel::<lodestar_sources::PeerRequest>(1);
    let (finder_b, mut requests_b) = mpsc::channel::<lodestar_sources::PeerRequest>(1);
    let (out, _results) = mpsc::channel(16);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        config(),
        Arc::new(Fake(Arc::new(FakeState::default()))),
        PoolIo {
            discoveries,
            peer_finders: vec![finder_a, finder_b],
            out,
            funnel: Arc::new(FunnelCounts::default()),
            shutdown,
        },
        HashMap::new(),
        vec![],
    );
    disc.send(found(0, 58, vec![])).await.unwrap();
    let a = tokio::time::timeout(Duration::from_secs(10), requests_a.recv())
        .await
        .unwrap()
        .unwrap();
    let b = tokio::time::timeout(Duration::from_secs(10), requests_b.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!((a.key, b.key), (key(58), key(58)));
    let (mut done_a, rx_a) = tokio::sync::oneshot::channel();
    let (mut done_b, rx_b) = tokio::sync::oneshot::channel();
    a.admit.send(Some(rx_a)).unwrap();
    b.admit.send(Some(rx_b)).unwrap();
    // Persistence snapshots deliberately exclude the in-progress attempt.
    // Its single charge becomes visible once both finder receivers time out.
    assert_eq!(handle.snapshot().await[0].attempts, 0);
    assert_eq!(handle.stats().lookups.load(Ordering::Relaxed), 1);
    tokio::time::timeout(Duration::from_secs(30), async {
        done_a.closed().await;
        done_b.closed().await;
    })
    .await
    .unwrap();
    assert_eq!(handle.stats().lookups.load(Ordering::Relaxed), 0);
    assert_eq!(handle.snapshot().await[0].attempts, 1);
    assert!(done_a.send(vec![peer(1, 1)]).is_err());
    assert!(done_b.send(vec![peer(2, 2)]).is_err());
    stop.send(true).unwrap();
    let queued = task.await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].attempts, 1);
}

#[tokio::test(start_paused = true)]
async fn fetches_with_the_next_peer_and_records_the_funnel() {
    let mut h = Harness::start(config(), &[], vec![]);
    h.script(peer(1, 1), 100, Err(FailureKind::Refused));
    h.send(found(0, 1, vec![peer(1, 1), peer(2, 1)]));
    h.send(found(3, 1, vec![])); // a second source reports it too
    let got = h.out.recv().await.unwrap();
    assert_eq!(got.key, key(1));
    assert_eq!(got.source, src(0));
    assert_eq!(got.sources, 0b1001);
    assert_eq!((got.cohort, got.discovered_at), (COHORT, T0));
    assert_eq!(h.handle.stats().failure_count(FailureKind::Refused), 1);
    for stage in [Stage::Harvested, Stage::PeerFound, Stage::Fetched] {
        assert_eq!(h.funnel.get(COHORT, src(0), stage, None), 1, "{stage:?}");
    }
    assert!(h.drain_lookups().is_empty(), "had peers, no lookup needed");
    // A rediscovery right after the fetch isn't fetched again.
    h.send(found(0, 1, vec![peer(9, 9)]));
    sleep(Duration::from_secs(5)).await;
    assert_eq!(h.fake.started.lock().unwrap().len(), 2);
    assert_eq!(h.handle.stats().duplicates.load(Ordering::Relaxed), 2);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn looks_up_peers_with_backoff_then_gives_up() {
    let mut h = Harness::start(config(), &[], vec![]);
    let start = Instant::now();
    h.send(found(0, 7, vec![]));
    let mut asks = Vec::new();
    // Lookups at 0, 10+60 and 80+240 seconds; given up at 330.
    for _ in 0..3 {
        assert_eq!(h.lookups.recv().await, Some(key(7)));
        asks.push(start.elapsed().as_secs());
    }
    assert_eq!(asks, vec![0, 70, 320]);
    sleep(Duration::from_secs(11)).await;
    assert_eq!(
        h.funnel
            .get(COHORT, src(0), Stage::Lost, Some(Loss::NoPeers)),
        1
    );
    assert_eq!(h.handle.stats().lost_count(Loss::NoPeers), 1);
    assert!(h.stop().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn lookup_peers_resume_the_fetch_and_the_last_failure_is_the_loss() {
    let mut h = Harness::start(config(), &[], vec![]);
    h.script(peer(1, 1), 50, Err(FailureKind::TimedOut));
    h.script(peer(2, 1), 50, Err(FailureKind::Refused));
    h.send(found(0, 1, vec![]));
    h.send(found(0, 2, vec![]));
    assert_eq!(h.lookups.recv().await, Some(key(1)));
    assert_eq!(h.lookups.recv().await, Some(key(2)));
    // The DHT answers: key 1 has a good peer, key 2 only a bad one.
    h.send(found(0, 1, vec![peer(3, 1)]));
    h.send(found(0, 2, vec![peer(1, 1)]));
    assert_eq!(h.out.recv().await.unwrap().key, key(1));
    // Key 2 fails, looks again, gets another bad peer, and so on.
    for bad in [peer(2, 1)] {
        assert_eq!(h.lookups.recv().await, Some(key(2)));
        h.send(found(0, 2, vec![bad]));
    }
    assert_eq!(h.lookups.recv().await, Some(key(2)));
    sleep(Duration::from_secs(20)).await;
    // Three lookups used; the last attempt failed with Refused.
    assert_eq!(
        h.funnel
            .get(COHORT, src(0), Stage::Lost, Some(Loss::Refused)),
        1
    );
    assert_eq!(h.funnel.total(Stage::PeerFound, None), 2);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn sockets_are_shared_by_weight() {
    let cfg = PoolConfig {
        max_sockets: 1,
        ..config()
    };
    let h = Harness::start(cfg, &[(0, 1), (1, 3)], vec![]);
    for n in 0..40 {
        h.send(found(0, n, vec![peer(1, n as u16)]));
        h.send(found(1, 1000 + n, vec![peer(2, n as u16)]));
    }
    sleep(Duration::from_millis(20_500)).await;
    let started = h.fake.started.lock().unwrap().clone();
    let order: String = started
        .iter()
        .take(20)
        .map(|(k, _)| {
            if k.0[..4] < 1000u32.to_be_bytes()[..] {
                'L'
            } else {
                'H'
            }
        })
        .collect();
    assert_eq!(order, "LHHH".repeat(5));
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn interest_jumps_the_queue() {
    let cfg = PoolConfig {
        max_sockets: 1,
        ..config()
    };
    let h = Harness::start(cfg, &[], vec![]);
    for n in 0..10 {
        h.send(found(0, n, vec![peer(1, n as u16)]));
    }
    sleep(Duration::from_millis(500)).await;
    assert!(h.handle.interest(key(9)).await);
    assert!(!h.handle.interest(key(99)).await);
    let mut user = found(0, 50, vec![peer(5, 5)]);
    user.found.interest = true;
    h.send(user);
    sleep(Duration::from_millis(2_600)).await;
    let started: Vec<InfoHash> = h.fake.started.lock().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(&started[..4], &[key(0), key(9), key(50), key(1)]);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn caps_sockets_and_connections_per_ip() {
    let cfg = PoolConfig {
        max_sockets: 5,
        per_ip: 2,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    for n in 0..20 {
        h.send(found(0, n, vec![peer(1, 1), peer(1, 2), peer(2, 1)]));
    }
    for _ in 0..20 {
        h.out.recv().await.unwrap();
    }
    // Two IPs at two connections each: the IP cap binds before the socket cap.
    assert_eq!(h.fake.max_active.load(Ordering::SeqCst), 4);
    assert_eq!(h.fake.max_per_ip.load(Ordering::SeqCst), 2);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn private_keys_never_reach_peer_finders() {
    let mut h = Harness::start(config(), &[], vec![]);
    let mut private = found(5, 1, vec![]);
    private.found.private_source = true;
    h.send(private.clone());
    h.send(Discovery {
        found: Found {
            key: key(2),
            ..private.found.clone()
        },
        ..private.clone()
    });
    sleep(Duration::from_secs(15)).await;
    assert!(h.drain_lookups().is_empty());
    // A public sighting makes key 2 public: its next lookup goes out.
    h.send(found(0, 2, vec![]));
    sleep(Duration::from_secs(60)).await;
    assert_eq!(h.drain_lookups(), vec![key(2)]);
    // Private keys wait for their own source without fictitious attempts.
    sleep(Duration::from_secs(400)).await;
    assert!(h.drain_lookups().iter().all(|k| *k != key(1)));
    assert_eq!(
        h.funnel
            .get(COHORT, src(5), Stage::Lost, Some(Loss::NoPeers)),
        1
    );
    let remaining = h.stop().await;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].info_hash, key(1));
    assert_eq!(remaining[0].attempts, 0);
}

#[tokio::test(start_paused = true)]
async fn a_full_queue_evicts_the_oldest_idle_key() {
    let cfg = PoolConfig {
        max_pending: 3,
        ..config()
    };
    let h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 60_000, Ok(()));
    h.send(found(0, 1, vec![peer(1, 1)])); // has peers: kept
    h.send(found(0, 2, vec![]));
    h.send(found(0, 3, vec![]));
    h.send(found(0, 4, vec![]));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(
        h.funnel
            .get(COHORT, src(0), Stage::Lost, Some(Loss::Evicted)),
        1
    );
    let left: Vec<InfoHash> = h.stop().await.iter().map(|q| q.info_hash).collect();
    assert_eq!(left, vec![key(1), key(3), key(4)]);
}

#[tokio::test(start_paused = true)]
async fn oversize_is_terminal() {
    let h = Harness::start(config(), &[], vec![]);
    h.script(peer(1, 1), 10, Err(FailureKind::Oversize));
    h.send(found(0, 1, vec![peer(1, 1), peer(2, 2)]));
    sleep(Duration::from_secs(1)).await;
    assert_eq!(
        h.funnel
            .get(COHORT, src(0), Stage::Lost, Some(Loss::Oversize)),
        1
    );
    assert_eq!(h.fake.started.lock().unwrap().len(), 1);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn the_queue_survives_a_restart() {
    let mut h = Harness::start(config(), &[], vec![]);
    let mut wanted = found(2, 2, vec![]);
    wanted.found.interest = true;
    wanted.found.strategy = 4;
    h.send(found(1, 1, vec![]));
    h.send(wanted);
    sleep(Duration::from_secs(1)).await;
    assert_eq!(h.drain_lookups().len(), 2);
    let saved = h.stop().await;
    assert_eq!(saved.len(), 2);
    assert_eq!(
        (
            saved[0].info_hash,
            saved[0].priority,
            saved[0].strategy,
            saved[0].attempts
        ),
        (key(2), 1, 4, 0),
        "interest first; the lookup in progress isn't counted"
    );
    let mut h = Harness::start(config(), &[], saved);
    sleep(Duration::from_secs(1)).await;
    let mut asked = h.drain_lookups();
    asked.sort_by_key(|k| k.0);
    assert_eq!(asked, vec![key(1), key(2)]);
    assert_eq!(
        h.funnel.total(Stage::Harvested, None),
        0,
        "counted before the restart"
    );
    h.send(found(1, 1, vec![peer(1, 1)]));
    let got = h.out.recv().await.unwrap();
    assert_eq!((got.key, got.cohort), (key(1), COHORT));
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn late_peer_replies_are_ignored() {
    let mut h = Harness::start(config(), &[], vec![]);
    let mut reply = found(0, 9, vec![peer(1, 1)]);
    reply.reply = true;
    // A reply for a key the pool never held (or already finished).
    h.send(reply.clone());
    sleep(Duration::from_secs(2)).await;
    assert!(h.fake.started.lock().unwrap().is_empty());
    assert_eq!(h.funnel.total(Stage::Harvested, None), 0);
    // While the pool holds the key, the reply's peers are used.
    h.send(found(0, 9, vec![]));
    assert_eq!(h.lookups.recv().await, Some(key(9)));
    h.send(reply);
    assert_eq!(h.out.recv().await.unwrap().key, key(9));
    assert_eq!(h.funnel.total(Stage::Harvested, None), 1);
    h.stop().await;
}

/// The crawler's pause: nothing new starts, nothing queued is lost, and
/// resuming picks up where it stopped. A fetch already out finishes.
#[tokio::test(start_paused = true)]
async fn a_paused_pool_starts_nothing_and_loses_nothing() {
    let mut h = Harness::start(config(), &[], vec![]);
    h.script(peer(1, 1), 5_000, Ok(()));
    h.send(found(0, 1, vec![peer(1, 1)]));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.fake.started.lock().unwrap().len(), 1);
    h.handle.set_paused(true).await;
    // Pausing twice is the same as once.
    h.handle.set_paused(true).await;
    h.send(found(0, 2, vec![peer(2, 1)]));
    h.send(found(0, 3, vec![]));
    // Far past every lookup wait and back-off: still queued, not lost.
    sleep(Duration::from_secs(3_600)).await;
    assert_eq!(h.fake.started.lock().unwrap().len(), 1, "no new fetch");
    assert!(h.drain_lookups().is_empty(), "no new lookup");
    let first = h.out.recv().await.unwrap();
    assert_eq!(first.key, key(1), "the fetch in flight finished");
    let lost: u64 = h
        .handle
        .stats()
        .lost
        .iter()
        .map(|n| n.load(Ordering::Relaxed))
        .sum();
    assert_eq!(lost, 0);
    assert_eq!(h.handle.stats().pending.load(Ordering::Relaxed), 2);

    h.handle.set_paused(false).await;
    let second = h.out.recv().await.unwrap();
    assert_eq!(second.key, key(2));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.drain_lookups(), vec![key(3)], "the held lookup goes out");
    let queue = h.stop().await;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].attempts, 0, "waiting used no attempt");
}

#[tokio::test(start_paused = true)]
async fn a_pool_can_start_paused() {
    let restore = vec![QueuedKey {
        info_hash: key(9),
        source: src(0),
        cohort: COHORT,
        strategy: 0,
        priority: 0,
        attempts: 0,
        private_source: false,
    }];
    let mut h = Harness::start(
        PoolConfig {
            paused: true,
            ..config()
        },
        &[],
        restore,
    );
    sleep(Duration::from_secs(60)).await;
    assert!(h.drain_lookups().is_empty());
    h.handle.set_paused(false).await;
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.drain_lookups(), vec![key(9)]);
    h.stop().await;
}
