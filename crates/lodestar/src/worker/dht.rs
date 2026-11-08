//! The worker's DHT (architecture §19.5; workers plan WK-10c): one node on
//! the shard its `Assign` names, finding keys for the claims and peers for
//! the fetch pool.
//!
//! - **A shard is fixed for a node's lifetime** (WK-4), so a new shard
//!   restarts the node, not the process: the old node is shut down (its
//!   state saved) and a new one bound on the same addresses. A new
//!   intensity is applied to the running node.
//! - **It runs only while the worker may work:** a session is up, its
//!   first `Assign` is in, the stages run and the assignment names a
//!   shard. A lost session pauses it at once: no samples, lookups or
//!   harvests (WK-5: the coordinator re-plans without this worker and
//!   widens the others' shares). The node is stopped if no session is
//!   back within [`LINGER`]; one that's back in time resumes, or restarts
//!   if its first `Assign` names another shard. A short drop so costs no
//!   restart, whose sampling suppression lasts six hours without the
//!   restart proof (`dht.safe_restart`).
//! - **The pool's peer requests** reach whichever node runs; with none,
//!   a request is dropped, which the pool reads as a refusal.
//! - A node that fails to bind is started again after a back-off (1 s
//!   doubling to 60 s), and one that stops by itself likewise: its step
//!   resets only once a node has run for a minute.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_cluster::assign::{Intensity, ShardSpec};
use lodestar_dht::node::{self as dht, Harvest, Node, Source, shard::Shard};
use lodestar_pipeline::strategy;
use lodestar_sources::{Found, PeerRequest};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

/// How long a node's shutdown (its state save) may take before it's let
/// go. Its actor still finishes (and saves) by itself, and keeps its port
/// until then: a new node binding the same `[dht].bind` fails meanwhile
/// and is retried after the back-off.
pub const STOP_DEADLINE: Duration = Duration::from_secs(10);
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How long a paused node waits for a session before it's stopped.
pub const LINGER: Duration = Duration::from_secs(60);
/// How often the running node's voted address is read, for `Hello`.
const ADDRESS_EVERY: Duration = Duration::from_secs(60);
/// Harvests waiting for the claims' queue; the node drops and counts the
/// rest, so the DHT never blocks.
const HARVEST_QUEUE: usize = 4096;

/// What the node should run with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Want {
    pub shard: ShardSpec,
    pub intensity: Intensity,
}

/// What the DHT should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Desired {
    /// No node.
    #[default]
    Stop,
    /// Keep a running node, paused, for up to [`LINGER`]; start none.
    Pause,
    Run(Want),
}

/// The DHT's counters and state, for the heartbeat and the tests.
#[derive(Debug, Default)]
pub struct DhtCounts {
    /// Nodes started, and starts that failed (a bind error).
    pub starts: AtomicU64,
    pub start_failures: AtomicU64,
    /// Nodes stopped for a new shard, a lost session or a drain.
    pub stops: AtomicU64,
    /// Pauses for a lost session, and resumes when one came back in time.
    pub pauses: AtomicU64,
    pub resumes: AtomicU64,
    /// New intensities applied to a running node (no restart).
    pub intensity_changes: AtomicU64,
    /// Keys the claims' queue had no room for (found again later).
    pub dropped: AtomicU64,
    /// Peer requests that arrived with no node running.
    pub refused: AtomicU64,
    /// This worker's public address as its DHT last voted it (from the
    /// saved state at start, then the running node), for `Hello`. Only a
    /// public address is kept.
    pub external_ip: std::sync::Mutex<Option<std::net::IpAddr>>,
}

impl DhtCounts {
    pub fn external_ip(&self) -> Option<std::net::IpAddr> {
        *self.external_ip.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn set_external_ip(&self, ip: Option<std::net::IpAddr>) {
        // A v4-mapped v6 address is the v4 one; a private or reserved
        // address says nothing to the planner. A later private or missing
        // vote keeps the last public address: the planner's grouping wants
        // the address peers saw, and a node with no vote yet has none.
        let ip = ip
            .map(|ip| ip.to_canonical())
            .filter(|ip| lodestar_network_policy::allows(std::net::SocketAddr::new(*ip, 1), false));
        if ip.is_some() {
            *self.external_ip.lock().unwrap_or_else(|e| e.into_inner()) = ip;
        }
    }
}

/// The running node's shard, `None` while none runs.
pub type Running = watch::Receiver<Option<ShardSpec>>;

/// Starts the DHT's task. It follows `want` until its sender is dropped,
/// then stops the node.
pub fn spawn(
    base: dht::Config,
    want: watch::Receiver<Desired>,
    finds: mpsc::Sender<Found>,
    requests: mpsc::Receiver<PeerRequest>,
    counts: Arc<DhtCounts>,
    linger: Duration,
) -> (tokio::task::JoinHandle<()>, Running) {
    let (running, rx) = watch::channel(None);
    // The address the last run voted, so the first `Hello` can carry it.
    if let Some(path) = &base.state_path
        && let Ok(Some(state)) = lodestar_dht::state::State::load(path)
    {
        counts.set_external_ip(state.external_ip);
    }
    let task = tokio::spawn(
        Driver {
            base,
            linger,
            finds,
            counts,
            running,
            node: None,
            failing: None,
        }
        .run(want, requests),
    );
    (task, rx)
}

struct Live {
    node: Node,
    want: Want,
    harvests: mpsc::Receiver<Harvest>,
    /// Paused for a lost session: stopped at this deadline.
    linger: Option<Instant>,
    started: Instant,
}

struct Driver {
    base: dht::Config,
    linger: Duration,
    finds: mpsc::Sender<Found>,
    counts: Arc<DhtCounts>,
    running: watch::Sender<Option<ShardSpec>>,
    node: Option<Live>,
    /// The error of the current streak of failed starts: warned once per
    /// streak, and again when the error changes; the rest are counted.
    /// Cleared by a start that works and by a target that isn't `Run`.
    failing: Option<std::io::ErrorKind>,
}

/// The back-off after a node stopped by itself: `prev` doubled while
/// each node `lived` less than a minute, up to a minute; the first step
/// again after one that ran longer (or the first time).
pub fn next_step(prev: Option<Duration>, lived: Duration) -> Duration {
    match prev {
        Some(step) if lived < RETRY_MAX => (step * 2).min(RETRY_MAX),
        _ => RETRY_FIRST,
    }
}

impl Driver {
    async fn run(
        mut self,
        mut want: watch::Receiver<Desired>,
        mut requests: mpsc::Receiver<PeerRequest>,
    ) {
        let mut requests_open = true;
        let mut retry: Option<(Instant, Duration)> = None;
        // The back-off step for a node that stops by itself.
        let mut crash: Option<Duration> = None;
        let mut address = tokio::time::interval(ADDRESS_EVERY);
        address.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let target = *want.borrow_and_update();
            let due = retry.is_none_or(|(at, _)| Instant::now() >= at);
            if due && let Err(()) = self.follow(target).await {
                let step = retry.map_or(RETRY_FIRST, |(_, s)| (s * 2).min(RETRY_MAX));
                retry = Some((Instant::now() + step, step));
            } else if self.node.is_some() || !matches!(target, Desired::Run(_)) {
                retry = None;
            }
            let linger = self.node.as_ref().and_then(|l| l.linger);
            let wake = retry.map(|(at, _)| at);
            tokio::select! {
                changed = want.changed() => if changed.is_err() { break },
                req = requests.recv(), if requests_open => match req {
                    Some(req) => self.forward(req),
                    None => requests_open = false,
                },
                h = harvest(&mut self.node) => match h {
                    Some(h) => self.found(h),
                    // The node stopped by itself: start it again, backing
                    // off while it keeps stopping within a minute.
                    None => {
                        let lived = self
                            .node
                            .as_ref()
                            .map_or(Duration::ZERO, |l| l.started.elapsed());
                        self.stop().await;
                        let step = next_step(crash, lived);
                        crash = Some(step);
                        retry = Some((Instant::now() + step, step));
                    }
                },
                () = sleep_until(wake) => {}
                _ = address.tick(), if self.node.is_some() => {
                    if let Some(live) = &self.node
                        && let Some(snap) = live.node.snapshot().await
                    {
                        self.counts.set_external_ip(snap.external_ip);
                    }
                }
                // No session came back in time.
                () = sleep_until(linger) => self.stop().await,
            }
        }
        self.stop().await;
    }

    /// Brings the node to `target`; `Err` when a start failed.
    async fn follow(&mut self, target: Desired) -> Result<(), ()> {
        // A streak of failed starts ends when nothing is to be started.
        if !matches!(target, Desired::Run(_)) {
            self.failing = None;
        }
        let t = match (&mut self.node, target) {
            (None, Desired::Stop | Desired::Pause) => return Ok(()),
            (Some(_), Desired::Stop) => {
                self.stop().await;
                return Ok(());
            }
            (Some(live), Desired::Pause) => {
                if live.linger.is_none() {
                    live.linger = Some(Instant::now() + self.linger);
                    self.counts.pauses.fetch_add(1, Ordering::Relaxed);
                    live.node.handle().set_paused(true).await;
                }
                return Ok(());
            }
            (Some(live), Desired::Run(t)) if live.want.shard == t.shard => {
                let handle = live.node.handle();
                if live.linger.take().is_some() {
                    self.counts.resumes.fetch_add(1, Ordering::Relaxed);
                    handle.set_paused(false).await;
                }
                if live.want.intensity != t.intensity {
                    live.want.intensity = t.intensity;
                    if handle.set_intensity(dht_intensity(t.intensity)).await {
                        self.counts
                            .intensity_changes
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                return Ok(());
            }
            (Some(_), Desired::Run(t)) => {
                self.stop().await;
                t
            }
            (None, Desired::Run(t)) => t,
        };
        // A decoded `ShardSpec` always makes a shard (WK-5 checks it).
        let shard = Shard::new(t.shard.prefixes(), t.shard.id_slots()).ok();
        let config = dht::Config {
            shard: shard.filter(|s| !s.is_full()),
            intensity: dht_intensity(t.intensity),
            ..self.base.clone()
        };
        let (tx, harvests) = mpsc::channel(HARVEST_QUEUE);
        match Node::start(config, Some(tx)).await {
            Ok(node) => {
                self.failing = None;
                self.counts.starts.fetch_add(1, Ordering::Relaxed);
                lodestar_log::log_event!(
                    crate::logging::events::WORKER_DHT_STARTED,
                    first = lodestar_log::Count(u64::from(t.shard.prefixes().start)),
                    end = lodestar_log::Count(u64::from(t.shard.prefixes().end)),
                    slots = lodestar_log::Count(u64::from(t.shard.id_slots().count_ones()))
                );
                self.node = Some(Live {
                    node,
                    want: t,
                    harvests,
                    linger: None,
                    started: Instant::now(),
                });
                self.running.send_replace(Some(t.shard));
                Ok(())
            }
            Err(e) => {
                self.counts.start_failures.fetch_add(1, Ordering::Relaxed);
                // Once per streak, and again if the error changes: a port
                // that stays taken would otherwise warn every minute for good.
                if self.failing.replace(e.kind()) != Some(e.kind()) {
                    lodestar_log::log_event!(
                        crate::logging::events::WORKER_DHT_START_FAILED,
                        error = lodestar_log::Error::new(&e)
                    );
                }
                Err(())
            }
        }
    }

    /// Stops the node, saving its state within [`STOP_DEADLINE`].
    async fn stop(&mut self) {
        let Some(live) = self.node.take() else {
            return;
        };
        // A save that fails or overruns loses only routing state: the next
        // node bootstraps again.
        let _ = tokio::time::timeout(STOP_DEADLINE, live.node.shutdown()).await;
        self.counts.stops.fetch_add(1, Ordering::Relaxed);
        // Said once it's saved, so a stop that waits for it waits for that.
        self.running.send_replace(None);
    }

    /// Hands a pool request to the node; dropped (a refusal) with none.
    fn forward(&self, req: PeerRequest) {
        let Some(live) = &self.node else {
            self.counts.refused.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let PeerRequest {
            key,
            admit,
            session,
            permit,
        } = req;
        // The worker's pool doesn't stream (`streaming_peers` off): a
        // session, if any, is refused and the request answered as before.
        if let Some(session) = session {
            let _ = session.send(None);
        }
        live.node.handle().request_peers_with(key, admit, permit);
    }

    fn found(&self, h: Harvest) {
        let strategy = match h.source {
            Source::Sample => strategy::SAMPLE,
            Source::GetPeers => strategy::GET_PEERS,
            Source::Announce => strategy::ANNOUNCE,
            // Answers to `Node::get_peers`, which the worker never asks;
            // the pool's own lookups answer through their admissions.
            Source::Lookup => return,
        };
        let found = Found::key(h.info_hash, strategy).with_peers(h.peers);
        if self.finds.try_send(found).is_err() {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Waits, up to [`STOP_DEADLINE`] and a second, for the DHT's task to
/// end, which it does once the runtime is gone, after stopping (and
/// saving) its node. For a process about to exit (`lodestar worker` after
/// its runtime is dropped). Waiting for the task, not for "no node", also
/// covers a drop while a node was starting.
pub async fn stopped(mut running: Running) {
    let _ = tokio::time::timeout(STOP_DEADLINE + Duration::from_secs(1), async {
        // `changed` fails once the task's sender is gone.
        while running.changed().await.is_ok() {}
    })
    .await;
}

async fn harvest(node: &mut Option<Live>) -> Option<Harvest> {
    match node {
        Some(live) => live.harvests.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

pub fn dht_intensity(i: Intensity) -> dht::Intensity {
    match i {
        Intensity::Low => dht::Intensity::Low,
        Intensity::Balanced => dht::Intensity::Balanced,
        Intensity::Aggressive => dht::Intensity::Aggressive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_metainfo::InfoHash;
    use tokio::sync::oneshot;

    /// With no node, a pool request is refused at once (its admission
    /// closes), never held; and a closed `want` ends the task.
    #[tokio::test]
    async fn a_request_with_no_node_is_refused() {
        let (want_tx, want) = watch::channel(Desired::Stop);
        let (finds, _rx) = mpsc::channel(4);
        let (requests, req_rx) = mpsc::channel(4);
        let counts = Arc::new(DhtCounts::default());
        let (task, running) = spawn(
            dht::Config::default(),
            want,
            finds,
            req_rx,
            counts.clone(),
            LINGER,
        );
        let (admit, admitted) = oneshot::channel();
        requests
            .send(PeerRequest {
                key: InfoHash([7; 20]),
                admit,
                session: None,
                permit: None,
            })
            .await
            .unwrap();
        assert!(admitted.await.is_err(), "refused: the admission closed");
        assert_eq!(counts.refused.load(Ordering::Relaxed), 1);
        assert!(running.borrow().is_none());
        drop(want_tx);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the task ends with its sender")
            .unwrap();
    }

    #[test]
    fn a_node_that_keeps_stopping_backs_off() {
        let short = Duration::from_secs(5);
        assert_eq!(next_step(None, short), RETRY_FIRST);
        let mut step = None;
        let mut seen = Vec::new();
        for _ in 0..8 {
            let s = next_step(step, short);
            seen.push(s.as_secs());
            step = Some(s);
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 32, 60, 60], "doubles to the cap");
        // One that ran a minute or more starts the back-off over.
        assert_eq!(next_step(Some(RETRY_MAX), RETRY_MAX), RETRY_FIRST);
    }

    #[test]
    fn a_v4_mapped_address_is_kept_as_v4_and_private_ones_are_not() {
        let c = DhtCounts::default();
        c.set_external_ip(Some("::ffff:81.2.69.7".parse().unwrap()));
        assert_eq!(c.external_ip(), Some("81.2.69.7".parse().unwrap()));
        // A private vote, or none, keeps the last public address.
        c.set_external_ip(Some("10.0.0.2".parse().unwrap()));
        c.set_external_ip(None);
        assert_eq!(c.external_ip(), Some("81.2.69.7".parse().unwrap()));
        for ip in ["100.64.1.1", "127.0.0.1", "::ffff:192.168.1.1"] {
            let fresh = DhtCounts::default();
            fresh.set_external_ip(Some(ip.parse().unwrap()));
            assert_eq!(fresh.external_ip(), None, "{ip}");
        }
    }

    /// A taken port: the start fails (counted, retried after the back-off)
    /// and the node starts once the port is free.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_bind_is_retried_until_it_works() {
        let held = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();
        let base = dht::Config {
            bind: vec![addr],
            bootstrap: Vec::new(),
            state_path: None,
            allow_private: true,
            ..dht::Config::default()
        };
        let shard = ShardSpec::new(0..128, 0xff).unwrap();
        let (_want_tx, want) = watch::channel(Desired::Run(Want {
            shard,
            intensity: Intensity::Balanced,
        }));
        let (finds, _rx) = mpsc::channel(4);
        let (_requests, req_rx) = mpsc::channel(4);
        let counts = Arc::new(DhtCounts::default());
        let (_task, mut running) = spawn(base, want, finds, req_rx, counts.clone(), LINGER);
        let deadline = Duration::from_secs(20);
        let start = Instant::now();
        while counts.start_failures.load(Ordering::Relaxed) == 0 {
            assert!(start.elapsed() < deadline, "no failed start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(running.borrow().is_none());
        drop(held);
        tokio::time::timeout(deadline, running.wait_for(|r| *r == Some(shard)))
            .await
            .expect("started once the port was free")
            .unwrap();
        assert!(counts.starts.load(Ordering::Relaxed) == 1);
    }

    #[test]
    fn intensities_map_one_to_one() {
        assert_eq!(dht_intensity(Intensity::Low), dht::Intensity::Low);
        assert_eq!(dht_intensity(Intensity::Balanced), dht::Intensity::Balanced);
        assert_eq!(
            dht_intensity(Intensity::Aggressive),
            dht::Intensity::Aggressive
        );
    }
}
