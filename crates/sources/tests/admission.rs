//! Coordinated admission (CR-ADM, DUR-04): the pool against a scripted
//! finder that accepts, refuses or holds each permitted request, in paused
//! time. Every `sleep` advances the virtual clock; waits poll a condition
//! with a 10 s (virtual) deadline.
//!
//! Every fixture checks conservation: permits granted = permits the
//! finder gave back + permits the pool gave back + permits in use.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::pool::spawn;
use lodestar_sources::{
    BoxFuture, Discovery, Fetcher, Found, FunnelCounts, Loss, PeerRequest, PoolConfig, PoolHandle,
    PoolIo, PoolStats, Shutdown,
};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

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

fn config(on: bool, capacity: usize) -> PoolConfig {
    PoolConfig {
        coordinated_admission: on,
        finder_capacity: capacity,
        lookup_wait: Duration::from_secs(600),
        retry_backoff: Duration::from_secs(60),
        max_attempts: 3,
        ..PoolConfig::default()
    }
}

/// A request the scripted finder accepted and holds, as the actor holds
/// a slot: its permit goes back when it completes or is dropped.
struct Held {
    key: InfoHash,
    done: oneshot::Sender<Vec<SocketAddr>>,
    permit: Option<OwnedSemaphorePermit>,
}

struct Harness {
    disc: mpsc::Sender<Discovery>,
    requests: mpsc::Receiver<PeerRequest>,
    handle: PoolHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<Vec<QueuedKey>>,
    /// Permits the scripted finder gave back.
    returned: u64,
}

impl Harness {
    fn start(config: PoolConfig) -> Harness {
        let (disc, discoveries) = mpsc::channel(64);
        let (finder, requests) = mpsc::channel(64);
        // Nothing has peers here, so nothing is ever fetched or sent out.
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

    async fn discover(&self, n: u32, interest: bool) {
        let mut found = Found::key(key(n), 0);
        found.interest = interest;
        self.disc
            .send(Discovery {
                source: SourceId::new(0).unwrap(),
                found,
                at: 1_700_000_000,
                reply: false,
            })
            .await
            .unwrap();
    }

    /// The next request the pool sends, within 10 s.
    async fn next(&mut self) -> PeerRequest {
        tokio::time::timeout(Duration::from_secs(10), self.requests.recv())
            .await
            .expect("a request within 10 s")
            .expect("the pool is running")
    }

    /// No request arrives for `d` of virtual time.
    async fn quiet_for(&mut self, d: Duration) {
        if let Ok(r) = tokio::time::timeout(d, self.requests.recv()).await {
            panic!("unexpected request for {:?}", r.map(|r| r.key));
        }
    }

    fn give_back(&mut self, permit: Option<OwnedSemaphorePermit>) {
        if permit.is_some() {
            self.returned += 1;
        }
    }

    fn accept(&mut self, r: PeerRequest) -> Held {
        let (done, rx) = oneshot::channel();
        r.admit.send(Some(rx)).unwrap();
        Held {
            key: r.key,
            done,
            permit: r.permit,
        }
    }

    /// Refused, as the actor refuses: the answer, then the permit.
    fn refuse(&mut self, r: PeerRequest) {
        r.admit.send(None).unwrap();
        self.give_back(r.permit);
    }

    fn complete(&mut self, h: Held, peers: Vec<SocketAddr>) {
        self.give_back(h.permit);
        let _ = h.done.send(peers);
    }

    /// Re-reports a key: the pool polls that key's admission first, so
    /// this waits until it has seen the finder accept.
    async fn fence(&self, n: u32) {
        let before = load(&self.stats().duplicates);
        self.discover(n, false).await;
        self.until("the repeat sighting", |s| load(&s.duplicates) > before)
            .await;
    }

    /// The pool's caller went: the finder notices and frees the slot.
    /// Within a minute of virtual time, so a `lookup_wait` can lapse.
    async fn reclaim(&mut self, mut h: Held) {
        tokio::time::timeout(Duration::from_secs(60), h.done.closed())
            .await
            .expect("the pool dropped the request");
        self.give_back(h.permit.take());
    }

    async fn until(&self, what: &str, done: impl Fn(&PoolStats) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(self.stats()) {
            assert!(Instant::now() < deadline, "timed out waiting: {what}");
            sleep(Duration::from_millis(1)).await;
        }
    }

    /// Permits granted = given back here + given back in the pool + in use.
    /// The in-use gauge refreshes on the pool's next turn, so each check
    /// gives the pool one (a command round trip).
    async fn conserved(&self) {
        let returned = self.returned;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.handle.queued().await;
            sleep(Duration::from_millis(1)).await;
            let s = self.stats();
            let granted: u64 = s.admission_grants.iter().flatten().map(load).sum();
            let pool: u64 = s.admission_permit_returns[..2].iter().map(load).sum();
            let in_use = load(&s.admission_permits_in_use);
            if granted == returned + pool + in_use {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "permits: granted {granted}, returned {returned} + pool {pool} + in use {in_use}"
            );
        }
    }

    async fn stop(self) -> (Vec<QueuedKey>, mpsc::Receiver<PeerRequest>) {
        self.stop.send(true).unwrap();
        (self.task.await.unwrap(), self.requests)
    }
}

/// After the pool stops, every request it sent is dead: whatever is still
/// queued for the finder has a closed acknowledgment, and then the channel
/// ends.
async fn drained(requests: &mut mpsc::Receiver<PeerRequest>) {
    while let Some(r) = requests.recv().await {
        assert!(r.admit.is_closed(), "a live request outlived the pool");
    }
}

fn load(a: &std::sync::atomic::AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

#[tokio::test(start_paused = true)]
async fn saturation_queues_instead_of_re_asking_and_recovers_at_once() {
    let mut h = Harness::start(config(true, 2));
    for n in 1..=5 {
        h.discover(n, false).await;
    }
    let a = h.next().await;
    let a = h.accept(a);
    let b = h.next().await;
    let b = h.accept(b);
    assert_eq!((a.key, b.key), (key(1), key(2)));
    // Saturated: the other three wait, and nothing re-asks the finder.
    h.quiet_for(Duration::from_secs(120)).await;
    h.until("three waiters", |s| load(&s.admission_waiters) == 3)
        .await;
    assert_eq!(load(&h.stats().admission_waits[0]), 3);
    assert_eq!(load(&h.stats().admission_refusals[0]), 0);
    assert_eq!(load(&h.stats().lookup_unaccepted), 0);
    assert_eq!(load(&h.stats().lookups), 2);
    h.conserved().await;

    // A slot frees: the oldest waiter is asked at once, with no back-off.
    let freed = Instant::now();
    h.complete(a, vec![]);
    let c = h.next().await;
    assert_eq!(c.key, key(3));
    assert!(Instant::now() - freed < Duration::from_millis(100));
    let c = h.accept(c);
    h.complete(b, vec![]);
    h.complete(c, vec![]);
    for n in [4, 5] {
        let r = h.next().await;
        assert_eq!(r.key, key(n));
        let r = h.accept(r);
        h.complete(r, vec![]);
    }
    h.until("drained", |s| {
        load(&s.lookups) == 0 && load(&s.admission_waiters) == 0
    })
    .await;
    assert_eq!(load(&h.stats().admission_grants[0][1]), 5);
    h.conserved().await;
    assert_eq!(load(&h.stats().admission_permits_in_use), 0);
    let (saved, mut requests) = h.stop().await;
    // Each used one attempt and backs off; none was lost.
    assert!(saved.iter().all(|q| q.attempts == 1));
    assert_eq!(saved.len(), 5);
    drained(&mut requests).await;
}

#[tokio::test(start_paused = true)]
async fn a_refusal_costs_no_attempt_or_back_off_and_holds_one_second() {
    let mut h = Harness::start(config(true, 4));
    h.discover(1, false).await;
    let r = h.next().await;
    let refused_at = Instant::now();
    h.refuse(r);
    // Asked again after the 1 s hold, not the 60 s back-off.
    let r = h.next().await;
    assert_eq!(r.key, key(1));
    let waited = Instant::now() - refused_at;
    assert!(
        waited >= Duration::from_secs(1) && waited < Duration::from_secs(2),
        "{waited:?}"
    );
    assert_eq!(load(&h.stats().admission_refusals[0]), 1);
    assert_eq!(load(&h.stats().admission_holds), 1);
    assert_eq!(load(&h.stats().admission_waits[2]), 1, "a requeue");
    assert_eq!(load(&h.stats().lookup_unaccepted), 1);
    assert_eq!(load(&h.stats().retry_unaccepted), 1);
    assert!(h.handle.snapshot().await.iter().all(|q| q.attempts == 0));
    // Accepted: the hold is gone, so a new key is asked at once.
    let held = h.accept(r);
    h.discover(2, false).await;
    let r = h.next().await;
    assert_eq!(r.key, key(2));
    let second = h.accept(r);
    h.complete(held, vec![]);
    h.complete(second, vec![]);
    h.until("drained", |s| load(&s.lookups) == 0).await;
    h.conserved().await;
    // An unacknowledged handoff is a requeue too.
    h.discover(3, false).await;
    let r = h.next().await;
    h.give_back(r.permit);
    drop(r.admit);
    h.until("unacknowledged", |s| load(&s.admission_refusals[1]) == 1)
        .await;
    let r = h.next().await;
    assert_eq!(r.key, key(3));
    h.refuse(r);
    h.conserved().await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn pause_and_resume_while_waiting_and_while_admitted() {
    let mut h = Harness::start(config(true, 1));
    h.discover(1, false).await;
    h.discover(2, false).await;
    let a = h.next().await;
    let a = h.accept(a);
    h.until("B waits", |s| load(&s.admission_waiters) == 1)
        .await;
    // The crawler's pause: the pool first, then the actor drops its queue.
    h.handle.set_paused(true).await;
    h.give_back(a.permit);
    drop(a.done);
    h.until("A requeued", |s| {
        load(&s.lookups) == 0 && load(&s.admission_waiters) == 2
    })
    .await;
    assert_eq!(load(&h.stats().admission_waits[2]), 1);
    h.quiet_for(Duration::from_secs(300)).await;
    assert!(h.handle.snapshot().await.iter().all(|q| q.attempts == 0));
    h.conserved().await;
    // Resumed: A first (it was admitted before), then B.
    h.handle.set_paused(false).await;
    let r = h.next().await;
    assert_eq!(r.key, key(1));
    assert_eq!(load(&h.stats().admission_grants[2][1]), 1);
    let a = h.accept(r);
    h.complete(a, vec![]);
    let r = h.next().await;
    assert_eq!(r.key, key(2));
    let b = h.accept(r);
    h.complete(b, vec![]);
    h.until("drained", |s| load(&s.lookups) == 0).await;
    assert_eq!(load(&h.stats().admission_refusals[0]), 0);
    h.conserved().await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn cancel_while_waiting_sends_nothing() {
    let mut h = Harness::start(PoolConfig {
        max_pending: 2,
        ..config(true, 1)
    });
    // A user-wanted key is never evicted, so the waiter is.
    h.discover(1, true).await;
    let a = h.next().await;
    let a = h.accept(a);
    h.discover(2, false).await;
    h.until("B waits", |s| load(&s.admission_waiters) == 1)
        .await;
    h.discover(3, false).await;
    h.until("B evicted", |s| load(&s.admission_wait_cancelled[0]) == 1)
        .await;
    assert_eq!(h.stats().lost_count(Loss::Evicted), 1);
    assert_eq!(load(&h.stats().evicted_service[0]), 1, "before any service");
    h.complete(a, vec![]);
    let r = h.next().await;
    assert_eq!(r.key, key(3), "B never reached the finder");
    h.refuse(r);
    h.conserved().await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn cancel_while_admitted_returns_the_permit_once() {
    let mut h = Harness::start(PoolConfig {
        lookup_wait: Duration::from_secs(30),
        ..config(true, 1)
    });
    h.discover(1, false).await;
    let a = h.next().await;
    let a = h.accept(a);
    h.discover(2, false).await;
    h.until("B waits", |s| load(&s.admission_waiters) == 1)
        .await;
    // A's lookup runs out of time and the pool drops it; A's slot frees
    // once the finder notices, and that return wakes the pool for B.
    h.reclaim(a).await;
    let r = h.next().await;
    assert_eq!(r.key, key(2));
    assert_eq!(load(&h.stats().admission_wakeups), 1);
    let b = h.accept(r);
    h.complete(b, vec![]);
    h.until("drained", |s| load(&s.lookups) == 0).await;
    h.conserved().await;
    assert_eq!(load(&h.stats().admission_permits_in_use), 0);
    // A used its attempt and backs off; it isn't asked again meanwhile.
    h.quiet_for(Duration::from_secs(30)).await;
    h.stop().await;
}

/// Eviction never takes a lookup that holds a permit: it would cancel
/// the work just granted. The waiter goes instead.
#[tokio::test(start_paused = true)]
async fn eviction_skips_permitted_lookups() {
    let mut h = Harness::start(PoolConfig {
        max_pending: 2,
        ..config(true, 1)
    });
    h.discover(1, false).await;
    let a = h.next().await;
    let a = h.accept(a);
    h.discover(2, false).await;
    h.discover(3, false).await;
    h.until("B evicted", |s| load(&s.admission_wait_cancelled[0]) == 1)
        .await;
    assert!(!a.done.is_closed(), "A keeps its slot");
    assert_eq!(load(&h.stats().evicted_service[0]), 1);
    h.complete(a, vec![]);
    let r = h.next().await;
    assert_eq!(r.key, key(3));
    let c = h.accept(r);
    h.complete(c, vec![]);
    h.until("drained", |s| load(&s.lookups) == 0).await;
    h.conserved().await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn shutdown_with_waiters_drains_and_saves_them_unattempted() {
    let mut h = Harness::start(config(true, 1));
    for n in 1..=3 {
        h.discover(n, false).await;
    }
    let a = h.next().await;
    let mut a = h.accept(a);
    h.until("two waiters", |s| load(&s.admission_waiters) == 2)
        .await;
    h.conserved().await;
    let (saved, mut requests) = h.stop().await;
    assert_eq!(saved.len(), 3);
    assert!(saved.iter().all(|q| q.attempts == 0));
    // The pool's side of the admitted request is gone, so the finder's
    // permit goes back; no request is left anywhere.
    tokio::time::timeout(Duration::from_secs(10), a.done.closed())
        .await
        .unwrap();
    drop(a.permit.take());
    drained(&mut requests).await;
}

#[tokio::test(start_paused = true)]
async fn interactive_keys_go_first_but_background_keeps_moving() {
    let mut h = Harness::start(config(true, 1));
    h.discover(100, false).await;
    let first = h.next().await;
    let first = h.accept(first);
    for n in 1..=6 {
        h.discover(n, true).await;
    }
    for n in 101..=102 {
        h.discover(n, false).await;
    }
    h.until("all wait", |s| load(&s.admission_waiters) == 8)
        .await;
    h.complete(first, vec![]);
    let mut order = Vec::new();
    for _ in 0..8 {
        let r = h.next().await;
        order.push(r.key);
        let r = h.accept(r);
        h.complete(r, vec![]);
    }
    let want: Vec<_> = [1, 2, 3, 4, 101, 5, 6, 102].map(key).into();
    assert_eq!(order, want);
    h.conserved().await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_closed_finder_is_never_woken_for_again() {
    let (disc, discoveries) = mpsc::channel(8);
    let (finder, requests) = mpsc::channel::<PeerRequest>(8);
    drop(requests);
    let (out, _results) = mpsc::channel(8);
    let (stop, shutdown) = Shutdown::new();
    let (handle, task) = spawn(
        config(true, 2),
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
    for n in 1..=3 {
        let found = Found::key(key(n), 0);
        disc.send(Discovery {
            source: SourceId::new(0).unwrap(),
            found,
            at: 1_700_000_000,
            reply: false,
        })
        .await
        .unwrap();
    }
    let s = handle.stats();
    let deadline = Instant::now() + Duration::from_secs(10);
    while load(&s.lookup_unaccepted) < 3 {
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(1)).await;
    }
    // One permit came back closed; then the ordinary no-finder path.
    assert_eq!(load(&s.admission_permit_returns[1]), 1);
    assert_eq!(load(&s.finder_closed), 3);
    assert_eq!(load(&s.admission_waiters), 0);
    assert_eq!(load(&s.admission_wakeups), 0, "never woken for");
    stop.send(true).unwrap();
    assert!(task.await.unwrap().iter().all(|q| q.attempts == 0));
}

/// The protocol replays ("Admission accounting model") with the switch on.
/// Off, they're the existing `tests/pool.rs` cases, which still pass.
#[tokio::test(start_paused = true)]
async fn replay_a_b_c_a_rejection_churn_is_bounded_without_back_off() {
    let mut h = Harness::start(PoolConfig {
        max_pending: 2,
        ..config(true, 1)
    });
    h.discover(1, false).await;
    let r = h.next().await;
    assert_eq!(r.key, key(1));
    h.refuse(r);
    h.until("A requeued", |s| load(&s.admission_refusals[0]) == 1)
        .await;
    // B and C arrive and wait behind A; C evicts A, A evicts B.
    for n in [2, 3, 1] {
        h.discover(n, false).await;
    }
    h.until("two evictions", |s| s.lost_count(Loss::Evicted) == 2)
        .await;
    assert_eq!(load(&h.stats().evicted_service[0]), 2, "both pre-service");
    assert_eq!(load(&h.stats().admission_wait_cancelled[0]), 2);
    // A refusing finder is asked at most once a second, by one waiter.
    let start = Instant::now();
    let mut asked = 0;
    while Instant::now() - start < Duration::from_secs(10) {
        let r = h.next().await;
        assert_eq!(r.key, key(3), "the front waiter, requeued");
        h.refuse(r);
        asked += 1;
    }
    assert!((9..=11).contains(&asked), "{asked}");
    assert!(h.handle.snapshot().await.iter().all(|q| q.attempts == 0));
    h.conserved().await;
    let (saved, mut requests) = h.stop().await;
    assert_eq!(saved.len(), 2);
    drained(&mut requests).await;
}

#[tokio::test(start_paused = true)]
async fn replay_retained_a_is_retried_after_the_hold_not_the_back_off() {
    let mut h = Harness::start(PoolConfig {
        max_pending: 2,
        ..config(true, 1)
    });
    h.discover(1, false).await;
    let r = h.next().await;
    h.refuse(r);
    let r = h.next().await;
    assert_eq!(r.key, key(1));
    h.refuse(r);
    assert_eq!(h.stats().lost_count(Loss::Evicted), 0);
    h.until("one lifetime retry", |s| load(&s.retry_unaccepted) == 1)
        .await;
    assert!(h.handle.snapshot().await.iter().all(|q| q.attempts == 0));
    h.conserved().await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn replay_accepted_and_held_a_under_arrivals() {
    // Off (`churn_evicts_an_accepted_lookup_and_cancels_its_ownership`),
    // C evicts accepted A. On, A keeps its permit and slot to completion;
    // B, still waiting, is evicted before any service instead.
    let mut h = Harness::start(PoolConfig {
        max_pending: 2,
        ..config(true, 1)
    });
    h.discover(1, false).await;
    let a = h.next().await;
    let a = h.accept(a);
    h.fence(1).await;
    h.discover(2, false).await;
    h.discover(3, false).await;
    h.until("B evicted", |s| s.lost_count(Loss::Evicted) == 1)
        .await;
    assert_eq!(load(&h.stats().evicted_service[0]), 1, "B, unserved");
    assert_eq!(load(&h.stats().evicted_service[1]), 0, "not A");
    h.complete(a, vec!["10.0.0.1:1".parse().unwrap()]);
    let r = h.next().await;
    assert_eq!(r.key, key(3));
    let c = h.accept(r);
    h.complete(c, vec![]);
    h.until("C done", |s| load(&s.lookups) == 0).await;
    assert_eq!(load(&h.stats().admission_refusals[0]), 0);
    assert_eq!(load(&h.stats().lookup_unaccepted), 0);
    h.conserved().await;
    let (saved, mut requests) = h.stop().await;
    assert_eq!(saved.len(), 2);
    drained(&mut requests).await;
}

/// With the switch off, a saturating finder sees today's behaviour: no
/// permits, no waits, and every new counter stays zero.
#[tokio::test(start_paused = true)]
async fn off_means_no_permits_and_no_new_counters() {
    let mut h = Harness::start(config(false, 1));
    for n in 1..=3 {
        h.discover(n, false).await;
    }
    for _ in 0..3 {
        let r = h.next().await;
        assert!(r.permit.is_none());
        h.refuse(r);
    }
    h.until("three refusals", |s| load(&s.lookup_unaccepted) == 3)
        .await;
    let s = h.stats();
    let new: u64 = s.admission_waits.iter().map(load).sum::<u64>()
        + s.admission_grants.iter().flatten().map(load).sum::<u64>()
        + s.admission_refusals.iter().map(load).sum::<u64>()
        + s.admission_permit_returns.iter().map(load).sum::<u64>()
        + load(&s.admission_wakeups)
        + load(&s.admission_holds)
        + load(&s.admission_waiters)
        + load(&s.admission_permits_in_use);
    assert_eq!(new, 0);
    h.stop().await;
}

/// The eviction exemption never pins an entry: whichever way a permitted
/// lookup ends (completion, timeout, the finder dropping it, the pause,
/// shutdown), its permit comes back and the entry is evictable again.
#[tokio::test(start_paused = true)]
async fn a_permitted_lookup_is_released_and_evictable_after_every_boundary() {
    for boundary in ["completion", "timeout", "dropped", "pause", "shutdown"] {
        let mut h = Harness::start(PoolConfig {
            max_pending: 2,
            lookup_wait: Duration::from_secs(30),
            ..config(true, 1)
        });
        h.discover(1, false).await;
        let a = h.next().await;
        let mut a = h.accept(a);
        h.fence(1).await;
        match boundary {
            "completion" => {
                h.complete(a, vec![]);
                h.until("A done", |s| load(&s.lookups) == 0).await;
            }
            "timeout" => {
                h.reclaim(a).await;
                h.until("A timed out", |s| load(&s.lookups) == 0).await;
            }
            "dropped" => {
                // The finder drops it unanswered: A is requeued with its
                // attempt back, granted again, and this time completes.
                h.give_back(a.permit.take());
                drop(a.done);
                let r = h.next().await;
                assert_eq!(r.key, key(1));
                let again = h.accept(r);
                h.complete(again, vec![]);
                h.until("A done", |s| load(&s.lookups) == 0).await;
            }
            "pause" => {
                h.handle.set_paused(true).await;
                h.give_back(a.permit.take());
                drop(a.done);
                h.until("A requeued", |s| load(&s.admission_waiters) == 1)
                    .await;
            }
            _ => {
                h.conserved().await;
                let (saved, mut requests) = h.stop().await;
                assert_eq!(saved.len(), 1, "{boundary}");
                tokio::time::timeout(Duration::from_secs(10), a.done.closed())
                    .await
                    .unwrap();
                drop(a.permit.take());
                drained(&mut requests).await;
                continue;
            }
        }
        h.conserved().await;
        assert_eq!(load(&h.stats().admission_permits_in_use), 0, "{boundary}");
        // A is the oldest and no longer holds a permit: it goes first.
        h.discover(2, false).await;
        h.discover(3, false).await;
        h.until("one eviction", |s| s.lost_count(Loss::Evicted) == 1)
            .await;
        let left: Vec<_> = h
            .handle
            .snapshot()
            .await
            .iter()
            .map(|q| q.info_hash)
            .collect();
        assert!(!left.contains(&key(1)), "{boundary}: A was pinned");
        h.stop().await;
    }
}

/// The exemption is bounded by the permits: with every entry holding one,
/// arrivals are refused entry instead of growing the pool, and the pool
/// never holds more than `max_pending` keys.
#[tokio::test(start_paused = true)]
async fn the_eviction_bound_holds_when_every_entry_holds_a_permit() {
    let mut h = Harness::start(PoolConfig {
        max_pending: 8,
        ..config(true, 32)
    });
    let mut held = Vec::new();
    for n in 1..=8 {
        h.discover(n, false).await;
        let r = h.next().await;
        held.push(h.accept(r));
    }
    h.until("eight admitted", |s| load(&s.admission_permits_in_use) == 8)
        .await;
    for n in 100..150 {
        h.discover(n, false).await;
        assert!(load(&h.stats().pending) <= 8);
    }
    h.until("arrivals turned away", |s| {
        s.lost_count(Loss::Evicted) == 50
    })
    .await;
    assert_eq!(load(&h.stats().pending), 8);
    assert_eq!(load(&h.stats().evicted_service[0]), 50, "none were served");
    assert!(
        held.iter().all(|a| !a.done.is_closed()),
        "none was cancelled"
    );
    assert!(load(&h.stats().admission_permits_in_use) <= 32);
    // Their lookups end, and arrivals can evict them again.
    for a in held {
        h.complete(a, vec![]);
    }
    h.until("all done", |s| load(&s.lookups) == 0).await;
    h.discover(200, false).await;
    h.until("an entry evicted", |s| s.lost_count(Loss::Evicted) == 51)
        .await;
    assert!(
        h.handle
            .snapshot()
            .await
            .iter()
            .any(|q| q.info_hash == key(200)),
        "the arrival got in"
    );
    assert_eq!(load(&h.stats().pending), 8);
    h.conserved().await;
    h.stop().await;
}

/// A refusal whose permit comes back while the pool is busy with something
/// else (here an arrival) is read before that permit is taken again: the
/// arrival waits, and the refused key is asked again after its hold.
#[tokio::test(start_paused = true)]
async fn a_refusal_is_read_before_its_permit_is_reused_on_any_path() {
    let mut h = Harness::start(config(true, 1));
    h.discover(1, false).await;
    let r = h.next().await;
    let refused_at = Instant::now();
    h.refuse(r);
    h.discover(2, false).await;
    let r = h.next().await;
    assert_eq!(r.key, key(1), "the refused key, requeued in front");
    assert!(Instant::now() - refused_at >= Duration::from_secs(1));
    assert_eq!(load(&h.stats().admission_holds), 1);
    let a = h.accept(r);
    h.complete(a, vec![]);
    let r = h.next().await;
    assert_eq!(r.key, key(2));
    h.refuse(r);
    h.conserved().await;
    h.stop().await;
}

/// Private keys never take permits, and a paused pool still serves them
/// after it resumes, as it does with the switch off.
#[tokio::test(start_paused = true)]
async fn private_keys_bypass_admission_and_survive_a_pause() {
    let mut h = Harness::start(config(true, 1));
    h.handle.set_paused(true).await;
    let mut found = Found::key(key(1), 0);
    found.private_source = true;
    h.disc
        .send(Discovery {
            source: SourceId::new(0).unwrap(),
            found,
            at: 1_700_000_000,
            reply: false,
        })
        .await
        .unwrap();
    h.until("queued", |s| load(&s.pending) == 1).await;
    h.handle.set_paused(false).await;
    // Asked of no finder: the no-finder path, then its back-off.
    h.until("the private key's lookup", |s| {
        load(&s.lookup_unaccepted) == 1
    })
    .await;
    h.until("nothing waiting", |s| load(&s.admission_waiters) == 0)
        .await;
    assert_eq!(load(&h.stats().admission_waits[0]), 0);
    let grants: u64 = h.stats().admission_grants.iter().flatten().map(load).sum();
    assert_eq!(grants, 0);
    h.quiet_for(Duration::from_secs(30)).await;
    h.stop().await;
}
