//! What a scenario scripts: the network, the swarms behind the hashes,
//! the node and pool settings, and the candidate switches.

use std::time::Duration;

use lodestar_dht::node::{Config, Intensity};
use lodestar_sources::PoolConfig;

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: &'static str,
    pub seed: u64,
    /// Simulated run length.
    pub minutes: u32,
    pub net: NetConfig,
    pub swarm: SwarmConfig,
    pub intensity: Intensity,
    pub pool: PoolConfig,
    pub switches: Switches,
    /// Two legs with a clean restart between them; `minutes` is then
    /// leg 2's length.
    pub restart: Option<Restart>,
    /// Print the capacity ledger (`W`, `q`, `y`, occupancy) in the report.
    /// Off in the baselines, whose goldens predate it.
    pub ledger: bool,
}

/// CR-SIM-restart: leg 1 starts fresh with `dht.safe_restart` on, sits out
/// its first-start suppression (6 h plus the query timeout), samples for
/// `leg1_minutes` and stops cleanly. After `gap_s`, leg 2 starts from the
/// same data and witness dirs against the same network.
#[derive(Debug, Clone, Copy)]
pub struct Restart {
    pub leg1_minutes: u32,
    pub gap_s: u64,
    /// `dht.safe_restart` in leg 2. Off is the negative control: the full
    /// suppression again.
    pub safe_restart: bool,
}

/// The simulated DHT nodes.
#[derive(Debug, Clone)]
pub struct NetConfig {
    pub nodes: usize,
    /// Addresses that are in routing tables but never answer.
    pub dead_permille: u32,
    /// Datagrams lost, each way independently.
    pub loss_permille: u32,
    /// Live nodes answering a query with KRPC error 202.
    pub error_permille: u32,
    /// Queries per second one node answers from us; the rest are dropped
    /// (a remote rate limit). `None` is unlimited.
    pub rate_limit_qps: Option<u32>,
    /// One-way-and-back delay of a reply.
    pub latency: Latency,
    /// Live nodes that implement BEP 51; the rest answer error 204.
    pub bep51_permille: u32,
    /// The BEP 51 `interval` a node returns.
    pub sample_interval_s: u32,
    /// Hashes per `sample_infohashes` reply (BEP 51 caps it near 20).
    pub sample_size: usize,
    /// Hashes each BEP 51 node holds; with [`SwarmConfig::catalogue`] this
    /// sets the overlap between nodes.
    pub held_per_node: usize,
    /// A BEP 51 node also answers `get_peers` for the hashes it samples,
    /// as a responder storing them would (CR-1a-H's screen). Off in the
    /// baselines, whose goldens predate it.
    pub samplers_hold: bool,
    /// Stale contacts per node, in permille of one (floor, plus the
    /// remainder as a chance): addresses beside the node in id space that
    /// never answer, listed with it in replies from near a target. Each
    /// must time out before a lookup can end (`live-lookup`). 0 in the
    /// baselines.
    pub stale_permille: u32,
}

/// Reply delay: `base + U(0, jitter)`, or `slow` instead with
/// `slow_permille`.
#[derive(Debug, Clone, Copy)]
pub struct Latency {
    pub base_ms: u64,
    pub jitter_ms: u64,
    pub slow_permille: u32,
    pub slow_ms: u64,
}

/// The torrents behind the hashes, and the peers the pool fetches from.
#[derive(Debug, Clone)]
pub struct SwarmConfig {
    /// Distinct hashes held across the network.
    pub catalogue: usize,
    /// Hashes with a swarm; the rest have no peers anywhere.
    pub live_permille: u32,
    /// Peers per live swarm: uniform in `1..=max_peers`.
    pub max_peers: usize,
    pub fetch: FetchConfig,
}

/// How one (peer, hash) fetch goes, decided by a hash of both so it doesn't
/// depend on the order the pool asks. Permilles are checked in field order;
/// the remainder times out.
#[derive(Debug, Clone, Copy)]
pub struct FetchConfig {
    /// Peers that can't be reached at all (per peer, every hash).
    pub unreachable_permille: u32,
    pub success_permille: u32,
    pub slow_permille: u32,
    pub refuse_permille: u32,
    pub success_ms: u64,
    pub slow_ms: u64,
    pub refuse_ms: u64,
    /// When an unreachable peer's connect gives up (peer-wire's
    /// `connect_timeout`, 4 s).
    pub connect_timeout_ms: u64,
    /// The whole fetch's deadline (peer-wire's `timeout`, 8 s): a peer
    /// that would answer later, slow ones included, times out here.
    pub timeout_ms: u64,
}

/// Candidate runtime switches. Empty until a candidate lands: each
/// candidate adds its own field here (default = off) and maps it in
/// [`Switches::apply`], so a scenario toggles it with
/// `Scenario { switches: Switches { my_candidate: true, ..Default::default() }, ..base }`.
/// See `README.md` ("A/B a candidate").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Switches {
    /// CR-ADM: `dht.coordinated_admission`.
    pub coordinated_admission: bool,
    /// CR-1a-H: `dht.responder_hints`.
    pub responder_hints: bool,
    /// CR-HINT-FIRST: `dht.hint_first`.
    pub hint_first: bool,
    /// CR-1a-S: `dht.streaming_peers`.
    pub streaming_peers: bool,
    /// CR-1b: `dht.target_prefixes`.
    pub target_prefixes: bool,
    /// CR-LOOKUP-END: `dht.lookup_end`.
    pub lookup_end: bool,
    /// CR-SOFT-TIMEOUT: `dht.soft_timeout`, with its wait in seconds
    /// (`Some(1)` is the shipped 1 s; `Some(2)` the screen's variant).
    pub soft_timeout: Option<u64>,
    /// CR-1c: `dht.preset_slots`.
    pub preset_slots: bool,
    /// X-sampler-cooldown-split: `dht.sampler_cooldown_split`.
    pub sampler_cooldown_split: bool,
    /// CR-SAMPLE-BACKOFF-ESCALATE: `dht.sample_backoff_escalate`.
    pub sample_backoff_escalate: bool,
}

impl Switches {
    /// Writes the switches into the real configs the run starts with.
    pub fn apply(&self, dht: &mut Config, pool: &mut PoolConfig) {
        dht.preset_slots = self.preset_slots;
        dht.sampler_cooldown_split = self.sampler_cooldown_split;
        dht.sample_backoff_escalate = self.sample_backoff_escalate;
        if self.coordinated_admission {
            // As the pipeline wires it (`crates/pipeline/src/lib.rs`): one
            // permit per DHT slot (32, or the preset's with CR-1c).
            pool.coordinated_admission = true;
            pool.finder_capacity = dht.peer_slots();
        }
        dht.responder_hints = self.responder_hints;
        if self.hint_first {
            // As the pipeline wires it: the glue's one source is the DHT's.
            dht.hint_first = true;
            pool.hint_first = true;
            pool.hint_sources = vec![lodestar_store::SourceId::new(1).expect("valid source id")];
        }
        pool.streaming_peers = self.streaming_peers;
        dht.target_prefixes = self.target_prefixes;
        dht.lookup_end = self.lookup_end;
        if let Some(secs) = self.soft_timeout {
            dht.soft_timeout = true;
            dht.soft_timeout_after = std::time::Duration::from_secs(secs);
        }
    }

    /// For the report header: `name=value` of every switch that's on.
    pub fn describe(&self) -> String {
        let soft = self
            .soft_timeout
            .map(|s| format!("soft_timeout=true(after={s}s)"));
        let on: Vec<&str> = [
            (self.coordinated_admission, "coordinated_admission=true"),
            (self.responder_hints, "responder_hints=true"),
            (self.hint_first, "hint_first=true"),
            (self.streaming_peers, "streaming_peers=true"),
            (self.target_prefixes, "target_prefixes=true"),
            (self.lookup_end, "lookup_end=true"),
            (self.preset_slots, "preset_slots=true"),
            (self.sampler_cooldown_split, "sampler_cooldown_split=true"),
            (self.sample_backoff_escalate, "sample_backoff_escalate=true"),
            (soft.is_some(), soft.as_deref().unwrap_or_default()),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        if on.is_empty() {
            "none".to_string()
        } else {
            on.join(",")
        }
    }
}

/// Settings shared by the baselines; each scenario changes what it tests.
pub fn base(name: &'static str) -> Scenario {
    Scenario {
        name,
        seed: 1,
        minutes: 10,
        net: NetConfig {
            nodes: 2_000,
            dead_permille: 150,
            loss_permille: 20,
            error_permille: 5,
            rate_limit_qps: Some(20),
            latency: Latency {
                base_ms: 40,
                jitter_ms: 160,
                slow_permille: 30,
                slow_ms: 1_500,
            },
            bep51_permille: 600,
            sample_interval_s: 300,
            sample_size: 20,
            held_per_node: 60,
            samplers_hold: false,
            stale_permille: 0,
        },
        swarm: SwarmConfig {
            catalogue: 20_000,
            live_permille: 450,
            max_peers: 12,
            fetch: FetchConfig {
                unreachable_permille: 300,
                success_permille: 450,
                slow_permille: 100,
                refuse_permille: 250,
                success_ms: 900,
                slow_ms: 6_000,
                refuse_ms: 80,
                connect_timeout_ms: 4_000,
                timeout_ms: 8_000,
            },
        },
        intensity: Intensity::Balanced,
        pool: PoolConfig::default(),
        switches: Switches::default(),
        restart: None,
        ledger: false,
    }
}

/// Everything well behaved: the reference shape.
pub fn healthy() -> Scenario {
    base("healthy")
}

/// H1: lookup latency × the 32-request cap. Replies are slow and often
/// near the 4 s query timeout, and more of the table is dead, so each
/// owned lookup holds its slot far longer.
pub fn high_latency() -> Scenario {
    let mut s = base("high-latency");
    s.net.dead_permille = 350;
    s.net.loss_permille = 60;
    s.net.latency = Latency {
        base_ms: 350,
        jitter_ms: 1_400,
        slow_permille: 200,
        slow_ms: 3_600,
    };
    s
}

/// H2: admission and retry churn. Many short-interval BEP 51 nodes feed far
/// more keys than the lookup cap can serve, most of them dead, so the
/// pool's requests are refused for capacity and retried.
pub fn admission_pressure() -> Scenario {
    let mut s = base("admission-pressure");
    s.net.bep51_permille = 900;
    s.net.sample_interval_s = 60;
    s.net.held_per_node = 200;
    s.swarm.catalogue = 200_000;
    s.swarm.live_permille = 150;
    s.pool.max_pending = 20_000;
    s.pool.retry_backoff = Duration::from_secs(30);
    s
}

/// CR-0d's restored restrictions end to end: a clean restart with
/// `dht.safe_restart` on. Intervals of 15 min ± 25% keep restored
/// obligations pending well into leg 2; leg 2's hour is the restart-cost
/// baseline against leg 1's fresh 6 h suppression.
pub fn restart_honoured() -> Scenario {
    let mut s = base("restart-honoured");
    s.minutes = 60;
    s.net.sample_interval_s = 900;
    s.restart = Some(Restart {
        leg1_minutes: 20,
        gap_s: 120,
        safe_restart: true,
    });
    s
}

/// X-crawler-sim-calibrate: the live lookup as CR-0's 24 h measured it
/// (README, "The calibrated scenario"). Keys far outrun the 32 slots, as
/// live. Stale contacts near every target must each time out (4 s, at
/// most 3 in flight) before a lookup can end, and each closer responder
/// lists more, so most lookups run to the 25 s deadline.
/// The knobs are calibrated, not the outputs: `live_lookup_is_calibrated`
/// checks what they produce against CR-0 on every `SCREEN_SEEDS` seed.
pub fn live_lookup() -> Scenario {
    let mut s = base("live-lookup");
    s.minutes = 12;
    s.ledger = true;
    s.net.nodes = 5_000;
    s.net.dead_permille = 30;
    s.net.loss_permille = 5;
    // About 1.75 stale contacts beside every node: most lookups end at
    // the deadline.
    s.net.stale_permille = 1_750;
    s.net.latency = Latency {
        base_ms: 60,
        jitter_ms: 400,
        slow_permille: 150,
        slow_ms: 3_000,
    };
    // Enough sampling to bring the query rate to CR-0's 76/s.
    s.net.bep51_permille = 700;
    s.net.sample_interval_s = 120;
    s.net.held_per_node = 200;
    s.swarm.catalogue = 200_000;
    // With the fetch outcomes below: CR-0's 3.7 attempts and 0.16 stored
    // per admitted lookup.
    s.swarm.live_permille = 700;
    s.swarm.max_peers = 16;
    // CR-0's fetch failures by attempt: 4.4% stored, 10.7% refused, 14.1%
    // unreachable, the rest timed out. Success and refusal are permille
    // of the 85.9% reachable: 0.859 × 51 = 4.4%, 0.859 × 125 = 10.7%.
    s.swarm.fetch = FetchConfig {
        unreachable_permille: 141,
        success_permille: 51,
        slow_permille: 0,
        refuse_permille: 125,
        ..s.swarm.fetch
    };
    s
}
