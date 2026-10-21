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
    PoolIo, Shutdown, Stage, StrategyCounts, evidence,
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
    }
}

/// A sighting from source 0 with the given evidence; the strategy code is
/// the evidence level, so per-strategy counts can be told apart.
fn sighting(n: u32, level: u8) -> Discovery {
    Discovery {
        source: src(0),
        found: Found::key(key(n), level).with_evidence(level),
        at: T0,
    }
}

/// A peer finder's answer to the pool's ask.
fn answer(n: u32, peers: Vec<SocketAddr>) -> Discovery {
    Discovery {
        source: src(0),
        found: Found::key(key(n), 9).with_peers(peers).answering_lookup(),
        at: T0,
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
        let (finder, lookups) = mpsc::channel(4096);
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
    // Key 1 stays private through all its attempts.
    sleep(Duration::from_secs(400)).await;
    assert!(h.drain_lookups().iter().all(|k| *k != key(1)));
    assert_eq!(
        h.funnel
            .get(COHORT, src(5), Stage::Lost, Some(Loss::NoPeers)),
        2
    );
    h.stop().await;
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
async fn an_empty_answer_ends_the_lookup_without_waiting() {
    let mut h = Harness::start(config(), &[], vec![]);
    let start = Instant::now();
    h.send(found(0, 7, vec![]));
    assert_eq!(h.lookups.recv().await, Some(key(7)));
    sleep(Duration::from_secs(1)).await;
    h.send(answer(7, vec![]));
    // The retry follows the back-off from the answer (1 + 60 s), not from
    // the end of the 10 s lookup wait.
    assert_eq!(h.lookups.recv().await, Some(key(7)));
    assert_eq!(start.elapsed().as_secs(), 61);
    // An answer for a key that's gone admits nothing and asks nothing.
    h.send(answer(99, vec![]));
    sleep(Duration::from_secs(1)).await;
    assert_eq!(h.funnel.total(Stage::Harvested, None), 1);
    assert!(h.drain_lookups().is_empty());
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn lookups_go_to_the_strongest_evidence_newest_first() {
    let cfg = PoolConfig {
        max_lookups: 1,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.send(sighting(1, evidence::NONE)); // takes the only slot
    h.send(sighting(2, evidence::NONE));
    h.send(sighting(3, evidence::NONE));
    h.send(sighting(4, evidence::WANTED));
    h.send(sighting(5, evidence::NONE));
    h.send(sighting(5, evidence::NONE)); // seen twice: one rank up
    assert_eq!(h.lookups.recv().await, Some(key(1)));
    let mut order = Vec::new();
    for n in [1, 4, 5, 3, 2] {
        h.send(answer(n, vec![]));
        let next = h.lookups.recv().await.unwrap();
        order.push(u32::from_be_bytes(next.0[..4].try_into().unwrap()));
        if n == 2 {
            break;
        }
    }
    // Wanted, then seen twice, then plain samples newest first; key 1's
    // retry (due 60 s later) comes last.
    assert_eq!(order, vec![4, 5, 3, 2, 1]);
    // The other retries came due with it and wait.
    assert_eq!(h.handle.stats().lookup_backlog.load(Ordering::Relaxed), 4);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn first_lookups_beat_retries() {
    let cfg = PoolConfig {
        max_lookups: 1,
        // Key 2 holds its slot until it's answered.
        lookup_wait: Duration::from_secs(600),
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.send(found(0, 1, vec![]));
    assert_eq!(h.lookups.recv().await, Some(key(1)));
    h.send(answer(1, vec![]));
    h.send(found(0, 2, vec![])); // holds the slot from here on
    assert_eq!(h.lookups.recv().await, Some(key(2)));
    // Key 1's retry comes due and queues; then a fresh key arrives.
    sleep(Duration::from_secs(61)).await;
    h.send(found(0, 3, vec![]));
    sleep(Duration::from_secs(1)).await;
    assert_eq!(h.handle.stats().lookup_backlog.load(Ordering::Relaxed), 2);
    h.send(answer(2, vec![]));
    assert_eq!(h.lookups.recv().await, Some(key(3)));
    h.send(answer(3, vec![]));
    assert_eq!(h.lookups.recv().await, Some(key(1)));
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_full_pool_evicts_the_lowest_rank_and_never_a_higher_one() {
    let cfg = PoolConfig {
        max_pending: 3,
        ..config()
    };
    let h = Harness::start(cfg, &[], vec![]);
    h.send(sighting(1, evidence::WANTED));
    h.send(sighting(2, evidence::NONE));
    h.send(sighting(3, evidence::NONE));
    h.send(sighting(4, evidence::WANTED)); // evicts 2, the oldest sample
    h.send(sighting(3, evidence::WANTED)); // 3 rises above the samples
    h.send(sighting(5, evidence::NONE)); // nothing ranks lower: dropped
    sleep(Duration::from_millis(10)).await;
    let stats = h.handle.stats();
    assert_eq!(stats.lost_count(Loss::Evicted), 2);
    assert_eq!(stats.lost_by_rank[0].load(Ordering::Relaxed), 2);
    let mut left: Vec<InfoHash> = h.stop().await.iter().map(|q| q.info_hash).collect();
    left.sort_by_key(|k| k.0);
    assert_eq!(left, vec![key(1), key(3), key(4)]);
}

#[tokio::test(start_paused = true)]
async fn counts_each_strategy_through_the_stages() {
    let mut h = Harness::start(config(), &[], vec![]);
    h.script(peer(1, 1), 10, Err(FailureKind::TimedOut));
    h.send(sighting(1, evidence::WANTED));
    h.send(sighting(2, evidence::NONE));
    sleep(Duration::from_millis(1)).await;
    assert_eq!(h.drain_lookups().len(), 2);
    h.send(answer(1, vec![peer(1, 1), peer(2, 1)]));
    h.send(answer(2, vec![]));
    assert_eq!(h.out.recv().await.unwrap().key, key(1));
    let stats = h.handle.stats();
    let by: HashMap<u8, StrategyCounts> = stats
        .strategies()
        .into_iter()
        .map(|((s, code), c)| {
            assert_eq!(s, src(0));
            (code, c)
        })
        .collect();
    assert_eq!(
        by[&evidence::WANTED],
        StrategyCounts {
            harvested: 1,
            lookups: 1,
            peer_found: 1,
            attempts: 2,
            fetched: 1,
            lost: 0,
        }
    );
    assert_eq!(
        by[&evidence::NONE],
        StrategyCounts {
            harvested: 1,
            lookups: 1,
            ..StrategyCounts::default()
        }
    );
    assert!(
        !by.contains_key(&9),
        "answers are attributed to the key, not the finder"
    );
    assert_eq!(
        stats.fetched_by_rank[usize::from(evidence::WANTED)].load(Ordering::Relaxed),
        1
    );
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn churn_keeps_every_key_accounted_for() {
    let cfg = PoolConfig {
        max_pending: 40,
        max_lookups: 4,
        max_sockets: 3,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    for ip in 0..8 {
        let result = match ip % 4 {
            0 => Ok(()),
            1 => Err(FailureKind::TimedOut),
            2 => Err(FailureKind::Refused),
            _ => Err(FailureKind::Unreachable),
        };
        h.script(peer(ip, 1), 300 + u64::from(ip) * 100, result);
    }
    // A fixed pseudo-random mix of sightings, answers (empty or not) and
    // interest, over a key space small enough to collide often.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..3000 {
        let r = next();
        let n = (r % 120) as u32;
        let d = match (r >> 8) % 6 {
            0 | 1 => sighting(n, ((r >> 16) % 4) as u8),
            2 => answer(n, vec![]),
            3 => answer(n, vec![peer(((r >> 16) % 8) as u8, 1)]),
            4 => found(0, n, vec![]),
            _ => {
                let mut d = found(0, n, vec![]);
                d.found.interest = true;
                d
            }
        };
        h.send(d);
        sleep(Duration::from_millis((r >> 24) % 400)).await;
        h.drain_lookups();
    }
    let stats = h.handle.stats();
    let fetched = stats.fetched.load(Ordering::Relaxed);
    while h.out.try_recv().is_ok() {}
    let lost: u64 = stats.lost.iter().map(|a| a.load(Ordering::Relaxed)).sum();
    let by_rank: u64 = stats
        .fetched_by_rank
        .iter()
        .chain(&stats.lost_by_rank)
        .map(|a| a.load(Ordering::Relaxed))
        .sum();
    let harvested = h.funnel.total(Stage::Harvested, None) as u64;
    let per_strategy: StrategyCounts =
        stats
            .strategies()
            .into_iter()
            .fold(StrategyCounts::default(), |a, (_, c)| StrategyCounts {
                harvested: a.harvested + c.harvested,
                fetched: a.fetched + c.fetched,
                lost: a.lost + c.lost,
                ..a
            });
    let left = h.stop().await.len() as u64;
    assert!(fetched > 0 && lost > 0, "the mix exercises both outcomes");
    assert_eq!(
        harvested,
        fetched + lost + left,
        "each key leaves exactly once"
    );
    assert_eq!(by_rank, fetched + lost);
    assert_eq!(
        (
            per_strategy.harvested,
            per_strategy.fetched,
            per_strategy.lost
        ),
        (harvested, fetched, lost)
    );
}
