//! The node's event loop. See the module docs in `node/mod.rs`.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rand::RngExt;
use rand::rngs::StdRng;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use lodestar_metainfo::InfoHash;

use super::lookup::Lookup;
use super::peers::PeerStore;
use super::proof::Proof;
use super::sampler::{self, Sampler};
use super::seen::SeenFilter;
use super::tokens::Tokens;
use super::transport::{Map, MapEntry, RecvSocket, SendSocket, Set};
use super::{
    Command, Config, DhtEstimate, ExternalVotes, Harvest, Intensity, Snapshot, Source, Stats,
};
use crate::compact::{self, CompactNodes, NodeId, NodeInfo};
use crate::krpc::{self, Body, ErrorBody, Message, Method, Query, Response, Want, error_code};
use crate::routing::{Contact, Inserted, RoutingTable, Status};
use crate::secure_id;
use crate::state::{State, StateError};

/// Our client version (`v`): "LD" + version.
const VERSION: &[u8] = b"LD\x00\x01";
/// BEP 51: how long others should wait before sampling us again.
const SAMPLE_INTERVAL: u32 = 6 * 3600;
/// Concurrent maintenance lookups (bootstrap and bucket refresh).
const MAX_LOOKUPS: usize = 16;
/// Questionable nodes pinged per tick.
const PINGS_PER_TICK: usize = 16;
/// Below this many nodes the table is thin: keep re-running the self-lookup
/// (every [`REBOOTSTRAP_TICKS`] ticks, i.e. 30 s by default) until it fills.
const HEALTHY_NODES: usize = 32;
const REBOOTSTRAP_TICKS: u32 = 30;
/// Seen-filter generation size: ~1.2 MB per generation at 1% false positives.
const SEEN_CAPACITY: usize = 1_000_000;
/// Hashes we offer when sampled ourselves (BEP 51 allows about 20 per reply).
const RECENT_SAMPLES: usize = 20;
/// Concurrent get_peers lookups, and how many requests may wait.
const MAX_GET_PEERS: usize = 32;
const MAX_GET_PEERS_QUEUE: usize = MAX_GET_PEERS;
const LOOKUP_DEADLINE: Duration = Duration::from_secs(25);
const MAX_PEER_QUERIES: usize = 64;
/// Share of the query budget spent on BEP 51 sampling; the rest stays
/// free for routing maintenance and lookups.
const SAMPLE_SHARE: f64 = 0.5;
/// Share of the query budget BEP 33 scrapes may use. It's spent when a
/// scrape lookup starts, at [`QUERIES_PER_SCRAPE`] each, never by failing
/// queries mid-lookup.
const SCRAPE_SHARE: f64 = 0.1;
/// What a scrape lookup costs at most: 2K answers plus a few that time out.
const QUERIES_PER_SCRAPE: f64 = 20.0;
/// Concurrent scrape lookups, and how many requests may wait.
const MAX_SCRAPES: usize = 16;
const MAX_SCRAPE_QUEUE: usize = 1024;
/// A scrape lookup still running after this answers with what it has.
const SCRAPE_DEADLINE: Duration = Duration::from_secs(30);
/// Below this frontier size, refill it and explore a random region.
const LOW_FRONTIER: usize = 64;
/// Backoff for a sample query that timed out (may be transient).
const SAMPLE_TIMEOUT_BACKOFF: Duration = sampler::MAX_INTERVAL;
/// IPs we sent to (queries, replies, errors), remembered so a query from
/// them isn't counted as unsolicited. One generation lasts at least ~3
/// minutes at Aggressive even if every packet went to a new IP (1000
/// queries plus 2000 replies a second), about the longest a home router
/// keeps a UDP mapping open; ~1.4 MB for both generations.
const CONTACTED_CAPACITY: usize = 600_000;
/// Distinct networks (/24 or /64) whose last unsolicited query is kept,
/// newest first; the reachability check needs only a few.
const UNSOLICITED_SOURCES: usize = 64;

#[derive(Debug, Clone, Copy)]
enum Purpose {
    Ping,
    Lookup(u32),
    Sample,
}

#[derive(Debug)]
struct Pending {
    to: SocketAddr,
    id: Option<NodeId>,
    sent: Instant,
    family: usize,
    purpose: Purpose,
}

/// What happened to a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sent {
    Yes,
    /// Over the rate limit or the outstanding cap: try again later.
    Throttled,
    /// The socket refused it.
    Failed,
}

struct TokenBucket {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Instant,
}

/// Outbound queries at `intensity`'s rate.
fn query_bucket(intensity: Intensity, now: Instant) -> TokenBucket {
    TokenBucket::new(intensity.queries_per_sec(), now)
}

/// Replies are capped too, so we can't be used as an amplifier.
fn reply_bucket(intensity: Intensity, now: Instant) -> TokenBucket {
    TokenBucket::new(intensity.queries_per_sec() * 2, now)
}

/// Scrape lookups: [`SCRAPE_SHARE`] of the query rate.
fn scrape_bucket(intensity: Intensity, now: Instant) -> TokenBucket {
    let qps = f64::from(intensity.queries_per_sec());
    let per_sec = qps * SCRAPE_SHARE / QUERIES_PER_SCRAPE;
    TokenBucket::with_burst(per_sec, per_sec, now)
}

impl TokenBucket {
    fn new(per_sec: u32, now: Instant) -> TokenBucket {
        let rate = f64::from(per_sec);
        TokenBucket::with_burst(rate, rate, now)
    }

    /// A bucket holding up to `burst` tokens (at least one), full at start.
    fn with_burst(rate: f64, burst: f64, now: Instant) -> TokenBucket {
        let burst = burst.max(1.0);
        TokenBucket {
            rate,
            burst,
            tokens: burst,
            last: now,
        }
    }

    fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A reporter's network: its /24 (IPv4) or /64 (IPv6). One network
/// gets one say, so a few hosts in one range can't outvote the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Network(u8, u64);

fn network(ip: IpAddr) -> Network {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            Network(4, u64::from_be_bytes([0, 0, 0, 0, 0, a, b, c]))
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            Network(6, u64::from_be_bytes(o[..8].try_into().expect("8 bytes")))
        }
    }
}

/// Majority vote over the external address other nodes report (BEP 42).
/// One vote per reporting network, the last [`ExternalIp::WINDOW`] of
/// them, tallied apart for IPv4 and IPv6 addresses so one family never
/// dilutes the other's majority.
#[derive(Default)]
struct ExternalIp {
    v4: VecDeque<(Network, SocketAddr)>,
    v6: VecDeque<(Network, SocketAddr)>,
    current: Option<IpAddr>,
}

impl ExternalIp {
    const WINDOW: usize = 32;
    const QUORUM: usize = 5;

    /// Records a report; returns the new address when the majority changes.
    fn vote(&mut self, reporter: IpAddr, reported: SocketAddr) -> Option<IpAddr> {
        let reporter = network(reporter);
        // `::ffff:a.b.c.d` is `a.b.c.d`: one address, one count.
        let reported = SocketAddr::new(reported.ip().to_canonical(), reported.port());
        let votes = if reported.is_ipv4() {
            &mut self.v4
        } else {
            &mut self.v6
        };
        votes.retain(|(r, _)| *r != reporter);
        votes.push_back((reporter, reported));
        if votes.len() > Self::WINDOW {
            votes.pop_front();
        }
        let t = self.tally();
        match t.ip {
            Some(best) if self.current != Some(best) => {
                self.current = Some(best);
                Some(best)
            }
            _ => None,
        }
    }

    /// The IPv4 majority if there is one, else the IPv6 one, else the
    /// evidence of the family with more voters.
    fn tally(&self) -> ExternalVotes {
        let (v4, v6) = (Self::tally_of(&self.v4), Self::tally_of(&self.v6));
        if v4.ip.is_some() || (v6.ip.is_none() && v4.voters >= v6.voters) {
            v4
        } else {
            v6
        }
    }

    /// The leading address and its port, with the evidence. Ties go to
    /// the smaller value, so the answer doesn't depend on map order.
    fn tally_of(votes: &VecDeque<(Network, SocketAddr)>) -> ExternalVotes {
        let mut ips: HashMap<IpAddr, usize> = HashMap::new();
        for (_, addr) in votes {
            *ips.entry(addr.ip()).or_default() += 1;
        }
        let Some((best, n)) = ips
            .into_iter()
            .max_by_key(|(ip, n)| (*n, std::cmp::Reverse(*ip)))
        else {
            return ExternalVotes::default();
        };
        let majority = n >= Self::QUORUM && n * 2 > votes.len();
        let port = majority
            .then(|| {
                let mut ports: HashMap<u16, usize> = HashMap::new();
                for (_, addr) in votes.iter().filter(|(_, a)| a.ip() == best) {
                    *ports.entry(addr.port()).or_default() += 1;
                }
                ports
                    .into_iter()
                    .max_by_key(|(p, n)| (*n, std::cmp::Reverse(*p)))
                    .map(|(p, _)| p)
            })
            .flatten();
        ExternalVotes {
            ip: majority.then_some(best),
            port,
            voters: votes.len(),
            agreeing: n,
        }
    }
}

/// When each network last sent us an unsolicited query: the newest
/// [`UNSOLICITED_SOURCES`] networks.
#[derive(Default)]
struct UnsolicitedFrom(Map<Network, Instant>);

impl UnsolicitedFrom {
    fn note(&mut self, ip: IpAddr, now: Instant) {
        let net = network(ip);
        if !self.0.contains_key(&net)
            && self.0.len() >= UNSOLICITED_SOURCES
            && let Some(oldest) = self.0.iter().min_by_key(|(_, t)| **t).map(|(n, _)| *n)
        {
            self.0.remove(&oldest);
        }
        self.0.insert(net, now);
    }

    /// How long ago each network's last unsolicited query was.
    fn ages(&self, now: Instant) -> Vec<Duration> {
        self.0
            .values()
            .map(|t| now.saturating_duration_since(*t))
            .collect()
    }
}

/// The filter key for an IP we queried.
fn contact_key(ip: IpAddr) -> InfoHash {
    let mut key = [0u8; 20];
    match ip {
        IpAddr::V4(v4) => {
            key[0] = 4;
            key[1..5].copy_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            key[0] = 6;
            key[1..17].copy_from_slice(&v6.octets());
        }
    }
    InfoHash(key)
}

struct Family {
    table: RoutingTable,
    ipv6: bool,
}

struct PeerRequest {
    hash: InfoHash,
    reply: Option<oneshot::Sender<Vec<SocketAddr>>>,
    remaining: usize,
    inflight: usize,
    sent: usize,
    peers: Set<SocketAddr>,
    started: Instant,
}

struct Actor {
    cfg: Config,
    families: Vec<Family>,
    ids: Vec<NodeId>,
    external: ExternalIp,
    tokens: Tokens,
    pending: Map<u64, Pending>,
    /// Queries in flight per address, for cheap "already asked?" checks.
    pending_addrs: Map<SocketAddr, u32>,
    next_tid: u64,
    lookups: Map<u32, Lookup>,
    next_lookup: u32,
    query_rate: TokenBucket,
    reply_rate: TokenBucket,
    stats: Arc<Stats>,
    rng: StdRng,
    epoch: Instant,
    bootstrap: Vec<SocketAddr>,
    last_bootstrap: Option<Instant>,
    /// Bootstrap-router queries in flight, for `dht.bootstrap_unreachable`.
    seed_tids: Set<u64>,
    /// The last bootstrap's routers refused (policy or socket) and throttled
    /// at send, and whether any router answered this round.
    seed_refused: usize,
    seed_throttled: usize,
    seed_answered: bool,
    /// The warning was written; re-armed when a router answers.
    seed_warned: bool,
    last_save: Instant,
    out: Vec<u8>,
    local_addrs: Vec<SocketAddr>,
    harvest: Option<mpsc::Sender<Harvest>>,
    seen: SeenFilter,
    peer_store: PeerStore,
    sampler: Sampler,
    recent: VecDeque<InfoHash>,
    get_peers_queue: VecDeque<(InfoHash, Option<oneshot::Sender<Vec<SocketAddr>>>)>,
    peer_requests: Map<u32, PeerRequest>,
    next_request: u32,
    active_get_peers: usize,
    scrape_queue: VecDeque<(InfoHash, oneshot::Sender<Option<DhtEstimate>>)>,
    active_scrapes: usize,
    /// Who's waiting for each running scrape lookup.
    scrape_waiters: Map<u32, oneshot::Sender<Option<DhtEstimate>>>,
    scrape_rate: TokenBucket,
    /// Scrape lookups that ran out of query budget, to drive next tick.
    stalled: Vec<u32>,
    /// When `dht.harvest_summary` was last written, and the counters then
    /// (sample, get_peers, announce, lookups, dropped).
    last_summary: (Instant, [u64; 5]),
    /// IPs we've sent to lately, to tell unsolicited queries apart.
    contacted: SeenFilter,
    unsolicited_from: UnsolicitedFrom,
    /// The crawler's pause: no sampling, lookups, scrapes or harvests.
    paused: bool,
    /// CR-0d: when sampling may start, and the restart proof.
    proof: Proof,
}

pub(crate) async fn run(
    cfg: Config,
    sockets: Vec<(RecvSocket, SendSocket)>,
    rng: StdRng,
    stats: Arc<Stats>,
    mut commands: mpsc::Receiver<Command>,
    mut shutdown: watch::Receiver<bool>,
    harvest: Option<mpsc::Sender<Harvest>>,
) -> Result<(), StateError> {
    let now = Instant::now();
    let saved = match &cfg.state_path {
        Some(path) => State::load(path).unwrap_or_default(),
        None => None,
    };
    let (recv, sockets): (Vec<RecvSocket>, Vec<SendSocket>) = sockets.into_iter().unzip();
    let local_addrs = recv.iter().filter_map(|s| s.local_addr().ok()).collect();
    let bootstrap = resolve(&cfg.bootstrap).await;
    let mut actor = Actor::new(
        cfg,
        stats,
        local_addrs,
        saved.as_ref(),
        bootstrap,
        harvest,
        now,
        rng,
    );
    actor.start_bootstrap(&sockets, saved.map(|s| s.nodes).unwrap_or_default());

    let mut tick = tokio::time::interval(actor.cfg.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut buf0 = vec![0u8; 65_536];
    let mut buf1 = vec![0u8; 65_536];
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = tick.tick() => actor.maintain(&sockets),
            Some(cmd) = commands.recv() => actor.on_command(cmd),
            (family, r) = async {
                // Maintenance has priority, but neither socket does.
                tokio::select! {
                    r = recv[0].recv_from(&mut buf0) => (0, r),
                    r = recv_second(&recv, &mut buf1), if recv.len() > 1 => (1, r),
                }
            } => {
                if let Ok((n, from)) = r {
                    let buf = if family == 0 { &buf0 } else { &buf1 };
                    actor.on_packet(&sockets, family, &buf[..n], from);
                }
            }
        }
    }
    actor.finish()
}

async fn recv_second(
    sockets: &[RecvSocket],
    buf: &mut [u8],
) -> std::io::Result<(usize, SocketAddr)> {
    sockets[1].recv_from(buf).await
}

/// Saved ids if they still suit the saved external address, else random
/// ones until the network tells us our address.
fn initial_ids(cfg: &Config, saved: Option<&State>, rng: &mut StdRng) -> Vec<NodeId> {
    let want = cfg.virtual_ids.clamp(1, 64);
    if let Some(s) = saved {
        let valid = s
            .external_ip
            .is_none_or(|ip| s.ids.iter().all(|id| secure_id::is_valid(id, &ip)));
        if !s.ids.is_empty() && valid {
            let mut ids = s.ids.clone();
            ids.truncate(want);
            while ids.len() < want {
                ids.push(NodeId(rng.random()));
            }
            return ids;
        }
        if let Some(ip) = s.external_ip {
            return secure_id::virtual_ids(&ip, want, rng);
        }
    }
    (0..want).map(|_| NodeId(rng.random())).collect()
}

async fn resolve(hosts: &[String]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for host in hosts {
        let lookup = tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host(host.as_str()),
        );
        if let Ok(Ok(addrs)) = lookup.await {
            out.extend(addrs);
        }
    }
    out
}

fn endpoint_allowed(cfg: &Config, addr: SocketAddr) -> bool {
    let addr = lodestar_network_policy::normalize(addr);
    lodestar_network_policy::allows(addr, cfg.allow_private)
        && !cfg.deny.as_ref().is_some_and(|d| d.denies(addr.ip()))
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        cfg: Config,
        stats: Arc<Stats>,
        local_addrs: Vec<SocketAddr>,
        saved: Option<&State>,
        bootstrap: Vec<SocketAddr>,
        harvest: Option<mpsc::Sender<Harvest>>,
        now: Instant,
        mut rng: StdRng,
    ) -> Self {
        let ids = initial_ids(&cfg, saved, &mut rng);
        let families = local_addrs
            .iter()
            .map(|addr| Family {
                table: RoutingTable::new(ids[0], addr.is_ipv6()),
                ipv6: addr.is_ipv6(),
            })
            .collect();
        Self {
            tokens: Tokens::new(now, &mut rng),
            query_rate: query_bucket(cfg.intensity, now),
            reply_rate: reply_bucket(cfg.intensity, now),
            families,
            ids,
            external: ExternalIp {
                current: saved.and_then(|s| s.external_ip),
                ..ExternalIp::default()
            },
            pending: Map::default(),
            pending_addrs: Map::default(),
            // The lower half leaves 2^63 tokens before exhaustion.
            next_tid: rng.random::<u64>() >> 1,
            lookups: Map::default(),
            next_lookup: 0,
            stats,
            rng,
            epoch: now,
            bootstrap: bootstrap
                .into_iter()
                .map(lodestar_network_policy::normalize)
                .collect(),
            last_bootstrap: None,
            seed_tids: Set::default(),
            seed_refused: 0,
            seed_throttled: 0,
            seed_answered: false,
            seed_warned: false,
            last_save: now,
            out: Vec::with_capacity(1500),
            local_addrs,
            harvest,
            seen: SeenFilter::new(SEEN_CAPACITY, 0.01),
            peer_store: PeerStore::default(),
            sampler: Sampler::default(),
            recent: VecDeque::with_capacity(RECENT_SAMPLES),
            get_peers_queue: VecDeque::new(),
            peer_requests: Map::default(),
            next_request: 0,
            active_get_peers: 0,
            scrape_queue: VecDeque::new(),
            active_scrapes: 0,
            scrape_waiters: Map::default(),
            scrape_rate: scrape_bucket(cfg.intensity, now),
            stalled: Vec::new(),
            last_summary: (now, [0; 5]),
            contacted: SeenFilter::new(CONTACTED_CAPACITY, 0.01),
            unsolicited_from: UnsolicitedFrom::default(),
            paused: cfg.paused,
            proof: Proof::new(
                cfg.state_path.as_deref(),
                cfg.restart.as_ref(),
                cfg.sample_start_delay,
                sampler::MAX_INTERVAL + cfg.query_timeout,
                cfg.tick,
                now,
            ),
            cfg,
        }
    }

    fn now_secs(&self) -> u64 {
        self.epoch.elapsed().as_secs()
    }

    /// Whether a node entry is actually us: other nodes list us in their
    /// replies, and a lookup that queried itself would waste one of its K
    /// slots (it did, before this check).
    fn is_self(ids: &[NodeId], local: &[SocketAddr], n: &NodeInfo) -> bool {
        ids.contains(&n.id) || local.contains(&n.addr)
    }

    /// The virtual id we present to a given node: the closest one, so each
    /// peer always sees us under the same id.
    fn id_for(&self, peer: &NodeId) -> NodeId {
        *self
            .ids
            .iter()
            .min_by_key(|id| id.distance(peer))
            .expect("at least one id")
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd {
            Command::Snapshot(reply) => {
                let _ = reply.send(Snapshot {
                    ids: self.ids.clone(),
                    external_ip: self.external.current,
                    table_sizes: self.families.iter().map(|f| f.table.len()).collect(),
                    nodes: self
                        .families
                        .iter()
                        .flat_map(|f| f.table.entries().map(|e| e.info))
                        .collect(),
                    lookups: self.lookups.len(),
                    sample_frontier: self.sampler.frontier_len(),
                    sample_known: self.sampler.known(),
                    stored_peer_hashes: self.peer_store.len(),
                    seen_filter_bytes: self.seen.memory(),
                    intensity: self.cfg.intensity,
                    paused: self.paused,
                    external: self.external.tally(),
                    unsolicited_sources: self.unsolicited_from.ages(Instant::now()),
                });
            }
            Command::SetPaused(paused) => {
                self.paused = paused;
                if paused {
                    // Queued, accepted requests are dropped unanswered: the
                    // closed receiver tells the pool the work was cancelled,
                    // not attempted, so the pause costs no attempt. Running
                    // ones finish and still answer.
                    self.get_peers_queue.retain(|(_, reply)| reply.is_none());
                    for (_, reply) in self.scrape_queue.drain(..) {
                        let _ = reply.send(None);
                    }
                }
            }
            Command::SetIntensity(intensity) => {
                // The rates and the outstanding cap follow at once; the
                // buckets start full, as at start.
                let now = Instant::now();
                self.cfg.intensity = intensity;
                self.query_rate = query_bucket(intensity, now);
                self.reply_rate = reply_bucket(intensity, now);
                self.scrape_rate = scrape_bucket(intensity, now);
            }
            Command::GetPeers(hash) => {
                if self.get_peers_queue.len() + self.active_get_peers < MAX_GET_PEERS_QUEUE {
                    self.get_peers_queue.push_back((hash, None));
                }
            }
            Command::RequestPeers(hash, admit) => {
                self.get_peers_queue
                    .retain(|(_, reply)| reply.as_ref().is_none_or(|r| !r.is_closed()));
                if !admit.is_closed()
                    && !self.paused
                    && self.get_peers_queue.len() + self.active_get_peers < MAX_GET_PEERS
                {
                    let (tx, rx) = oneshot::channel();
                    if admit.send(Some(rx)).is_ok() {
                        Stats::inc(&self.stats.peer_admitted);
                        self.get_peers_queue.push_back((hash, Some(tx)));
                    } else {
                        Stats::inc(&self.stats.peer_ack_failed);
                    }
                } else {
                    Stats::inc(&self.stats.peer_rejected);
                    // Exclusive precedence; keep the existing total's boundary.
                    if admit.is_closed() {
                        Stats::inc(&self.stats.peer_rejected_caller_closed);
                    } else if self.paused {
                        Stats::inc(&self.stats.peer_rejected_paused);
                    } else {
                        Stats::inc(&self.stats.peer_rejected_capacity);
                    }
                    let _ = admit.send(None);
                }
            }
            Command::Scrape(hash, reply) => {
                Stats::inc(&self.stats.scrapes_requested);
                if self.paused {
                    let _ = reply.send(None);
                } else if self.scrape_queue.len() < MAX_SCRAPE_QUEUE {
                    self.scrape_queue.push_back((hash, reply));
                } else {
                    Stats::inc(&self.stats.scrapes_dropped);
                    let _ = reply.send(None);
                }
            }
        }
    }

    /// Writes `dht.harvest_summary`: what was harvested since the last
    /// one, if anything was.
    fn summarize(&mut self, now_i: Instant) {
        let s = &self.stats;
        let g = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
        let now = [
            g(&s.harvested_sample),
            g(&s.harvested_get_peers),
            g(&s.harvested_announce),
            g(&s.lookups_completed),
            g(&s.harvest_dropped),
        ];
        let (then, before) = self.last_summary;
        let d = |i: usize| now[i].saturating_sub(before[i]);
        if (0..5).any(|i| d(i) > 0) {
            lodestar_log::log_event!(
                crate::log_events::HARVEST_SUMMARY,
                seconds = now_i.duration_since(then).as_secs(),
                sample = d(0),
                get_peers = d(1),
                announce = d(2),
                lookups = d(3),
                dropped = d(4)
            );
        }
        self.last_summary = (now_i, now);
    }

    /// Hands a hash to the pipeline. Sightings without peers are
    /// deduplicated; announces and lookup results always pass.
    fn harvest_hash(&mut self, hash: InfoHash, source: Source, peers: Vec<SocketAddr>) {
        // Paused, nothing is harvested; the answers of lookups already
        // running still reach the pool that asked.
        if self.paused && source != Source::Lookup {
            return;
        }
        let new = self.seen.insert(&hash);
        if new {
            if self.recent.len() == RECENT_SAMPLES {
                self.recent.pop_front();
            }
            self.recent.push_back(hash);
        }
        let counter = match source {
            Source::Sample if new => &self.stats.harvested_sample,
            Source::GetPeers if new => &self.stats.harvested_get_peers,
            Source::Announce => &self.stats.harvested_announce,
            Source::Lookup => &self.stats.lookups_completed,
            Source::Sample | Source::GetPeers => return,
        };
        Stats::inc(counter);
        lodestar_log::log_event!(
            crate::log_events::KEY_HARVESTED,
            torrent = lodestar_log::Torrent(&hash.0),
            source = source_label(source),
            peers = peers.len()
        );
        if let Some(tx) = &self.harvest {
            let h = Harvest {
                info_hash: hash,
                source,
                peers,
            };
            if tx.try_send(h).is_err() {
                Stats::inc(&self.stats.harvest_dropped);
            }
        }
    }

    fn send(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        to: SocketAddr,
        msg: &Message<'_>,
    ) -> Sent {
        if self.cfg.deny.as_ref().is_some_and(|d| d.denies(to.ip())) {
            Stats::inc(&self.stats.denied);
            return Sent::Failed;
        }
        self.out.clear();
        msg.encode_into(&mut self.out);
        match sockets[family].send_to(&self.out, to) {
            Ok(_) => {
                Stats::inc(&self.stats.packets_out);
                // Every packet we send (query, reply or error) opens or
                // keeps open a NAT mapping to `to`, so its queries stop
                // proving anything about our port. Renewed each time: a
                // node we talk to all the time never ages out.
                self.contacted.remember(&contact_key(to.ip()));
                Sent::Yes
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Sent::Throttled,
            Err(_) => {
                Stats::inc(&self.stats.send_errors);
                Sent::Failed
            }
        }
    }

    /// Sends a query if the rate limit and outstanding cap allow it.
    fn query(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        to: SocketAddr,
        node: Option<NodeId>,
        method: Method<'_>,
        purpose: Purpose,
    ) -> bool {
        self.try_query(sockets, family, to, node, method, purpose) == Sent::Yes
    }

    /// [`Actor::query`], telling a throttled query from a failed send.
    fn try_query(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        to: SocketAddr,
        node: Option<NodeId>,
        method: Method<'_>,
        purpose: Purpose,
    ) -> Sent {
        let to = lodestar_network_policy::normalize(to);
        if !endpoint_allowed(&self.cfg, to) {
            Stats::inc(&self.stats.denied);
            return Sent::Failed;
        }
        match self.dispatch_query(sockets, family, to, node, method, purpose) {
            Ok(_) => Sent::Yes,
            Err(sent) => sent,
        }
    }

    /// A query to a resolved bootstrap router. Fake-ip DNS and DNS64 answer
    /// public router names inside 198.18.0.0/15 and NAT64 prefixes, which
    /// only these addresses may use; the blocklist and every other policy
    /// check still apply. Anything else gets the full policy.
    fn query_seed(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        to: SocketAddr,
        method: Method<'_>,
        lid: u32,
    ) -> Sent {
        let to = lodestar_network_policy::normalize(to);
        if !self.bootstrap.contains(&to) {
            return self.try_query(sockets, family, to, None, method, Purpose::Lookup(lid));
        }
        if !lodestar_network_policy::allows_seed(to, self.cfg.allow_private)
            || self.cfg.deny.as_ref().is_some_and(|d| d.denies(to.ip()))
        {
            Stats::inc(&self.stats.denied);
            return Sent::Failed;
        }
        match self.dispatch_query(sockets, family, to, None, method, Purpose::Lookup(lid)) {
            Ok(tid) => {
                self.seed_tids.insert(tid);
                Sent::Yes
            }
            Err(sent) => sent,
        }
    }

    /// Sends a query whose destination already passed a policy check, and
    /// returns its transaction id.
    fn dispatch_query(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        to: SocketAddr,
        node: Option<NodeId>,
        method: Method<'_>,
        purpose: Purpose,
    ) -> Result<u64, Sent> {
        if self.families[family].ipv6 != to.is_ipv6() {
            Stats::inc(&self.stats.denied);
            return Err(Sent::Failed);
        }
        let now = Instant::now();
        if self.pending.len() >= self.cfg.intensity.max_outstanding()
            || !self.query_rate.try_take(now)
        {
            Stats::inc(&self.stats.throttled);
            return Err(Sent::Throttled);
        }
        // Never reuse a wire token in this process, even after success.
        let Some(tid) = self.next_tid.checked_add(1) else {
            return Err(Sent::Throttled);
        };
        self.next_tid = tid;
        if matches!(purpose, Purpose::Sample) && !self.sampler.reserve(to, tid, now) {
            Stats::inc(&self.stats.sample_deferred);
            return Err(Sent::Throttled);
        }
        let tid_bytes = tid.to_be_bytes();
        let id = match node {
            Some(n) => self.id_for(&n),
            None => self.ids[0],
        };
        let msg = Message {
            transaction_id: &tid_bytes,
            version: Some(VERSION),
            ip: None,
            read_only: self.cfg.read_only,
            body: Body::Query(Query { id, method }),
        };
        let sent = self.send(sockets, family, to, &msg);
        if sent != Sent::Yes {
            if matches!(purpose, Purpose::Sample) {
                self.sampler.abort(to, tid);
            }
            return Err(sent);
        }
        Stats::inc(&self.stats.queries_out);
        *self.pending_addrs.entry(to).or_default() += 1;
        self.pending.insert(
            tid,
            Pending {
                to,
                id: node,
                sent: now,
                family,
                purpose,
            },
        );
        Ok(tid)
    }

    fn take_pending(&mut self, tid: u64) -> Pending {
        let p = self.pending.remove(&tid).expect("caller checked the tid");
        if self.seed_tids.remove(&tid) {
            self.end_seed_round_if_done();
        }
        if let Purpose::Lookup(lid) = p.purpose {
            if let Some(r) = self
                .lookups
                .get(&lid)
                .and_then(|l| l.owner)
                .and_then(|o| self.peer_requests.get_mut(&o))
            {
                r.inflight = r.inflight.saturating_sub(1);
            }
        }
        if let MapEntry::Occupied(mut e) = self.pending_addrs.entry(p.to) {
            *e.get_mut() -= 1;
            if *e.get() == 0 {
                e.remove();
            }
        }
        p
    }

    fn on_packet(&mut self, sockets: &[SendSocket], family: usize, data: &[u8], from: SocketAddr) {
        Stats::inc(&self.stats.packets_in);
        if self.cfg.deny.as_ref().is_some_and(|d| d.denies(from.ip())) {
            Stats::inc(&self.stats.denied);
            return;
        }
        let Ok(msg) = krpc::parse(data) else {
            Stats::inc(&self.stats.malformed_in);
            return;
        };
        match &msg.body {
            Body::Query(q) => {
                Stats::inc(&self.stats.queries_in);
                if !self.contacted.contains(&contact_key(from.ip())) {
                    Stats::inc(&self.stats.queries_in_unsolicited);
                    self.unsolicited_from.note(from.ip(), Instant::now());
                }
                self.on_query(sockets, family, &msg, q, from);
            }
            Body::Response(r) => {
                Stats::inc(&self.stats.responses_in);
                self.on_response(sockets, family, &msg, Some(r), from);
            }
            Body::Error(_) => {
                Stats::inc(&self.stats.errors_in);
                self.on_response(sockets, family, &msg, None, from);
            }
        }
    }

    /// Closest nodes for a reply, honouring BEP 32 `want`.
    fn nodes_for(&self, family: usize, target: &NodeId, want: Want) -> (Vec<u8>, Vec<u8>) {
        let now = self.now_secs();
        let (mut v4, mut v6) = (Vec::new(), Vec::new());
        let socket_v6 = self.families[family].ipv6;
        let want_v4 = want.v4 || (!want.v4 && !want.v6 && !socket_v6);
        let want_v6 = want.v6 || (!want.v4 && !want.v6 && socket_v6);
        for f in &self.families {
            let closest = f.table.closest(target, crate::routing::K, now);
            if !f.ipv6 && want_v4 {
                v4 = compact::encode_nodes(&closest, false);
            }
            if f.ipv6 && want_v6 {
                v6 = compact::encode_nodes(&closest, true);
            }
        }
        (v4, v6)
    }

    fn on_query(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        msg: &Message<'_>,
        q: &Query<'_>,
        from: SocketAddr,
    ) {
        let now = self.now_secs();
        if !msg.read_only {
            let node = NodeInfo {
                id: q.id,
                addr: from,
            };
            let table = &mut self.families[family].table;
            let worth_verifying = table.would_accept(&q.id, now);
            table.insert(node, Contact::Query, now);
            // A querier's address may be spoofed, so it only enters the
            // table once it answers a ping of ours.
            if worth_verifying && !self.cfg.read_only && !self.pending_addrs.contains_key(&from) {
                self.query(
                    sockets,
                    family,
                    from,
                    Some(q.id),
                    Method::Ping,
                    Purpose::Ping,
                );
            }
        }
        // BEP 43: a read-only node doesn't answer.
        if self.cfg.read_only {
            return;
        }
        if !self.reply_rate.try_take(Instant::now()) {
            Stats::inc(&self.stats.replies_dropped);
            return;
        }
        let id = self.id_for(&q.id);
        let target = match &q.method {
            Method::FindNode { target, .. } | Method::SampleInfohashes { target, .. } => *target,
            Method::GetPeers { info_hash, .. } => NodeId(info_hash.0),
            _ => q.id,
        };
        let want = match &q.method {
            Method::FindNode { want, .. }
            | Method::GetPeers { want, .. }
            | Method::SampleInfohashes { want, .. } => *want,
            _ => Want::default(),
        };
        let (v4, v6) = self.nodes_for(family, &target, want);
        let token;
        let now_i = Instant::now();
        let values = match &q.method {
            Method::GetPeers {
                info_hash, noseed, ..
            } => self
                .peer_store
                .peers(info_hash, self.families[family].ipv6, *noseed, now_i),
            _ => Vec::new(),
        };
        // BEP 33: filters only when asked, and only for a hash we store.
        let blooms = match &q.method {
            Method::GetPeers {
                info_hash,
                scrape: true,
                ..
            } => self.peer_store.blooms(info_hash, now_i),
            _ => None,
        };
        let samples: Vec<u8> = match &q.method {
            Method::SampleInfohashes { .. } => self.recent.iter().flat_map(|h| h.0).collect(),
            _ => Vec::new(),
        };
        let body = match &q.method {
            Method::Ping => Body::Response(Response {
                id,
                ..Response::default()
            }),
            Method::FindNode { .. } => Body::Response(Response {
                id,
                nodes: CompactNodes::v4(&v4).unwrap_or_default(),
                nodes6: CompactNodes::v6(&v6).unwrap_or_default(),
                ..Response::default()
            }),
            Method::GetPeers { .. } => {
                token = self.tokens.issue(&from.ip());
                Body::Response(Response {
                    id,
                    nodes: CompactNodes::v4(&v4).unwrap_or_default(),
                    nodes6: CompactNodes::v6(&v6).unwrap_or_default(),
                    token: Some(&token),
                    values: values.clone(),
                    seeds_bloom: blooms.as_ref().map(|(sd, _)| &sd.0),
                    peers_bloom: blooms.as_ref().map(|(_, pe)| &pe.0),
                    ..Response::default()
                })
            }
            Method::AnnouncePeer { token, .. } => {
                if self.tokens.is_valid(&from.ip(), token) {
                    Body::Response(Response {
                        id,
                        ..Response::default()
                    })
                } else {
                    Body::Error(ErrorBody {
                        code: error_code::PROTOCOL,
                        message: b"bad token",
                    })
                }
            }
            Method::SampleInfohashes { .. } => Body::Response(Response {
                id,
                nodes: CompactNodes::v4(&v4).unwrap_or_default(),
                nodes6: CompactNodes::v6(&v6).unwrap_or_default(),
                interval: Some(SAMPLE_INTERVAL),
                num: Some(self.recent.len() as u32),
                samples: Some(krpc::Samples::new(&samples).unwrap_or_default()),
                ..Response::default()
            }),
            Method::Unknown(_) => Body::Error(ErrorBody {
                code: error_code::METHOD_UNKNOWN,
                message: b"Method Unknown",
            }),
        };
        let announced = matches!(body, Body::Response(_));
        let reply = Message {
            transaction_id: msg.transaction_id,
            version: Some(VERSION),
            ip: Some(from),
            read_only: false,
            body,
        };
        self.send(sockets, family, from, &reply);

        // Passive harvest, after replying so the querier isn't kept waiting.
        match &q.method {
            Method::GetPeers { info_hash, .. } if self.cfg.harvest.passive => {
                self.harvest_hash(*info_hash, Source::GetPeers, Vec::new());
            }
            Method::AnnouncePeer {
                info_hash,
                port,
                implied_port,
                seed,
                ..
            } if announced => {
                let port = if *implied_port { from.port() } else { *port };
                if port != 0 {
                    let peer = SocketAddr::new(from.ip(), port);
                    self.peer_store.announce(*info_hash, peer, *seed, now_i);
                    if self.cfg.harvest.passive {
                        self.harvest_hash(*info_hash, Source::Announce, vec![peer]);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_response(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        msg: &Message<'_>,
        r: Option<&Response<'_>>,
        from: SocketAddr,
    ) {
        let Ok(tid) = <[u8; 8]>::try_from(msg.transaction_id).map(u64::from_be_bytes) else {
            return;
        };
        let Some(pending) = self.pending.get(&tid) else {
            return;
        };
        // Only the address we asked may answer; anything else is spoofed.
        if pending.to != from || pending.family != family {
            Stats::inc(&self.stats.spoofed);
            return;
        }
        if self.seed_tids.contains(&tid) {
            // A response or an error: the router is reachable.
            self.seed_answered = true;
            self.seed_warned = false;
        }
        let pending = self.take_pending(tid);
        self.stats
            .outstanding
            .store(self.pending.len() as u64, Ordering::Relaxed);
        let now = self.now_secs();

        if let Some(ip) = msg.ip {
            self.on_external_ip(from.ip(), ip);
        }
        // A node that answers under a different id than the one we queried
        // is suspicious: don't route to it and don't trust its answer.
        let consistent = match (pending.id, r) {
            (Some(expected), Some(r)) => expected == r.id,
            _ => true,
        };
        let responder = r.map(|r| r.id).or(pending.id);
        // A bootstrap router reached through the seed exemption answers,
        // but isn't routed to or handed to other nodes.
        let routable = endpoint_allowed(&self.cfg, from);
        if let (Some(id), true, false, true) = (responder, consistent, msg.read_only, routable) {
            let inserted = self.families[family].table.insert(
                NodeInfo { id, addr: from },
                Contact::Response,
                now,
            );
            if let Inserted::Pending(to_ping) = inserted {
                for n in to_ping.into_iter().take(2) {
                    self.query(
                        sockets,
                        family,
                        n.addr,
                        Some(n.id),
                        Method::Ping,
                        Purpose::Ping,
                    );
                }
            }
        }

        // Every node we hear about is a candidate for BEP 51 sampling.
        if let (Some(r), true) = (r, consistent) {
            let now_i = Instant::now();
            let nodes = if self.families[family].ipv6 {
                r.nodes6
            } else {
                r.nodes
            };
            for n in nodes.iter() {
                if endpoint_allowed(&self.cfg, n.addr)
                    && !Actor::is_self(&self.ids, &self.local_addrs, &n)
                {
                    self.sampler.offer(n, now_i);
                }
            }
        }

        if let Purpose::Sample = pending.purpose {
            let now_i = Instant::now();
            match (r, consistent) {
                (Some(r), true) => {
                    // A reply without any BEP 51 field means "not supported".
                    let supported =
                        r.samples.is_some() && r.interval.is_some_and(|i| i <= SAMPLE_INTERVAL);
                    for h in r.samples.iter().flat_map(|s| s.iter()) {
                        self.harvest_hash(h, Source::Sample, Vec::new());
                    }
                    let wait = if supported {
                        Duration::from_secs(u64::from(r.interval.unwrap_or(SAMPLE_INTERVAL)))
                    } else {
                        sampler::UNSUPPORTED_BACKOFF
                    };
                    if !self.sampler.complete(from, tid, wait, now_i) {
                        Stats::inc(&self.stats.sample_stale);
                    }
                }
                _ => {
                    Stats::inc(&self.stats.sample_failures);
                    self.sampler
                        .complete(from, tid, sampler::UNSUPPORTED_BACKOFF, now_i);
                }
            }
            return;
        }

        let Purpose::Lookup(lid) = pending.purpose else {
            return;
        };
        let Some(lookup) = self.lookups.get_mut(&lid) else {
            return;
        };
        match (r, consistent) {
            (Some(r), true) => {
                let nodes = if self.families[lookup.family].ipv6 {
                    r.nodes6
                } else {
                    r.nodes
                };
                let (ids, local) = (&self.ids, &self.local_addrs);
                let cfg = &self.cfg;
                let ipv6 = self.families[lookup.family].ipv6;
                let others = nodes
                    .iter()
                    .map(|mut n| {
                        n.addr = lodestar_network_policy::normalize(n.addr);
                        n
                    })
                    .filter(|n| {
                        n.addr.is_ipv6() == ipv6
                            && endpoint_allowed(cfg, n.addr)
                            && !Actor::is_self(ids, local, n)
                    });
                let mut unique = Set::default();
                let peers: Vec<_> = r
                    .values
                    .iter()
                    .copied()
                    .map(lodestar_network_policy::normalize)
                    .filter(|p| endpoint_allowed(cfg, *p) && unique.insert(*p))
                    .take(64)
                    .collect();
                match pending.id {
                    Some(id) => {
                        if lookup.scrape {
                            lookup.record_blooms(&id, r.seeds_bloom, r.peers_bloom);
                        }
                        lookup.on_response(&id, others, &peers);
                    }
                    None => {
                        lookup.extra_in_flight = lookup.extra_in_flight.saturating_sub(1);
                        lookup.on_response(&r.id, others, &peers);
                    }
                }
            }
            _ => match pending.id {
                Some(id) => lookup.on_failure(&id),
                None => lookup.extra_in_flight = lookup.extra_in_flight.saturating_sub(1),
            },
        }
        self.drive_lookup(sockets, lid);
    }

    fn on_external_ip(&mut self, reporter: IpAddr, reported: SocketAddr) {
        let Some(ip) = self.external.vote(reporter, reported) else {
            return;
        };
        if secure_id::is_exempt(&ip) || self.ids.iter().all(|id| secure_id::is_valid(id, &ip)) {
            return;
        }
        // Our address changed: take BEP 42 ids for it and rebuild the tables
        // around the new primary id.
        self.ids = secure_id::virtual_ids(&ip, self.ids.len(), &mut self.rng);
        let now = self.now_secs();
        for f in &mut self.families {
            let old = std::mem::replace(&mut f.table, RoutingTable::new(self.ids[0], f.ipv6));
            for e in old.entries() {
                if e.status(now) != Status::Bad {
                    f.table.insert(e.info, Contact::Response, now);
                }
            }
        }
    }

    fn start_peer_request(
        &mut self,
        sockets: &[SendSocket],
        hash: InfoHash,
        reply: Option<oneshot::Sender<Vec<SocketAddr>>>,
    ) {
        if reply.as_ref().is_some_and(|r| r.is_closed()) {
            return;
        }
        let owner = self.next_request;
        self.next_request = self.next_request.wrapping_add(1);
        while self.peer_requests.contains_key(&self.next_request) || self.next_request == owner {
            self.next_request = self.next_request.wrapping_add(1);
        }
        self.active_get_peers += 1;
        self.peer_requests.insert(
            owner,
            PeerRequest {
                hash,
                reply,
                remaining: self.families.len(),
                inflight: 0,
                sent: 0,
                peers: Set::default(),
                started: Instant::now(),
            },
        );
        for family in 0..self.families.len() {
            self.start_lookup_as(
                sockets,
                family,
                NodeId(hash.0),
                &[],
                &[],
                Some(hash),
                false,
                Some(owner),
            );
        }
    }

    /// Starts a BEP 33 scrape lookup; `reply` gets the estimate when it
    /// finishes.
    fn start_scrape(
        &mut self,
        sockets: &[SendSocket],
        hash: InfoHash,
        reply: oneshot::Sender<Option<DhtEstimate>>,
    ) {
        self.active_scrapes += 1;
        // A hash-bearing lookup always starts, under the next id; the
        // waiter is in place before the first drive, which may finish it.
        self.scrape_waiters.insert(self.next_lookup, reply);
        self.start_lookup_as(sockets, 0, NodeId(hash.0), &[], &[], Some(hash), true, None);
    }

    /// Answers a finished (or timed-out) scrape lookup with its counts.
    fn finish_scrape(&mut self, lid: u32, lookup: &Lookup) {
        self.active_scrapes -= 1;
        let (seeds, peers, responders) = lookup.scrape_blooms();
        Stats::inc(&self.stats.scrapes_completed);
        if responders > 0 {
            Stats::inc(&self.stats.scrapes_with_responders);
        }
        if let Some(reply) = self.scrape_waiters.remove(&lid) {
            let _ = reply.send(Some(DhtEstimate {
                seeds: seeds.count(),
                peers: peers.count(),
                responders,
            }));
        }
    }

    /// Lookups that aren't for a hash: bootstrap, refresh, exploration.
    fn maintenance_lookups(&self) -> usize {
        self.lookups
            .values()
            .filter(|l| l.info_hash.is_none())
            .count()
    }

    /// Starts a lookup (find_node, or get_peers when `info_hash` is set)
    /// seeded from the routing table, `known` nodes, and `seeds`
    /// (addresses whose ids we don't know yet).
    fn start_lookup(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        target: NodeId,
        known: &[NodeInfo],
        seeds: &[SocketAddr],
        info_hash: Option<InfoHash>,
    ) {
        self.start_lookup_as(
            sockets, family, target, known, seeds, info_hash, false, None,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn start_lookup_as(
        &mut self,
        sockets: &[SendSocket],
        family: usize,
        target: NodeId,
        known: &[NodeInfo],
        seeds: &[SocketAddr],
        info_hash: Option<InfoHash>,
        scrape: bool,
        owner: Option<u32>,
    ) {
        if info_hash.is_none() && self.maintenance_lookups() >= MAX_LOOKUPS {
            return;
        }
        let lid = self.next_lookup;
        self.next_lookup = self.next_lookup.wrapping_add(1);
        while self.lookups.contains_key(&self.next_lookup) || self.next_lookup == lid {
            self.next_lookup = self.next_lookup.wrapping_add(1);
        }
        let mut lookup = Lookup::new(target, family);
        lookup.info_hash = info_hash;
        lookup.scrape = scrape;
        lookup.owner = owner;
        for n in self.families[family]
            .table
            .closest(&target, 16, self.now_secs())
        {
            if endpoint_allowed(&self.cfg, n.addr) {
                lookup.add(n);
            }
        }
        for n in known {
            if endpoint_allowed(&self.cfg, n.addr)
                && !Actor::is_self(&self.ids, &self.local_addrs, n)
            {
                lookup.add(*n);
            }
        }
        self.lookups.insert(lid, lookup);
        // A router answers in the family it sees the query arrive in. Through
        // NAT64 that's IPv4, so an IPv6 family asks for its own (BEP 32).
        let ipv6 = self.families[family].ipv6;
        let want = Want {
            v4: !ipv6,
            v6: ipv6,
        };
        for addr in seeds {
            let method = Method::FindNode { target, want };
            match self.query_seed(sockets, family, *addr, method, lid) {
                Sent::Yes => {
                    if let Some(l) = self.lookups.get_mut(&lid) {
                        l.extra_in_flight += 1;
                    }
                }
                Sent::Throttled => self.seed_throttled += 1,
                Sent::Failed => self.seed_refused += 1,
            }
        }
        self.drive_lookup(sockets, lid);
    }

    /// Check every continuation, including response-driven ones, before dispatch.
    fn finish_if_due(&mut self, lid: u32, now: Instant) -> bool {
        let Some(l) = self.lookups.get(&lid) else {
            return true;
        };
        let request = l.owner.and_then(|o| self.peer_requests.get(&o));
        let expired = now.duration_since(l.started)
            >= if l.scrape {
                SCRAPE_DEADLINE
            } else {
                LOOKUP_DEADLINE
            }
            || request.is_some_and(|r| now.duration_since(r.started) >= LOOKUP_DEADLINE);
        let cancelled = request.is_some_and(|r| r.reply.as_ref().is_some_and(|s| s.is_closed()))
            || self.scrape_waiters.get(&lid).is_some_and(|s| s.is_closed());
        if expired || cancelled {
            if expired {
                Stats::inc(&self.stats.lookup_deadlines);
            }
            self.finish_lookup(lid);
            true
        } else {
            false
        }
    }

    fn drive_lookup(&mut self, sockets: &[SendSocket], lid: u32) {
        if self.finish_if_due(lid, Instant::now()) {
            return;
        }
        let Some(lookup) = self.lookups.get_mut(&lid) else {
            return;
        };
        let (family, target, info_hash, scrape) = (
            lookup.family,
            lookup.target,
            lookup.info_hash,
            lookup.scrape,
        );
        let owner = lookup.owner;
        let slots = owner
            .and_then(|o| self.peer_requests.get(&o))
            .map_or(3, |r| {
                if r.reply.as_ref().is_some_and(|s| s.is_closed()) {
                    0
                } else {
                    3usize
                        .saturating_sub(r.inflight)
                        .min(MAX_PEER_QUERIES.saturating_sub(r.sent))
                }
            });
        let next = lookup.next_limit(slots);
        let mut throttled = false;
        for n in next {
            if throttled {
                // The rest of this batch waits too.
                if let Some(l) = self.lookups.get_mut(&lid) {
                    l.retry_later(&n.id);
                }
                continue;
            }
            let method = match info_hash {
                Some(info_hash) => Method::GetPeers {
                    info_hash,
                    want: Want::default(),
                    scrape,
                    noseed: false,
                },
                None => Method::FindNode {
                    target,
                    want: Want::default(),
                },
            };
            let sent = self.try_query(
                sockets,
                family,
                n.addr,
                Some(n.id),
                method,
                Purpose::Lookup(lid),
            );
            let Some(l) = self.lookups.get_mut(&lid) else {
                continue;
            };
            match sent {
                Sent::Yes => {
                    if let Some(r) = owner.and_then(|o| self.peer_requests.get_mut(&o)) {
                        r.inflight += 1;
                        r.sent += 1;
                    }
                }
                // A scrape's budget was paid when it started, so running
                // short of queries delays it rather than spoiling it.
                Sent::Throttled => {
                    l.retry_later(&n.id);
                    throttled = true;
                    if !self.stalled.contains(&lid) {
                        self.stalled.push(lid);
                    }
                }
                Sent::Failed => l.on_failure(&n.id),
            }
        }
        let exhausted = owner
            .and_then(|o| self.peer_requests.get(&o))
            .is_some_and(|r| r.sent >= MAX_PEER_QUERIES && r.inflight == 0);
        if exhausted || self.lookups.get(&lid).is_some_and(Lookup::is_done) {
            self.finish_lookup(lid);
        } else if !self.stalled.contains(&lid) {
            // Also covers immediate send failure with fresh candidates but
            // no pending RPC to trigger another drive.
            self.stalled.push(lid);
        }
    }

    fn finish_lookup(&mut self, lid: u32) {
        // Settle RPC ownership before removing the lookup that identifies it.
        let tids: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(t, p)| {
                matches!(p.purpose, Purpose::Lookup(id) if id == lid).then_some(*t)
            })
            .collect();
        for t in tids {
            self.take_pending(t);
        }
        self.stalled.retain(|id| *id != lid);
        let Some(done) = self.lookups.remove(&lid) else {
            return;
        };
        if done.scrape {
            self.finish_scrape(lid, &done);
        } else if let Some(owner) = done.owner {
            let Some(r) = self.peer_requests.get_mut(&owner) else {
                return;
            };
            for p in done.peers {
                if r.peers.len() < 64 {
                    r.peers.insert(p);
                }
            }
            r.remaining -= 1;
            if r.remaining == 0 {
                let r = self.peer_requests.remove(&owner).expect("present");
                self.active_get_peers -= 1;
                let mut peers: Vec<_> = r.peers.into_iter().collect();
                peers.sort_unstable();
                if let Some(reply) = r.reply {
                    // A cancelled request isn't a completed lookup.
                    if reply.is_closed() {
                        Stats::inc(&self.stats.peer_cancelled);
                    } else {
                        Stats::inc(&self.stats.lookups_completed);
                        let _ = reply.send(peers);
                    }
                } else {
                    self.harvest_hash(r.hash, Source::Lookup, peers);
                }
            }
        }
    }

    /// Self-lookups seeded with saved nodes and the bootstrap routers.
    fn start_bootstrap(&mut self, sockets: &[SendSocket], saved: Vec<NodeInfo>) {
        self.last_bootstrap = Some(Instant::now());
        if self.seed_tids.is_empty() {
            // A new round; one still in flight carries on.
            self.seed_answered = false;
        }
        (self.seed_refused, self.seed_throttled) = (0, 0);
        for f in 0..self.families.len() {
            let ipv6 = self.families[f].ipv6;
            let known: Vec<NodeInfo> = saved
                .iter()
                .filter(|n| n.addr.is_ipv6() == ipv6)
                .copied()
                .collect();
            let routers: Vec<SocketAddr> = self
                .bootstrap
                .iter()
                .filter(|a| a.is_ipv6() == ipv6)
                .copied()
                .collect();
            let target = self.ids[0];
            self.start_lookup(sockets, f, target, &known, &routers, None);
        }
        // Every router refused at send, or none resolved.
        self.end_seed_round_if_done();
    }

    /// Warns once when a round ends with no bootstrap router answering.
    fn end_seed_round_if_done(&mut self) {
        if self.cfg.bootstrap.is_empty()
            || !self.seed_tids.is_empty()
            || self.seed_answered
            || self.seed_warned
        {
            return;
        }
        self.seed_warned = true;
        lodestar_log::log_event!(
            crate::log_events::BOOTSTRAP_UNREACHABLE,
            configured = self.cfg.bootstrap.len(),
            resolved = self.bootstrap.len(),
            refused = self.seed_refused,
            throttled = self.seed_throttled
        );
    }

    fn maintain(&mut self, sockets: &[SendSocket]) {
        let now_i = Instant::now();
        let now = self.now_secs();
        self.proof
            .tick(now_i, &self.ids, &self.stats, &mut self.sampler);

        // Timeouts.
        let timeout = self.cfg.query_timeout;
        let expired: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, p)| now_i.duration_since(p.sent) >= timeout)
            .map(|(tid, _)| *tid)
            .collect();
        let mut touched = Vec::new();
        for tid in expired {
            let p = self.take_pending(tid);
            Stats::inc(&self.stats.timeouts);
            if let Some(id) = p.id {
                self.families[p.family].table.failed(&id, now);
            }
            if let Purpose::Sample = p.purpose {
                Stats::inc(&self.stats.sample_failures);
                self.sampler
                    .complete(p.to, tid, SAMPLE_TIMEOUT_BACKOFF, now_i);
            }
            if let Purpose::Lookup(lid) = p.purpose {
                if let Some(l) = self.lookups.get_mut(&lid) {
                    match p.id {
                        Some(id) => l.on_failure(&id),
                        None => l.extra_in_flight = l.extra_in_flight.saturating_sub(1),
                    }
                    touched.push(lid);
                }
            }
        }
        for lid in touched {
            self.drive_lookup(sockets, lid);
        }

        self.tokens.maybe_rotate(now_i, &mut self.rng);

        for f in 0..self.families.len() {
            // Refresh stale buckets.
            let targets = {
                let rng = &mut self.rng;
                self.families[f].table.refresh_targets(now, || rng.random())
            };
            for target in targets.into_iter().take(2) {
                self.start_lookup(sockets, f, target, &[], &[], None);
            }
            // Ping questionable nodes.
            let to_ping: Vec<NodeInfo> = self.families[f]
                .table
                .questionable(now)
                .into_iter()
                .take(PINGS_PER_TICK)
                .collect();
            for n in to_ping {
                let already = self.pending_addrs.contains_key(&n.addr);
                if !already {
                    self.query(sockets, f, n.addr, Some(n.id), Method::Ping, Purpose::Ping);
                }
            }
        }

        self.peer_store.expire(now_i);
        self.sampler.prune(now_i);
        self.stats
            .sample_inflight
            .store(self.sampler.inflight() as u64, Ordering::Relaxed);
        self.stats
            .sample_cooling
            .store(self.sampler.cooling() as u64, Ordering::Relaxed);
        if now_i.duration_since(self.last_summary.0) >= crate::log_events::SUMMARY_EVERY {
            self.summarize(now_i);
        }
        while !self.paused && self.active_get_peers < MAX_GET_PEERS {
            let Some((hash, reply)) = self.get_peers_queue.pop_front() else {
                break;
            };
            self.start_peer_request(sockets, hash, reply);
        }
        self.get_peers_queue
            .retain(|(_, r)| r.as_ref().is_none_or(|r| !r.is_closed()));
        self.scrape_tick(sockets, now_i);
        if self.cfg.harvest.sample && !self.paused {
            self.sample_tick(sockets, now_i);
        }

        let thin = self.families.iter().any(|f| f.table.len() < HEALTHY_NODES);
        let due = self
            .last_bootstrap
            .is_none_or(|t| now_i.duration_since(t) >= self.cfg.tick * REBOOTSTRAP_TICKS);
        // Scrapes run all the time; only the other lookups hold this back.
        if thin && due && self.lookups.len() == self.active_scrapes {
            self.start_bootstrap(sockets, Vec::new());
        }

        if self.cfg.state_path.is_some()
            && self.proof.may_save_state()
            && now_i.duration_since(self.last_save) >= self.cfg.save_every
        {
            self.last_save = now_i;
            let _ = self.save();
        }

        self.stats
            .peer_active
            .store(self.active_get_peers as u64, Ordering::Relaxed);
        self.stats
            .peer_queued
            .store(self.get_peers_queue.len() as u64, Ordering::Relaxed);
        self.stats
            .lookup_scheduled
            .store(self.stalled.len() as u64, Ordering::Relaxed);
        self.stats
            .sample_frontier
            .store(self.sampler.frontier_len() as u64, Ordering::Relaxed);
        self.stats
            .sample_suppression_seconds
            .store(self.proof.suppression_seconds(now_i), Ordering::Relaxed);
        let nodes: usize = self.families.iter().map(|f| f.table.len()).sum();
        self.stats
            .routing_nodes
            .store(nodes as u64, Ordering::Relaxed);
        self.stats
            .outstanding
            .store(self.pending.len() as u64, Ordering::Relaxed);
    }

    /// One tick of BEP 33 scraping: resume lookups that ran short of
    /// queries, answer the ones past their deadline with what they have,
    /// and start queued scrapes as the budget allows.
    fn scrape_tick(&mut self, sockets: &[SendSocket], now_i: Instant) {
        let ids: Vec<_> = self.lookups.keys().copied().collect();
        for lid in ids {
            self.finish_if_due(lid, now_i);
        }
        for lid in std::mem::take(&mut self.stalled) {
            self.drive_lookup(sockets, lid);
        }
        while !self.paused && self.active_scrapes < MAX_SCRAPES {
            // A caller that stopped waiting doesn't cost a lookup.
            while self
                .scrape_queue
                .front()
                .is_some_and(|(_, reply)| reply.is_closed())
            {
                self.scrape_queue.pop_front();
            }
            if self.scrape_queue.is_empty() || !self.scrape_rate.try_take(now_i) {
                break;
            }
            let (hash, reply) = self.scrape_queue.pop_front().expect("checked above");
            self.start_scrape(sockets, hash, reply);
        }
    }

    /// One tick of BEP 51 traversal: send this tick's share of
    /// sample_infohashes queries to nodes that are due, refilling the
    /// frontier from the routing tables and exploring a random region of
    /// the keyspace when it runs low.
    fn sample_tick(&mut self, sockets: &[SendSocket], now_i: Instant) {
        // The suppression, or the restart proof's gate (CR-0d).
        if !self.proof.may_sample(now_i) {
            return;
        }
        if self.sampler.saturated() {
            Stats::inc(&self.stats.sample_deferred);
            return;
        }
        if self.sampler.frontier_len() < LOW_FRONTIER {
            for f in 0..self.families.len() {
                let entries: Vec<NodeInfo> =
                    self.families[f].table.entries().map(|e| e.info).collect();
                for n in entries {
                    if endpoint_allowed(&self.cfg, n.addr) {
                        self.sampler.offer(n, now_i);
                    }
                }
                let target = NodeId(self.rng.random());
                self.start_lookup(sockets, f, target, &[], &[], None);
            }
        }
        let qps = f64::from(self.cfg.intensity.queries_per_sec());
        let budget = ((qps * self.cfg.tick.as_secs_f64() * SAMPLE_SHARE).ceil() as usize).max(1);
        let cap = self.cfg.intensity.max_outstanding();
        for _ in 0..budget {
            // Leave a fifth of the outstanding cap for maintenance.
            if self.pending.len() * 5 >= cap * 4 {
                break;
            }
            let Some(n) = self.sampler.next(now_i) else {
                break;
            };
            let Some(f) = self
                .families
                .iter()
                .position(|f| f.ipv6 == n.addr.is_ipv6())
            else {
                continue;
            };
            let method = Method::SampleInfohashes {
                target: NodeId(self.rng.random()),
                want: Want::default(),
            };
            match self.try_query(sockets, f, n.addr, Some(n.id), method, Purpose::Sample) {
                Sent::Yes => Stats::inc(&self.stats.samples_sent),
                Sent::Throttled => {
                    self.sampler.offer(n, now_i);
                    break;
                }
                Sent::Failed => {}
            }
        }
    }

    fn state(&self) -> State {
        let now = self.now_secs();
        let nodes = self
            .families
            .iter()
            .flat_map(|f| f.table.entries())
            .filter(|e| e.status(now) != Status::Bad && e.last_response.is_some())
            .map(|e| e.info)
            .collect();
        State {
            ids: self.ids.clone(),
            external_ip: self.external.current,
            nodes,
        }
    }

    fn save(&self) -> Result<(), StateError> {
        let Some(path) = &self.cfg.state_path else {
            return Ok(());
        };
        self.state().save(path)
    }

    /// At shutdown, after the event loop: the restart proof's commit
    /// (S8–S11, which saves `dht-state.bin` itself), or today's save.
    fn finish(&mut self) -> Result<(), StateError> {
        if let Some(path) = self.cfg.state_path.clone() {
            let state = self.state();
            if let Some(result) = self
                .proof
                .commit(&mut self.sampler, &state, &path, &self.stats)
            {
                // A failure is counted and logged, and leaves the marker
                // dirty: the next start suppresses. It isn't a node error.
                let _ = result;
                return Ok(());
            }
            // An on-run that never got the lock leaves `dht-state.bin`
            // to the run that holds it (§6.7).
            if !self.proof.may_save_state() {
                return Ok(());
            }
        }
        self.save()
    }
}

/// A harvest source as a label.
fn source_label(s: Source) -> &'static str {
    match s {
        Source::Sample => "sample",
        Source::GetPeers => "get_peers",
        Source::Announce => "announce",
        Source::Lookup => "lookup",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn fixture() -> (Actor, Vec<SendSocket>) {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let addr = socket.local_addr().unwrap();
        let cfg = Config {
            bind: vec![addr],
            bootstrap: vec![],
            allow_private: true,
            sample_start_delay: Some(Duration::ZERO),
            ..Default::default()
        };
        let actor = Actor::new(
            cfg,
            Arc::new(Stats::default()),
            vec![addr],
            None,
            vec![],
            None,
            Instant::now(),
            StdRng::seed_from_u64(3),
        );
        (actor, vec![SendSocket::Udp(socket)])
    }

    fn candidate(addr: SocketAddr) -> NodeInfo {
        NodeInfo {
            id: NodeId([42; 20]),
            addr,
        }
    }

    fn reply(a: &mut Actor, sockets: &[SendSocket], tid: u64, peers: Vec<SocketAddr>) {
        let pending = &a.pending[&tid];
        let (family, addr, id) = (pending.family, pending.to, pending.id.unwrap());
        deliver_reply(a, sockets, tid, family, addr, id, peers);
    }

    fn deliver_reply(
        a: &mut Actor,
        sockets: &[SendSocket],
        tid: u64,
        family: usize,
        addr: SocketAddr,
        id: NodeId,
        peers: Vec<SocketAddr>,
    ) {
        let bytes = tid.to_be_bytes();
        let response = Response {
            id,
            values: peers,
            ..Default::default()
        };
        let msg = Message {
            transaction_id: &bytes,
            version: None,
            ip: None,
            read_only: false,
            body: Body::Response(response.clone()),
        };
        a.on_response(sockets, family, &msg, Some(&response), addr);
    }

    fn assert_peer_ownership_drained(a: &Actor) {
        assert!(a.pending.is_empty());
        assert!(a.pending_addrs.is_empty());
        assert!(a.lookups.is_empty());
        assert!(a.stalled.is_empty());
        assert!(a.peer_requests.is_empty());
        assert_eq!(a.active_get_peers, 0);
    }

    #[test]
    fn cancelled_same_hash_retry_ignores_the_old_wire_response() {
        let (mut a, sockets) = fixture();
        let remote: SocketAddr = "127.0.0.1:65530".parse().unwrap();
        a.families[0]
            .table
            .insert(candidate(remote), Contact::Response, 0);
        let hash = InfoHash([9; 20]);
        let (tx, rx) = oneshot::channel();
        a.start_peer_request(&sockets, hash, Some(tx));
        let old_tid = *a.pending.keys().next().unwrap();
        drop(rx);
        a.scrape_tick(&sockets, Instant::now());
        assert_peer_ownership_drained(&a);
        let (tx, mut rx) = oneshot::channel();
        a.start_peer_request(&sockets, hash, Some(tx));
        let new_tid = *a.pending.keys().next().unwrap();
        assert_ne!(old_tid, new_tid);
        deliver_reply(
            &mut a,
            &sockets,
            old_tid,
            0,
            remote,
            candidate(remote).id,
            vec![remote],
        );
        assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert!(a.pending.contains_key(&new_tid));
        reply(&mut a, &sockets, new_tid, vec![]);
        assert_eq!(rx.try_recv().unwrap(), Vec::<SocketAddr>::new());
        assert_peer_ownership_drained(&a);
        assert_eq!(a.stats.snapshot().peer_cancelled, 1);
        assert_eq!(
            a.stats.snapshot().lookups_completed,
            1,
            "the cancelled request isn't a completed lookup"
        );
    }

    #[test]
    fn dual_stack_completion_and_deadline_drain_both_family_orders() {
        for v6_first in [false, true] {
            let (mut a, mut sockets) = fixture();
            // Skipped where the host has no IPv6.
            let Ok(v6) = std::net::UdpSocket::bind("[::1]:0") else {
                return;
            };
            v6.set_nonblocking(true).unwrap();
            sockets.push(SendSocket::Udp(v6));
            a.families.push(Family {
                table: RoutingTable::new(a.ids[0], true),
                ipv6: true,
            });
            if v6_first {
                a.families.swap(0, 1);
                sockets.swap(0, 1);
            }
            for family in &mut a.families {
                let addr = if family.ipv6 {
                    "[::1]:65530"
                } else {
                    "127.0.0.1:65530"
                };
                family
                    .table
                    .insert(candidate(addr.parse().unwrap()), Contact::Response, 0);
            }
            let peer: SocketAddr = "127.0.0.1:60000".parse().unwrap();
            let (tx, mut rx) = oneshot::channel();
            a.start_peer_request(&sockets, InfoHash([10; 20]), Some(tx));
            let tids: Vec<_> = a.pending.keys().copied().collect();
            assert_eq!(tids.len(), 2);
            reply(&mut a, &sockets, tids[0], vec![peer]);
            assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
            assert_eq!(a.active_get_peers, 1);
            reply(&mut a, &sockets, tids[1], vec![peer]);
            assert_eq!(
                rx.try_recv().unwrap(),
                vec![peer],
                "deduplicated across legs"
            );
            assert_peer_ownership_drained(&a);
            assert_eq!(a.stats.snapshot().lookups_completed, 1);

            let (tx, mut rx) = oneshot::channel();
            a.start_peer_request(&sockets, InfoHash([11; 20]), Some(tx));
            assert_eq!(a.pending.len(), 2);
            for request in a.peer_requests.values_mut() {
                request.started -= LOOKUP_DEADLINE;
            }
            a.scrape_tick(&sockets, Instant::now());
            assert!(rx.try_recv().unwrap().is_empty());
            assert_peer_ownership_drained(&a);
            a.scrape_tick(&sockets, Instant::now());
            assert_eq!(
                a.stats.snapshot().lookups_completed,
                2,
                "one terminal result per request"
            );
        }
    }

    #[test]
    fn abandoned_admission_and_pause_leave_no_queued_ownership() {
        let (mut a, sockets) = fixture();
        let (tx, rx) = oneshot::channel();
        drop(rx);
        a.on_command(Command::RequestPeers(InfoHash([12; 20]), tx));
        assert!(a.get_peers_queue.is_empty());
        assert_eq!(a.stats.snapshot().peer_admitted, 0);
        let (tx, mut rx) = oneshot::channel();
        a.on_command(Command::RequestPeers(InfoHash([12; 20]), tx));
        let mut done = rx.try_recv().unwrap().unwrap();
        assert_eq!(a.get_peers_queue.len(), 1);
        a.on_command(Command::SetPaused(true));
        // Dropped, not answered: the pool sees a cancel, not an attempt.
        assert_eq!(done.try_recv(), Err(oneshot::error::TryRecvError::Closed));
        assert!(a.get_peers_queue.is_empty());
        a.maintain(&sockets);
        assert_peer_ownership_drained(&a);
        assert_eq!(a.stats.snapshot().lookups_completed, 0);
    }

    #[test]
    fn transaction_ids_start_in_the_lower_half() {
        for seed in 0..64 {
            let a = Actor::new(
                Config::default(),
                Arc::new(Stats::default()),
                vec![],
                None,
                vec![],
                None,
                Instant::now(),
                StdRng::seed_from_u64(seed),
            );
            assert!(a.next_tid < 1 << 63, "{seed}: {}", a.next_tid);
        }
    }

    /// A fixture that applies the public outbound policy, as in production.
    fn public_fixture(seeds: Vec<SocketAddr>) -> (Actor, Vec<SendSocket>) {
        let (mut a, sockets) = fixture();
        a.cfg.allow_private = false;
        a.bootstrap = seeds;
        (a, sockets)
    }

    #[test]
    fn fake_ip_seeds_pass_the_policy_other_special_ranges_and_learned_nodes_do_not() {
        let fake_ip: SocketAddr = "198.18.0.7:6881".parse().unwrap();
        let lan: SocketAddr = "10.0.0.1:6881".parse().unwrap();
        let (mut a, sockets) = public_fixture(vec![fake_ip, lan]);
        assert!(!endpoint_allowed(&a.cfg, fake_ip), "special range");
        // An empty query budget stops a send that passed the policy, so no
        // packet leaves the host.
        a.query_rate.tokens = 0.0;
        a.query_rate.rate = 0.0;
        a.start_bootstrap(&sockets, Vec::new());
        // The fake-ip seed passed the policy and met the budget; the LAN
        // seed was refused by the policy.
        let s = a.stats.snapshot();
        assert_eq!(s.denied, 1);
        assert_eq!((a.seed_throttled, a.seed_refused), (1, 1));
        assert_eq!(s.queries_out + s.send_errors, 0);
        // The same address learned from the network is refused.
        assert!(!a.query(
            &sockets,
            0,
            fake_ip,
            Some(NodeId([7; 20])),
            Method::Ping,
            Purpose::Ping,
        ));
        assert_eq!(a.stats.snapshot().denied, 2);
        // So is an unconfigured address sent as a seed.
        let other: SocketAddr = "198.18.0.8:6881".parse().unwrap();
        assert_eq!(
            a.query_seed(&sockets, 0, other, Method::Ping, u32::MAX),
            Sent::Failed
        );
        assert_eq!(a.stats.snapshot().denied, 3);
        // The blocklist still applies to seeds.
        let denied_ip = fake_ip.ip();
        a.cfg.deny = Some(super::super::Deny(Arc::new(move |ip| ip == denied_ip)));
        assert_eq!(
            a.query_seed(&sockets, 0, fake_ip, Method::Ping, u32::MAX),
            Sent::Failed
        );
        assert_eq!(a.stats.snapshot().denied, 4);
    }

    #[test]
    fn a_nat64_seed_passes_the_policy_and_asks_for_ipv6_nodes() {
        let nat64: SocketAddr = "[64:ff9b::5db8:d822]:6881".parse().unwrap();
        let (mut a, mut sockets) = public_fixture(vec![nat64]);
        assert!(!endpoint_allowed(&a.cfg, nat64), "special range");
        // An IPv6 family. Its socket is an IPv4 stand-in, so the send fails
        // on every host, after the policy and with the packet encoded.
        let stand_in = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        stand_in.set_nonblocking(true).unwrap();
        sockets.push(SendSocket::Udp(stand_in));
        a.families.push(Family {
            table: RoutingTable::new(a.ids[0], true),
            ipv6: true,
        });
        a.families.swap(0, 1);
        sockets.swap(0, 1);
        a.start_bootstrap(&sockets, Vec::new());
        let s = a.stats.snapshot();
        assert_eq!(s.denied, 0, "the NAT64 seed isn't refused by policy");
        assert_eq!(s.send_errors, 1);
        let query = krpc::parse(&a.out).unwrap();
        let Body::Query(Query {
            method: Method::FindNode { want, .. },
            ..
        }) = query.body
        else {
            panic!("a find_node: {query:?}");
        };
        assert_eq!(
            want,
            Want {
                v4: false,
                v6: true
            }
        );
        // A NAT64 address learned from the network is still refused.
        assert!(!a.query(
            &sockets,
            0,
            nat64,
            Some(NodeId([7; 20])),
            Method::Ping,
            Purpose::Ping,
        ));
        assert_eq!(a.stats.snapshot().denied, 1);
    }

    /// Stands in for a seed query the socket sent, so a test can deliver
    /// the answer from an address it can't bind.
    fn pretend_seed_sent(a: &mut Actor, to: SocketAddr) -> u64 {
        let lid = a.next_lookup;
        a.next_lookup += 1;
        let mut lookup = Lookup::new(a.ids[0], 0);
        lookup.extra_in_flight = 1;
        a.lookups.insert(lid, lookup);
        a.next_tid += 1;
        let tid = a.next_tid;
        a.pending.insert(
            tid,
            Pending {
                to,
                id: None,
                sent: Instant::now(),
                family: 0,
                purpose: Purpose::Lookup(lid),
            },
        );
        *a.pending_addrs.entry(to).or_default() += 1;
        a.seed_tids.insert(tid);
        tid
    }

    #[test]
    fn a_fake_ip_router_bootstraps_and_its_learned_nodes_get_the_full_policy() {
        let router: SocketAddr = "198.18.0.7:6881".parse().unwrap();
        let (mut a, sockets) = public_fixture(vec![router]);
        let tid = pretend_seed_sent(&mut a, router);
        let learned_fake_ip = NodeInfo {
            id: NodeId([0x31; 20]),
            addr: "198.18.1.1:6881".parse().unwrap(),
        };
        let learned_lan = NodeInfo {
            id: NodeId([0x33; 20]),
            addr: "192.168.1.9:6881".parse().unwrap(),
        };
        let learned_public = NodeInfo {
            id: NodeId([0x32; 20]),
            addr: "8.8.8.8:6881".parse().unwrap(),
        };
        let raw = compact::encode_nodes(&[learned_fake_ip, learned_lan, learned_public], false);
        let response = Response {
            id: NodeId([0x30; 20]),
            nodes: CompactNodes::v4(&raw).unwrap(),
            ..Default::default()
        };
        let bytes = tid.to_be_bytes();
        let msg = Message {
            transaction_id: &bytes,
            version: None,
            ip: None,
            read_only: false,
            body: Body::Response(response.clone()),
        };
        let before = a.stats.snapshot();
        let frontier = a.sampler.frontier_len();
        a.on_response(&sockets, 0, &msg, Some(&response), router);
        assert!(a.seed_answered && a.seed_tids.is_empty());
        // Only the public node is a sampling candidate or gets queried.
        assert_eq!(a.sampler.frontier_len(), frontier + 1);
        let after = a.stats.snapshot();
        let tried = |s: &super::super::StatsSnapshot| s.queries_out + s.send_errors + s.denied;
        assert_eq!(tried(&after), tried(&before) + 1);
        assert!(!a.pending_addrs.contains_key(&learned_fake_ip.addr));
        assert!(!a.pending_addrs.contains_key(&learned_lan.addr));
        // The router isn't routed to or handed to other nodes.
        assert_eq!(a.families[0].table.len(), 0);
    }

    #[test]
    fn every_refused_or_silent_router_warns_once_per_outage() {
        use crate::log_events::BOOTSTRAP_UNREACHABLE;
        // The event's count is process-wide, shared by every test in this
        // binary. It's exact here only because this is the one test that
        // configures routers (`cfg.bootstrap`), which the warning requires;
        // a new test that does must not run its rounds alongside this one.
        let count = || BOOTSTRAP_UNREACHABLE::EVENT.count();
        let (mut a, sockets) = public_fixture(vec![
            "224.0.0.1:6881".parse().unwrap(),
            "198.18.0.7:0".parse().unwrap(),
            "10.0.0.1:6881".parse().unwrap(),
        ]);
        a.cfg.bootstrap = vec!["router.example:6881".into()];
        let start = count();
        a.start_bootstrap(&sockets, Vec::new());
        assert_eq!((a.seed_refused, a.seed_throttled), (3, 0));
        assert!(a.seed_warned);
        assert_eq!(count(), start + 1, "every router refused");
        a.start_bootstrap(&sockets, Vec::new());
        assert_eq!(count(), start + 1, "once, not every round");

        // A router answers: re-armed. Then it goes silent and times out.
        let router: SocketAddr = "198.18.0.7:6881".parse().unwrap();
        a.bootstrap = vec![router];
        let tid = pretend_seed_sent(&mut a, router);
        let bytes = tid.to_be_bytes();
        let msg = Message {
            transaction_id: &bytes,
            version: None,
            ip: None,
            read_only: false,
            body: Body::Error(ErrorBody {
                code: error_code::GENERIC,
                message: b"busy",
            }),
        };
        a.on_response(&sockets, 0, &msg, None, router);
        assert!(!a.seed_warned, "an error reply proves the router is there");
        a.seed_answered = false;
        pretend_seed_sent(&mut a, router);
        a.cfg.query_timeout = Duration::ZERO;
        a.maintain(&sockets);
        assert!(a.seed_tids.is_empty() && a.seed_warned);
        assert_eq!(count(), start + 2, "every router timed out");

        // Throttled routers are told apart from refused ones.
        a.seed_warned = false;
        a.query_rate.tokens = 0.0;
        a.query_rate.rate = 0.0;
        a.start_bootstrap(&sockets, Vec::new());
        assert_eq!((a.seed_refused, a.seed_throttled), (0, 1));
        assert_eq!(count(), start + 3);

        // No configured routers: nothing to warn about.
        let (mut quiet, sockets) = public_fixture(vec![]);
        quiet.start_bootstrap(&sockets, Vec::new());
        assert!(!quiet.seed_warned);
        assert_eq!(count(), start + 3);
    }

    #[test]
    fn sample_saturation_preserves_lookup_service_and_stale_wire_replies_are_ignored() {
        let (mut a, sockets) = fixture();
        let remote: SocketAddr = "127.0.0.1:65530".parse().unwrap();
        a.sampler = Sampler::with_capacity(1);
        assert!(a.sampler.reserve(remote, 0, Instant::now()));
        assert!(
            a.sampler
                .complete(remote, 0, sampler::MAX_INTERVAL, Instant::now())
        );
        assert!(a.sampler.saturated());
        a.families[0]
            .table
            .insert(candidate(remote), Contact::Response, 0);
        let (tx, mut rx) = oneshot::channel();
        a.start_peer_request(&sockets, InfoHash([8; 20]), Some(tx));
        assert_eq!(a.pending.len(), 1);
        let tid = *a.pending.keys().next().unwrap();
        let tid_bytes = tid.to_be_bytes();
        let response = Response {
            id: candidate(remote).id,
            values: vec![remote],
            ..Default::default()
        };
        let msg = Message {
            transaction_id: &tid_bytes,
            version: None,
            ip: None,
            read_only: false,
            body: Body::Response(response.clone()),
        };
        a.on_response(
            &sockets,
            0,
            &msg,
            Some(&response),
            "127.0.0.2:65530".parse().unwrap(),
        );
        assert_eq!(a.pending.len(), 1, "wrong endpoint cannot settle work");
        a.on_response(&sockets, 0, &msg, Some(&response), remote);
        assert_eq!(rx.try_recv().unwrap(), vec![remote]);
        a.on_response(&sockets, 0, &msg, Some(&response), remote);
        assert_eq!(a.stats.snapshot().lookups_completed, 1);
        assert_eq!(a.active_get_peers, 0);
        assert!(a.pending_addrs.is_empty());
        assert_eq!(a.sampler.cooling(), 1);
    }

    #[test]
    fn rejected_and_cancelled_admission_releases_every_slot() {
        let (mut a, sockets) = fixture();
        let mut held = Vec::new();
        for _ in 0..MAX_GET_PEERS {
            let (tx, mut rx) = oneshot::channel();
            a.on_command(Command::RequestPeers(InfoHash([1; 20]), tx));
            held.push(rx.try_recv().unwrap().unwrap());
        }
        let (tx, mut rx) = oneshot::channel();
        a.on_command(Command::RequestPeers(InfoHash([1; 20]), tx));
        assert!(rx.try_recv().unwrap().is_none());
        assert_eq!(a.stats.snapshot().peer_rejected_capacity, 1);
        a.on_command(Command::SetPaused(true));
        let (tx, mut rx) = oneshot::channel();
        a.on_command(Command::RequestPeers(InfoHash([1; 20]), tx));
        assert!(rx.try_recv().unwrap().is_none());
        let (tx, rx) = oneshot::channel();
        drop(rx);
        a.on_command(Command::RequestPeers(InfoHash([1; 20]), tx));
        let stats = a.stats.snapshot();
        assert_eq!(stats.peer_rejected, 3);
        assert_eq!(stats.peer_rejected_paused, 1);
        assert_eq!(stats.peer_rejected_caller_closed, 1);
        assert_eq!(stats.peer_ack_failed, 0);
        drop(held);
        a.maintain(&sockets);
        assert!(a.get_peers_queue.is_empty());
        assert!(a.peer_requests.is_empty());
        assert!(a.pending.is_empty());
        assert_eq!(a.active_get_peers, 0);
    }

    #[test]
    fn throttled_and_failed_lookups_have_a_continuation_and_deadline() {
        let (mut a, sockets) = fixture();
        a.query_rate.tokens = 0.0;
        a.query_rate.rate = 0.0;
        let candidates: Vec<_> = (1..=10)
            .map(|i| NodeInfo {
                id: NodeId([i; 20]),
                addr: "127.0.0.1:65530".parse().unwrap(),
            })
            .collect();
        a.start_lookup(&sockets, 0, NodeId([4; 20]), &candidates, &[], None);
        let lid = *a.lookups.keys().next().unwrap();
        assert!(a.pending.is_empty());
        assert!(a.stalled.contains(&lid));
        // Exercise permanent failure after admission (e.g. a live deny update).
        a.query_rate.tokens = 10.0;
        a.cfg.deny = Some(super::super::Deny(Arc::new(|_| true)));
        a.drive_lookup(&sockets, lid);
        assert!(a.lookups.contains_key(&lid));
        assert!(
            a.stalled.contains(&lid),
            "fresh candidates survive a whole failed batch"
        );
        for _ in 0..4 {
            a.drive_lookup(&sockets, lid);
        }
        assert!(!a.lookups.contains_key(&lid));
        assert!(!a.stalled.contains(&lid));
        a.cfg.deny = None;
        a.query_rate.tokens = 0.0;
        a.start_lookup(
            &sockets,
            0,
            NodeId([4; 20]),
            &[candidate("127.0.0.1:65530".parse().unwrap())],
            &[],
            None,
        );
        for l in a.lookups.values_mut() {
            l.started -= LOOKUP_DEADLINE;
        }
        a.scrape_tick(&sockets, Instant::now());
        assert!(a.lookups.is_empty());
        assert!(a.pending_addrs.is_empty());
    }

    #[test]
    fn dual_stack_cancellation_cleans_shared_rpc_ownership() {
        let (mut a, mut sockets) = fixture();
        // Skipped where the host has no IPv6.
        let Ok(v6) = std::net::UdpSocket::bind("[::1]:0") else {
            return;
        };
        v6.set_nonblocking(true).unwrap();
        sockets.push(SendSocket::Udp(v6));
        a.families.push(Family {
            table: RoutingTable::new(a.ids[0], true),
            ipv6: true,
        });
        for (family, addr) in ["127.0.0.1:65530", "[::1]:65530"].into_iter().enumerate() {
            a.families[family]
                .table
                .insert(candidate(addr.parse().unwrap()), Contact::Response, 0);
        }
        let (tx, rx) = oneshot::channel();
        a.start_peer_request(&sockets, InfoHash([7; 20]), Some(tx));
        assert_eq!(a.active_get_peers, 1);
        assert_eq!(a.lookups.len(), 2);
        assert_eq!(a.pending.len(), 2);
        for l in a.lookups.values_mut() {
            l.add(NodeInfo {
                id: NodeId([43; 20]),
                addr: if l.family == 0 {
                    "127.0.0.1:65530".parse().unwrap()
                } else {
                    "[::1]:65530".parse().unwrap()
                },
            });
        }
        for lid in a.lookups.keys().copied().collect::<Vec<_>>() {
            a.drive_lookup(&sockets, lid);
        }
        assert_eq!(
            a.pending.len(),
            3,
            "family legs share one concurrency budget"
        );
        assert!(a.peer_requests.values().all(|r| r.inflight <= 3));
        drop(rx);
        a.scrape_tick(&sockets, Instant::now());
        assert!(a.pending.is_empty());
        assert!(a.pending_addrs.is_empty());
        assert!(a.lookups.is_empty());
        assert!(a.stalled.is_empty());
        assert!(a.peer_requests.is_empty());
        assert_eq!(a.active_get_peers, 0);
    }

    #[test]
    fn sample_restart_suppression_and_wire_exhaustion_do_not_block_replies() {
        let (mut a, sockets) = fixture();
        let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = peer.local_addr().unwrap();
        a.sampler.offer(candidate(to), Instant::now());
        // A production start: the full suppression, no restart proof.
        a.cfg.sample_start_delay = None;
        a.proof = Proof::new(
            None,
            None,
            None,
            sampler::MAX_INTERVAL + a.cfg.query_timeout,
            a.cfg.tick,
            Instant::now(),
        );
        a.sample_tick(&sockets, Instant::now());
        assert_eq!(a.sampler.frontier_len(), 1);
        assert!(a.pending.is_empty());
        a.next_tid = u64::MAX;
        assert_eq!(
            a.try_query(&sockets, 0, to, None, Method::Ping, Purpose::Ping),
            Sent::Throttled
        );
        assert!(a.pending.is_empty());
        let msg = Message {
            transaction_id: b"reply",
            version: None,
            ip: None,
            read_only: false,
            body: Body::Response(Response {
                id: a.ids[0],
                ..Default::default()
            }),
        };
        assert_eq!(a.send(&sockets, 0, to, &msg), Sent::Yes);
        assert_eq!(a.stats.snapshot().packets_out, 1);
    }

    #[test]
    fn external_ip_needs_a_quorum_of_distinct_reporters() {
        let mut e = ExternalIp::default();
        let me: IpAddr = "203.0.113.9".parse().unwrap();
        let at = |port| SocketAddr::new(me, port);
        // One reporter repeating itself never reaches quorum.
        for _ in 0..10 {
            assert_eq!(e.vote("1.1.1.1".parse().unwrap(), at(6881)), None);
        }
        // Nor do many hosts of one /24: one network, one vote.
        for i in 2..=200u8 {
            assert_eq!(e.vote(IpAddr::from([1, 1, 1, i]), at(6881)), None);
        }
        assert_eq!(e.tally().voters, 1);
        for i in 2..=4u8 {
            assert_eq!(e.vote(IpAddr::from([1, 1, i, 1]), at(6881)), None);
        }
        assert_eq!(e.tally().ip, None, "4 reporters are short of quorum");
        assert_eq!(e.tally().voters, 4);
        assert_eq!(e.vote("1.1.5.1".parse().unwrap(), at(40000)), Some(me));
        // No change reported twice.
        assert_eq!(e.vote("1.1.6.1".parse().unwrap(), at(6881)), None);
        // A minority liar doesn't flip it.
        let liar: SocketAddr = "198.51.100.1:1".parse().unwrap();
        for i in 10..13u8 {
            assert_eq!(e.vote(IpAddr::from([2, 2, i, 1]), liar), None);
        }
        assert_eq!(e.current, Some(me));
        assert_eq!(
            e.tally(),
            ExternalVotes {
                ip: Some(me),
                port: Some(6881),
                voters: 9,
                agreeing: 6,
            }
        );
    }

    #[test]
    fn the_tally_is_bounded_and_needs_a_majority() {
        let mut e = ExternalIp::default();
        // Thousands of reporters keep only the last WINDOW votes.
        for i in 0..5000u32 {
            let reporter = IpAddr::from((i << 8).to_be_bytes());
            let reported = SocketAddr::from(([10, 0, (i % 7) as u8, 1], 1));
            e.vote(reporter, reported);
        }
        let t = e.tally();
        assert_eq!(t.voters, ExternalIp::WINDOW);
        // Seven addresses split the votes: no majority, no address.
        assert_eq!((t.ip, t.port), (None, None));
        assert!(t.agreeing <= ExternalIp::WINDOW / 7 + 1);
        assert_eq!(ExternalIp::default().tally(), ExternalVotes::default());
    }

    #[test]
    fn families_are_tallied_apart_and_v6_votes_per_64() {
        let mut e = ExternalIp::default();
        let me4: SocketAddr = "203.0.113.9:6881".parse().unwrap();
        let me6: SocketAddr = "[2001:db8::9]:6881".parse().unwrap();
        // Many IPv6 hosts of one /64 are one voter.
        for i in 1..100u16 {
            let r = IpAddr::from([0x2001, 0xdb8, 1, 1, 0, 0, 0, i]);
            e.vote(r, me6);
        }
        assert_eq!(ExternalIp::tally_of(&e.v6).voters, 1);
        // IPv6 reports don't dilute an IPv4 majority.
        for i in 1..=5u16 {
            e.vote(IpAddr::from([0x2001, 0xdb8, i + 1, 0, 0, 0, 0, 1]), me6);
        }
        for i in 1..=5u8 {
            e.vote(IpAddr::from([9, 9, i, 1]), me4);
        }
        let t = e.tally();
        assert_eq!((t.ip, t.voters, t.agreeing), (Some(me4.ip()), 5, 5));
        // Without an IPv4 majority, the IPv6 one is reported.
        let mut only6 = ExternalIp::default();
        for i in 1..=5u16 {
            only6.vote(IpAddr::from([0x2001, 0xdb8, i, 0, 0, 0, 0, 1]), me6);
        }
        assert_eq!(only6.tally().ip, Some(me6.ip()));
        // A v4-mapped report is the IPv4 address it maps.
        let mut mapped = ExternalIp::default();
        for i in 1..=5u8 {
            let at = if i % 2 == 0 {
                "[::ffff:203.0.113.9]:6881"
            } else {
                "203.0.113.9:6881"
            };
            mapped.vote(IpAddr::from([8, 8, i, 1]), at.parse().unwrap());
        }
        assert_eq!(
            (mapped.tally().ip, mapped.tally().agreeing),
            (Some(me4.ip()), 5)
        );
        // A v4-mapped reporter counts as its IPv4 network.
        assert_eq!(
            network("::ffff:1.2.3.4".parse().unwrap()),
            network("1.2.3.200".parse().unwrap())
        );
    }

    #[test]
    fn unsolicited_sources_are_distinct_networks_and_bounded() {
        let mut u = UnsolicitedFrom::default();
        let t0 = Instant::now();
        // Many hosts of one /24 are one source, at its latest query.
        for i in 1..50u8 {
            u.note(IpAddr::from([5, 5, 5, i]), t0);
        }
        u.note("5.5.5.1".parse().unwrap(), t0 + Duration::from_secs(5));
        assert_eq!(u.ages(t0 + Duration::from_secs(5)), vec![Duration::ZERO]);
        // Bounded: the oldest networks make room for new ones.
        for i in 0..200u32 {
            let t = t0 + Duration::from_secs(10 + u64::from(i));
            u.note(IpAddr::from((i << 8 | 0x0a00_0000).to_be_bytes()), t);
        }
        let ages = u.ages(t0 + Duration::from_secs(300));
        assert_eq!(ages.len(), UNSOLICITED_SOURCES);
        assert!(ages.iter().all(|a| *a <= Duration::from_secs(300 - 146)));
    }

    #[test]
    fn contact_keys_keep_families_and_addresses_apart() {
        let v4 = contact_key("1.2.3.4".parse().unwrap());
        let v6 = contact_key("::1.2.3.4".parse().unwrap());
        assert_ne!(v4, v6);
        assert_ne!(v4, contact_key("1.2.3.5".parse().unwrap()));
        let mut f = SeenFilter::new(100, 0.01);
        f.insert(&v4);
        assert!(f.contains(&v4));
        assert!(!f.contains(&v6));
    }

    #[test]
    fn token_bucket_refills_at_rate() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(10, t0);
        let taken = (0..20).filter(|_| b.try_take(t0)).count();
        assert_eq!(taken, 10, "starts full at one second's worth");
        assert!(!b.try_take(t0));
        assert!(b.try_take(t0 + Duration::from_millis(150)));
    }

    /// At Low, scrapes get 50 × 10% / 20 = 0.25 lookups a second: the
    /// bucket must still hold a whole token, or no scrape would ever start.
    #[test]
    fn slow_scrape_budget_still_starts_lookups() {
        let t0 = Instant::now();
        let per_sec = 50.0 * SCRAPE_SHARE / QUERIES_PER_SCRAPE;
        let mut b = TokenBucket::with_burst(per_sec, per_sec, t0);
        assert!(b.try_take(t0));
        assert!(!b.try_take(t0 + Duration::from_secs(3)));
        assert!(b.try_take(t0 + Duration::from_secs(4)));
    }
}

/// CR-0d at the wire: a run samples real peers with mixed intervals and
/// stops cleanly; the next run resumes and, on a paused clock, no peer
/// receives a `sample_infohashes` before its restored deadline (§6 item 6).
#[cfg(test)]
mod restart_wire {
    use super::*;
    use crate::restart::{Clock, Found};
    use rand::SeedableRng;
    use tokio::time::advance;

    #[derive(Debug)]
    struct SimClock {
        base: Duration,
        start: Instant,
    }

    impl Clock for SimClock {
        fn wall(&self) -> Duration {
            self.base + Instant::now().duration_since(self.start)
        }
        fn mono(&self) -> Instant {
            Instant::now()
        }
    }

    fn node(dir: &std::path::Path, clock: &Arc<SimClock>) -> (Actor, Vec<SendSocket>) {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let addr = socket.local_addr().unwrap();
        let cfg = Config {
            bind: vec![addr],
            bootstrap: vec![],
            allow_private: true,
            state_path: Some(dir.join("data").join("dht-state.bin")),
            restart: Some(super::super::RestartConfig {
                witness_dir: Some(dir.join("witness")),
                clock: clock.clone(),
                inline_io: true,
            }),
            ..Default::default()
        };
        let actor = Actor::new(
            cfg,
            Arc::new(Stats::default()),
            vec![addr],
            None,
            vec![],
            None,
            Instant::now(),
            StdRng::seed_from_u64(3),
        );
        (actor, vec![SendSocket::Udp(socket)])
    }

    /// `sample_infohashes` datagrams each peer has received since last asked.
    fn samples_received(peers: &[std::net::UdpSocket]) -> Vec<usize> {
        let mut buf = [0u8; 2048];
        peers
            .iter()
            .map(|p| {
                let mut n = 0;
                while let Ok((len, _)) = p.recv_from(&mut buf) {
                    if let Ok(m) = crate::krpc::parse(&buf[..len])
                        && let Body::Query(q) = m.body
                        && matches!(q.method, Method::SampleInfohashes { .. })
                    {
                        n += 1;
                    }
                }
                n
            })
            .collect()
    }

    /// One maintenance turn with every peer offered as a candidate.
    fn turn(a: &mut Actor, sockets: &[SendSocket], peers: &[SocketAddr]) {
        for p in peers {
            a.sampler.offer(
                NodeInfo {
                    id: NodeId([42; 20]),
                    addr: *p,
                },
                Instant::now(),
            );
        }
        a.maintain(sockets);
    }

    #[tokio::test(start_paused = true)]
    async fn no_restored_peer_is_sampled_before_its_deadline() {
        let root = std::env::temp_dir().join(format!("lodestar-wire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        let clock = Arc::new(SimClock {
            base: Duration::from_secs(1_700_000_000),
            start: Instant::now(),
        });
        let peer_sockets: Vec<std::net::UdpSocket> = (0..40)
            .map(|_| {
                let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                s.set_nonblocking(true).unwrap();
                s
            })
            .collect();
        let peers: Vec<SocketAddr> = peer_sockets
            .iter()
            .map(|s| s.local_addr().unwrap())
            .collect();

        // Run 1: first start suppresses; after the window, sample every
        // peer once and give each a different interval, 1 min to 6 h.
        let (mut a, sockets) = node(&root, &clock);
        turn(&mut a, &sockets, &peers);
        assert!(samples_received(&peer_sockets).iter().all(|&n| n == 0));
        advance(sampler::MAX_INTERVAL + a.cfg.query_timeout).await;
        for _ in 0..20 {
            turn(&mut a, &sockets, &peers);
            advance(a.cfg.tick).await;
        }
        assert!(
            samples_received(&peer_sockets).iter().all(|&n| n == 1),
            "each sampled once"
        );
        let tids: Vec<(u64, SocketAddr)> = a
            .pending
            .iter()
            .filter(|(_, p)| matches!(p.purpose, Purpose::Sample))
            .map(|(t, p)| (*t, p.to))
            .collect();
        for (i, (tid, to)) in tids.into_iter().enumerate() {
            // The last few stay in flight: they're sealed for the horizon.
            if i >= peers.len() - 4 {
                continue;
            }
            let wait = Duration::from_secs(60 + (i as u64) * 547);
            a.take_pending(tid);
            assert!(a.sampler.complete(to, tid, wait, Instant::now()));
        }
        // 960 more restrictions on addresses with no socket behind them,
        // made as a run's own samples would be, for 1,000 in all (§6.6).
        // Their sends are read from the actor's own pending queries.
        let synthetic: Vec<SocketAddr> = (0..960u32)
            .map(|i| SocketAddr::from(([127, 0, 3 + (i / 250) as u8, (i % 250) as u8 + 1], 7000)))
            .collect();
        for (i, s) in synthetic.iter().enumerate() {
            let token = 1 << 40 | i as u64;
            assert!(a.sampler.reserve(*s, token, Instant::now()));
            let wait = Duration::from_secs(60 + (i as u64 * 22) % 21_500);
            assert!(a.sampler.complete(*s, token, wait, Instant::now()));
        }
        let stop = Instant::now();
        a.finish().unwrap();
        // The truth: run 1's table at the stop, after S8 sealed what was
        // still in flight. Some samples timed out during run 1 and cool
        // for 6 h from their timeout; the rest have their interval.
        let truth: Map<SocketAddr, Instant> = a.sampler.restrictions(stop).collect();
        assert_eq!(
            truth.len(),
            peers.len() + synthetic.len(),
            "1,000 restrictions"
        );

        // Run 2, a minute later: resumes, and the wire shows every
        // restriction held to the instant.
        advance(Duration::from_secs(60)).await;
        let (mut b, sockets) = node(&root, &clock);
        b.maintain(&sockets);
        assert_eq!(b.proof.reason(), crate::restart::Reason::Resumed);
        let sealed = stop + sampler::MAX_INTERVAL + b.cfg.query_timeout;
        let deadline = |p: &SocketAddr| truth[p];
        let mut first_seen: Map<SocketAddr, Instant> = Map::default();
        let step = Duration::from_secs(30);
        let end = sealed + Duration::from_secs(120);
        let everyone: Vec<SocketAddr> = peers.iter().chain(&synthetic).copied().collect();
        while Instant::now() < end {
            turn(&mut b, &sockets, &everyone);
            for p in b.pending.values() {
                if matches!(p.purpose, Purpose::Sample) && synthetic.contains(&p.to) {
                    first_seen.entry(p.to).or_insert(Instant::now());
                }
            }
            // Answer nothing: timed-out samples back off, which only
            // delays later ones.
            for (i, n) in samples_received(&peer_sockets).into_iter().enumerate() {
                if n > 0 {
                    first_seen.entry(peers[i]).or_insert(Instant::now());
                }
            }
            advance(step).await;
        }
        for p in &everyone {
            let seen = first_seen.get(p).copied();
            let due = deadline(p);
            assert!(
                seen.is_none_or(|t| t >= due),
                "{p} sampled at {:?}, before its deadline {:?}",
                seen.map(|t| t.duration_since(stop)),
                due.duration_since(stop)
            );
            assert!(
                seen.is_some(),
                "{p} was never sampled again after its deadline"
            );
        }
        let counts = b.sampler.restored_counts();
        assert_eq!(counts.0, 0, "every restored restriction has expired");
        assert!(counts.1 > 0, "restored restrictions turned candidates away");
        // Those already over at the restart weren't seeded (one here).
        let restored = b.stats.restart_obligations_restored.load(Ordering::Relaxed);
        assert!(restored >= 990, "{restored}");
        assert_eq!(counts.2, restored, "each restored one expired unsampled");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn marker_bytes(root: &std::path::Path) -> Vec<u8> {
        std::fs::read(root.join("data").join(crate::restart::MARKER_FILE)).unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_paused_run_invalidates_writes_nothing_more_and_commits() {
        // §6.14: a start in the paused state still invalidates before any
        // sample; ticks while paused write nothing; a shutdown while paused
        // commits.
        let root = std::env::temp_dir().join(format!("lodestar-pause-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        let clock = Arc::new(SimClock {
            base: Duration::from_secs(1_700_000_000),
            start: Instant::now(),
        });
        let (mut a, sockets) = node(&root, &clock);
        a.paused = true;
        a.maintain(&sockets);
        let Found::Ok(m) = crate::restart::read_marker(&root.join("data")) else {
            panic!("the marker is written before any sample")
        };
        assert_eq!(m.state, crate::restart::MarkerState::Dirty);
        let dirty = marker_bytes(&root);
        for _ in 0..30 {
            advance(Duration::from_secs(60)).await;
            a.maintain(&sockets);
        }
        assert_eq!(marker_bytes(&root), dirty, "a pause writes nothing");
        a.finish().unwrap();
        let Found::Ok(m) = crate::restart::read_marker(&root.join("data")) else {
            panic!()
        };
        assert!(matches!(m.state, crate::restart::MarkerState::Clean { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }
}
