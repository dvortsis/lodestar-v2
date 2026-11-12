//! The chaos harness (WK-20, architecture §19.8): one coordinator and N
//! workers in one test process, with injectors for worker kills,
//! partitions, coordinator restarts and clock skew, a funnel reconciler
//! and a per-key fetch log. Test-only (`#[cfg(test)]` on its `mod` line).
//! WK-G's §19.8 gate tests drive it.
//!
//! **What runs is the real thing.** The coordinator is `serve`'s
//! [`Cluster`] on a real [`Pipeline`] and store (its lease authority,
//! intake, writer and tick), as `worker/runtime.rs`'s end-to-end test
//! builds it. Each worker is the real [`Runtime`] behind the real
//! [`Worker`] session loop, with its own data folder, joined through the
//! real join path (a join token, `lodestar_cluster::join`, the saved
//! credentials) and talking mutual TLS over TCP. Only two things are
//! fake: the peers a worker fetches from ([`Swarm`], which records every
//! fetch) and the discoveries, which a test hands a worker with
//! [`Harness::find`] (the DHT's [`Found`]s, as the DHT would).
//!
//! # Driving it
//! ```ignore
//! let mut h = Harness::start(Options::workers(3)).await;
//! let key = h.swarm.add("A torrent");
//! h.swarm.hold(0);               // worker 0's fetches wait
//! h.find(0, key);                // worker 0 sees it, claims, fetches
//! h.wait_fetched(0, &[key]).await;
//! h.kill(0).await;               // abrupt: no drain, no outcomes
//! h.advance(h.lease_ttl() + Duration::from_secs(1)); // leases expire
//! h.wait_leased(1).await;        // re-granted to a live worker
//! h.wait_stored_sighting(&[key], DEADLINE).await; // the others' sightings
//!                                // bring it peers; stored
//! h.wait_reconciled(|f| f.stored == 1).await;
//! assert_eq!(h.fetch_violations(), Vec::<String>::new());
//! h.restart(0).await;            // from its saved credentials and folder
//! h.shutdown().await;
//! ```
//! Workers are numbered from 0 in the order [`Options::workers`] made
//! them; [`Harness::add_worker`] joins one more. A failed wait prints
//! [`Harness::diagnosis`]: the funnel, the server's counters, the fetch
//! log, and for each worker its counters, what its runtime was handed
//! ([`Seen`]) and its status lines.
//!
//! # Time
//! One [`ManualClock`] (the *base*) moves only when a test says so
//! ([`Harness::advance`]); every node reads it through its own
//! [`NodeClock`], whose offset [`Harness::skew`] and
//! [`Harness::skew_coordinator`] set. The coordinator's leases, heartbeat
//! offsets, silent-session sweep and certificate checks, and each worker's
//! TLS and session upkeep, follow these clocks. The base starts at the
//! wall clock and only moves forward, so it is never behind it: the
//! pipeline also expires remote leases by the wall clock (its heartbeat
//! task), and a base behind the wall would let leases expire before the
//! test moved time at all. The converse limit: a test whose wall time
//! outruns its base time by a TTL (10 minutes) would see the same early
//! expiry, so keep a scenario well under that. A coordinator ahead of the
//! workers' wall-clock sealing day by less than an hour is fine: from
//! 23:00Z it pre-sends the next day's key.
//!
//! Two things in the stack still read the wall clock, so a test can't move
//! them: the worker runtime's sealing day and discovery times, and the
//! pipeline's epoch (its start second) and the leases it restores at a
//! start. [`Harness::restart_coordinator`] waits for the wall second to
//! change so each restart gets a new epoch.
//!
//! The server's heartbeat (5 s) and tick run on Tokio's real timers. A
//! jump past the silent window (3 heartbeats, 15 s) times out every
//! session at the next tick, and the workers reconnect; the re-grants of
//! expired leases go to the sessions that follow ([`Harness::wait_leased`]).
//!
//! # What a test must allow for
//! - **Same-IP politeness.** Every node here has the same (unknown) public
//!   address, so the planner divides each peer IP's connections between
//!   the coordinator and the workers (WK-5). Each find names [`PEERS`]
//!   peers, so every member has some it may dial.
//! - **A re-granted key comes with no peers**, and these workers run no
//!   DHT to look any up: the holder gets them as hints from the other
//!   workers' sightings ([`Harness::sight`], never from the holder itself).
//!   The pool's lookup wait and retry back-off are shortened
//!   ([`POOL_RETRY`]) so those peers are used at once.
//!
//! # Injectors
//! - [`kill`](Harness::kill): stops the worker's link both ways first, so
//!   nothing it says while dying gets out, then aborts its task (the
//!   runtime and session are dropped, never drained or shut down), then
//!   closes its connections. Its leases stay with the coordinator until
//!   they expire. It's a crash for the cluster, not for the disk: the
//!   runtime's `Drop` still runs (its DHT saves its state), so a torn or
//!   unflushed file needs a process kill (as `tests/worker_join.rs`
//!   starts the binary) or a crash hook in the runtime.
//!   [`restart`](Harness::restart) runs it again from its saved credentials
//!   and the same data folder (and so its spool, once WK-11 has one).
//! - [`partition`](Harness::partition) with a [`Cut`]: each worker dials
//!   the coordinator through its own TCP relay. A cut direction holds its
//!   bytes, end of stream included, as a real partition does under TCP's
//!   retransmissions; [`heal`](Harness::heal) lets them through in order.
//!   [`sever`](Harness::sever) closes the worker's connections instead
//!   (both ends see them drop).
//! - [`restart_coordinator`](Harness::restart_coordinator): shuts the
//!   cluster and the pipeline down cleanly and starts both again on the
//!   same store and data folder. The relays keep the workers' address and
//!   follow the coordinator's new port.
//! - [`skew`](Harness::skew), [`skew_coordinator`](Harness::skew_coordinator)
//!   and [`advance`](Harness::advance) (above).
//!
//! # Checks
//! - [`funnel`](Harness::funnel) reads the funnel from the coordinator's
//!   store (`cohort_hour`, and `rollup_hour`'s late results) and the lease
//!   authority's in-flight count; [`Funnel::reconciles`] is plan Q7's
//!   "harvested = Σ terminal − late results + in flight".
//!   [`wait_reconciled`](Harness::wait_reconciled) waits for it to hold
//!   together with a condition, since the writer flushes the counts on its
//!   own cadence.
//! - [`fetch_violations`](Harness::fetch_violations): from the fetch log,
//!   each key fetched by two nodes outside a lease expiry: a second node
//!   may fetch it only after a coordinator restart, a TTL of base time, or
//!   an early hand-back the test recorded ([`Swarm::handed_back`]). Each
//!   fetch also records the key's lease (holder and token), and two
//!   fetches under the same lease are a violation at any gap; the leases
//!   never excuse a fetch, so a coordinator bug can't hide behind them.
//!
//! Every wait is a condition with a deadline of at least 10 s
//! ([`wait_until`]); there are no fixed sleeps.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lodestar_cluster::{
    Clock, ClusterServer, Credentials, FleetSnapshot, JoinToken, Joining, LeaseAuthority,
    ManualClock, SystemClock, WorkerEvent, WorkerId,
};
use lodestar_metainfo::InfoHash;
use lodestar_peer_wire::{FailureKind, Fetched};
use lodestar_pipeline::remote::RemoteStats;
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_sources::{BoxFuture, Fetcher, Found, PoolConfig, Stage};
use lodestar_store::{Store, StoreConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::placement::Placement;
use super::serve::{Fleet, Wiring};
use super::{Cluster, ClusterHandle};
use crate::worker::Layout;
use crate::worker::runtime::{Config, Counts, Finds, Runtime};
use crate::worker::session::{Exit, Settings, Tcp, Work, Worker, sleep};

// The scenarios live under `tests/` but are compiled here: `lodestar` is a
// binary with no library target, so a test binary there couldn't link the
// runtime. That folder has no `main.rs` on purpose, so cargo doesn't build
// it as a test target of its own; don't add one.
#[path = "../../tests/cluster_chaos/scenarios.rs"]
mod scenarios;

/// The default deadline for a wait.
pub(crate) const DEADLINE: Duration = Duration::from_secs(30);

/// How many peers every find names ([`peer`]), so each of the planner's
/// per-peer-IP groups has some (members behind one public IP divide each
/// peer IP between them, WK-5).
pub(crate) const PEERS: u8 = 16;

/// The `n`th peer a find names. Public addresses, so the network policy
/// passes them as hints (the coordinator relays none from documentation
/// ranges); nothing ever dials them: every fetcher in the harness, the
/// coordinator's included, is the [`Swarm`], and no node runs a DHT.
pub(crate) fn peer(n: u8) -> SocketAddr {
    SocketAddr::from(([203, 0, 114, n], 6881))
}

/// How long a worker's pool waits for a lookup, and then to try a key
/// again (30 s and 120 s by default).
const POOL_RETRY: Duration = Duration::from_secs(1);

/// The coordinator's built-in worker in the fetch log.
pub(crate) const COORDINATOR: usize = usize::MAX;

/// The coordinator's sealing key (`seal.key`), the same across restarts.
const SEAL_KEY: [u8; 32] = [6; 32];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Polls `what` until it holds, failing with what it last saw once
/// `within` (at least 10 s, owner §2.24) has passed.
pub(crate) async fn wait_until<T: std::fmt::Debug>(
    name: &str,
    within: Duration,
    mut what: impl FnMut() -> (bool, T),
) -> T {
    assert!(
        within >= Duration::from_secs(10),
        "{name}: a deadline under 10 s fails on slow runners"
    );
    let start = Instant::now();
    loop {
        let (done, seen) = what();
        if done {
            return seen;
        }
        assert!(start.elapsed() < within, "{name}: still {seen:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A node's view of the shared base clock, `offset` off it.
#[derive(Debug)]
pub(crate) struct NodeClock {
    base: Arc<ManualClock>,
    offset_ms: AtomicI64,
}

impl NodeClock {
    fn new(base: &Arc<ManualClock>) -> Arc<NodeClock> {
        Arc::new(NodeClock {
            base: base.clone(),
            offset_ms: AtomicI64::new(0),
        })
    }

    pub(crate) fn offset_ms(&self) -> i64 {
        self.offset_ms.load(Ordering::SeqCst)
    }
}

impl Clock for NodeClock {
    fn now_ms(&self) -> i64 {
        self.base.now_ms().saturating_add(self.offset_ms())
    }
}

/// Which way a worker's link is cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// Nothing: the link is up.
    None,
    Both,
    /// What the worker sends is held; what the coordinator sends arrives.
    ToCoordinator,
    /// What the coordinator sends is held; what the worker sends arrives.
    ToWorker,
}

impl Cut {
    fn holds(self, to_coordinator: bool) -> bool {
        match self {
            Cut::None => false,
            Cut::Both => true,
            Cut::ToCoordinator => to_coordinator,
            Cut::ToWorker => !to_coordinator,
        }
    }
}

/// A worker's own TCP relay to the coordinator: a fixed address for the
/// worker, whichever port the coordinator listens on now, and the place a
/// partition happens.
pub(crate) struct Relay {
    addr: SocketAddr,
    target: watch::Sender<Option<SocketAddr>>,
    cut: watch::Sender<Cut>,
    /// Reads waiting for their direction to heal, now.
    held: Arc<AtomicUsize>,
    conns: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl Relay {
    pub(crate) async fn start(target: Option<SocketAddr>) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let target = watch::channel(target).0;
        let cut = watch::channel(Cut::None).0;
        let conns: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::default();
        let held = Arc::new(AtomicUsize::new(0));
        let accept = tokio::spawn({
            let (target, cut, conns) = (target.subscribe(), cut.clone(), conns.clone());
            let held = held.clone();
            async move {
                while let Ok((inbound, _)) = listener.accept().await {
                    let to = *target.borrow();
                    let task = tokio::spawn(relay(inbound, to, cut.subscribe(), held.clone()));
                    let mut conns = lock(&conns);
                    conns.retain(|t| !t.is_finished());
                    conns.push(task);
                }
            }
        });
        Relay {
            addr,
            target,
            cut,
            held,
            conns,
            accept,
        }
    }

    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Where new connections go (`None`: refused at once).
    pub(crate) fn retarget(&self, to: Option<SocketAddr>) {
        self.target.send_replace(to);
    }

    pub(crate) fn cut(&self, cut: Cut) {
        self.cut.send_replace(cut);
    }

    pub(crate) fn cut_now(&self) -> Cut {
        *self.cut.borrow()
    }

    /// Reads held by a cut now (each up to 16 KiB).
    pub(crate) fn held(&self) -> usize {
        self.held.load(Ordering::SeqCst)
    }

    /// Closes every connection through the relay, at both ends.
    pub(crate) fn sever(&self) {
        for t in lock(&self.conns).drain(..) {
            t.abort();
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.accept.abort();
        self.sever();
    }
}

async fn relay(
    inbound: TcpStream,
    to: Option<SocketAddr>,
    cut: watch::Receiver<Cut>,
    held: Arc<AtomicUsize>,
) {
    let Some(to) = to else { return };
    let Ok(outbound) = TcpStream::connect(to).await else {
        return;
    };
    let _ = (inbound.set_nodelay(true), outbound.set_nodelay(true));
    let (wr, ww) = inbound.into_split();
    let (cr, cw) = outbound.into_split();
    tokio::join!(
        pump(wr, cw, cut.clone(), true, held.clone()),
        pump(cr, ww, cut, false, held)
    );
}

/// Copies one direction, holding what it read (end of stream included)
/// while that direction is cut.
async fn pump(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    mut cut: watch::Receiver<Cut>,
    to_coordinator: bool,
    held: Arc<AtomicUsize>,
) {
    let mut buf = vec![0u8; 16 << 10];
    loop {
        let n = from.read(&mut buf).await.unwrap_or(0);
        if cut.borrow().holds(to_coordinator) {
            // Counted while held; the guard uncounts it on an abort too.
            let _holding = Holding::new(&held);
            if cut.wait_for(|c| !c.holds(to_coordinator)).await.is_err() {
                return;
            }
        }
        if n == 0 {
            let _ = to.shutdown().await;
            return;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

/// One read a cut holds, counted in [`Relay::held`] while it lives.
struct Holding<'a>(&'a AtomicUsize);

impl<'a> Holding<'a> {
    fn new(n: &'a AtomicUsize) -> Holding<'a> {
        n.fetch_add(1, Ordering::SeqCst);
        Holding(n)
    }
}

impl Drop for Holding<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One fetch a worker started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FetchRecord {
    pub key: InfoHash,
    /// The worker's number in the harness.
    pub node: usize,
    /// The peer it dialled.
    pub peer: SocketAddr,
    /// Coordinator restarts before it. Each one starts a new epoch: every
    /// token is new, and saved leases are restored to the built-in worker.
    pub generation: u32,
    /// Base-clock time (no node's skew).
    pub at_ms: i64,
    /// The key's lease as the coordinator's authority had it as the fetch
    /// started: holder and token (`None`: no lease).
    pub lease: Option<(WorkerId, u64)>,
    /// Its place in the log (marks are placed between fetches by it).
    pub seq: usize,
}

/// A node's leases handed back early (a drain or a revoke), before the
/// fetch at `seq` in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HandBack {
    pub node: usize,
    pub seq: usize,
}

/// The peers every worker fetches from: an info dict per key, and a log
/// of every fetch. A held worker's fetches wait until it's released (its
/// leases stay held meanwhile).
pub(crate) struct Swarm {
    base: Arc<ManualClock>,
    infos: Mutex<HashMap<InfoHash, Vec<u8>>>,
    log: Mutex<Vec<FetchRecord>>,
    held: watch::Sender<HashSet<usize>>,
    generation: AtomicU32,
    /// The running coordinator's lease authority, for each fetch's lease.
    authority: Mutex<Option<Arc<LeaseAuthority>>>,
    marks: Mutex<Vec<HandBack>>,
}

impl Swarm {
    fn new(base: Arc<ManualClock>) -> Arc<Swarm> {
        Arc::new(Swarm {
            base,
            infos: Mutex::default(),
            log: Mutex::default(),
            held: watch::channel(HashSet::new()).0,
            generation: AtomicU32::new(0),
            authority: Mutex::new(None),
            marks: Mutex::default(),
        })
    }

    /// A new single-file torrent named `name`, served by every peer; its
    /// key.
    pub(crate) fn add(&self, name: &str) -> InfoHash {
        let info = info_dict(name);
        let key = lodestar_metainfo::Info::parse(&info).unwrap().v1.unwrap();
        lock(&self.infos).insert(key, info);
        key
    }

    pub(crate) fn hold(&self, node: usize) {
        self.held.send_modify(|h| {
            h.insert(node);
        });
    }

    pub(crate) fn release(&self, node: usize) {
        self.held.send_modify(|h| {
            h.remove(&node);
        });
    }

    pub(crate) fn log(&self) -> Vec<FetchRecord> {
        lock(&self.log).clone()
    }

    /// Records that `node`'s leases were handed back early, by a drain or
    /// a revoke the test asked for: another node may then fetch its keys
    /// before a TTL has passed. Call it once the hand-back is done.
    pub(crate) fn handed_back(&self, node: usize) {
        let seq = lock(&self.log).len();
        lock(&self.marks).push(HandBack { node, seq });
    }

    pub(crate) fn hand_backs(&self) -> Vec<HandBack> {
        lock(&self.marks).clone()
    }

    /// The keys `node` has started fetching.
    pub(crate) fn fetched_by(&self, node: usize) -> HashSet<InfoHash> {
        lock(&self.log)
            .iter()
            .filter(|r| r.node == node)
            .map(|r| r.key)
            .collect()
    }
}

/// A bencoded single-file info dict.
fn info_dict(name: &str) -> Vec<u8> {
    let mut v = b"d6:lengthi1048576e4:name".to_vec();
    v.extend(format!("{}:{name}", name.len()).bytes());
    v.extend(b"12:piece lengthi262144e6:pieces20:");
    v.extend([0u8; 20]);
    v.push(b'e');
    v
}

/// One worker's fetcher: the swarm, as seen from that worker.
struct NodeFetch {
    node: usize,
    swarm: Arc<Swarm>,
}

impl Fetcher for NodeFetch {
    fn fetch(
        &self,
        peer: SocketAddr,
        key: InfoHash,
    ) -> BoxFuture<'static, Result<Fetched, FailureKind>> {
        let (node, swarm) = (self.node, self.swarm.clone());
        let lease = lock(&swarm.authority)
            .as_ref()
            .and_then(|a| a.get(&key))
            .map(|l| (l.holder, l.token.0));
        let mut log = lock(&swarm.log);
        let seq = log.len();
        log.push(FetchRecord {
            seq,
            lease,
            key,
            node,
            peer,
            generation: swarm.generation.load(Ordering::SeqCst),
            at_ms: swarm.base.now_ms(),
        });
        drop(log);
        let mut held = swarm.held.subscribe();
        Box::pin(async move {
            held.wait_for(|h| !h.contains(&node))
                .await
                .map_err(|_| FailureKind::Refused)?;
            let info = lock(&swarm.infos).get(&key).cloned();
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

/// What a worker saw, as its runtime was handed it: the session's events
/// by kind, the assignment it holds and whether its stages run. For a
/// test's waits and a failed wait's message.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seen {
    pub sessions: u32,
    pub connected: bool,
    pub events: std::collections::BTreeMap<&'static str, u64>,
    pub started: bool,
    pub assignment: Option<lodestar_cluster::assign::Assignment>,
    /// Connects, disconnects and the events that move leases (re-grants,
    /// hints, drains, accepted batches), in order: `regranted3@s2` is a
    /// re-grant of 3 keys on the worker's second session.
    pub trail: Vec<String>,
}

/// The real runtime, recording what it's handed into [`Seen`].
struct Probe {
    rt: Runtime,
    seen: Arc<Mutex<Seen>>,
}

impl Probe {
    fn note(&self, f: impl FnOnce(&mut Seen)) {
        let mut seen = lock(&self.seen);
        f(&mut seen);
        seen.started = self.rt.is_started();
        seen.assignment = self.rt.assignment();
    }
}

fn kind(e: &WorkerEvent) -> &'static str {
    match e {
        WorkerEvent::Assign(_) => "assign",
        WorkerEvent::Regranted(_) => "regranted",
        WorkerEvent::Drain => "drain",
        WorkerEvent::Hints(_) => "hints",
        WorkerEvent::Accepted { .. } => "accepted",
        WorkerEvent::Snapshot(_) => "snapshot",
        WorkerEvent::HaveSnapshot(_) => "have_snapshot",
        WorkerEvent::HaveDelta(_) => "have_delta",
        WorkerEvent::Reverify(_) => "reverify",
        WorkerEvent::ClockSkewed { .. } => "clock_skewed",
        WorkerEvent::Ended(_) => "ended",
    }
}

impl Work for Probe {
    fn capabilities(&self) -> lodestar_cluster::proto::Capabilities {
        self.rt.capabilities()
    }

    fn reconnects(&self) -> Option<watch::Receiver<u64>> {
        self.rt.reconnects()
    }

    fn connected(&mut self, session: lodestar_cluster::SessionHandle) {
        self.rt.connected(session);
        self.note(|s| {
            s.sessions += 1;
            s.connected = true;
            let t = format!("connected s{}", s.sessions);
            s.trail.push(t);
        });
    }

    fn disconnected(&mut self) {
        self.rt.disconnected();
        self.note(|s| {
            s.connected = false;
            s.trail.push("disconnected".into());
        });
    }

    fn event(&mut self, event: WorkerEvent) {
        let k = kind(&event);
        let n = match &event {
            WorkerEvent::Regranted(v) => v.len(),
            WorkerEvent::Hints(v) => v.len(),
            _ => 0,
        };
        self.rt.event(event);
        self.note(|s| {
            *s.events.entry(k).or_default() += 1;
            if !matches!(k, "assign" | "snapshot" | "have_snapshot" | "have_delta") {
                let t = format!("{k}{n}@s{}", s.sessions);
                s.trail.push(t);
            }
        });
    }

    async fn drain(&mut self) -> Vec<lodestar_cluster::proto::OutcomeItem> {
        self.rt.drain().await
    }
}

/// The funnel as the coordinator's store and lease authority have it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Funnel {
    pub harvested: i64,
    pub stored: i64,
    pub lost: i64,
    /// Late results committed (plan Q7): each also has an earlier
    /// terminal outcome.
    pub late: i64,
    /// Leases held or waiting for a re-grant now.
    pub in_flight: i64,
}

impl Funnel {
    /// Plan Q7's identity: harvested = Σ terminal − late results + in
    /// flight.
    pub(crate) fn reconciles(&self) -> bool {
        self.harvested == self.stored + self.lost - self.late + self.in_flight
    }
}

/// The coordinator: a pipeline and `serve`'s cluster port on it.
struct Coordinator {
    pipeline: Pipeline,
    cluster: Cluster,
    placement: Arc<Placement>,
}

impl Coordinator {
    /// Its built-in worker fetches from `swarm` too (keys restored at a
    /// restart are its own), never from the network.
    async fn start(
        dir: &std::path::Path,
        clock: Arc<dyn Clock>,
        max_skew_ms: i64,
        swarm: &Arc<Swarm>,
    ) -> Coordinator {
        let db = dir.join("lodestar.db");
        let store = Store::open(&db, StoreConfig::default()).unwrap();
        let config = PipelineConfig {
            remote_workers: true,
            flush_every: Duration::from_millis(100),
            batch_wait: Duration::from_millis(20),
            ..PipelineConfig::default()
        };
        let filter = config.filter.clone();
        let fetch = Arc::new(NodeFetch {
            node: COORDINATOR,
            swarm: swarm.clone(),
        });
        let pipeline = Pipeline::start_with(config, store, vec![], fetch)
            .await
            .unwrap();
        let monitor = pipeline.monitor();
        *lock(&swarm.authority) = Some(monitor.lease_authority().clone());
        let placement = Arc::new(Placement::new(false));
        let cluster = Cluster::start(Wiring {
            db,
            store: StoreConfig::default(),
            data_dir: dir.to_path_buf(),
            authority: monitor.lease_authority().clone(),
            remote: Some(monitor.remote().clone()),
            settings: crate::config::Cluster {
                enabled: true,
                listen: "127.0.0.1:0".parse().unwrap(),
                max_skew_ms,
                ..crate::config::Cluster::default()
            },
            clock,
            placement: placement.clone(),
            coordinator_dht: None,
            coordinator_share: None,
            held: None,
            fleet: Some(Fleet {
                stored: Arc::new(monitor),
                filter,
                seal_key: SEAL_KEY,
            }),
        })
        .await
        .unwrap();
        Coordinator {
            pipeline,
            cluster,
            placement,
        }
    }

    fn handle(&self) -> ClusterHandle {
        self.cluster.handle()
    }

    fn addr(&self) -> SocketAddr {
        self.handle().local_addr().expect("serving")
    }

    async fn stop(self) {
        self.cluster.shutdown().await;
        drop(self.pipeline.shutdown().await.unwrap());
    }
}

/// What a running worker shares with the test.
struct Running {
    task: JoinHandle<Exit>,
    /// Dropped: a local stop.
    _stop: oneshot::Sender<()>,
    finds: Finds,
    counts: Arc<Counts>,
    leases: Arc<lodestar_pipeline::stages::WorkerLeases>,
}

/// One worker: its folder, clock, link and, while it runs, its task.
pub(crate) struct Node {
    name: String,
    dir: tempfile::TempDir,
    clock: Arc<NodeClock>,
    relay: Relay,
    worker: WorkerId,
    lines: Arc<Mutex<Vec<String>>>,
    seen: Arc<Mutex<Seen>>,
    run: Option<Running>,
    starts: u32,
}

/// How many workers, and the coordinator's settings.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub workers: usize,
    /// `[cluster] max_skew_ms`.
    pub max_skew_ms: i64,
}

impl Options {
    pub(crate) fn workers(n: usize) -> Options {
        Options {
            workers: n,
            max_skew_ms: crate::config::Cluster::default().max_skew_ms,
        }
    }
}

/// A coordinator and its workers in this process (see the module docs).
pub(crate) struct Harness {
    base: Arc<ManualClock>,
    coord_clock: Arc<NodeClock>,
    coord_dir: tempfile::TempDir,
    coord: Option<Coordinator>,
    options: Options,
    nodes: Vec<Node>,
    pub swarm: Arc<Swarm>,
}

impl Harness {
    /// Starts the coordinator, then joins and starts each worker, and
    /// waits until every one is connected.
    pub(crate) async fn start(options: Options) -> Harness {
        let base = Arc::new(ManualClock::starting_now());
        let coord_clock = NodeClock::new(&base);
        let coord_dir = tempfile::tempdir().unwrap();
        let swarm = Swarm::new(base.clone());
        let coord = Coordinator::start(
            coord_dir.path(),
            coord_clock.clone(),
            options.max_skew_ms,
            &swarm,
        )
        .await;
        let mut h = Harness {
            swarm,
            base,
            coord_clock,
            coord_dir,
            coord: Some(coord),
            nodes: Vec::new(),
            options: options.clone(),
        };
        for _ in 0..options.workers {
            h.add_worker().await;
        }
        h.wait_all_connected().await;
        h
    }

    fn coord(&self) -> &Coordinator {
        self.coord.as_ref().expect("the coordinator runs")
    }

    fn node(&self, i: usize) -> &Node {
        &self.nodes[i]
    }

    /// The cluster server running now.
    pub(crate) fn server(&self) -> Arc<ClusterServer> {
        self.coord().handle().server().expect("serving")
    }

    pub(crate) fn fleet(&self) -> FleetSnapshot {
        self.server().snapshot()
    }

    /// The coordinator's intake counters (stale, late, refused results).
    pub(crate) fn remote_stats(&self) -> Arc<RemoteStats> {
        self.coord().pipeline.monitor().remote().stats.clone()
    }

    /// The coordinator's epoch: its pipeline's start second.
    pub(crate) fn epoch(&self) -> i64 {
        self.coord().pipeline.monitor().started_at()
    }

    /// How many times worker `i` was started, the first included.
    pub(crate) fn starts(&self, i: usize) -> u32 {
        self.node(i).starts
    }

    pub(crate) fn lease_ttl(&self) -> Duration {
        let secs = self.coord().pipeline.monitor().lease_authority().ttl_secs();
        Duration::from_secs(u64::try_from(secs).unwrap())
    }

    /// Joins a new worker through the real join path and starts it; its
    /// number.
    pub(crate) async fn add_worker(&mut self) -> usize {
        let i = self.nodes.len();
        let name = format!("w{i}");
        let dir = tempfile::tempdir().unwrap();
        let clock = NodeClock::new(&self.base);
        let relay = Relay::start(Some(self.coord().addr())).await;
        let token = self.server().create_join_token(&name).await.unwrap();
        let joining = Joining::new(JoinToken::parse(&token.expose()).unwrap()).unwrap();
        let creds =
            lodestar_cluster::join(&relay.addr().to_string(), joining, clock.clone(), DEADLINE)
                .await
                .unwrap();
        Layout::new(dir.path()).save(&creds).unwrap();
        self.nodes.push(Node {
            name,
            dir,
            clock,
            relay,
            worker: creds.worker(),
            lines: Arc::default(),
            seen: Arc::default(),
            run: None,
            starts: 0,
        });
        self.run(i, creds);
        i
    }

    /// Starts worker `i`'s runtime and session on `creds`.
    fn run(&mut self, i: usize, creds: Credentials) {
        let fetch = Arc::new(NodeFetch {
            node: i,
            swarm: self.swarm.clone(),
        });
        let node = &mut self.nodes[i];
        let base = Config::new(creds.worker(), fetch);
        let rt = Runtime::start(Config {
            allow_private: true,
            seed: Some(i as u64 + 1),
            drain_deadline: Duration::from_secs(2),
            // A key with no peers yet (a re-grant) ends its lookup and
            // tries again soon, so the peers a sighting brings are used
            // within a second or two, not after 30 s and 120 s: with no DHT
            // here a lookup has no peer finder to answer it.
            pool: PoolConfig {
                lookup_wait: POOL_RETRY,
                retry_backoff: POOL_RETRY,
                ..base.pool.clone()
            },
            ..base
        });
        let (finds, counts, leases) = (rt.finds(), rt.counts(), rt.leases());
        let clock: Arc<dyn Clock> = node.clock.clone();
        let lines = node.lines.clone();
        let worker = Worker {
            layout: Layout::new(node.dir.path()),
            creds,
            connect: Tcp {
                addr: node.relay.addr().to_string(),
                clock: clock.clone(),
                within: DEADLINE,
            },
            clock,
            settings: Settings {
                backoff_base: Duration::from_millis(10),
                backoff_cap: Duration::from_millis(200),
                connect_timeout: DEADLINE,
                ..Settings::default()
            },
            report: Arc::new(move |l: &str| lock(&lines).push(l.to_owned())),
            jitter: || 0,
            wait: sleep(),
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let probe = Probe {
            rt,
            seen: node.seen.clone(),
        };
        let task = tokio::spawn(worker.run(probe, async move {
            let _ = stopped.await;
        }));
        node.starts += 1;
        node.run = Some(Running {
            task,
            _stop: stop,
            finds,
            counts,
            leases,
        });
    }

    pub(crate) fn worker_id(&self, i: usize) -> WorkerId {
        self.node(i).worker
    }

    /// Whether worker `i` has a session now.
    pub(crate) fn connected(&self, i: usize) -> bool {
        let w = self.worker_id(i);
        self.fleet()
            .workers
            .iter()
            .any(|s| s.worker == w && s.connected)
    }

    /// Polls `done` until it holds; past `within` (at least 10 s), fails
    /// with [`diagnosis`](Self::diagnosis).
    pub(crate) async fn wait(&self, name: &str, within: Duration, mut done: impl FnMut() -> bool) {
        assert!(
            within >= Duration::from_secs(10),
            "{name}: a deadline under 10 s fails on slow runners"
        );
        let start = Instant::now();
        while !done() {
            assert!(
                start.elapsed() < within,
                "{name}: not after {within:?}\n{}",
                self.diagnosis()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub(crate) async fn wait_connected(&self, i: usize) {
        let name = format!("worker {i} connected");
        self.wait(&name, DEADLINE, || self.connected(i)).await;
    }

    pub(crate) async fn wait_all_connected(&self) {
        for i in 0..self.nodes.len() {
            if self.node(i).run.is_some() {
                self.wait_connected(i).await;
            }
        }
    }

    /// What worker `i`'s runtime has been handed since the harness
    /// started it (all its starts).
    pub(crate) fn seen(&self, i: usize) -> Seen {
        lock(&self.node(i).seen).clone()
    }

    /// Everything worker `i` reported, one line each.
    pub(crate) fn lines(&self, i: usize) -> String {
        lock(&self.node(i).lines).join("\n")
    }

    /// The running worker's counters (`None` while it's down).
    pub(crate) fn counts(&self, i: usize) -> Option<Arc<Counts>> {
        self.node(i).run.as_ref().map(|r| r.counts.clone())
    }

    /// The running worker's leases: held, and results out under them.
    pub(crate) fn leases(&self, i: usize) -> Option<Arc<lodestar_pipeline::stages::WorkerLeases>> {
        self.node(i).run.as_ref().map(|r| r.leases.clone())
    }

    /// Whether worker `i`'s task is still running (it hasn't exited).
    pub(crate) fn running(&self, i: usize) -> bool {
        self.node(i)
            .run
            .as_ref()
            .is_some_and(|r| !r.task.is_finished())
    }

    /// Worker `i` sees `key` with a peer, as its DHT would.
    pub(crate) fn find(&self, i: usize, key: InfoHash) {
        let run = self.node(i).run.as_ref().expect("the worker runs");
        let mut f = Found::key(key, 0);
        f.peers = (1..=PEERS).map(peer).collect();
        assert!(run.finds.found(f), "worker {i}'s find queue is full");
    }

    /// Waits until worker `i` has started fetching every one of `keys`.
    pub(crate) async fn wait_fetched(&self, i: usize, keys: &[InfoHash]) {
        let name = format!("worker {i} fetching");
        self.wait(&name, DEADLINE, || {
            let got = self.swarm.fetched_by(i);
            keys.iter().all(|k| got.contains(k))
        })
        .await;
    }

    /// Kills worker `i` abruptly (see the module docs).
    pub(crate) async fn kill(&mut self, i: usize) {
        let node = &mut self.nodes[i];
        let run = node.run.take().expect("the worker runs");
        node.relay.cut(Cut::Both);
        run.task.abort();
        let _ = run.task.await;
        node.relay.sever();
        node.relay.cut(Cut::None);
    }

    /// Starts worker `i` again from its saved credentials and folder.
    pub(crate) async fn restart(&mut self, i: usize) {
        assert!(self.node(i).run.is_none(), "kill it first");
        let creds = Layout::new(self.node(i).dir.path())
            .load()
            .unwrap()
            .expect("saved credentials");
        self.run(i, creds);
    }

    pub(crate) fn partition(&self, i: usize, cut: Cut) {
        self.node(i).relay.cut(cut);
    }

    pub(crate) fn heal(&self, i: usize) {
        self.node(i).relay.cut(Cut::None);
    }

    pub(crate) fn sever(&self, i: usize) {
        self.node(i).relay.sever();
    }

    /// Shuts the coordinator down cleanly and starts it again on the same
    /// store and folder, with a new epoch.
    pub(crate) async fn restart_coordinator(&mut self) {
        for n in &self.nodes {
            n.relay.retarget(None);
        }
        let started = self.coord().pipeline.monitor().started_at();
        self.coord.take().unwrap().stop().await;
        // The epoch is the pipeline's start second, by the wall clock.
        wait_until("a new epoch second", DEADLINE, || {
            let now = SystemClock.now_secs();
            (now > started, now)
        })
        .await;
        self.swarm.generation.fetch_add(1, Ordering::SeqCst);
        let coord = Coordinator::start(
            self.coord_dir.path(),
            self.coord_clock.clone(),
            self.options.max_skew_ms,
            &self.swarm,
        )
        .await;
        for n in &self.nodes {
            n.relay.retarget(Some(coord.addr()));
        }
        self.coord = Some(coord);
    }

    /// Sets worker `i`'s clock `offset_ms` off the base.
    pub(crate) fn skew(&self, i: usize, offset_ms: i64) {
        self.node(i)
            .clock
            .offset_ms
            .store(offset_ms, Ordering::SeqCst);
    }

    pub(crate) fn skew_coordinator(&self, offset_ms: i64) {
        self.coord_clock
            .offset_ms
            .store(offset_ms, Ordering::SeqCst);
    }

    /// Moves every node's clock forward by `by`.
    pub(crate) fn advance(&self, by: Duration) {
        self.base.advance(by);
    }

    fn reader(&self) -> Store {
        Store::open_reader(
            self.coord_dir.path().join("lodestar.db"),
            StoreConfig::default(),
        )
        .unwrap()
    }

    /// Whether every one of `keys` is stored.
    pub(crate) fn stored(&self, keys: &[InfoHash]) -> usize {
        let store = self.reader();
        keys.iter()
            .filter(|k| store.find(k).unwrap().is_some())
            .count()
    }

    pub(crate) async fn wait_stored(&self, keys: &[InfoHash]) {
        self.wait_stored_within(keys, DEADLINE, false).await;
    }

    /// [`wait_stored`](Self::wait_stored), sighting the keys not stored
    /// yet again every 2 s ([`sight`](Self::sight)), as a busy DHT would.
    pub(crate) async fn wait_stored_sighting(&self, keys: &[InfoHash], within: Duration) {
        self.wait_stored_within(keys, within, true).await;
    }

    /// Waits until `done` holds, sighting `keys` every 2 s meanwhile
    /// ([`sight`](Self::sight)): for example until a key's new holder has
    /// started fetching it.
    pub(crate) async fn wait_sighting(
        &self,
        name: &str,
        keys: &[InfoHash],
        within: Duration,
        mut done: impl FnMut() -> bool,
    ) {
        let mut last = None::<Instant>;
        self.wait(name, within, || {
            if done() {
                return true;
            }
            if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                self.sight(keys);
                last = Some(Instant::now());
            }
            false
        })
        .await;
    }

    async fn wait_stored_within(&self, keys: &[InfoHash], within: Duration, sighting: bool) {
        let mut last = None::<Instant>;
        self.wait("stored", within, || {
            let store = self.reader();
            let missing: Vec<InfoHash> = keys
                .iter()
                .copied()
                .filter(|k| store.find(k).unwrap().is_none())
                .collect();
            if sighting && last.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
                self.sight(&missing);
                last = Some(Instant::now());
            }
            missing.is_empty()
        })
        .await;
    }

    /// Each of `keys` is seen, with peers, by every running worker whose
    /// link isn't cut and that doesn't hold its lease now. The holder gets
    /// the sightings as peer hints (a re-granted key carries no peers, and
    /// these workers have no DHT to look any up); while the key waits for
    /// a re-grant, a claimer gets it with the peers itself.
    ///
    /// Not the holder itself (use [`sight_with`](Self::sight_with) to
    /// include it): its claim only gets its own lease back. That used to
    /// end the lease as lost once the key was fetched, until
    /// X-wk-own-lease-lost; `a_holder_sighting_its_own_keys_keeps_their_leases`
    /// pins the fix. And a sighting
    /// while the lease is still a dead holder's, before the expiry runs,
    /// sends its hints nowhere: sight after [`wait_leased`], or keep
    /// sighting with [`wait_stored_sighting`].
    ///
    /// [`wait_leased`]: Self::wait_leased
    /// [`wait_stored_sighting`]: Self::wait_stored_sighting
    pub(crate) fn sight(&self, keys: &[InfoHash]) {
        self.sight_with(keys, false);
    }

    /// [`sight`](Self::sight), the holder included when `holder` is set:
    /// for a test of the holder's own sighting (finding (a) in the
    /// decisions file).
    pub(crate) fn sight_with(&self, keys: &[InfoHash], holder: bool) {
        let authority = self.coord().pipeline.monitor().lease_authority().clone();
        for &key in keys {
            let held_by = authority.get(&key).map(|l| l.holder);
            for (i, n) in self.nodes.iter().enumerate() {
                let skip = !holder && held_by == Some(n.worker);
                if self.running(i) && n.relay.cut_now() == Cut::None && !skip {
                    self.find(i, key);
                }
            }
        }
    }

    /// Waits until `n` leases are held by workers that are running, not
    /// cut off and connected, none waits for a re-grant, and every such
    /// worker is connected: after an expiry, the re-grants have reached
    /// sessions that stay. A cut-off worker doesn't count even while the
    /// server still has it connected (until a tick sweeps it).
    pub(crate) async fn wait_leased(&self, n: usize) {
        self.wait("leased to reachable workers", DEADLINE, || {
            let f = self.fleet();
            let reachable: Vec<usize> = (0..self.nodes.len())
                .filter(|&i| self.running(i) && self.node(i).relay.cut_now() == Cut::None)
                .collect();
            let held: usize = f
                .workers
                .iter()
                .filter(|w| w.connected && reachable.iter().any(|&i| self.worker_id(i) == w.worker))
                .map(|w| w.leases)
                .sum();
            let up = reachable.iter().all(|&i| self.connected(i));
            held >= n && f.pending == 0 && up
        })
        .await;
    }

    /// The assignment the coordinator's planner holds for worker `i`.
    pub(crate) fn planned(&self, i: usize) -> Option<lodestar_cluster::assign::Assignment> {
        self.coord().placement.assignment(self.worker_id(i))
    }

    /// What a failed wait prints: the funnel, the fleet and each worker.
    pub(crate) fn diagnosis(&self) -> String {
        let f = self.fleet();
        let mut out = format!(
            "{:?}; leases {} pending {} regrants {} stale reports {} ticks {} \
             harvested {} hints relayed {} busy {} check failures {} refused {}",
            self.funnel(),
            f.leases,
            f.pending,
            f.regrants,
            f.stale_reports,
            f.ticks,
            f.harvested,
            f.hints_relayed,
            f.busy,
            f.check_failures,
            f.refused
        );
        let log: Vec<String> = self
            .swarm
            .log()
            .iter()
            .map(|r| {
                let who = if r.node == COORDINATOR {
                    "c".to_string()
                } else {
                    r.node.to_string()
                };
                format!(
                    "{:02x}:{who}/{}@{}",
                    r.key.0[0],
                    r.generation,
                    r.at_ms % 1_000_000
                )
            })
            .collect();
        out.push_str(&format!("\nfetches: {}", log.join(" ")));
        for (i, n) in self.nodes.iter().enumerate() {
            let state = f.workers.iter().find(|w| w.worker == n.worker);
            out.push_str(&format!(
                "\nworker {i} ({:?}, connected {:?}, leases {:?}): {}\n  seen {:?}\n  {}",
                n.worker,
                state.map(|w| w.connected),
                state.map(|w| w.leases),
                self.counts(i).map_or("down".into(), |c| fmt_counts(&c)),
                lock(&n.seen),
                self.lines(i).replace('\n', "\n  ")
            ));
        }
        out
    }

    pub(crate) fn funnel(&self) -> Funnel {
        let store = self.reader();
        let mut f = Funnel::default();
        for c in store.cohorts(i64::MIN, i64::MAX).unwrap() {
            let n = match c.stage {
                s if s == Stage::Harvested as u8 => &mut f.harvested,
                s if s == Stage::Stored as u8 => &mut f.stored,
                s if s == Stage::Lost as u8 => &mut f.lost,
                _ => continue,
            };
            *n += c.count;
        }
        let late = lodestar_pipeline::metric::LATE_RESULTS;
        f.late = store
            .rollup(late, i64::MIN, i64::MAX)
            .unwrap()
            .iter()
            .map(|c| c.value)
            .sum();
        // Two reads, not one snapshot: a key re-granted between them (from
        // waiting to held) is counted twice, and one expiring (from held to
        // waiting) not at all. Either way the count is off for one poll,
        // and a wait polls again.
        let authority = self.coord().pipeline.monitor().lease_authority().clone();
        let waiting = authority.pending();
        f.in_flight = i64::try_from(waiting + authority.len()).unwrap();
        f
    }

    /// Waits until the funnel reconciles and `also` holds.
    pub(crate) async fn wait_reconciled(&self, also: impl Fn(&Funnel) -> bool) -> Funnel {
        let mut last = Funnel::default();
        self.wait("the funnel reconciles", DEADLINE, || {
            last = self.funnel();
            last.reconciles() && also(&last)
        })
        .await;
        last
    }

    /// Each key fetched by two nodes with no hand-over of its lease
    /// between them ([`violations`]).
    pub(crate) fn fetch_violations(&self) -> Vec<String> {
        let ttl = i64::try_from(self.lease_ttl().as_millis()).unwrap();
        violations(&self.swarm.log(), &self.swarm.hand_backs(), ttl)
    }

    /// Stops every worker (a local stop) and the coordinator.
    pub(crate) async fn shutdown(mut self) {
        for n in &mut self.nodes {
            if let Some(run) = n.run.take() {
                drop(run._stop);
                let _ = tokio::time::timeout(DEADLINE, run.task).await;
            }
        }
        if let Some(c) = self.coord.take() {
            c.stop().await;
        }
    }
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness")
            .field(
                "workers",
                &self.nodes.iter().map(|n| &n.name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

fn fmt_counts(c: &Counts) -> String {
    let n = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    format!(
        "found {} skipped {} claimed {} granted {} held back {} busy retried {} \
         hints refused {} sent {} committed {} lost {} reconnects {}",
        n(&c.found),
        n(&c.skipped),
        n(&c.claimed),
        n(&c.granted),
        n(&c.held_back),
        n(&c.busy_retried),
        n(&c.hints_refused),
        n(&c.sent),
        n(&c.committed),
        n(&c.lost),
        n(&c.reconnects)
    )
}

/// Keys fetched by two different nodes outside a lease expiry. A second
/// node's fetch is allowed only after a hand-over the coordinator can
/// make: a coordinator restart (a new generation), a TTL of base time with
/// no fetch by the first node (an expiry; heartbeats renew a live
/// holder's lease, so the bound is the least an expiry needs), or an early
/// hand-back the test recorded ([`Swarm::handed_back`]) between the two.
/// Two fetches that saw the same lease are a violation whatever the gap:
/// nothing moved it. The leases recorded aren't trusted to allow a
/// fetch; a coordinator that re-granted a live key under a new token
/// would otherwise hide its own bug.
fn violations(log: &[FetchRecord], marks: &[HandBack], ttl_ms: i64) -> Vec<String> {
    let mut by_key: HashMap<InfoHash, Vec<FetchRecord>> = HashMap::new();
    for r in log {
        by_key.entry(r.key).or_default().push(*r);
    }
    let mut out = Vec::new();
    for (key, fetches) in by_key {
        // In the order they started (the log's order).
        for (j, b) in fetches.iter().enumerate() {
            let Some(a) = fetches[..j].iter().rev().find(|a| a.node != b.node) else {
                continue;
            };
            let same_lease = a.lease.is_some() && a.lease == b.lease;
            let handed_over = a.generation != b.generation
                || b.at_ms - a.at_ms >= ttl_ms
                || marks
                    .iter()
                    .any(|m| m.node == a.node && a.seq < m.seq && m.seq <= b.seq);
            if same_lease || !handed_over {
                out.push(format!(
                    "{key:?}: node {} ({:?}) and node {} ({:?}) at {} and {} ms",
                    a.node, a.lease, b.node, b.lease, a.at_ms, b.at_ms
                ));
            }
        }
    }
    out
}
