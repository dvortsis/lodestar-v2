//! Runs one scenario: the real DHT node actor and the real fetch pool,
//! wired the way `crates/pipeline/src/dht.rs` wires them, against the
//! scripted network and peers, in tokio's paused clock.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::testing::{self, SimPort};
use lodestar_dht::node::{Config, Harvest, Node, NodeHandle, Source, StatsSnapshot};
use lodestar_peer_wire::FailureKind;
use lodestar_sources::pool::spawn;
use lodestar_sources::{
    Discovery, FetchedKey, Found, FunnelCounts, Loss, PeerRequest, PoolConfig, PoolHandle, PoolIo,
    PoolStats, Shutdown, Stage,
};
use lodestar_store::{QueuedKey, SourceId};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::fetch::{SimFetcher, hex};
use crate::log::Log;
use crate::net::{Net, NetStats};
use crate::rng::{Rng, hash};
use crate::scenario::{FetchConfig, Scenario};

/// The node's own (simulated) public address.
pub const ME: ([u8; 4], u16) = ([81, 2, 3, 4], 6881);
/// Unix time at virtual zero, for discovery stamps.
pub const T0: i64 = 1_700_000_000;
/// Gauges are sampled once per virtual second.
const SAMPLE: Duration = Duration::from_secs(1);

/// What a run measured. `text` is the golden.
#[derive(Debug)]
pub struct Report {
    pub text: String,
    /// The full outcome log, when `CRAWLER_SIM_LOG` asked for it.
    pub log: Option<String>,
    pub dht: StatsSnapshot,
    pub net: NetStats,
    /// Metadata stored by the end of the run.
    pub stored: u64,
    /// Metadata stored per minute after warmup: what screening compares.
    pub steady_stored: f64,
    pub pool: PoolEnd,
}

/// Pool counters at the measurement boundary, for screening reports.
#[derive(Debug, Default, Clone, Copy)]
pub struct PoolEnd {
    pub lookup_unaccepted: u64,
    /// Furthest service before eviction: unaccepted, lookup, peers, fetch.
    pub evicted_service: [u64; 4],
    /// Coordinated admission (CR-ADM): waits, grants, refusals, holds.
    pub admission: [u64; 4],
}

#[derive(Default, Clone, Copy)]
pub struct Gauge {
    sum: u64,
    max: u64,
}

impl Gauge {
    fn add(&mut self, v: u64) {
        self.sum += v;
        self.max = self.max.max(v);
    }
}

/// One simulated minute: counter deltas and gauge means/maxima.
#[derive(Default)]
pub struct Minute {
    pub admitted: u64,
    pub rejected: u64,
    pub completed: u64,
    pub peers_found: u64,
    pub fetch_started: u64,
    pub fetched: u64,
    pub stored: u64,
    pub harvested: u64,
    pub dht_active: Gauge,
    pub dht_queued: Gauge,
    pub pool_lookups: Gauge,
    pub pool_active: Gauge,
}

/// Cumulative counters the per-minute rows are deltas of.
#[derive(Default, Clone, Copy)]
pub struct Totals {
    admitted: u64,
    rejected: u64,
    completed: u64,
    peers_found: u64,
    fetch_started: u64,
    fetched: u64,
    stored: u64,
    harvested: u64,
}

/// `crates/pipeline/src/dht.rs`'s strategy codes.
fn strategy(source: Source) -> u8 {
    match source {
        Source::Sample => 0,
        Source::GetPeers => 1,
        Source::Announce => 2,
        Source::Lookup => 3,
    }
}

/// The DHT plugin's loop (`DhtSource::run`), minus its stats mirror: owned
/// peer requests go to the node; lookup harvests answer as replies and the
/// rest are discoveries. Discovery times are virtual.
async fn glue(
    node: lodestar_dht::node::NodeHandle,
    mut requests: mpsc::Receiver<PeerRequest>,
    mut harvests: mpsc::Receiver<Harvest>,
    discoveries: mpsc::Sender<Discovery>,
    log: Arc<Mutex<Log>>,
    start: Instant,
    mut stop: watch::Receiver<bool>,
) -> u64 {
    let source = SourceId::new(1).expect("valid source id");
    let mut dropped = 0;
    let mut requests_open = true;
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            req = requests.recv(), if requests_open => match req {
                Some(r) => {
                    node.request_peers_with(r.key, r.admit, r.permit);
                }
                None => requests_open = false,
            },
            Some(h) = harvests.recv() => {
                log.lock().expect("log lock").event(format_args!(
                    "harvest {} {:?} peers={}",
                    hex(&h.info_hash),
                    h.source,
                    h.peers.len()
                ));
                let at = T0 + Instant::now().duration_since(start).as_secs() as i64;
                let d = Discovery {
                    source,
                    reply: h.source == Source::Lookup,
                    found: Found::key(h.info_hash, strategy(h.source)).with_peers(h.peers),
                    at,
                };
                if discoveries.try_send(d).is_err() {
                    dropped += 1;
                }
            }
        }
    }
    dropped
}

/// The writer: counts what reaches storage.
async fn writer(mut out: mpsc::Receiver<FetchedKey>, log: Arc<Mutex<Log>>, stored: Arc<AtomicU64>) {
    while let Some(f) = out.recv().await {
        stored.fetch_add(1, Ordering::Relaxed);
        log.lock()
            .expect("log lock")
            .event(format_args!("stored {}", hex(&f.key)));
    }
}

/// One run of the node and the pipeline around it, wired the way
/// `crates/pipeline/src/dht.rs` wires them. A restart scenario runs two.
pub struct Leg {
    pub node: Node,
    pub handle: NodeHandle,
    pub pool: PoolHandle,
    pub pool_task: JoinHandle<Vec<QueuedKey>>,
    pub glue_task: JoinHandle<u64>,
    pub writer_task: JoinHandle<()>,
    pub net_task: JoinHandle<(Net, SimPort)>,
    pub stop_tx: watch::Sender<bool>,
    pub pool_stop: watch::Sender<bool>,
    pub funnel: Arc<FunnelCounts>,
    pub fetcher: Arc<SimFetcher>,
    pub stored: Arc<AtomicU64>,
}

/// What a stopped leg leaves.
pub struct Stopped {
    /// The node's stats after shutdown.
    pub dht: StatsSnapshot,
    pub queued: Vec<QueuedKey>,
    pub dropped: u64,
    pub net: Net,
    pub port: SimPort,
    pub fetched: u64,
    pub funnel_fetched: i64,
    pub stored: u64,
}

#[derive(Clone, Copy)]
pub struct Seeds {
    pub node: u64,
    pub fetch: u64,
}

impl Leg {
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        dht: Config,
        pool_cfg: PoolConfig,
        fetch: FetchConfig,
        net: Net,
        seeds: Seeds,
        log: &Arc<Mutex<Log>>,
        start: Instant,
        restore: Vec<QueuedKey>,
    ) -> Leg {
        let (harvest_tx, harvest_rx) = mpsc::channel(4096);
        let (node, port) = testing::start(dht, SocketAddr::from(ME), seeds.node, Some(harvest_tx));
        let (stop_tx, stop_rx) = watch::channel(false);
        let net_task = tokio::spawn(net.run(port, stop_rx.clone()));

        // The pipeline's channel sizes (`crates/pipeline/src/lib.rs`).
        let (disc_tx, disc_rx) = mpsc::channel(16_384);
        let (finder_tx, finder_rx) = mpsc::channel(1024);
        let (out_tx, out_rx) = mpsc::channel(64);
        let funnel = Arc::new(FunnelCounts::default());
        let fetcher = Arc::new(SimFetcher {
            seed: seeds.fetch,
            cfg: fetch,
            log: log.clone(),
            started: Default::default(),
        });
        let (pool_stop, pool_shutdown) = Shutdown::new();
        let (pool, pool_task) = spawn(
            pool_cfg,
            fetcher.clone(),
            PoolIo {
                discoveries: disc_rx,
                peer_finders: vec![finder_tx],
                out: out_tx,
                funnel: funnel.clone(),
                shutdown: pool_shutdown,
            },
            HashMap::new(),
            restore,
        );
        let glue_task = tokio::spawn(glue(
            node.handle(),
            finder_rx,
            harvest_rx,
            disc_tx,
            log.clone(),
            start,
            stop_rx,
        ));
        let stored = Arc::new(AtomicU64::new(0));
        let writer_task = tokio::spawn(writer(out_rx, log.clone(), stored.clone()));
        Leg {
            handle: node.handle(),
            node,
            pool,
            pool_task,
            glue_task,
            writer_task,
            net_task,
            stop_tx,
            pool_stop,
            funnel,
            fetcher,
            stored,
        }
    }

    pub fn totals(&self, d: &StatsSnapshot, p: &PoolStats) -> Totals {
        Totals {
            admitted: d.peer_admitted,
            rejected: d.peer_rejected,
            completed: d.lookups_completed,
            peers_found: self.funnel.total(Stage::PeerFound, None) as u64,
            fetch_started: self.fetcher.started.load(Ordering::Relaxed),
            fetched: p.fetched.load(Ordering::Relaxed),
            stored: self.stored.load(Ordering::Relaxed),
            harvested: d.harvested_sample + d.harvested_get_peers + d.harvested_announce,
        }
    }

    /// Runs `minutes` from `from`, sampling the gauges each second.
    pub async fn minutes(&self, from: Instant, minutes: u32) -> Vec<Minute> {
        let mut out = Vec::new();
        let mut minute = Minute::default();
        let mut last = Totals::default();
        let seconds = u64::from(minutes) * 60;
        for sec in 1..=seconds {
            sleep_until(from + SAMPLE * sec as u32).await;
            let d = self.node.stats();
            let p = self.pool.stats();
            minute.dht_active.add(d.peer_active);
            minute.dht_queued.add(d.peer_queued);
            minute.pool_lookups.add(p.lookups.load(Ordering::Relaxed));
            minute.pool_active.add(p.active.load(Ordering::Relaxed));
            if sec % 60 == 0 {
                let t = self.totals(&d, p);
                minute.admitted = t.admitted - last.admitted;
                minute.rejected = t.rejected - last.rejected;
                minute.completed = t.completed - last.completed;
                minute.peers_found = t.peers_found - last.peers_found;
                minute.fetch_started = t.fetch_started - last.fetch_started;
                minute.fetched = t.fetched - last.fetched;
                minute.stored = t.stored - last.stored;
                minute.harvested = t.harvested - last.harvested;
                last = t;
                out.push(std::mem::take(&mut minute));
            }
        }
        out
    }

    /// Pool counters at the measurement boundary.
    pub fn pool_end(&self) -> PoolEnd {
        let p = self.pool.stats();
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let sum = |a: &[AtomicU64]| a.iter().map(l).sum::<u64>();
        PoolEnd {
            lookup_unaccepted: l(&p.lookup_unaccepted),
            evicted_service: std::array::from_fn(|i| l(&p.evicted_service[i])),
            admission: [
                sum(&p.admission_waits),
                p.admission_grants.iter().map(|g| sum(g)).sum(),
                sum(&p.admission_refusals),
                l(&p.admission_holds),
            ],
        }
    }

    /// Stops everything: the pool and the glue, then the node (a clean
    /// shutdown, which commits the restart proof), then the network.
    pub async fn stop(self) -> Stopped {
        let _ = self.pool_stop.send(true);
        let _ = self.stop_tx.send(true);
        let queued = self.pool_task.await.expect("pool task");
        let dropped = self.glue_task.await.expect("glue task");
        self.node.shutdown().await.expect("node shutdown");
        let (net, port) = self.net_task.await.expect("net task");
        let _ = self.writer_task.await;
        Stopped {
            dht: testing::stats(&self.handle),
            queued,
            dropped,
            net,
            port,
            fetched: self.pool.stats().fetched.load(Ordering::Relaxed),
            funnel_fetched: self.funnel.total(Stage::Fetched, None),
            stored: self.stored.load(Ordering::Relaxed),
        }
    }
}

pub async fn run(s: &Scenario) -> Report {
    let start = Instant::now();
    let mut seeds = Rng::new(s.seed);
    let (net_seed, node_seed, fetch_seed) = (seeds.next_u64(), seeds.next_u64(), seeds.next_u64());
    let me = SocketAddr::from(ME);
    let (net, bootstrap) = Net::build(&s.net, &s.swarm, net_seed, me, start);
    let live = net.live_hashes();
    let (held, distinct) = net.sample_overlap();

    let mut dht = Config {
        intensity: s.intensity,
        bootstrap: bootstrap.iter().map(|a| a.to_string()).collect(),
        state_path: None,
        sample_start_delay: Some(Duration::ZERO),
        save_every: Duration::from_secs(1 << 30),
        ..Config::default()
    };
    let mut pool_cfg = s.pool.clone();
    s.switches.apply(&mut dht, &mut pool_cfg);

    let keep = std::env::var_os("CRAWLER_SIM_LOG").is_some();
    let log = Arc::new(Mutex::new(Log::new(start, keep)));
    let mut text = header(s, live, held, distinct);
    let seeds = Seeds {
        node: node_seed,
        fetch: fetch_seed,
    };
    if let Some(r) = s.restart {
        return crate::restart::run(s, r, dht, pool_cfg, net, seeds, log, start, text).await;
    }

    let leg = Leg::start(
        dht,
        pool_cfg,
        s.swarm.fetch,
        net,
        seeds,
        &log,
        start,
        Vec::new(),
    );
    let minutes = leg.minutes(start, s.minutes).await;

    // Measure at the boundary, then stop everything.
    let dht_final = leg.node.stats();
    let stored_at_end = leg.stored.load(Ordering::Relaxed);
    let pool_end = leg.pool_end();
    write_minutes(&mut text, &minutes);
    write_totals(&mut text, &dht_final, leg.pool.stats(), &leg.funnel);
    let end = leg.stop().await;
    conservation(&dht_final, end.fetched, end.funnel_fetched, end.stored);
    write_net(&mut text, &end.net.stats, end.dropped, end.queued.len());
    finish(
        text,
        &log,
        dht_final,
        end.net.stats,
        stored_at_end,
        steady(&minutes, |m| m.stored),
        pool_end,
    )
}

/// The outcome log's digest, the fingerprint, and the report.
pub fn finish(
    mut text: String,
    log: &Mutex<Log>,
    dht: StatsSnapshot,
    net: NetStats,
    stored: u64,
    steady_stored: f64,
    pool: PoolEnd,
) -> Report {
    let mut log = log.lock().expect("log lock");
    let _ = writeln!(
        text,
        "\noutcome_log events={} fnv64={:016x}",
        log.events, log.fnv.0
    );
    let _ = writeln!(text, "fingerprint={:016x}", hash(&[log.fnv.0, stored]));
    Report {
        text,
        log: log.take_lines(),
        dht,
        net,
        stored,
        steady_stored,
        pool,
    }
}

/// Counters that must agree in every run, whatever a candidate changes.
pub fn conservation(d: &StatsSnapshot, fetched: u64, funnel_fetched: i64, stored: u64) {
    // After shutdown every fetch has reached the writer.
    assert_eq!(fetched, stored, "fetched vs stored");
    assert_eq!(
        funnel_fetched, fetched as i64,
        "funnel Fetched vs PoolStats::fetched"
    );
    // Every admitted request ends once at most: completed, cancelled
    // while active (exclusive since CR-0f) or dropped while queued. The
    // harness never starts legacy `get_peers` lookups, which also count
    // as completed.
    let done = d.lookups_completed + d.peer_cancelled + d.peer_queued_dropped.iter().sum::<u64>();
    assert!(
        d.peer_admitted >= done,
        "admitted {} < done {done}",
        d.peer_admitted
    );
    // The active/queued gauges refresh on the 1 s tick, so at the boundary
    // they may still count up to the 32-request cap that has since finished.
    let held = d.peer_active + d.peer_queued;
    assert!(
        d.peer_admitted + 32 >= done + held,
        "admitted {} < done {done} + held {held} - 32",
        d.peer_admitted
    );
}

/// The per-minute mean of `f` after warmup.
pub fn steady(minutes: &[Minute], f: fn(&Minute) -> u64) -> f64 {
    let steady = minutes.get(WARMUP_MINUTES..).unwrap_or_default();
    steady.iter().map(f).sum::<u64>() as f64 / steady.len().max(1) as f64
}

fn header(s: &Scenario, live: usize, held: usize, distinct: usize) -> String {
    let mut t = String::new();
    let _ = writeln!(t, "# crawler-sim scenario `{}`", s.name);
    let _ = writeln!(
        t,
        "# Golden output. Regenerate: CRAWLER_SIM_BLESS=1 cargo test -p lodestar-crawler-sim --test sim"
    );
    let _ = writeln!(
        t,
        "seed={} minutes={} intensity={:?} switches={}",
        s.seed,
        s.minutes,
        s.intensity,
        s.switches.describe()
    );
    if let Some(r) = s.restart {
        let _ = writeln!(
            t,
            "restart: leg1_minutes={} gap={}s safe_restart={}",
            r.leg1_minutes, r.gap_s, r.safe_restart
        );
    }
    let n = &s.net;
    let _ = writeln!(
        t,
        "net: nodes={} dead={}‰ loss={}‰ error={}‰ rate_limit={:?} latency={}+U({})ms slow={}‰@{}ms bep51={}‰ interval={}s sample_size={}",
        n.nodes,
        n.dead_permille,
        n.loss_permille,
        n.error_permille,
        n.rate_limit_qps,
        n.latency.base_ms,
        n.latency.jitter_ms,
        n.latency.slow_permille,
        n.latency.slow_ms,
        n.bep51_permille,
        n.sample_interval_s,
        n.sample_size
    );
    if n.samplers_hold {
        let _ = writeln!(t, "net: samplers_hold=true");
    }
    let w = &s.swarm;
    let _ = writeln!(
        t,
        "swarm: catalogue={} live={} held={} distinct_held={} max_peers={}",
        w.catalogue, live, held, distinct, w.max_peers
    );
    let f = &w.fetch;
    let _ = writeln!(
        t,
        "fetch: unreachable={}‰ ok={}‰@{}ms slow={}‰@{}ms refuse={}‰@{}ms connect_timeout={}ms timeout={}ms",
        f.unreachable_permille,
        f.success_permille,
        f.success_ms,
        f.slow_permille,
        f.slow_ms,
        f.refuse_permille,
        f.refuse_ms,
        f.connect_timeout_ms,
        f.timeout_ms
    );
    let p = &s.pool;
    let _ = writeln!(
        t,
        "pool: sockets={} per_ip={} max_pending={} max_lookups={} lookup_wait={:?} retry_backoff={:?} max_attempts={}",
        p.max_sockets,
        p.per_ip,
        p.max_pending,
        p.max_lookups,
        p.lookup_wait,
        p.retry_backoff,
        p.max_attempts
    );
    t
}

/// Minutes before this are warmup: the first BEP 51 sweep and its
/// synchronized retries. The steady-state line averages the rest.
const WARMUP_MINUTES: usize = 3;

pub fn write_minutes(t: &mut String, minutes: &[Minute]) {
    let _ = writeln!(
        t,
        "\n# per minute: counter deltas; gauges sampled each second as mean/max\nmin admitted rejected completed peers_found fetch_started fetched stored harvested dht_active dht_queued pool_lookups pool_active"
    );
    let g = |g: Gauge| format!("{}/{}", g.sum / 60, g.max);
    for (i, m) in minutes.iter().enumerate() {
        let _ = writeln!(
            t,
            "{:>3} {} {} {} {} {} {} {} {} {} {} {} {}",
            i + 1,
            m.admitted,
            m.rejected,
            m.completed,
            m.peers_found,
            m.fetch_started,
            m.fetched,
            m.stored,
            m.harvested,
            g(m.dht_active),
            g(m.dht_queued),
            g(m.pool_lookups),
            g(m.pool_active)
        );
    }
    let mean = |f: fn(&Minute) -> u64| steady(minutes, f);
    let _ = writeln!(
        t,
        "steady (after {WARMUP_MINUTES} warmup min), per min: admitted={:.1} rejected={:.1} completed={:.1} fetched={:.1} stored={:.1}",
        mean(|m| m.admitted),
        mean(|m| m.rejected),
        mean(|m| m.completed),
        mean(|m| m.fetched),
        mean(|m| m.stored)
    );
}

pub fn write_totals(t: &mut String, d: &StatsSnapshot, p: &PoolStats, funnel: &FunnelCounts) {
    let _ = writeln!(
        t,
        "\n# DHT node stats at the end (node::StatsSnapshot), one per line"
    );
    // An explicit list, not `Debug`: a candidate adding a counter must not
    // move the golden with its switch off (a new `name=0` line is ignored).
    let dht = [
        ("packets_in", d.packets_in),
        ("packets_out", d.packets_out),
        ("malformed_in", d.malformed_in),
        ("queries_in", d.queries_in),
        ("responses_in", d.responses_in),
        ("errors_in", d.errors_in),
        ("queries_out", d.queries_out),
        ("timeouts", d.timeouts),
        ("throttled", d.throttled),
        ("send_errors", d.send_errors),
        ("spoofed", d.spoofed),
        ("routing_nodes", d.routing_nodes),
        ("outstanding", d.outstanding),
        ("harvested_sample", d.harvested_sample),
        ("harvest_dropped", d.harvest_dropped),
        ("lookups_completed", d.lookups_completed),
        ("samples_sent", d.samples_sent),
        ("sample_failures", d.sample_failures),
        ("sample_cooling", d.sample_cooling),
        ("sample_inflight", d.sample_inflight),
        ("sample_stale", d.sample_stale),
        ("sample_frontier", d.sample_frontier),
        ("sample_deferred", d.sample_deferred),
        ("peer_admitted", d.peer_admitted),
        ("peer_rejected", d.peer_rejected),
        ("peer_rejected_capacity", d.peer_rejected_capacity),
        ("peer_rejected_paused", d.peer_rejected_paused),
        ("peer_rejected_caller_closed", d.peer_rejected_caller_closed),
        ("peer_ack_failed", d.peer_ack_failed),
        ("peer_command_full", d.peer_command_full),
        ("peer_command_closed", d.peer_command_closed),
        ("peer_cancelled", d.peer_cancelled),
        (
            "peer_queued_dropped_caller_closed",
            d.peer_queued_dropped[0],
        ),
        ("peer_queued_dropped_paused", d.peer_queued_dropped[1]),
        ("peer_active", d.peer_active),
        ("peer_queued", d.peer_queued),
        ("lookup_scheduled", d.lookup_scheduled),
        ("lookup_deadlines", d.lookup_deadlines),
        ("denied", d.denied),
    ];
    for (name, v) in dht {
        let _ = writeln!(t, "{name}={v}");
    }
    let l = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let _ = writeln!(t, "\n# fetch pool stats at the end (PoolStats)");
    let _ = writeln!(
        t,
        "pending={} active={} waiting={} lookups={} lookup_backlog={}",
        l(&p.pending),
        l(&p.active),
        l(&p.waiting),
        l(&p.lookups),
        l(&p.lookup_backlog)
    );
    let _ = writeln!(
        t,
        "fetched={} duplicates={} finder_full={} finder_closed={} lookup_unaccepted={} retry_unaccepted={}",
        l(&p.fetched),
        l(&p.duplicates),
        l(&p.finder_full),
        l(&p.finder_closed),
        l(&p.lookup_unaccepted),
        l(&p.retry_unaccepted)
    );
    let kinds = [
        FailureKind::Unreachable,
        FailureKind::TimedOut,
        FailureKind::Refused,
        FailureKind::EncryptionFailed,
        FailureKind::Invalid,
        FailureKind::Oversize,
        FailureKind::HashMismatch,
    ];
    let failures: Vec<String> = kinds
        .iter()
        .map(|k| format!("{k:?}={}", p.failure_count(*k)))
        .collect();
    let _ = writeln!(t, "failures: {}", failures.join(" "));
    let losses = [
        Loss::NoPeers,
        Loss::TimedOut,
        Loss::Refused,
        Loss::EncryptionFailed,
        Loss::Oversize,
        Loss::Invalid,
        Loss::Blocked,
        Loss::Evicted,
        Loss::GaveUp,
        Loss::Unreachable,
    ];
    let lost: Vec<String> = losses
        .iter()
        .map(|loss| format!("{loss:?}={}", p.lost_count(*loss)))
        .collect();
    let _ = writeln!(t, "lost: {}", lost.join(" "));
    let service: Vec<String> = ["unaccepted", "lookup", "peers", "fetch"]
        .iter()
        .zip(&p.evicted_service)
        .map(|(n, c)| format!("{n}={}", l(c)))
        .collect();
    let _ = writeln!(t, "evicted_service: {}", service.join(" "));
    // Coordinated admission (CR-ADM). One counter a line, so with the
    // switch off they're new zero counters the goldens ignore.
    let sum = |a: &[std::sync::atomic::AtomicU64]| a.iter().map(l).sum::<u64>();
    for (name, v) in [
        ("admission_waits", sum(&p.admission_waits)),
        ("admission_wait_cancelled", sum(&p.admission_wait_cancelled)),
        ("admission_wakeups", l(&p.admission_wakeups)),
        (
            "admission_grants",
            p.admission_grants.iter().map(|g| sum(g)).sum(),
        ),
        ("admission_refusals", sum(&p.admission_refusals)),
        ("admission_holds", l(&p.admission_holds)),
        ("admission_permit_returns", sum(&p.admission_permit_returns)),
        (
            "dht_permit_returns",
            d.peer_permit_returns.iter().sum::<u64>(),
        ),
    ] {
        let _ = writeln!(t, "{name}={v}");
    }
    let stages = [
        Stage::Harvested,
        Stage::PeerFound,
        Stage::Fetched,
        Stage::Lost,
    ];
    let funnel: Vec<String> = stages
        .iter()
        .map(|s| format!("{s:?}={}", funnel.total(*s, None)))
        .collect();
    let _ = writeln!(t, "funnel: {}", funnel.join(" "));
}

pub fn write_net(t: &mut String, n: &NetStats, glue_dropped: u64, queued_at_end: usize) {
    let _ = writeln!(t, "\n# scripted network");
    let _ = writeln!(
        t,
        "queries={} ping={} find_node={} get_peers={} sample={} other={}",
        n.queries, n.ping, n.find_node, n.get_peers, n.sample, n.other
    );
    let _ = writeln!(
        t,
        "to_dead={} to_unknown={} lost={} rate_limited={} errors={} unsupported={} sample_early={} replies={} values_replies={}",
        n.to_dead,
        n.to_unknown,
        n.lost,
        n.rate_limited,
        n.errors,
        n.unsupported,
        n.sample_early,
        n.replies,
        n.values_replies
    );
    let _ = writeln!(
        t,
        "discoveries_dropped={glue_dropped} pool_queue_at_end={queued_at_end}"
    );
}
