//! CR-HINT-FIRST (`dht.hint_first`, docs/decisions/2026-11-21-CR-HINT-FIRST.md):
//! the pool's T1 tier against a scripted finder, in paused time. Waits poll
//! a condition with a 10 s (virtual) deadline; no fixed sleeps.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::pool::spawn;
use lodestar_sources::{
    BoxFuture, Discovery, Fetcher, Found, FunnelCounts, PeerRequest, PoolConfig, PoolHandle,
    PoolIo, PoolStats, Shutdown,
};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

/// The DHT's source in these fixtures, and another plugin's.
fn dht() -> SourceId {
    SourceId::new(1).unwrap()
}
fn other() -> SourceId {
    SourceId::new(2).unwrap()
}

fn key(n: u32) -> InfoHash {
    let mut h = [0u8; 20];
    h[..4].copy_from_slice(&n.to_be_bytes());
    InfoHash(h)
}

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(
        &self,
        _: SocketAddr,
        _: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        Box::pin(async { Err(FailureKind::Refused) })
    }
}

/// `coordinated` admission with one permit, or the plain backlog with
/// one lookup slot; hint-first on, trusting the DHT's source.
fn config(coordinated: bool, hint_first: bool) -> PoolConfig {
    PoolConfig {
        coordinated_admission: coordinated,
        finder_capacity: 1,
        max_lookups: if coordinated { 256 } else { 1 },
        lookup_wait: Duration::from_secs(600),
        retry_backoff: Duration::from_secs(600),
        max_attempts: 3,
        hint_first,
        hint_sources: vec![dht()],
        ..PoolConfig::default()
    }
}

struct Held {
    done: oneshot::Sender<Vec<SocketAddr>>,
    permit: Option<OwnedSemaphorePermit>,
}

struct Harness {
    disc: mpsc::Sender<Discovery>,
    requests: mpsc::Receiver<PeerRequest>,
    handle: PoolHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<Vec<QueuedKey>>,
    returned: u64,
}

impl Harness {
    fn start(config: PoolConfig) -> Harness {
        let (disc, discoveries) = mpsc::channel(64);
        let (finder, requests) = mpsc::channel(512);
        let (out, _) = mpsc::channel(64);
        let (stop, shutdown) = Shutdown::new();
        let (handle, task) = spawn(
            config,
            Arc::new(NoFetch),
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
        Harness {
            disc,
            requests,
            handle,
            stop,
            task,
            returned: 0,
        }
    }

    fn stats(&self) -> &PoolStats {
        self.handle.stats()
    }

    async fn discover(&self, n: u32, source: SourceId, hint: bool) {
        let mut found = Found::key(key(n), 0);
        found.hint = hint;
        self.disc
            .send(Discovery {
                source,
                found,
                at: 1_700_000_000,
                reply: false,
            })
            .await
            .unwrap();
    }

    async fn next(&mut self) -> PeerRequest {
        tokio::time::timeout(Duration::from_secs(10), self.requests.recv())
            .await
            .expect("a request within 10 s")
            .expect("the pool is running")
    }

    fn accept(&mut self, r: PeerRequest) -> Held {
        let (done, rx) = oneshot::channel();
        r.admit.send(Some(rx)).unwrap();
        Held {
            done,
            permit: r.permit,
        }
    }

    fn complete(&mut self, h: Held) {
        if h.permit.is_some() {
            self.returned += 1;
        }
        let _ = h.done.send(vec![]);
    }

    async fn until(&self, what: &str, done: impl Fn(&PoolStats) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self.stats()) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            sleep(Duration::from_millis(1)).await;
        }
    }

    /// Serves `n` requests one at a time, in the order the pool sends
    /// them; returns their keys.
    async fn serve(&mut self, n: usize) -> Vec<InfoHash> {
        let mut order = Vec::new();
        for _ in 0..n {
            let r = self.next().await;
            order.push(r.key);
            let r = self.accept(r);
            self.complete(r);
        }
        order
    }

    /// Permits granted = given back here + given back in the pool + in use.
    async fn conserved(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.handle.queued().await;
            sleep(Duration::from_millis(1)).await;
            let s = self.stats();
            let granted: u64 = s.admission_grants.iter().flatten().map(load).sum();
            let pool: u64 = s.admission_permit_returns[..2].iter().map(load).sum();
            let in_use = load(&s.admission_permits_in_use);
            if granted == self.returned + pool + in_use {
                return;
            }
            assert!(Instant::now() < deadline, "permits not conserved");
        }
    }

    async fn stop(self) {
        self.stop.send(true).unwrap();
        self.task.await.unwrap();
    }
}

fn load(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

fn keys(ns: &[u32]) -> Vec<InfoHash> {
    ns.iter().map(|n| key(*n)).collect()
}

/// Under coordinated admission T1 goes before background, newest first,
/// takes the same one permit per request (the `hinted` grant class), and
/// background still gets every fifth grant.
#[tokio::test(start_paused = true)]
async fn t1_goes_first_with_the_same_permits_and_background_keeps_moving() {
    let mut h = Harness::start(config(true, true));
    h.discover(100, dht(), false).await;
    let first = h.next().await;
    let first = h.accept(first);
    for n in 1..=6 {
        h.discover(n, dht(), true).await;
    }
    for n in 101..=102 {
        h.discover(n, dht(), false).await;
    }
    h.until("all wait", |s| load(&s.admission_waiters) == 8)
        .await;
    assert_eq!(load(&h.stats().hint_t1_waits), 6);
    h.complete(first);
    let order = h.serve(8).await;
    assert_eq!(order, keys(&[6, 5, 4, 3, 101, 2, 1, 102]));
    let s = h.stats();
    assert_eq!(load(&s.hint_keys), 6);
    assert_eq!(load(&s.hint_lookups), 6);
    assert_eq!(load(&s.admission_grants[0][2]), 6, "T1 grants, arrivals");
    h.conserved().await;
    // Each key was asked once: a key never holds two queue places.
    let mut sorted = order.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), order.len());
    h.stop().await;
}

/// The pool honours a hint mark only from a trusted source with the
/// switch on; any other mark is ignored and the key waits as background.
#[tokio::test(start_paused = true)]
async fn only_a_trusted_source_with_the_switch_on_buys_t1() {
    for (hint_first, source) in [(true, other()), (false, dht())] {
        let mut h = Harness::start(config(true, hint_first));
        h.discover(100, dht(), false).await;
        let first = h.next().await;
        let first = h.accept(first);
        h.discover(101, dht(), false).await;
        h.discover(1, source, true).await;
        h.until("both wait", |s| load(&s.admission_waiters) == 2)
            .await;
        h.complete(first);
        assert_eq!(h.serve(2).await, keys(&[101, 1]), "FIFO: no T1");
        let s = h.stats();
        assert_eq!(load(&s.hint_ignored), 1);
        assert_eq!(load(&s.hint_keys) + load(&s.hint_t1_waits), 0);
        assert!(s.admission_grants.iter().all(|k| load(&k[2]) == 0));
        h.stop().await;
    }
}

/// A T1 key that waits [`T1_MAX_WAIT`] goes behind the background queue:
/// its hint has expired, so T1 has nothing left to give it.
#[tokio::test(start_paused = true)]
async fn an_aged_t1_key_goes_behind_background() {
    let mut h = Harness::start(config(true, true));
    h.discover(100, dht(), false).await;
    let first = h.next().await;
    let first = h.accept(first);
    h.discover(1, dht(), true).await;
    h.discover(101, dht(), false).await;
    h.until("both wait", |s| load(&s.admission_waiters) == 2)
        .await;
    tokio::time::advance(Duration::from_secs(60)).await;
    h.complete(first);
    assert_eq!(h.serve(2).await, keys(&[101, 1]));
    let s = h.stats();
    assert_eq!(load(&s.hint_t1_left[1]), 1);
    assert_eq!(load(&s.hint_lookups), 0, "served as background");
    assert_eq!(load(&s.admission_grants[0][2]), 0);
    h.conserved().await;
    h.stop().await;
}

/// The plain backlog (no coordinated admission) has T1 too, newest first,
/// with the same burst rule.
#[tokio::test(start_paused = true)]
async fn the_plain_backlog_serves_t1_first_and_keeps_background_moving() {
    let mut h = Harness::start(config(false, true));
    h.discover(100, dht(), false).await;
    let first = h.next().await;
    let first = h.accept(first);
    for n in 1..=6 {
        h.discover(n, dht(), true).await;
    }
    for n in 101..=102 {
        h.discover(n, dht(), false).await;
    }
    h.until("all wait", |s| load(&s.hint_t1_waits) == 6).await;
    h.complete(first);
    assert_eq!(h.serve(8).await, keys(&[6, 5, 4, 3, 101, 2, 1, 102]));
    assert_eq!(load(&h.stats().hint_lookups), 6);
    h.stop().await;
}

/// T1 holds at most `T1_WAITING` keys; past it the oldest leaves for its
/// ordinary queue, holding one place there.
#[tokio::test(start_paused = true)]
async fn a_full_t1_sends_hinted_keys_to_their_ordinary_queue() {
    const T1_WAITING: u32 = 256;
    let mut h = Harness::start(config(true, true));
    h.discover(100_000, dht(), false).await;
    let first = h.next().await;
    let first = h.accept(first);
    for n in 1..=T1_WAITING + 1 {
        h.discover(n, dht(), true).await;
    }
    h.until("all wait", |s| {
        load(&s.admission_waiters) == u64::from(T1_WAITING + 1)
    })
    .await;
    let s = h.stats();
    assert_eq!(load(&s.hint_t1_waits), u64::from(T1_WAITING + 1));
    assert_eq!(load(&s.hint_t1_left[0]), 1);
    h.complete(first);
    let order = h.serve((T1_WAITING + 1) as usize).await;
    // Newest first; the oldest, which left T1, waited as background:
    // served fifth, after a T1 burst, and once.
    assert_eq!(order[0], key(T1_WAITING + 1));
    assert_eq!(order[4], key(1));
    assert_eq!(order.iter().filter(|k| **k == key(1)).count(), 1);
    assert_eq!(load(&h.stats().hint_lookups), u64::from(T1_WAITING));
    h.conserved().await;
    h.stop().await;
}

/// The plain backlog's T1 holds at most `T1_WAITING` keys too; past it
/// the oldest leaves for the backlog, served once.
#[tokio::test(start_paused = true)]
async fn a_full_backlog_t1_sends_its_oldest_to_the_backlog() {
    const T1_WAITING: u32 = 256;
    let mut h = Harness::start(config(false, true));
    h.discover(100_000, dht(), false).await;
    let first = h.next().await;
    let first = h.accept(first);
    for n in 1..=T1_WAITING + 1 {
        h.discover(n, dht(), true).await;
    }
    h.until("all wait", |s| {
        load(&s.hint_t1_waits) == u64::from(T1_WAITING + 1)
    })
    .await;
    assert_eq!(load(&h.stats().hint_t1_left[0]), 1);
    h.complete(first);
    let order = h.serve((T1_WAITING + 1) as usize).await;
    assert_eq!(order[0], key(T1_WAITING + 1));
    assert_eq!(order[4], key(1), "the oldest, from the backlog");
    assert_eq!(order.iter().filter(|k| **k == key(1)).count(), 1);
    assert_eq!(load(&h.stats().hint_lookups), u64::from(T1_WAITING));
    h.stop().await;
}
