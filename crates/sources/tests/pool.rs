//! The shared fetch pool against a scripted fetcher, in paused time.
//!
//! Every test here is `start_paused`, so each `sleep` advances the virtual clock the pool
//! reads: it simulates time passing for the code under test and waits for nothing.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::pool::{MAX_VOUCHED_PER_IP, Reservation, SHUTDOWN_DRAIN, spawn};
use lodestar_sources::{
    BoxFuture, Discovery, FetchedKey, Fetcher, Found, Funnel, FunnelCounts, Loss, PeerIpCap,
    PoolConfig, PoolHandle, PoolIo, Shutdown, Stage,
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
    /// The length of every info dict served; 0 serves the key's 20 bytes.
    info_len: AtomicUsize,
    /// Reserve the dict's bytes 100 ms in, as peer-wire does at the
    /// extended handshake, before the rest of the script's delay.
    reserving: AtomicBool,
    /// With `reserving`: how long a fetch may wait for its reservation
    /// before it times out, as peer-wire's deadline would end it. Zero
    /// waits for ever.
    reserve_deadline_ms: AtomicU64,
    /// Bytes reserved by fetches downloading now, and the most at once.
    reserved: AtomicU64,
    max_reserved: AtomicU64,
    /// With `reserving`: peers that announce this many bytes, reserve as
    /// peer-wire does (the first 64 KiB, the rest after it arrives), then
    /// send nothing until peer-wire's 8 s deadline ends them.
    silent: Mutex<HashMap<SocketAddr, usize>>,
}

/// Peer-wire's first request window (`FIRST_WINDOW`), and its deadline.
const WINDOW: usize = 64 << 10;
const DEADLINE: Duration = Duration::from_secs(8);

struct Fake(Arc<FakeState>);

impl Fetcher for Fake {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        Box::pin(serve(self.0.clone(), peer, key, None))
    }

    fn fetch_reserving(
        &self,
        peer: SocketAddr,
        key: InfoHash,
        reservation: Reservation,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let reserving = self.0.reserving.load(Ordering::SeqCst);
        Box::pin(serve(
            self.0.clone(),
            peer,
            key,
            reserving.then_some(reservation),
        ))
    }
}

async fn serve(
    s: Arc<FakeState>,
    peer: SocketAddr,
    key: InfoHash,
    reservation: Option<Reservation>,
) -> Result<Fetched, FailureKind> {
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
    let len = s.info_len.load(Ordering::SeqCst).max(20);
    let mut delay = script.delay;
    let mut reserved = 0;
    if let Some(r) = &reservation {
        let handshake = Duration::from_millis(100);
        sleep(handshake).await;
        delay = delay.saturating_sub(handshake);
        let silent = s.silent.lock().unwrap().get(&peer).copied();
        if let Some(size) = silent {
            // Waiting for room counts against the deadline too.
            let _ = tokio::time::timeout(DEADLINE - handshake, async {
                r.reserve(size.min(WINDOW), size).await;
                s.reserved.fetch_add(r.bytes(), Ordering::SeqCst);
                std::future::pending::<()>().await
            })
            .await;
            if r.bytes() > 0 {
                s.reserved.fetch_sub(r.bytes(), Ordering::SeqCst);
            }
            s.active.fetch_sub(1, Ordering::SeqCst);
            *s.per_ip.lock().unwrap().get_mut(&peer.ip()).unwrap() -= 1;
            return Err(FailureKind::TimedOut);
        }
        let deadline = s.reserve_deadline_ms.load(Ordering::SeqCst);
        let done = if deadline == 0 {
            r.reserve(len, len).await;
            true
        } else {
            tokio::time::timeout(Duration::from_millis(deadline), r.reserve(len, len))
                .await
                .is_ok()
        };
        if !done {
            s.active.fetch_sub(1, Ordering::SeqCst);
            *s.per_ip.lock().unwrap().get_mut(&peer.ip()).unwrap() -= 1;
            return Err(FailureKind::TimedOut);
        }
        reserved = r.bytes();
        let now = s.reserved.fetch_add(reserved, Ordering::SeqCst) + reserved;
        s.max_reserved.fetch_max(now, Ordering::SeqCst);
    }
    sleep(delay).await;
    s.reserved.fetch_sub(reserved, Ordering::SeqCst);
    s.active.fetch_sub(1, Ordering::SeqCst);
    *s.per_ip.lock().unwrap().get_mut(&peer.ip()).unwrap() -= 1;
    let mut info = vec![0u8; len];
    info[..20].copy_from_slice(&key.0);
    script.result.map(|()| Fetched {
        info,
        v2: false,
        encrypted: true,
        pex: Vec::new(),
        client: None,
    })
}

/// The funnel's counts, plus each loss's cause as the pool reported it.
struct Causes {
    counts: Arc<FunnelCounts>,
    lost: Mutex<Vec<(InfoHash, Loss, Option<FailureKind>)>>,
}

impl Funnel for Causes {
    fn record(&self, cohort: i64, source: SourceId, stage: Stage, loss: Option<Loss>) {
        self.counts.record(cohort, source, stage, loss);
    }

    fn lost(
        &self,
        key: &InfoHash,
        cohort: i64,
        source: SourceId,
        loss: Loss,
        cause: Option<FailureKind>,
    ) {
        self.lost.lock().unwrap().push((*key, loss, cause));
        self.record(cohort, source, Stage::Lost, Some(loss));
    }
}

struct Harness {
    disc: mpsc::Sender<Discovery>,
    lookups: mpsc::Receiver<InfoHash>,
    out: mpsc::Receiver<FetchedKey>,
    funnel: Arc<FunnelCounts>,
    causes: Arc<Causes>,
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
        Harness::with_output(config, weights, restore, fake, 4096)
    }

    /// With an output channel of `capacity` slots.
    fn with_output(
        config: PoolConfig,
        weights: &[(u8, u32)],
        restore: Vec<QueuedKey>,
        fake: Arc<FakeState>,
        capacity: usize,
    ) -> Harness {
        Harness::with_parts(config, weights, restore, fake, capacity, true)
    }

    /// A pool with no peer finder, as a worker that runs no DHT has.
    fn without_finders(config: PoolConfig) -> Harness {
        let fake = Arc::new(FakeState::default());
        Harness::with_parts(config, &[], vec![], fake, 4096, false)
    }

    fn with_parts(
        config: PoolConfig,
        weights: &[(u8, u32)],
        restore: Vec<QueuedKey>,
        fake: Arc<FakeState>,
        capacity: usize,
        finders: bool,
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
        let (out_tx, out) = mpsc::channel(capacity);
        let (stop, shutdown) = Shutdown::new();
        let funnel = Arc::new(FunnelCounts::default());
        let causes = Arc::new(Causes {
            counts: funnel.clone(),
            lost: Mutex::default(),
        });
        let (handle, task) = spawn(
            config,
            Arc::new(Fake(fake.clone())),
            PoolIo {
                discoveries,
                peer_finders: if finders { vec![finder] } else { Vec::new() },
                out: out_tx,
                funnel: causes.clone(),
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
            causes,
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
async fn finder_channel_failures_do_not_consume_attempts() {
    for closed in [false, true] {
        let (disc, discoveries) = mpsc::channel(4);
        let (finder, requests) = mpsc::channel::<lodestar_sources::PeerRequest>(1);
        // Either leave a full channel with its receiver alive, or close it.
        let (admit, _admission) = tokio::sync::oneshot::channel();
        finder
            .try_send(lodestar_sources::PeerRequest {
                key: key(99),
                admit,
                session: None,
                permit: None,
            })
            .unwrap();
        let _requests = if closed {
            drop(requests);
            None
        } else {
            Some(requests)
        };
        let (out, _results) = mpsc::channel(4);
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
        disc.send(found(0, 1, vec![])).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while handle.stats().lookup_unaccepted.load(Ordering::Relaxed) == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            handle.stats().finder_closed.load(Ordering::Relaxed),
            u64::from(closed)
        );
        assert_eq!(
            handle.stats().finder_full.load(Ordering::Relaxed),
            u64::from(!closed)
        );
        assert_eq!(handle.stats().lookup_unaccepted.load(Ordering::Relaxed), 1);
        assert_eq!(handle.stats().lookups.load(Ordering::Relaxed), 0);
        assert_eq!(handle.snapshot().await[0].attempts, 0);
        stop.send(true).unwrap();
        assert_eq!(task.await.unwrap()[0].attempts, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn eviction_diagnostics_distinguish_unserved_and_fetch_work() {
    for fetched in [false, true] {
        let h = Harness::start(
            PoolConfig {
                max_pending: 1,
                paused: !fetched,
                ..config()
            },
            &[],
            vec![],
        );
        h.script(peer(1, 1), 1, Err(FailureKind::Refused));
        if fetched {
            h.send(found(0, 1, vec![peer(1, 1)]));
            tokio::time::timeout(Duration::from_secs(10), async {
                while h
                    .handle
                    .stats()
                    .failures
                    .iter()
                    .all(|n| n.load(Ordering::Relaxed) == 0)
                {
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        } else {
            h.send(found(0, 1, vec![]));
        }
        h.send(found(0, 2, vec![]));
        tokio::time::timeout(Duration::from_secs(10), async {
            while h.handle.stats().lost[Loss::Evicted as usize].load(Ordering::Relaxed) == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let stages: Vec<_> = h
            .handle
            .stats()
            .evicted_service
            .iter()
            .map(|n| n.load(Ordering::Relaxed))
            .collect();
        assert_eq!(
            stages,
            if fetched {
                vec![0, 0, 0, 1]
            } else {
                vec![1, 0, 0, 0]
            }
        );
        assert_eq!(h.stop().await[0].info_hash, key(2));
    }
}

#[tokio::test(start_paused = true)]
async fn rejected_rediscovery_is_distinct_from_a_retained_entry_retry() {
    for rediscover in [true, false] {
        let (disc, discoveries) = mpsc::channel(16);
        let (finder, mut requests) = mpsc::channel::<lodestar_sources::PeerRequest>(1);
        let (out, _results) = mpsc::channel(16);
        let (stop, shutdown) = Shutdown::new();
        let (handle, task) = spawn(
            PoolConfig {
                max_pending: 2,
                ..config()
            },
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
        let arrivals: &[u32] = if rediscover { &[1, 2, 3, 1] } else { &[1, 1] };
        for (index, &id) in arrivals.iter().enumerate() {
            if rediscover || index == 0 {
                disc.send(found(0, id, vec![])).await.unwrap();
            }
            // In the retained case paused time advances to the existing retry
            // timer. No second discovery is sent, so this is an actual retry.
            let request = tokio::time::timeout(Duration::from_secs(120), requests.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(request.key, key(id));
            request.admit.send(None).unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                while handle.stats().lookup_unaccepted.load(Ordering::Relaxed) != (index + 1) as u64
                {
                    sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert!(handle.snapshot().await.iter().all(|row| row.attempts == 0));
        }
        assert_eq!(
            handle.stats().retry_unaccepted.load(Ordering::Relaxed),
            u64::from(!rediscover)
        );
        let evictions = if rediscover { 2 } else { 0 };
        assert_eq!(handle.stats().lost_count(Loss::Evicted), evictions);
        assert_eq!(
            handle.stats().evicted_service[0].load(Ordering::Relaxed),
            evictions
        );
        assert_eq!(handle.stats().lookups.load(Ordering::Relaxed), 0);
        assert_eq!(handle.stats().active.load(Ordering::Relaxed), 0);
        stop.send(true).unwrap();
        let saved = task.await.unwrap();
        let expected = if rediscover {
            vec![key(1), key(3)]
        } else {
            vec![key(1)]
        };
        assert_eq!(
            saved.iter().map(|row| row.info_hash).collect::<Vec<_>>(),
            expected
        );
        assert!(requests.recv().await.is_none(), "finder ownership drains");
    }
}

#[tokio::test(start_paused = true)]
async fn churn_evicts_an_accepted_lookup_and_cancels_its_ownership() {
    let (disc, discoveries) = mpsc::channel(16);
    let (finder, mut requests) = mpsc::channel::<lodestar_sources::PeerRequest>(1);
    let (out, _results) = mpsc::channel(16);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        PoolConfig {
            max_pending: 2,
            ..config()
        },
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
    disc.send(found(0, 1, vec![])).await.unwrap();
    let request = requests.recv().await.unwrap();
    assert_eq!(request.key, key(1));
    let (mut done, rx) = tokio::sync::oneshot::channel();
    request.admit.send(Some(rx)).unwrap();
    // A repeat sighting polls A's admission before mutating the entry. Waiting
    // for its duplicate counter is an explicit acknowledgment-processing fence.
    disc.send(found(0, 1, vec![])).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while handle.stats().duplicates.load(Ordering::Relaxed) != 1 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    for (index, id) in [2, 3].into_iter().enumerate() {
        disc.send(found(0, id, vec![])).await.unwrap();
        let request = requests.recv().await.unwrap();
        assert_eq!(request.key, key(id));
        request.admit.send(None).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while handle.stats().lookup_unaccepted.load(Ordering::Relaxed) != (index + 1) as u64 {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(10), done.closed())
        .await
        .unwrap();
    assert!(done.send(vec![peer(1, 1)]).is_err());
    assert_eq!(handle.stats().lost_count(Loss::Evicted), 1);
    assert_eq!(handle.stats().evicted_service[1].load(Ordering::Relaxed), 1);
    assert_eq!(handle.stats().evicted_service[0].load(Ordering::Relaxed), 0);
    assert_eq!(handle.stats().retry_unaccepted.load(Ordering::Relaxed), 0);
    assert_eq!(handle.stats().lookups.load(Ordering::Relaxed), 0);
    assert_eq!(handle.stats().active.load(Ordering::Relaxed), 0);
    stop.send(true).unwrap();
    let saved = task.await.unwrap();
    assert_eq!(saved.len(), 2);
    assert!(saved.iter().all(|row| row.attempts == 0));
    assert!(requests.recv().await.is_none());
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
    assert_eq!(handle.stats().lookup_unaccepted.load(Ordering::Relaxed), 1);
    let request = requests.recv().await.unwrap();
    assert_eq!(handle.stats().retry_unaccepted.load(Ordering::Relaxed), 1);
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

/// A per-peer-IP policy (WK-10e): each listed IP's cap, 2 for the rest.
struct ByIp(Mutex<HashMap<IpAddr, usize>>);

impl ByIp {
    fn new(caps: &[(u8, usize)]) -> Arc<ByIp> {
        Arc::new(ByIp(Mutex::new(
            caps.iter().map(|&(ip, n)| (peer(ip, 0).ip(), n)).collect(),
        )))
    }
}

impl PeerIpCap for ByIp {
    fn connections_to(&self, ip: IpAddr) -> usize {
        self.0.lock().unwrap().get(&ip).copied().unwrap_or(2)
    }
}

/// Sets the pool's policy and waits until the pool has taken it (its
/// commands are handled in order).
async fn set_cap(h: &Harness, cap: Option<Arc<dyn PeerIpCap>>) {
    h.handle.set_ip_cap(cap).await;
    h.handle.queued().await;
}

/// WK-10e: a peer IP the policy gives 0 (another same-IP member's bucket)
/// is never dialled; one it gives 1 gets at most one connection; a key
/// whose only peers are in another bucket asks for more peers instead.
#[tokio::test(start_paused = true)]
async fn a_peer_ip_outside_our_share_is_never_dialled() {
    let mut h = Harness::start(config(), &[], vec![]);
    set_cap(&h, Some(ByIp::new(&[(1, 0), (2, 1)]))).await;
    for n in 0..6 {
        h.send(found(0, n, vec![peer(1, 1), peer(1, 2), peer(2, 1)]));
    }
    h.send(found(0, 99, vec![peer(1, 3)]));
    for _ in 0..6 {
        h.out.recv().await.unwrap();
    }
    // At once, not after a back-off: its peer was never queued.
    until_within("a lookup for the key with no peer of ours", 10, || {
        h.drain_lookups().contains(&key(99))
    })
    .await;
    let started = h.fake.started.lock().unwrap().clone();
    assert!(
        started.iter().all(|(_, p)| p.ip() == peer(2, 0).ip()),
        "{started:?}"
    );
    assert_eq!(h.fake.max_per_ip.load(Ordering::SeqCst), 1);
    h.stop().await;
}

/// WK-10e: a single install's policy (2 for every IP) is today's `per_ip`.
#[tokio::test(start_paused = true)]
async fn a_single_installs_policy_dials_as_today() {
    let cfg = PoolConfig {
        max_sockets: 5,
        per_ip: 2,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    set_cap(&h, Some(ByIp::new(&[]))).await;
    for n in 0..20 {
        h.send(found(0, n, vec![peer(1, 1), peer(1, 2), peer(2, 1)]));
    }
    for _ in 0..20 {
        h.out.recv().await.unwrap();
    }
    assert_eq!(h.fake.max_active.load(Ordering::SeqCst), 4);
    assert_eq!(h.fake.max_per_ip.load(Ordering::SeqCst), 2);
    h.stop().await;
}

/// WK-10e: a policy change applies to the next dial. Fetches already out
/// to an IP that's no longer ours finish; peers on it that were already
/// queued are skipped from then on, and a key left with none asks again.
#[tokio::test(start_paused = true)]
async fn a_lowered_cap_stops_new_dials_and_lets_open_ones_finish() {
    let cfg = PoolConfig {
        max_sockets: 8,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 5_000, Ok(()));
    h.script(peer(1, 2), 5_000, Ok(()));
    // Two fetches out to IP 1, which then fills its cap of 2.
    h.send(found(0, 1, vec![peer(1, 1)]));
    h.send(found(0, 2, vec![peer(1, 2)]));
    until("two fetches out", || {
        h.fake.active.load(Ordering::SeqCst) == 2
    })
    .await;
    // Queued behind them: keys whose peers are on IP 1, one with IP 2 too.
    h.send(found(0, 3, vec![peer(1, 3), peer(2, 3)]));
    h.send(found(0, 4, vec![peer(1, 4)]));
    until("key 3 dialled on IP 2", || {
        h.fake
            .started
            .lock()
            .unwrap()
            .iter()
            .any(|(k, _)| *k == key(3))
    })
    .await;
    set_cap(&h, Some(ByIp::new(&[(1, 0)]))).await;
    // The open fetches finish and are sent on.
    let mut done = Vec::new();
    for _ in 0..3 {
        done.push(h.out.recv().await.unwrap().key);
    }
    done.sort_by_key(|k| k.0);
    assert_eq!(done, vec![key(1), key(2), key(3)]);
    // Key 4's queued peer on IP 1 is skipped: it asks for peers instead.
    until("a lookup for key 4", || h.drain_lookups().contains(&key(4))).await;
    let started = h.fake.started.lock().unwrap().clone();
    let on_ip1: Vec<InfoHash> = started
        .iter()
        .filter(|(_, p)| p.ip() == peer(1, 0).ip())
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(on_ip1, vec![key(1), key(2)], "{started:?}");
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

/// One address has peers in a bounded number of keys at once. Past that, a
/// key goes without its peer there, however often it is found with it, and
/// is looked up like any other, so it gives way to newer keys when the
/// queue is full.
#[tokio::test(start_paused = true)]
async fn one_address_has_peers_in_a_bounded_number_of_keys() {
    let max = MAX_VOUCHED_PER_IP as u32;
    let extra = 40u32;
    let cfg = PoolConfig {
        max_pending: (max + extra) as usize,
        max_lookups: 4096,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    // The address never finishes a fetch while the test runs.
    for port in 1..=4u16 {
        h.script(peer(1, port), 3_600_000, Ok(()));
    }
    let at = |n: u32| vec![peer(1, 1 + (n % 4) as u16)];
    for n in 1..=max {
        h.send(found(0, n, at(n)));
    }
    // Past the limit: found with the peer twice, or first with none and
    // then with it.
    for n in max + 1..=max + extra {
        h.send(found(0, n, if n % 2 == 0 { vec![] } else { at(n) }));
        h.send(found(0, n, at(n)));
    }
    sleep(Duration::from_millis(10)).await;
    // The keys past the limit, and only they, went to a lookup.
    let mut looked = h.drain_lookups();
    looked.sort();
    looked.dedup();
    let want: Vec<InfoHash> = (max + 1..=max + extra).map(key).collect();
    assert_eq!(looked, want);
    // The queue is full. Nearly all new keys still get in: the ones looked
    // up make room, and none of the address's first keys is touched.
    let new = extra;
    for n in 0..new {
        h.send(found(0, 900_000 + n, vec![]));
    }
    sleep(Duration::from_millis(10)).await;
    // No fetch was started for a key past the limit.
    let past: Vec<InfoHash> = h
        .fake
        .started
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.0)
        .filter(|k| want.contains(k))
        .collect();
    assert!(past.is_empty(), "{past:?}");
    let left: Vec<InfoHash> = h.stop().await.iter().map(|q| q.info_hash).collect();
    for n in 1..=max {
        assert!(left.contains(&key(n)), "key {n} kept");
    }
    let admitted = (0..new)
        .filter(|n| left.contains(&key(900_000 + n)))
        .count();
    assert!(
        admitted >= new as usize - 5,
        "{admitted} of {new} new keys admitted"
    );
}

/// A fetched key gives its place back, and a key counts at each address
/// its peers are at.
#[tokio::test(start_paused = true)]
async fn a_finished_key_gives_its_place_back() {
    let max = MAX_VOUCHED_PER_IP as u32;
    let cfg = PoolConfig {
        max_lookups: 4096,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 1, Ok(()));
    for ip in 2..=4 {
        h.script(peer(ip, 1), 3_600_000, Ok(()));
    }
    for round in 0..3u32 {
        for n in 1..=max {
            h.send(found(0, round * 10_000 + n, vec![peer(1, 1)]));
        }
        // Every one is fetched before the next round arrives.
        let mut fetched = 0;
        while fetched < max {
            tokio::time::timeout(Duration::from_secs(600), h.out.recv())
                .await
                .expect("fetched in time")
                .expect("a fetched key");
            fetched += 1;
        }
    }
    assert!(h.drain_lookups().is_empty(), "none was past the limit");
    // Peers at two addresses take a place at each.
    for n in 1..=max {
        h.send(found(0, 500_000 + n, vec![peer(2, 1), peer(3, 1)]));
    }
    // One more keeps the peer it has at an address with room, and needs
    // no lookup; one with a peer at a full address alone is looked up.
    h.send(found(0, 600_001, vec![peer(2, 1), peer(4, 1)]));
    h.send(found(0, 600_002, vec![peer(3, 1)]));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.drain_lookups(), vec![key(600_002)]);
    h.stop().await;
}

/// A key that is lost gives its place back too.
#[tokio::test(start_paused = true)]
async fn a_lost_key_gives_its_place_back() {
    let max = MAX_VOUCHED_PER_IP as u32;
    let cfg = PoolConfig {
        max_lookups: 4096,
        max_attempts: 1,
        ..config()
    };
    let h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 1, Err(FailureKind::Refused));
    for round in 0..2u32 {
        for n in 1..=max {
            h.send(found(0, round * 10_000 + n, vec![peer(1, 1)]));
        }
        // Every one is lost before the next round arrives.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        while h.causes.lost.lock().unwrap().len() < ((round + 1) * max) as usize {
            assert!(tokio::time::Instant::now() < deadline, "round {round}");
            sleep(Duration::from_millis(5)).await;
        }
        // Each had its peer: none went without it to a lookup first.
        let started = h.fake.started.lock().unwrap().len();
        assert_eq!(started, ((round + 1) * max) as usize);
    }
    h.stop().await;
}

/// A private source's peers are the user's own: never counted. Once a
/// public source finds the key too, the peers it has count like any other.
#[tokio::test(start_paused = true)]
async fn a_private_keys_peers_count_once_it_is_public() {
    let max = MAX_VOUCHED_PER_IP as u32;
    let cfg = PoolConfig {
        max_lookups: 4096,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 3_600_000, Ok(()));
    for n in 1..=max + 10 {
        let mut d = found(0, n, vec![peer(1, 1)]);
        d.found.private_source = true;
        h.send(d);
    }
    // They took none of the address's places: a public key still has one.
    h.send(found(0, 700_001, vec![peer(1, 1)]));
    sleep(Duration::from_millis(10)).await;
    assert!(h.drain_lookups().is_empty());
    // A public source finds them too. They keep their peers, which count
    // from here: the address is full for the next public key.
    for n in 1..=max + 10 {
        h.send(found(0, n, vec![]));
    }
    h.send(found(0, 700_002, vec![peer(1, 1)]));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.drain_lookups(), vec![key(700_002)]);
    h.stop().await;
}

/// A key that turns private gives its places back, and takes them again
/// when a public source next finds it.
#[tokio::test(start_paused = true)]
async fn a_key_that_turns_private_gives_its_places_back() {
    let max = MAX_VOUCHED_PER_IP as u32;
    let cfg = PoolConfig {
        max_lookups: 4096,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 3_600_000, Ok(()));
    // Found by a public source with the peer, and by a private one.
    for n in 1..=max {
        h.send(found(1, n, vec![peer(1, 1)]));
        let mut private = found(2, n, vec![]);
        private.found.private_source = true;
        h.send(private);
    }
    sleep(Duration::from_millis(10)).await;
    assert!(h.drain_lookups().is_empty());
    // The public source goes: the keys are private, and the address has
    // room for a public key again.
    assert_eq!(h.handle.drop_source(src(1)).await, 0);
    h.send(found(3, 800_001, vec![peer(1, 1)]));
    sleep(Duration::from_millis(10)).await;
    assert!(h.drain_lookups().is_empty());
    // A public sighting of each: they count again, and the address is
    // full for the next key.
    for n in 1..=max {
        h.send(found(3, n, vec![]));
    }
    h.send(found(3, 800_002, vec![peer(1, 1)]));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.drain_lookups(), vec![key(800_002)]);
    h.stop().await;
}

/// A key whose peers the policy no longer lets this install dial gives
/// their places back with them.
#[tokio::test(start_paused = true)]
async fn dropped_peers_give_their_places_back() {
    let max = MAX_VOUCHED_PER_IP as u32;
    let cfg = PoolConfig {
        max_lookups: 4096,
        ..config()
    };
    let mut h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 5_000, Ok(()));
    h.script(peer(1, 2), 3_600_000, Ok(()));
    for n in 1..=max {
        h.send(found(0, n, vec![peer(1, 1)]));
    }
    until("two fetches out", || {
        h.fake.active.load(Ordering::SeqCst) == 2
    })
    .await;
    // The address is no longer ours to dial: once the two open fetches
    // finish, every key still waiting drops its peer and asks for others.
    set_cap(&h, Some(ByIp::new(&[(1, 0)]))).await;
    let mut asked = 0;
    // (After the retry back-off, which is why this waits longer.)
    until_within("the waiting keys ask for peers", 600, || {
        asked += h.drain_lookups().len();
        asked >= max as usize - 2
    })
    .await;
    // Ours again: new keys keep their peers there, more of them than the
    // two finished fetches alone made room for.
    set_cap(&h, None).await;
    let new: Vec<InfoHash> = (0..10).map(|n| key(800_010 + n)).collect();
    for n in 0..10 {
        h.send(found(0, 800_010 + n, vec![peer(1, 2)]));
    }
    sleep(Duration::from_millis(10)).await;
    let looked = h.drain_lookups();
    assert!(new.iter().all(|k| !looked.contains(k)), "{looked:?}");
    h.stop().await;
}

/// A pool with no peer finder can't look a key up, so it keeps the peers
/// its keys came with, however many are at one address.
#[tokio::test(start_paused = true)]
async fn a_pool_with_no_finder_keeps_every_peer() {
    let total = MAX_VOUCHED_PER_IP as u32 + 20;
    let mut h = Harness::without_finders(config());
    h.script(peer(1, 1), 1, Ok(()));
    for n in 1..=total {
        h.send(found(0, n, vec![peer(1, 1)]));
    }
    for n in 1..=total {
        tokio::time::timeout(Duration::from_secs(600), h.out.recv())
            .await
            .unwrap_or_else(|_| panic!("key {n} of {total} fetched in time"))
            .expect("a fetched key");
    }
    h.stop().await;
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

/// A hash mismatch and a protocol violation are both the funnel's
/// `invalid` loss; only the cause passed with it tells them apart.
#[tokio::test(start_paused = true)]
async fn an_invalid_loss_carries_its_cause() {
    let cfg = PoolConfig {
        max_attempts: 1,
        ..config()
    };
    let h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 10, Err(FailureKind::HashMismatch));
    h.script(peer(2, 1), 10, Err(FailureKind::Invalid));
    h.send(found(0, 1, vec![peer(1, 1)]));
    h.send(found(0, 2, vec![peer(2, 1)]));
    // Each fails, backs off, looks again (no peers) and gives up.
    sleep(Duration::from_secs(600)).await;
    let mut lost = h.causes.lost.lock().unwrap().clone();
    lost.sort_by_key(|l| l.0);
    assert_eq!(
        lost,
        vec![
            (key(1), Loss::Invalid, Some(FailureKind::HashMismatch)),
            (key(2), Loss::Invalid, Some(FailureKind::Invalid)),
        ]
    );
    assert_eq!(
        h.funnel
            .get(COHORT, src(0), Stage::Lost, Some(Loss::Invalid)),
        2
    );
    let stats = h.handle.stats();
    assert_eq!(stats.failure_count(FailureKind::HashMismatch), 1);
    assert_eq!(stats.failure_count(FailureKind::Invalid), 1);
    h.stop().await;
}

/// A key evicted after a hash mismatch was lost to the full queue, not to
/// bad data: its loss carries no cause, so the gate can't count it.
#[tokio::test(start_paused = true)]
async fn an_evicted_key_carries_no_cause() {
    let cfg = PoolConfig {
        max_pending: 1,
        ..config()
    };
    let h = Harness::start(cfg, &[], vec![]);
    h.script(peer(1, 1), 10, Err(FailureKind::HashMismatch));
    h.send(found(0, 1, vec![peer(1, 1)]));
    // It fails and backs off with attempts left, so it's evictable.
    sleep(Duration::from_secs(1)).await;
    assert_eq!(h.handle.stats().failure_count(FailureKind::HashMismatch), 1);
    h.send(found(0, 2, vec![]));
    sleep(Duration::from_secs(1)).await;
    assert_eq!(
        h.causes.lost.lock().unwrap().clone(),
        vec![(key(1), Loss::Evicted, None)]
    );
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

#[tokio::test(start_paused = true)]
async fn a_stale_timer_cannot_expire_a_rediscovered_keys_lookup() {
    // A's first lifetime leaves its lookup timer behind when B evicts it.
    // A found again is a new entry; the old timer, due first, must not
    // expire the new lifetime's lookup.
    let (disc, discoveries) = mpsc::channel(16);
    let (finder, mut requests) = mpsc::channel::<lodestar_sources::PeerRequest>(4);
    let (out, _results) = mpsc::channel(16);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        PoolConfig {
            max_pending: 1,
            ..config()
        },
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
    let t0 = Instant::now();
    let mut accept = async |n: u32| {
        let request = requests.recv().await.unwrap();
        assert_eq!(request.key, key(n));
        let (done, rx) = tokio::sync::oneshot::channel::<Vec<SocketAddr>>();
        request.admit.send(Some(rx)).unwrap();
        done
    };
    disc.send(found(0, 1, vec![])).await.unwrap();
    let mut first = accept(1).await;
    disc.send(found(0, 2, vec![])).await.unwrap();
    let mut second = accept(2).await;
    tokio::time::timeout(Duration::from_secs(10), first.closed())
        .await
        .expect("B evicts A");
    sleep(Duration::from_secs(5)).await;
    disc.send(found(0, 1, vec![])).await.unwrap();
    let mut again = accept(1).await;
    tokio::time::timeout(Duration::from_secs(10), second.closed())
        .await
        .expect("A evicts B");
    // Past the first lifetime's `lookup_wait` (10 s from t0).
    tokio::time::sleep_until(t0 + Duration::from_millis(10_500)).await;
    assert!(
        !again.is_closed(),
        "the first lifetime's timer expired the new lookup"
    );
    assert_eq!(handle.stats().lookups.load(Ordering::Relaxed), 1);
    assert_eq!(handle.stats().lookup_unaccepted.load(Ordering::Relaxed), 0);
    // Its own `lookup_wait`, 10 s from t0 + 5 s, still ends it.
    tokio::time::timeout(Duration::from_secs(10), again.closed())
        .await
        .expect("its own timer ends it");
    assert!(Instant::now() >= t0 + Duration::from_secs(15));
    assert_eq!(handle.stats().lost_count(Loss::Evicted), 2);
    stop.send(true).unwrap();
    let saved = task.await.unwrap();
    assert_eq!(saved.len(), 1);
}

/// A purge drops what only its source found, in any state, and keeps what
/// others found too under them (SRC-14b).
#[tokio::test(start_paused = true)]
async fn drop_source_drops_its_own_keys_and_keeps_shared_ones() {
    let mut h = Harness::start(config(), &[], vec![]);
    // Key 1 is fetching, key 2 is looking for peers, key 3 is shared.
    h.script(peer(1, 1), 5_000, Ok(()));
    h.send(found(1, 1, vec![peer(1, 1)]));
    h.send(found(1, 2, vec![]));
    h.send(found(1, 3, vec![]));
    h.send(found(2, 3, vec![]));
    sleep(Duration::from_millis(10)).await;
    assert_eq!(h.fake.started.lock().unwrap().len(), 1);

    assert_eq!(h.handle.drop_source(src(1)).await, 2);
    let queue = h.handle.snapshot().await;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].info_hash, key(3));
    assert_eq!(queue[0].source, src(2), "the remaining finder takes it");
    // The fetch in flight ends unseen: nothing comes out for key 1.
    sleep(Duration::from_secs(10)).await;
    assert!(h.out.try_recv().is_err());
    // Nothing left of the source's: a second drop finds none.
    assert_eq!(h.handle.drop_source(src(1)).await, 0);
    let queue = h.stop().await;
    assert_eq!(queue.len(), 1);
}

/// Results fetched but still waiting for an output slot are dropped by
/// their source like any of its keys, and shared ones pass to the rest.
#[tokio::test(start_paused = true)]
async fn drop_source_drops_its_results_waiting_for_the_consumer() {
    let fake = Arc::new(FakeState::default());
    let mut h = Harness::with_output(config(), &[], vec![], fake, 1);
    // Key 1 fills the channel; keys 2 (source 1 only) and 3 (shared) wait.
    h.script(peer(1, 1), 100, Ok(()));
    h.script(peer(2, 1), 1_000, Ok(()));
    h.script(peer(3, 1), 1_000, Ok(()));
    h.send(found(1, 1, vec![peer(1, 1)]));
    h.send(found(1, 2, vec![peer(2, 1)]));
    h.send(found(1, 3, vec![peer(3, 1)]));
    h.send(found(2, 3, vec![]));
    let stats = h.handle.stats();
    until("all three fetched", || {
        stats.fetched.load(Ordering::Relaxed) == 3 && stats.active.load(Ordering::Relaxed) == 0
    })
    .await;
    assert_eq!(h.handle.drop_source(src(1)).await, 1);
    assert_eq!(
        h.causes.lost.lock().unwrap()[..],
        [(key(2), Loss::Evicted, None)]
    );
    let first = h.out.recv().await.unwrap();
    assert_eq!(first.key, key(1));
    let shared = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shared.key, key(3));
    assert_eq!(shared.source, src(2), "the remaining finder takes it");
    assert_eq!(shared.sources, 1 << 2);
    assert!(shared.private_source);
    sleep(Duration::from_secs(10)).await;
    assert!(h.out.try_recv().is_err(), "nothing for key 2");
    h.stop().await;
}

/// A key the dropped source saw publicly and another saw privately is
/// private from then on: whether it was public came from both (SRC-14b).
#[tokio::test(start_paused = true)]
async fn drop_source_leaves_a_shared_key_private() {
    let h = Harness::start(config(), &[], vec![]);
    h.send(found(1, 9, vec![]));
    let mut private = found(2, 9, vec![]);
    private.found.private_source = true;
    h.send(private);
    sleep(Duration::from_millis(10)).await;
    assert!(
        !h.handle.snapshot().await[0].private_source,
        "public: one public sighting"
    );
    assert_eq!(h.handle.drop_source(src(1)).await, 0);
    let queue = h.handle.snapshot().await;
    assert_eq!(queue[0].source, src(2));
    assert!(
        queue[0].private_source,
        "the public sighting went with its source"
    );
    // A live public sighting makes it public again.
    h.send(found(2, 9, vec![]));
    sleep(Duration::from_millis(10)).await;
    assert!(!h.handle.snapshot().await[0].private_source);
    h.stop().await;
}

// X-fetch-pool-bytes: the byte budget on finished fetches.

const MIB: usize = 1 << 20;

/// What one served dict of `len` bytes weighs in the pool's budget.
fn weight(len: usize) -> u64 {
    lodestar_sources::pool::weigh(&Fetched {
        info: vec![0; len],
        v2: false,
        encrypted: true,
        pex: Vec::new(),
        client: None,
    })
}

/// Polls `done` on the paused clock until it holds, for at most 60 s.
async fn until(what: &str, done: impl FnMut() -> bool) {
    until_within(what, 60, done).await;
}

/// [`until`], for at most `secs` seconds of the paused clock.
async fn until_within(what: &str, secs: u64, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(10)).await;
    }
}

/// Starts a pool whose fetches all serve `len`-byte dicts after 1 s,
/// each key with a peer of its own, and queues `keys` keys.
fn budgeted(sockets: usize, budget: usize, len: usize, capacity: usize, keys: u32) -> Harness {
    let fake = Arc::new(FakeState::default());
    fake.info_len.store(len, Ordering::SeqCst);
    let h = Harness::with_output(
        PoolConfig {
            max_sockets: sockets,
            max_held_bytes: budget,
            ..config()
        },
        &[],
        vec![],
        fake,
        capacity,
    );
    for n in 0..keys {
        h.send(found(0, n, vec![peer(n as u8 + 1, 6881)]));
    }
    h
}

fn started(h: &Harness) -> usize {
    h.fake.started.lock().unwrap().len()
}

fn held(h: &Harness) -> u64 {
    h.handle.stats().held_bytes.load(Ordering::Relaxed)
}

/// A stalled consumer: the pool fills its budget, stops starting fetches
/// and stays there; the held bytes never pass the budget by more than the
/// fetches in flight when it filled. Once the consumer moves, every key
/// arrives exactly once.
#[tokio::test(start_paused = true)]
async fn a_stalled_consumer_pauses_fetching_within_the_budget() {
    const SOCKETS: usize = 8;
    const KEYS: u32 = 64;
    let budget = 4 * MIB;
    let w = weight(MIB);
    let bound = budget as u64 + SOCKETS as u64 * w;
    let mut h = budgeted(SOCKETS, budget, MIB, 16, KEYS);

    until("the budget to stop fetching", || {
        h.handle.stats().held_full.load(Ordering::Relaxed) > 0
            && h.handle.stats().active.load(Ordering::Relaxed) == 0
    })
    .await;
    let paused_at = started(&h);
    assert!(held(&h) >= budget as u64, "{} held", held(&h));
    assert!(held(&h) <= bound, "{} held, bound {bound}", held(&h));
    // Time passes with the consumer stalled: nothing more starts.
    sleep(Duration::from_secs(120)).await;
    assert_eq!(started(&h), paused_at, "fetching paused");
    assert!(paused_at < KEYS as usize, "{paused_at} fetches");
    assert!(held(&h) <= bound, "{} held, bound {bound}", held(&h));
    assert!(h.causes.lost.lock().unwrap().is_empty(), "nothing lost");

    // The consumer moves: every key arrives once, within the bound.
    let mut seen = std::collections::HashSet::new();
    while seen.len() < KEYS as usize {
        let got = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
            .await
            .expect("the next result")
            .expect("the pool is running");
        assert!(seen.insert(got.key), "{:?} twice", got.key);
        assert!(held(&h) <= bound, "{} held, bound {bound}", held(&h));
    }
    assert_eq!(started(&h), KEYS as usize, "each key fetched once");
    assert!(h.causes.lost.lock().unwrap().is_empty(), "nothing lost");
    h.stop().await;
}

/// A dict larger than the whole budget is delivered, not lost, and passes
/// alone: nothing else starts while it's held.
#[tokio::test(start_paused = true)]
async fn a_dict_over_the_budget_passes_alone() {
    let mut h = budgeted(1, MIB, 4 * MIB, 1, 3);
    until("the first dict to be held", || held(&h) >= weight(4 * MIB)).await;
    sleep(Duration::from_secs(120)).await;
    assert_eq!(started(&h), 1, "nothing else starts");
    let mut seen = std::collections::HashSet::new();
    while seen.len() < 3 {
        let got = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
            .await
            .expect("the next result")
            .expect("the pool is running");
        assert_eq!(got.fetched.info.len(), 4 * MIB);
        assert!(seen.insert(got.key), "once each");
    }
    assert!(h.causes.lost.lock().unwrap().is_empty(), "nothing lost");
    h.stop().await;
}

/// Shutdown with the budget full: what was fetched is drained to the
/// consumer, and every other key is saved with the queue or reported lost;
/// each key exactly once.
#[tokio::test(start_paused = true)]
async fn shutdown_with_the_budget_full_drains_or_saves_every_key() {
    const KEYS: u32 = 64;
    let h = budgeted(8, 4 * MIB, MIB, 2, KEYS);
    until("the budget to stop fetching", || {
        h.handle.stats().held_full.load(Ordering::Relaxed) > 0
            && h.handle.stats().active.load(Ordering::Relaxed) == 0
    })
    .await;
    let fetched = started(&h);
    let Harness {
        mut out,
        stop,
        task,
        causes,
        ..
    } = h;
    stop.send(true).unwrap();
    // The consumer runs until the pool ends, as the pipeline's does.
    let consumer = tokio::spawn(async move {
        let mut got = Vec::new();
        while let Some(r) = out.recv().await {
            got.push(r.key);
        }
        got
    });
    let queue = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the pool stops")
        .unwrap();
    let got = consumer.await.unwrap();
    assert_eq!(got.len(), fetched, "every fetched dict delivered");
    let lost: Vec<InfoHash> = causes.lost.lock().unwrap().iter().map(|l| l.0).collect();
    let mut all: Vec<InfoHash> = got
        .iter()
        .copied()
        .chain(queue.iter().map(|q| q.info_hash))
        .chain(lost)
        .collect();
    all.sort_by_key(|k| k.0);
    let mut want: Vec<InfoHash> = (0..KEYS).map(key).collect();
    want.sort_by_key(|k| k.0);
    assert_eq!(all, want, "each key delivered, saved or lost, once");
}

/// Shutdown with no consumer left: what the pool fetched and couldn't
/// hand over goes back into the saved queue instead of vanishing.
#[tokio::test(start_paused = true)]
async fn shutdown_without_a_consumer_saves_what_it_fetched() {
    const KEYS: u32 = 16;
    let h = budgeted(4, 64 * MIB, 20, 1, KEYS);
    // One result fills the channel; the rest of the first round waits.
    until("a round to finish", || {
        started(&h) >= 4 && h.handle.stats().active.load(Ordering::Relaxed) == 0
    })
    .await;
    let Harness {
        out,
        stop,
        task,
        causes,
        ..
    } = h;
    // The receiver takes the one in the channel with it.
    drop(out);
    stop.send(true).unwrap();
    let queue = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the pool stops")
        .unwrap();
    assert!(causes.lost.lock().unwrap().is_empty());
    assert_eq!(
        queue.len(),
        KEYS as usize - 1,
        "all but the one handed over"
    );
    let keys: std::collections::HashSet<_> = queue.iter().map(|q| q.info_hash.0).collect();
    assert_eq!(keys.len(), queue.len(), "once each");
}

/// Shutdown with fetches still running and results waiting for the
/// consumer: the running fetches are saved with their attempts unchanged,
/// the waiting results are delivered, and each key appears once.
#[tokio::test(start_paused = true)]
async fn shutdown_saves_running_fetches_and_delivers_waiting_results() {
    let h = budgeted(4, 64 * MIB, 20, 1, 4);
    // Keys 0 and 1 finish (one fills the channel, one waits); 2 and 3 run on.
    h.script(peer(1, 6881), 500, Ok(()));
    h.script(peer(2, 6881), 500, Ok(()));
    h.script(peer(3, 6881), 100_000, Ok(()));
    h.script(peer(4, 6881), 100_000, Ok(()));
    let stats = h.handle.stats();
    until("two results", || {
        stats.fetched.load(Ordering::Relaxed) == 2 && stats.active.load(Ordering::Relaxed) == 2
    })
    .await;
    // A periodic save meanwhile holds the waiting result too.
    let saved: Vec<InfoHash> = h
        .handle
        .snapshot()
        .await
        .iter()
        .map(|q| q.info_hash)
        .collect();
    assert_eq!(saved.len(), 3, "two running, one waiting: {saved:?}");
    let before: HashMap<InfoHash, u16> = h
        .handle
        .snapshot()
        .await
        .iter()
        .map(|q| (q.info_hash, q.attempts))
        .collect();
    let Harness {
        mut out,
        stop,
        task,
        causes,
        handle,
        ..
    } = h;
    stop.send(true).unwrap();
    let consumer = tokio::spawn(async move {
        let mut got = Vec::new();
        while let Some(r) = out.recv().await {
            got.push(r.key);
        }
        got
    });
    let queue = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the pool stops")
        .unwrap();
    let mut got = consumer.await.unwrap();
    got.sort_by_key(|k| k.0);
    assert_eq!(got, vec![key(0), key(1)], "both results delivered");
    let mut saved: Vec<(InfoHash, u16)> = queue.iter().map(|q| (q.info_hash, q.attempts)).collect();
    saved.sort_by_key(|(k, _)| k.0);
    assert_eq!(
        saved,
        vec![(key(2), before[&key(2)]), (key(3), before[&key(3)])],
        "the running fetches saved, attempts unchanged"
    );
    assert!(causes.lost.lock().unwrap().is_empty(), "nothing lost");
    for kind in [FailureKind::Invalid, FailureKind::TimedOut] {
        assert_eq!(
            handle.stats().failure_count(kind),
            0,
            "a cancel isn't a failure"
        );
    }
}

/// The held-bytes gauge falls as the consumer takes results, even while
/// others still wait for a slot.
#[tokio::test(start_paused = true)]
async fn held_bytes_fall_as_the_consumer_takes_results() {
    let mut h = budgeted(4, 64 * MIB, MIB, 1, 4);
    let w = weight(MIB);
    let stats = h.handle.stats();
    until("four results held", || {
        stats.fetched.load(Ordering::Relaxed) == 4 && held(&h) == 4 * w
    })
    .await;
    for left in (0..4u64).rev() {
        tokio::time::timeout(Duration::from_secs(60), h.out.recv())
            .await
            .unwrap()
            .unwrap();
        until("the gauge to follow", || held(&h) == left * w).await;
    }
    h.stop().await;
}

// X-fetch-meta-reserve: fetches reserve their result's bytes before they
// download them.

fn reserved(h: &Harness) -> u64 {
    h.handle.stats().reserved_bytes.load(Ordering::Relaxed)
}

/// Like [`budgeted`], with fetches that reserve their dict's bytes 100 ms
/// in, waiting at most `deadline_ms` (0: for ever).
fn reserving(
    sockets: usize,
    budget: usize,
    len: usize,
    capacity: usize,
    keys: u32,
    deadline_ms: u64,
) -> Harness {
    let fake = Arc::new(FakeState::default());
    fake.info_len.store(len, Ordering::SeqCst);
    fake.reserving.store(true, Ordering::SeqCst);
    fake.reserve_deadline_ms
        .store(deadline_ms, Ordering::SeqCst);
    let h = Harness::with_output(
        PoolConfig {
            max_sockets: sockets,
            max_held_bytes: budget,
            ..config()
        },
        &[],
        vec![],
        fake,
        capacity,
    );
    for n in 0..keys {
        h.send(found(0, n, vec![peer(n as u8 + 1, 6881)]));
    }
    h
}

/// Many fetches with large dicts against a small budget: they wait for
/// room instead of downloading, so what's reserved and held together never
/// passes the budget, even with the consumer stalled. While a reservation
/// waits, the pool starts nothing more. Once the consumer moves, every key
/// arrives once, and no peer was charged for waiting.
#[tokio::test(start_paused = true)]
async fn reservations_keep_held_and_reserved_bytes_within_the_budget() {
    const SOCKETS: usize = 16;
    const KEYS: u32 = 48;
    let budget = 4 * MIB;
    let w = weight(MIB);
    let mut h = reserving(SOCKETS, budget, MIB, 16, KEYS, 0);
    let within = |h: &Harness| {
        let (held, reserved) = (held(h), reserved(h));
        assert!(
            held + reserved <= budget as u64,
            "{held} held + {reserved} reserved over {budget}"
        );
        assert!(
            h.fake.max_reserved.load(Ordering::SeqCst) <= budget as u64,
            "downloads reserved too much at once"
        );
    };

    until("the budget to stop fetching", || {
        within(&h);
        h.handle.stats().held_full.load(Ordering::Relaxed) > 0
            && h.fake.reserved.load(Ordering::SeqCst) == 0
            && held(&h) > 0
    })
    .await;
    // Only what fits is held: the old bound let every socket's dict in.
    assert!(held(&h) <= budget as u64, "{} held", held(&h));
    assert_eq!(held(&h), held(&h) / w * w, "whole dicts");
    let paused_at = started(&h);
    assert!(paused_at <= SOCKETS, "{paused_at} started");
    // Waiting reservations hold their sockets, and nothing else starts.
    sleep(Duration::from_secs(120)).await;
    within(&h);
    assert_eq!(started(&h), paused_at, "fetching paused");
    assert!(h.handle.stats().active.load(Ordering::Relaxed) > 0);

    let mut seen = std::collections::HashSet::new();
    while seen.len() < KEYS as usize {
        let got = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
            .await
            .expect("the next result")
            .expect("the pool is running");
        assert!(seen.insert(got.key), "{:?} twice", got.key);
        within(&h);
    }
    assert_eq!(started(&h), KEYS as usize, "each key fetched once");
    assert!(h.causes.lost.lock().unwrap().is_empty(), "nothing lost");
    let stats = h.handle.stats();
    assert_eq!(stats.reserve_timeouts.load(Ordering::Relaxed), 0);
    until("the reservations released", || reserved(&h) == 0).await;
    h.stop().await;
}

/// A fetch whose deadline runs out while it waits for a reservation frees
/// its place in the queue and holds nothing. Its peer isn't charged: the
/// key is fetched from the same peer later, and nothing counts as failed.
#[tokio::test(start_paused = true)]
async fn a_reservation_that_waits_out_its_deadline_is_freed_and_retried() {
    const KEYS: u32 = 4;
    let budget = 2 * MIB;
    // Two fit; the other two wait 2 s, then time out, while the consumer
    // is stalled.
    let mut h = reserving(4, budget, MIB - 4096, 4, KEYS, 2_000);
    let stats = h.handle.stats();
    until("a reservation to time out", || {
        stats.reserve_timeouts.load(Ordering::Relaxed) >= 2
    })
    .await;
    assert!(held(&h) + reserved(&h) <= budget as u64);
    for kind in [FailureKind::TimedOut, FailureKind::Invalid] {
        assert_eq!(stats.failure_count(kind), 0, "not the peer's failure");
    }
    let mut seen = std::collections::HashSet::new();
    while seen.len() < KEYS as usize {
        // A key whose only peer waited out backs off `retry_backoff`
        // (60 s here) before it's retried.
        let got = tokio::time::timeout(Duration::from_secs(300), h.out.recv())
            .await
            .expect("the next result")
            .expect("the pool is running");
        assert!(seen.insert(got.key), "{:?} twice", got.key);
    }
    assert!(h.causes.lost.lock().unwrap().is_empty(), "nothing lost");
    assert!(h.drain_lookups().is_empty(), "no lookup: no attempt used");
    // Retried with the same peer each time, as it was never charged.
    let started = h.fake.started.lock().unwrap().clone();
    for (k, p) in &started {
        let n = u32::from_be_bytes(k.0[..4].try_into().unwrap());
        assert_eq!(*p, peer(n as u8 + 1, 6881));
    }
    assert!(started.len() > KEYS as usize, "some were started again");
    until("the reservations released", || reserved(&h) == 0).await;
    h.stop().await;
}

/// The reservation gate: a waiting reservation stops new fetches, and a
/// dict larger than the whole budget reserves alone once nothing else is
/// reserved or held.
#[tokio::test(start_paused = true)]
async fn a_reservation_over_the_budget_waits_for_the_budget_to_empty() {
    let mut h = reserving(4, MIB, 2 * MIB, 4, 3, 0);
    until("one dict held", || held(&h) >= weight(2 * MIB)).await;
    sleep(Duration::from_secs(60)).await;
    // The others started before the first reserved, and wait for room.
    assert!(started(&h) <= 3);
    assert_eq!(held(&h), weight(2 * MIB), "only one at a time");
    let mut seen = std::collections::HashSet::new();
    while seen.len() < 3 {
        let got = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
            .await
            .expect("the next result")
            .expect("the pool is running");
        assert!(seen.insert(got.key));
        assert!(held(&h) <= weight(2 * MIB), "{} held", held(&h));
    }
    h.stop().await;
}

/// The receiver goes away while the pool runs: the pool stops fetching,
/// keeps what it fetched, and saves it with the queue at shutdown.
#[tokio::test(start_paused = true)]
async fn a_receiver_closing_while_the_pool_runs_stops_fetching() {
    const KEYS: u32 = 32;
    let h = budgeted(4, 64 * MIB, 20, 1, KEYS);
    let stats = h.handle.stats();
    until("some fetched", || {
        stats.fetched.load(Ordering::Relaxed) >= 4
    })
    .await;
    let Harness {
        out,
        stop,
        task,
        causes,
        fake,
        handle,
        ..
    } = h;
    drop(out);
    // Whatever was running finishes; then nothing more starts.
    until("the running fetches to end", || {
        handle.stats().active.load(Ordering::Relaxed) == 0
    })
    .await;
    let after = fake.started.lock().unwrap().len();
    sleep(Duration::from_secs(120)).await;
    assert_eq!(fake.started.lock().unwrap().len(), after, "no more fetches");
    stop.send(true).unwrap();
    let queue = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the pool stops")
        .unwrap();
    assert!(causes.lost.lock().unwrap().is_empty());
    let keys: std::collections::HashSet<_> = queue.iter().map(|q| q.info_hash.0).collect();
    assert_eq!(keys.len(), queue.len(), "once each");
    // The one in the channel went with the receiver; the rest are saved.
    assert_eq!(queue.len(), KEYS as usize - 1);
}

/// A consumer that's alive but stalled at shutdown: the pool waits for it
/// at most `SHUTDOWN_DRAIN`, then saves what's left with the queue.
#[tokio::test(start_paused = true)]
async fn shutdown_with_a_stalled_consumer_saves_the_rest_after_the_deadline() {
    const KEYS: u32 = 8;
    let h = budgeted(4, 64 * MIB, 20, 1, KEYS);
    until("a round to finish", || {
        started(&h) >= 4 && h.handle.stats().active.load(Ordering::Relaxed) == 0
    })
    .await;
    let Harness {
        out,
        stop,
        task,
        causes,
        ..
    } = h;
    let stopped_at = Instant::now();
    stop.send(true).unwrap();
    let queue = tokio::time::timeout(Duration::from_secs(60), task)
        .await
        .expect("the pool stops despite the stalled consumer")
        .unwrap();
    let waited = Instant::now() - stopped_at;
    assert!(waited >= SHUTDOWN_DRAIN, "waited {waited:?}");
    assert!(
        waited < SHUTDOWN_DRAIN + Duration::from_secs(1),
        "{waited:?}"
    );
    assert!(causes.lost.lock().unwrap().is_empty());
    // The receiver still holds the one in the channel; every other key
    // is saved, once.
    assert_eq!(out.len(), 1);
    let keys: std::collections::HashSet<_> = queue.iter().map(|q| q.info_hash.0).collect();
    assert_eq!(keys.len(), queue.len(), "once each");
    assert_eq!(queue.len(), KEYS as usize - 1);
}

/// A result purged while it waits for the consumer leaves the recently
/// fetched order too, so the key fetched again later stays remembered
/// for its whole turn there.
#[tokio::test(start_paused = true)]
async fn a_purged_result_leaves_the_recent_order() {
    let fake = Arc::new(FakeState::default());
    let mut h = Harness::with_output(
        PoolConfig {
            recent: 2,
            ..config()
        },
        &[],
        vec![],
        fake,
        1,
    );
    // Key 1 fills the channel; key 2 (source 1 only) waits and is purged.
    h.script(peer(1, 1), 100, Ok(()));
    h.script(peer(2, 1), 1_000, Ok(()));
    h.send(found(1, 1, vec![peer(1, 1)]));
    h.send(found(1, 2, vec![peer(2, 1)]));
    let stats = h.handle.stats();
    until("both fetched", || {
        stats.fetched.load(Ordering::Relaxed) == 2 && stats.active.load(Ordering::Relaxed) == 0
    })
    .await;
    assert_eq!(h.handle.drop_source(src(1)).await, 1);
    assert_eq!(h.out.recv().await.unwrap().key, key(1));
    // Source 2 finds key 2 again: fetched afresh, then key 3.
    h.send(found(2, 2, vec![peer(2, 1)]));
    let again = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.key, key(2));
    h.send(found(2, 3, vec![peer(3, 1)]));
    let third = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(third.key, key(3));
    // Key 2 is among the two most recent, so a new sighting is a duplicate.
    let before = stats.duplicates.load(Ordering::Relaxed);
    h.send(found(0, 2, vec![peer(2, 1)]));
    until("the sighting counted", || {
        stats.duplicates.load(Ordering::Relaxed) > before
    })
    .await;
    sleep(Duration::from_secs(10)).await;
    assert_eq!(
        stats.fetched.load(Ordering::Relaxed),
        4,
        "not fetched again"
    );
    h.stop().await;
}

/// Silent peers that announce a dict and send nothing pin only their first
/// 64 KiB window each, whether they announce 1 MiB (the old first window, a
/// partial claim) or 64 KiB (a whole one). Against a 4 MiB worker budget,
/// 20 of each don't stop small dicts flowing (runner, 2026-11-07).
#[tokio::test(start_paused = true)]
async fn silent_claimers_pin_one_small_window_and_small_dicts_still_flow() {
    const SILENT: u32 = 20;
    const SMALL: u32 = 64;
    let budget = 4 * MIB;
    let fake = Arc::new(FakeState::default());
    fake.info_len.store(30_000, Ordering::SeqCst);
    fake.reserving.store(true, Ordering::SeqCst);
    let mut h = Harness::with_output(
        PoolConfig {
            max_sockets: 2 * SILENT as usize + 16,
            per_ip: 64,
            max_held_bytes: budget,
            ..config()
        },
        &[],
        vec![],
        fake,
        4096,
    );
    // Silent peers first, so they hold their windows when the small ones
    // come: half announcing the old 1 MiB window, half the new 64 KiB.
    for n in 0..2 * SILENT {
        let p = SocketAddr::from(([10, 1, (n / 200) as u8, (n % 200) as u8 + 1], 6881));
        let announced = if n < SILENT { MIB } else { WINDOW };
        h.fake.silent.lock().unwrap().insert(p, announced);
        h.send(found(0, 10_000 + n, vec![p]));
    }
    let stats = h.handle.stats();
    until("the silent peers to reserve", || {
        h.fake.reserved.load(Ordering::SeqCst) >= 2 * SILENT as u64 * WINDOW as u64
    })
    .await;
    let pinned = h.fake.reserved.load(Ordering::SeqCst);
    // Each pins its window plus the bound's PEX room and the struct.
    assert!(
        pinned <= 2 * SILENT as u64 * (WINDOW as u64 + (8 << 10)),
        "{pinned}"
    );
    for n in 0..SMALL {
        h.send(found(0, n, vec![peer(n as u8 + 1, 6881)]));
    }
    // Every small dict arrives well inside the silent peers' deadline.
    let started = Instant::now();
    let mut seen = std::collections::HashSet::new();
    while seen.len() < SMALL as usize {
        let got = tokio::time::timeout(Duration::from_secs(60), h.out.recv())
            .await
            .expect("the next small dict")
            .expect("the pool is running");
        assert!(seen.insert(got.key));
        assert!(held(&h) + reserved(&h) <= budget as u64);
    }
    assert!(
        Instant::now() - started < DEADLINE,
        "they didn't wait for the liars"
    );
    assert_eq!(stats.reserve_timeouts.load(Ordering::Relaxed), 0);
    h.stop().await;
}

/// A key that keeps waiting out its reservation is charged on the
/// `RESERVE_WAITS_FREE`th time: the timeout counts as a failed fetch, so
/// it can't cycle for ever on waits alone.
#[tokio::test(start_paused = true)]
async fn the_third_reserve_timeout_is_charged() {
    use lodestar_sources::pool::RESERVE_WAITS_FREE;
    // Key 0 fills the budget and stays held (the consumer is stalled);
    // key 1 waits 2 s each time and times out.
    let mut h = reserving(2, MIB, MIB - 4096, 4, 2, 2_000);
    let stats = h.handle.stats();
    until("the first wait out", || {
        stats.reserve_timeouts.load(Ordering::Relaxed) == 1
    })
    .await;
    // Backing off with its peer still to try, interest makes it ready at
    // once: no lookup.
    assert!(h.handle.interest(key(1)).await);
    until("it waits out again", || {
        stats.reserve_timeouts.load(Ordering::Relaxed) == 2
    })
    .await;
    assert!(
        h.lookups.try_recv().is_err(),
        "no lookup for a key with a peer"
    );
    // The third wait, `retry_backoff` (60 s) later, is charged.
    until_within("the waits to be charged", 600, || {
        stats.failure_count(FailureKind::TimedOut) > 0
    })
    .await;
    assert_eq!(
        stats.reserve_timeouts.load(Ordering::Relaxed),
        u64::from(RESERVE_WAITS_FREE)
    );
    assert_eq!(stats.failure_count(FailureKind::TimedOut), 1);
    // Charged, its only peer is spent: it looks for more.
    until("a lookup for key 1", || {
        h.lookups.try_recv().is_ok_and(|k| k == key(1))
    })
    .await;
    h.stop().await;
}

/// With streaming peers on, a fetch that waits out its reservation while
/// the key's stream is still open leaves the stream to bring the key back:
/// no second lookup (raising interest included), and the lookup count
/// returns to 0 when the stream ends.
#[tokio::test(start_paused = true)]
async fn a_reserve_timeout_under_a_live_stream_starts_no_second_lookup() {
    reserve_timeout_under_a_live_stream(3).await;
}

/// The same on the key's last attempt: when the stream ends, the key is
/// ready with its untried peer, not lost, and the lookup count is
/// decremented once (runner's review: it underflowed).
#[tokio::test(start_paused = true)]
async fn a_reserve_timeout_under_a_stream_on_the_last_attempt_keeps_the_key() {
    reserve_timeout_under_a_live_stream(1).await;
}

async fn reserve_timeout_under_a_live_stream(max_attempts: u16) {
    use lodestar_sources::{LookupSession, PeerRequest, Terminal};
    let len = MIB - 4096;
    let fake = Arc::new(FakeState::default());
    fake.info_len.store(len, Ordering::SeqCst);
    fake.reserving.store(true, Ordering::SeqCst);
    fake.reserve_deadline_ms.store(2_000, Ordering::SeqCst);
    let (disc, discoveries) = mpsc::channel(64);
    let (finder, mut requests) = mpsc::channel::<PeerRequest>(64);
    let (out_tx, mut out) = mpsc::channel(4);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        PoolConfig {
            max_sockets: 4,
            // Room left, but not a third dict's.
            max_held_bytes: 2 * weight(len) as usize + 1000,
            streaming_peers: true,
            max_attempts,
            ..config()
        },
        Arc::new(Fake(fake.clone())),
        PoolIo {
            discoveries,
            peer_finders: vec![finder],
            out: out_tx,
            funnel: Arc::new(FunnelCounts::default()),
            shutdown,
        },
        HashMap::new(),
        Vec::new(),
    );
    let stats = handle.stats();
    // Keys 1 and 2 fill the budget, held while the consumer is stalled.
    disc.try_send(found(0, 1, vec![peer(1, 6881)])).unwrap();
    disc.try_send(found(0, 2, vec![peer(2, 6881)])).unwrap();
    until("the budget full", || {
        stats.held_bytes.load(Ordering::Relaxed) == 2 * weight(len)
    })
    .await;
    // Key 3 has no peers: its lookup streams one peer and stays open.
    disc.try_send(found(0, 3, vec![])).unwrap();
    let request = tokio::time::timeout(Duration::from_secs(60), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.key, key(3));
    let (batches_tx, batches) = mpsc::channel(4);
    let (terminal_tx, terminal) = tokio::sync::oneshot::channel();
    request
        .session
        .expect("a streaming request")
        .send(Some(LookupSession { batches, terminal }))
        .unwrap();
    batches_tx.send(vec![peer(3, 6881)]).await.unwrap();
    until("key 3 to wait out its reservation", || {
        stats.reserve_timeouts.load(Ordering::Relaxed) == 1
    })
    .await;
    assert_eq!(stats.lookups.load(Ordering::Relaxed), 1, "the stream's one");
    // It waits for the stream, as after a failure, rather than redialling:
    // interest neither starts a second lookup nor a fetch.
    let starts = |fake: &FakeState| {
        fake.started
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == key(3))
            .count()
    };
    let before = starts(&fake);
    assert!(handle.interest(key(3)).await);
    sleep(Duration::from_secs(1)).await;
    assert!(requests.try_recv().is_err(), "no second lookup");
    assert_eq!(starts(&fake), before, "the stream brings it back");
    assert_eq!(stats.lookups.load(Ordering::Relaxed), 1);
    // The stream ends; the consumer moves; key 3 is fetched with its peer.
    drop(batches_tx);
    terminal_tx.send(Terminal::Complete).unwrap();
    let mut seen = std::collections::HashSet::new();
    while seen.len() < 3 {
        let got = tokio::time::timeout(Duration::from_secs(600), out.recv())
            .await
            .expect("the next result")
            .unwrap();
        seen.insert(got.key);
    }
    until("the lookup count back to 0", || {
        stats.lookups.load(Ordering::Relaxed) == 0
    })
    .await;
    drop(requests);
    drop(disc);
    stop.send(true).unwrap();
    task.await.unwrap();
}

/// A stream that ends empty on the key's last attempt loses the key as
/// "no peers", once, and gives its lookup slot back once. CR-1a-S counted
/// it twice, which underflowed `lookups` (runner's review, a bug already on
/// `main`).
#[tokio::test(start_paused = true)]
async fn a_stream_that_ends_empty_on_the_last_attempt_counts_its_lookup_once() {
    use lodestar_sources::{LookupSession, PeerRequest, Terminal};
    let (disc, discoveries) = mpsc::channel(8);
    let (finder, mut requests) = mpsc::channel::<PeerRequest>(8);
    let (out_tx, _out) = mpsc::channel(4);
    let (stop, shutdown) = Shutdown::new();
    let causes = Arc::new(Causes {
        counts: Arc::new(FunnelCounts::default()),
        lost: Mutex::default(),
    });
    let (handle, task) = spawn(
        PoolConfig {
            streaming_peers: true,
            max_attempts: 1,
            ..config()
        },
        Arc::new(Fake(Arc::new(FakeState::default()))),
        PoolIo {
            discoveries,
            peer_finders: vec![finder],
            out: out_tx,
            funnel: causes.clone(),
            shutdown,
        },
        HashMap::new(),
        Vec::new(),
    );
    disc.try_send(found(0, 1, vec![])).unwrap();
    let request = tokio::time::timeout(Duration::from_secs(60), requests.recv())
        .await
        .unwrap()
        .unwrap();
    let (batches_tx, batches) = mpsc::channel(4);
    let (terminal_tx, terminal) = tokio::sync::oneshot::channel();
    request
        .session
        .expect("a streaming request")
        .send(Some(LookupSession { batches, terminal }))
        .unwrap();
    let stats = handle.stats();
    until("the lookup to start", || {
        stats.lookups.load(Ordering::Relaxed) == 1
    })
    .await;
    drop(batches_tx);
    terminal_tx.send(Terminal::Complete).unwrap();
    until("the key to be lost", || {
        !causes.lost.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        causes.lost.lock().unwrap()[..],
        [(key(1), Loss::NoPeers, None)]
    );
    // A round of the pool publishes its stats.
    handle.queued().await;
    assert_eq!(stats.lookups.load(Ordering::Relaxed), 0);
    stop.send(true).unwrap();
    task.await.unwrap();
}
