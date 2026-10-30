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
pub struct Switches {}

impl Switches {
    /// Writes the switches into the real configs the run starts with.
    pub fn apply(&self, _dht: &mut Config, _pool: &mut PoolConfig) {}

    /// For the report header: `name=value` of every switch that's on.
    pub fn describe(&self) -> String {
        "none".to_string()
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
