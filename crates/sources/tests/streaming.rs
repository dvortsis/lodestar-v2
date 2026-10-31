//! Streaming peers (CR-1a-S): the pool against a scripted streaming finder
//! and a scripted fetcher, in paused time. The cases are the streaming
//! rows of "Prepared regression cases" in
//! `docs/plan/2026-10-27-streaming-and-hints.md`. Waits poll a condition
//! with a 10 s (virtual) deadline; no sleep assumes work has finished.
//!
//! Every fixture ends with the final-drain row: no lookup owned, and every
//! session the finder handed out closed by the pool.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_sources::pool::spawn;
use lodestar_sources::{
    BoxFuture, Discovery, FetchedKey, Fetcher, Found, FunnelCounts, LookupSession, PeerRequest,
    PoolConfig, PoolHandle, PoolIo, PoolStats, Shutdown, Terminal,
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

fn peer(n: u8) -> SocketAddr {
    SocketAddr::from(([203, 0, 113, n], 6881))
}

/// Each peer's fetch: how long it takes and whether it verifies. A peer
/// with no script fails at once.
#[derive(Default)]
struct Script {
    plan: Mutex<HashMap<SocketAddr, (Duration, bool)>>,
    started: Mutex<Vec<(InfoHash, SocketAddr, Instant)>>,
}

struct ScriptFetcher(Arc<Script>);

impl Fetcher for ScriptFetcher {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let s = self.0.clone();
        Box::pin(async move {
            s.started.lock().unwrap().push((key, peer, Instant::now()));
            let (delay, ok) = s
                .plan
                .lock()
                .unwrap()
                .get(&peer)
                .copied()
                .unwrap_or((Duration::ZERO, false));
            sleep(delay).await;
            if ok {
                Ok(Fetched {
                    info: key.0.to_vec(),
                    v2: false,
                    encrypted: true,
                    pex: Vec::new(),
                    client: None,
                })
            } else {
                Err(FailureKind::Refused)
            }
        })
    }
}

fn config() -> PoolConfig {
    PoolConfig {
        streaming_peers: true,
        lookup_wait: Duration::from_secs(30),
        retry_backoff: Duration::from_secs(600),
        max_attempts: 3,
        ..PoolConfig::default()
    }
}

/// An accepted session as the finder holds it.
struct Held {
    batches: mpsc::Sender<Vec<SocketAddr>>,
    end: oneshot::Sender<Terminal>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl Held {
    fn batch(&self, peers: &[SocketAddr]) {
        self.batches.try_send(peers.to_vec()).expect("room");
    }

    fn end(self, t: Terminal) {
        drop(self.batches);
        let _ = self.end.send(t);
    }
}

struct Harness {
    disc: mpsc::Sender<Discovery>,
    finders: Vec<mpsc::Receiver<PeerRequest>>,
    out: mpsc::Receiver<FetchedKey>,
    handle: PoolHandle,
    stop: watch::Sender<bool>,
    task: JoinHandle<Vec<QueuedKey>>,
    script: Arc<Script>,
}

impl Harness {
    fn start(config: PoolConfig, finders: usize) -> Harness {
        let (disc, discoveries) = mpsc::channel(64);
        let (senders, receivers): (Vec<_>, Vec<_>) =
            (0..finders).map(|_| mpsc::channel(64)).unzip();
        let (out_tx, out) = mpsc::channel(64);
        let (stop, shutdown) = Shutdown::new();
        let script = Arc::new(Script::default());
        let (handle, task) = spawn(
            config,
            Arc::new(ScriptFetcher(script.clone())),
            PoolIo {
                discoveries,
                peer_finders: senders,
                out: out_tx,
                funnel: Arc::new(FunnelCounts::default()),
                shutdown,
            },
            HashMap::new(),
            vec![],
        );
        Harness {
            disc,
            finders: receivers,
            out,
            handle,
            stop,
            task,
            script,
        }
    }

    fn stats(&self) -> &PoolStats {
        self.handle.stats()
    }

    fn lookups(&self) -> u64 {
        self.stats().lookups.load(Ordering::Relaxed)
    }

    fn terminals(&self, t: Terminal) -> u64 {
        self.stats().stream_terminals[t as usize].load(Ordering::Relaxed)
    }

    fn plan(&self, p: SocketAddr, delay: Duration, ok: bool) {
        self.script.plan.lock().unwrap().insert(p, (delay, ok));
    }

    fn started(&self) -> Vec<SocketAddr> {
        self.script
            .started
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.1)
            .collect()
    }

    async fn discover(&self, n: u32) {
        self.disc
            .send(Discovery {
                source: SourceId::new(0).unwrap(),
                found: Found::key(key(n), 0),
                at: 1_700_000_000,
                reply: false,
            })
            .await
            .unwrap();
    }

    /// Finder `f`'s next request, within 10 s.
    async fn next(&mut self, f: usize) -> PeerRequest {
        tokio::time::timeout(Duration::from_secs(10), self.finders[f].recv())
            .await
            .expect("a request within 10 s")
            .expect("the pool is running")
    }

    async fn quiet_for(&mut self, f: usize, d: Duration) {
        if let Ok(r) = tokio::time::timeout(d, self.finders[f].recv()).await {
            panic!("unexpected request for {:?}", r.map(|r| r.key));
        }
    }

    async fn stop(self) -> Vec<QueuedKey> {
        self.stop.send(true).unwrap();
        self.task.await.unwrap()
    }
}

/// Accepts a streaming request with a two-batch session, as the DHT does.
fn accept(r: PeerRequest) -> Held {
    let (batches, rx) = mpsc::channel(2);
    let (end, terminal) = oneshot::channel();
    drop(r.admit);
    r.session
        .expect("a streaming request")
        .send(Some(LookupSession {
            batches: rx,
            terminal,
        }))
        .unwrap();
    Held {
        batches,
        end,
        _permit: r.permit,
    }
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn early_batch_starts_a_fetch_while_the_lookup_keeps_its_slot() {
    let mut h = Harness::start(config(), 1);
    h.plan(peer(1), Duration::from_secs(60), false);
    h.discover(1).await;
    let s = accept(h.next(0).await);
    s.batch(&[peer(1)]);
    until("the fetch started", || h.started() == [peer(1)]).await;
    assert_eq!(h.lookups(), 1, "the batch released no slot");
    assert_eq!(h.stats().stream_batches.load(Ordering::Relaxed), 1);
    assert_eq!(h.stats().stream_first_fetches.load(Ordering::Relaxed), 1);
    s.end(Terminal::Complete);
    until("the lookup released", || h.lookups() == 0).await;
    assert_eq!(h.terminals(Terminal::Complete), 1);
    let q = h.stop().await;
    assert_eq!(q[0].attempts, 1, "one logical attempt");
}

#[tokio::test(start_paused = true)]
async fn a_terminal_drains_the_batches_queued_before_it() {
    let mut h = Harness::start(config(), 1);
    h.discover(1).await;
    let s = accept(h.next(0).await);
    // Both queued and the terminal ready before the pool reads any.
    let first: Vec<_> = (1..=8).map(peer).collect();
    let second: Vec<_> = (9..=16).map(peer).collect();
    s.batch(&first);
    s.batch(&second);
    s.end(Terminal::Complete);
    until("all sixteen tried", || h.started().len() == 16).await;
    assert_eq!(h.stats().stream_batches.load(Ordering::Relaxed), 2);
    assert_eq!(h.lookups(), 0);
    assert_eq!(h.terminals(Terminal::Complete), 1);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn entry_cap_rejections_are_counted_apart_from_backpressure() {
    let mut h = Harness::start(
        PoolConfig {
            max_peers_per_key: 4,
            ..config()
        },
        1,
    );
    h.discover(1).await;
    let s = accept(h.next(0).await);
    s.batch(&(1..=6).map(peer).collect::<Vec<_>>());
    // A repeat of a retained or tried peer isn't a cap rejection.
    s.batch(&[peer(1), peer(7)]);
    s.end(Terminal::Complete);
    until("released", || h.lookups() == 0).await;
    assert_eq!(h.stats().stream_peers_capped.load(Ordering::Relaxed), 3);
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn success_racing_a_terminal_and_a_batch_closes_ownership_once() {
    // Every order of: the fetch verifying, a second batch, the terminal.
    let delays = [0u64, 500, 1000];
    for fetch in delays {
        for batch in delays {
            for terminal in delays {
                let mut h = Harness::start(config(), 1);
                h.plan(peer(1), Duration::from_millis(fetch), true);
                h.discover(1).await;
                let s = accept(h.next(0).await);
                s.batch(&[peer(1)]);
                until("fetch started", || !h.started().is_empty()).await;
                let start = Instant::now();
                let mut s = Some(s);
                for step in 0..=1000u64 {
                    tokio::time::sleep_until(start + Duration::from_millis(step)).await;
                    if step == batch
                        && let Some(s) = &s
                    {
                        let _ = s.batches.try_send(vec![peer(2)]);
                    }
                    if step == terminal
                        && let Some(s) = s.take()
                    {
                        s.end(Terminal::Complete);
                    }
                }
                let got = tokio::time::timeout(Duration::from_secs(10), h.out.recv())
                    .await
                    .expect("a result")
                    .expect("one");
                assert_eq!(got.key, key(1));
                until("ownership closed", || h.lookups() == 0).await;
                assert!(h.out.try_recv().is_err(), "exactly one result");
                assert!(h.terminals(Terminal::Complete) <= 1);
                assert!(h.stop().await.is_empty());
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_first_peer_waits_for_the_next_batch_without_a_second_lookup() {
    let mut h = Harness::start(config(), 1);
    h.plan(peer(2), Duration::from_millis(10), true);
    h.discover(1).await;
    let s = accept(h.next(0).await);
    s.batch(&[peer(1)]);
    until("the first peer failed", || h.started() == [peer(1)]).await;
    // Still owned, waiting: no second lookup is asked for.
    h.quiet_for(0, Duration::from_secs(5)).await;
    assert_eq!(h.lookups(), 1);
    s.batch(&[peer(2)]);
    let got = tokio::time::timeout(Duration::from_secs(10), h.out.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.key, key(1));
    assert_eq!(h.started(), [peer(1), peer(2)]);
    until("released", || h.lookups() == 0).await;
    assert!(s.batches.is_closed(), "success cancelled the session");
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_stale_session_cannot_touch_a_recreated_entry() {
    let mut h = Harness::start(
        PoolConfig {
            max_pending: 1,
            ..config()
        },
        1,
    );
    h.discover(1).await;
    let old = accept(h.next(0).await);
    // B evicts A; A comes back and evicts B.
    h.discover(2).await;
    let b = accept(h.next(0).await);
    h.discover(1).await;
    let new = accept(h.next(0).await);
    until("old sessions closed", || {
        old.batches.is_closed() && b.batches.is_closed()
    })
    .await;
    assert_eq!(h.lookups(), 1, "only the new lookup is owned");
    // The old session's late batch and terminal go nowhere.
    assert!(old.batches.try_send(vec![peer(9)]).is_err());
    old.end(Terminal::Complete);
    h.quiet_for(0, Duration::from_secs(1)).await;
    assert!(
        h.started().is_empty(),
        "no stale peer reached the new entry"
    );
    assert_eq!(h.terminals(Terminal::Complete), 0);
    new.end(Terminal::Complete);
    until("released", || h.lookups() == 0).await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn two_finders_are_one_attempt_under_one_deadline() {
    let mut h = Harness::start(
        PoolConfig {
            lookup_wait: Duration::from_secs(5),
            ..config()
        },
        2,
    );
    let sent = Instant::now();
    h.discover(1).await;
    let a = accept(h.next(0).await);
    let b = accept(h.next(1).await);
    // One fails; the other holds: the lookup stays one logical attempt.
    drop(a);
    until("the failure seen", || {
        h.terminals(Terminal::SourceFailed) == 1
    })
    .await;
    assert_eq!(h.lookups(), 1);
    // The shared deadline, from the send, cancels the one left.
    until("deadline", || b.batches.is_closed()).await;
    assert!(Instant::now().duration_since(sent) >= Duration::from_secs(5));
    assert_eq!(h.lookups(), 0);
    let q = h.stop().await;
    assert_eq!(q[0].attempts, 1);
}

#[tokio::test(start_paused = true)]
async fn a_dropped_terminal_is_a_source_failure_never_success() {
    let mut h = Harness::start(config(), 1);
    h.discover(1).await;
    let s = accept(h.next(0).await);
    s.batch(&[peer(1)]);
    // The finder goes: its queued batch still counts, its end is a failure.
    drop(s);
    until("the queued peer tried", || h.started() == [peer(1)]).await;
    until("released", || h.lookups() == 0).await;
    assert_eq!(h.terminals(Terminal::SourceFailed), 1);
    assert_eq!(h.terminals(Terminal::Complete), 0);

    // With nothing delivered it's work dropped unanswered (a pause):
    // no attempt is used and it's asked again at once.
    h.discover(2).await;
    let s = accept(h.next(0).await);
    drop(s);
    let again = h.next(0).await;
    assert_eq!(again.key, key(2));
    drop(again);
    until("released", || h.lookups() == 0).await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_finder_that_does_not_stream_still_answers() {
    let mut h = Harness::start(config(), 1);
    h.plan(peer(1), Duration::ZERO, true);
    h.discover(1).await;
    let r = h.next(0).await;
    let (done, rx) = oneshot::channel();
    r.admit.send(Some(rx)).unwrap();
    done.send(vec![peer(1)]).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), h.out.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.key, key(1));
    until("released", || h.lookups() == 0).await;
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn off_sends_no_session_and_counts_nothing() {
    let mut h = Harness::start(
        PoolConfig {
            streaming_peers: false,
            ..config()
        },
        1,
    );
    h.discover(1).await;
    let r = h.next(0).await;
    assert!(r.session.is_none());
    drop(r);
    until("released", || h.lookups() == 0).await;
    let s = h.stats();
    assert_eq!(s.stream_batches.load(Ordering::Relaxed), 0);
    assert!(
        s.stream_terminals
            .iter()
            .all(|t| t.load(Ordering::Relaxed) == 0)
    );
    h.stop().await;
}

#[tokio::test(start_paused = true)]
async fn coordinated_admission_keeps_one_permit_per_streaming_request() {
    let mut h = Harness::start(
        PoolConfig {
            coordinated_admission: true,
            finder_capacity: 1,
            ..config()
        },
        1,
    );
    h.plan(peer(1), Duration::from_secs(60), false);
    h.discover(1).await;
    h.discover(2).await;
    let s = accept(h.next(0).await);
    s.batch(&[peer(1)]);
    until("fetching", || !h.started().is_empty()).await;
    // The permit stays with the lookup, not the fetch: key 2 waits.
    h.quiet_for(0, Duration::from_secs(2)).await;
    assert_eq!(
        h.stats().admission_permits_in_use.load(Ordering::Relaxed),
        1
    );
    // The finder gives the slot back as it ends the session.
    s.end(Terminal::Complete);
    let next = h.next(0).await;
    assert_eq!(next.key, key(2));
    drop(next);
    until("released", || h.lookups() == 0).await;
    h.stop().await;
}
