//! The node's event loop. See the module docs in `node/mod.rs`.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::net::UdpSocket as StdUdpSocket;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};

use lodestar_metainfo::InfoHash;

use super::lookup::Lookup;
use super::peers::PeerStore;
use super::sampler::{self, Sampler};
use super::seen::SeenFilter;
use super::tokens::Tokens;
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
const MAX_GET_PEERS_QUEUE: usize = 10_000;
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
const SAMPLE_TIMEOUT_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// IPs we queried, remembered so a query from them isn't counted as
/// unsolicited: one generation lasts at least ~3 minutes at Aggressive,
/// about the longest a home router keeps a UDP mapping open.
const CONTACTED_CAPACITY: usize = 200_000;

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

/// Majority vote over the external address other nodes report (BEP 42).
/// One vote per reporter, the last [`ExternalIp::WINDOW`] reporters.
#[derive(Default)]
struct ExternalIp {
    votes: VecDeque<(IpAddr, SocketAddr)>,
    current: Option<IpAddr>,
}

impl ExternalIp {
    const WINDOW: usize = 32;
    const QUORUM: usize = 5;

    /// Records a report; returns the new address when the majority changes.
    fn vote(&mut self, reporter: IpAddr, reported: SocketAddr) -> Option<IpAddr> {
        self.votes.retain(|(r, _)| *r != reporter);
        self.votes.push_back((reporter, reported));
        if self.votes.len() > Self::WINDOW {
            self.votes.pop_front();
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

    /// The leading address and its port, with the evidence. Ties go to
    /// the smaller value, so the answer doesn't depend on map order.
    fn tally(&self) -> ExternalVotes {
        let mut ips: HashMap<IpAddr, usize> = HashMap::new();
        for (_, addr) in &self.votes {
            *ips.entry(addr.ip()).or_default() += 1;
        }
        let Some((best, n)) = ips
            .into_iter()
            .max_by_key(|(ip, n)| (*n, std::cmp::Reverse(*ip)))
        else {
            return ExternalVotes::default();
        };
        let majority = n >= Self::QUORUM && n * 2 > self.votes.len();
        let port = majority
            .then(|| {
                let mut ports: HashMap<u16, usize> = HashMap::new();
                for (_, addr) in self.votes.iter().filter(|(_, a)| a.ip() == best) {
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
            voters: self.votes.len(),
            agreeing: n,
        }
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

struct Actor {
    cfg: Config,
    families: Vec<Family>,
    ids: Vec<NodeId>,
    external: ExternalIp,
    tokens: Tokens,
    pending: HashMap<u16, Pending>,
    /// Queries in flight per address, for cheap "already asked?" checks.
    pending_addrs: HashMap<SocketAddr, u32>,
    next_tid: u16,
    lookups: HashMap<u32, Lookup>,
    next_lookup: u32,
    query_rate: TokenBucket,
    reply_rate: TokenBucket,
    stats: Arc<Stats>,
    rng: StdRng,
    epoch: Instant,
    bootstrap: Vec<SocketAddr>,
    last_bootstrap: Option<Instant>,
    last_save: Instant,
    out: Vec<u8>,
    local_addrs: Vec<SocketAddr>,
    harvest: Option<mpsc::Sender<Harvest>>,
    seen: SeenFilter,
    peer_store: PeerStore,
    sampler: Sampler,
    recent: VecDeque<InfoHash>,
    get_peers_queue: VecDeque<InfoHash>,
    active_get_peers: usize,
    scrape_queue: VecDeque<(InfoHash, oneshot::Sender<Option<DhtEstimate>>)>,
    active_scrapes: usize,
    /// Who's waiting for each running scrape lookup.
    scrape_waiters: HashMap<u32, oneshot::Sender<Option<DhtEstimate>>>,
    scrape_rate: TokenBucket,
    /// Scrape lookups that ran out of query budget, to drive next tick.
    stalled: Vec<u32>,
    last_prune: Instant,
    /// IPs we've queried lately, to tell unsolicited queries apart.
    contacted: SeenFilter,
    /// The crawler's pause: no sampling, lookups, scrapes or harvests.
    paused: bool,
}

pub(crate) async fn run(
    cfg: Config,
    sockets: Vec<(UdpSocket, StdUdpSocket)>,
    stats: Arc<Stats>,
    mut commands: mpsc::Receiver<Command>,
    mut shutdown: watch::Receiver<bool>,
    harvest: Option<mpsc::Sender<Harvest>>,
) -> Result<(), StateError> {
    let now = Instant::now();
    let mut rng = StdRng::from_rng(&mut rand::rng());
    let saved = match &cfg.state_path {
        Some(path) => State::load(path).unwrap_or_default(),
        None => None,
    };
    let ids = initial_ids(&cfg, saved.as_ref(), &mut rng);
    let (recv, sockets): (Vec<UdpSocket>, Vec<StdUdpSocket>) = sockets.into_iter().unzip();
    let local_addrs = recv.iter().filter_map(|s| s.local_addr().ok()).collect();
    let families = sockets
        .iter()
        .map(|s| {
            let ipv6 = s.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
            Family {
                table: RoutingTable::new(ids[0], ipv6),
                ipv6,
            }
        })
        .collect();
    let bootstrap = resolve(&cfg.bootstrap).await;
    let mut actor = Actor {
        tokens: Tokens::new(now, &mut rng),
        query_rate: query_bucket(cfg.intensity, now),
        reply_rate: reply_bucket(cfg.intensity, now),
        families,
        ids,
        external: ExternalIp {
            current: saved.as_ref().and_then(|s| s.external_ip),
            ..ExternalIp::default()
        },
        pending: HashMap::new(),
        pending_addrs: HashMap::new(),
        next_tid: rng.random(),
        lookups: HashMap::new(),
        next_lookup: 0,
        stats,
        rng,
        epoch: now,
        bootstrap,
        last_bootstrap: None,
        last_save: now,
        out: Vec::with_capacity(1500),
        local_addrs,
        harvest,
        seen: SeenFilter::new(SEEN_CAPACITY, 0.01),
        peer_store: PeerStore::default(),
        sampler: Sampler::default(),
        recent: VecDeque::with_capacity(RECENT_SAMPLES),
        get_peers_queue: VecDeque::new(),
        active_get_peers: 0,
        scrape_queue: VecDeque::new(),
        active_scrapes: 0,
        scrape_waiters: HashMap::new(),
        scrape_rate: scrape_bucket(cfg.intensity, now),
        stalled: Vec::new(),
        last_prune: now,
        contacted: SeenFilter::new(CONTACTED_CAPACITY, 0.01),
        paused: cfg.paused,
        cfg,
    };
    actor.start_bootstrap(&sockets, saved.map(|s| s.nodes).unwrap_or_default());

    let mut tick = tokio::time::interval(actor.cfg.tick);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut buf0 = vec![0u8; 65_536];
    let mut buf1 = vec![0u8; 65_536];
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            Some(cmd) = commands.recv() => actor.on_command(cmd),
            r = recv[0].recv_from(&mut buf0) => {
                if let Ok((n, from)) = r {
                    actor.on_packet(&sockets, 0, &buf0[..n], from);
                }
            }
            r = recv_second(&recv, &mut buf1), if recv.len() > 1 => {
                if let Ok((n, from)) = r {
                    actor.on_packet(&sockets, 1, &buf1[..n], from);
                }
            }
            _ = tick.tick() => actor.maintain(&sockets),
        }
    }
    actor.save()
}

async fn recv_second(
    sockets: &[UdpSocket],
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

impl Actor {
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
                });
            }
            Command::SetPaused(paused) => {
                self.paused = paused;
                if paused {
                    // Waiting callers hear "no estimate" now rather than
                    // after the pause.
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
                if self.get_peers_queue.len() < MAX_GET_PEERS_QUEUE {
                    self.get_peers_queue.push_back(hash);
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
        sockets: &[StdUdpSocket],
        family: usize,
        to: SocketAddr,
        msg: &Message<'_>,
    ) -> bool {
        if self.cfg.deny.as_ref().is_some_and(|d| d.denies(to.ip())) {
            Stats::inc(&self.stats.denied);
            return false;
        }
        self.out.clear();
        msg.encode_into(&mut self.out);
        match sockets[family].send_to(&self.out, to) {
            Ok(_) => {
                Stats::inc(&self.stats.packets_out);
                true
            }
            Err(_) => {
                Stats::inc(&self.stats.send_errors);
                false
            }
        }
    }

    /// Sends a query if the rate limit and outstanding cap allow it.
    fn query(
        &mut self,
        sockets: &[StdUdpSocket],
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
        sockets: &[StdUdpSocket],
        family: usize,
        to: SocketAddr,
        node: Option<NodeId>,
        method: Method<'_>,
        purpose: Purpose,
    ) -> Sent {
        let now = Instant::now();
        if self.pending.len() >= self.cfg.intensity.max_outstanding()
            || !self.query_rate.try_take(now)
        {
            Stats::inc(&self.stats.throttled);
            return Sent::Throttled;
        }
        let tid = loop {
            self.next_tid = self.next_tid.wrapping_add(1);
            if !self.pending.contains_key(&self.next_tid) {
                break self.next_tid;
            }
        };
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
        if !self.send(sockets, family, to, &msg) {
            return Sent::Failed;
        }
        Stats::inc(&self.stats.queries_out);
        self.contacted.insert(&contact_key(to.ip()));
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
        Sent::Yes
    }

    fn take_pending(&mut self, tid: u16) -> Pending {
        let p = self.pending.remove(&tid).expect("caller checked the tid");
        if let std::collections::hash_map::Entry::Occupied(mut e) = self.pending_addrs.entry(p.to) {
            *e.get_mut() -= 1;
            if *e.get() == 0 {
                e.remove();
            }
        }
        p
    }

    fn on_packet(
        &mut self,
        sockets: &[StdUdpSocket],
        family: usize,
        data: &[u8],
        from: SocketAddr,
    ) {
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
        sockets: &[StdUdpSocket],
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
                samples: krpc::Samples::new(&samples).unwrap_or_default(),
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
        sockets: &[StdUdpSocket],
        family: usize,
        msg: &Message<'_>,
        r: Option<&Response<'_>>,
        from: SocketAddr,
    ) {
        let Ok(tid) = <[u8; 2]>::try_from(msg.transaction_id).map(u16::from_be_bytes) else {
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
        if let (Some(id), true, false) = (responder, consistent, msg.read_only) {
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
                if !Actor::is_self(&self.ids, &self.local_addrs, &n) {
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
                        !r.samples.is_empty() || r.interval.is_some() || r.num.is_some();
                    for h in r.samples.iter() {
                        self.harvest_hash(h, Source::Sample, Vec::new());
                    }
                    let wait = if supported {
                        Duration::from_secs(u64::from(r.interval.unwrap_or(SAMPLE_INTERVAL)))
                    } else {
                        sampler::UNSUPPORTED_BACKOFF
                    };
                    self.sampler.sampled(from, wait, now_i);
                }
                _ => {
                    Stats::inc(&self.stats.sample_failures);
                    self.sampler
                        .sampled(from, sampler::UNSUPPORTED_BACKOFF, now_i);
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
                let others = nodes.iter().filter(|n| !Actor::is_self(ids, local, n));
                match pending.id {
                    Some(id) => {
                        if lookup.scrape {
                            lookup.record_blooms(&id, r.seeds_bloom, r.peers_bloom);
                        }
                        lookup.on_response(&id, others, &r.values);
                    }
                    None => {
                        lookup.extra_in_flight = lookup.extra_in_flight.saturating_sub(1);
                        lookup.on_response(&r.id, others, &r.values);
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

    /// Starts a get_peers lookup for a hash the pipeline asked about.
    fn start_get_peers(&mut self, sockets: &[StdUdpSocket], hash: InfoHash) {
        self.active_get_peers += 1;
        self.start_lookup(sockets, 0, NodeId(hash.0), &[], &[], Some(hash));
    }

    /// Starts a BEP 33 scrape lookup; `reply` gets the estimate when it
    /// finishes.
    fn start_scrape(
        &mut self,
        sockets: &[StdUdpSocket],
        hash: InfoHash,
        reply: oneshot::Sender<Option<DhtEstimate>>,
    ) {
        self.active_scrapes += 1;
        // A hash-bearing lookup always starts, under the next id; the
        // waiter is in place before the first drive, which may finish it.
        self.scrape_waiters.insert(self.next_lookup, reply);
        self.start_lookup_as(sockets, 0, NodeId(hash.0), &[], &[], Some(hash), true);
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
            .len()
            .saturating_sub(self.active_get_peers + self.active_scrapes)
    }

    /// Starts a lookup (find_node, or get_peers when `info_hash` is set)
    /// seeded from the routing table, `known` nodes, and `seeds`
    /// (addresses whose ids we don't know yet).
    fn start_lookup(
        &mut self,
        sockets: &[StdUdpSocket],
        family: usize,
        target: NodeId,
        known: &[NodeInfo],
        seeds: &[SocketAddr],
        info_hash: Option<InfoHash>,
    ) {
        self.start_lookup_as(sockets, family, target, known, seeds, info_hash, false);
    }

    #[allow(clippy::too_many_arguments)]
    fn start_lookup_as(
        &mut self,
        sockets: &[StdUdpSocket],
        family: usize,
        target: NodeId,
        known: &[NodeInfo],
        seeds: &[SocketAddr],
        info_hash: Option<InfoHash>,
        scrape: bool,
    ) {
        if info_hash.is_none() && self.maintenance_lookups() >= MAX_LOOKUPS {
            return;
        }
        let lid = self.next_lookup;
        self.next_lookup = self.next_lookup.wrapping_add(1);
        let mut lookup = Lookup::new(target, family);
        lookup.info_hash = info_hash;
        lookup.scrape = scrape;
        for n in self.families[family]
            .table
            .closest(&target, 16, self.now_secs())
        {
            lookup.add(n);
        }
        for n in known {
            if !Actor::is_self(&self.ids, &self.local_addrs, n) {
                lookup.add(*n);
            }
        }
        self.lookups.insert(lid, lookup);
        for addr in seeds {
            let sent = self.query(
                sockets,
                family,
                *addr,
                None,
                Method::FindNode {
                    target,
                    want: Want::default(),
                },
                Purpose::Lookup(lid),
            );
            if sent {
                if let Some(l) = self.lookups.get_mut(&lid) {
                    l.extra_in_flight += 1;
                }
            }
        }
        self.drive_lookup(sockets, lid);
    }

    fn drive_lookup(&mut self, sockets: &[StdUdpSocket], lid: u32) {
        let Some(lookup) = self.lookups.get_mut(&lid) else {
            return;
        };
        let (family, target, info_hash, scrape) = (
            lookup.family,
            lookup.target,
            lookup.info_hash,
            lookup.scrape,
        );
        let next = lookup.next();
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
                Sent::Yes => {}
                // A scrape's budget was paid when it started, so running
                // short of queries delays it rather than spoiling it.
                Sent::Throttled if scrape => {
                    l.retry_later(&n.id);
                    throttled = true;
                    if !self.stalled.contains(&lid) {
                        self.stalled.push(lid);
                    }
                }
                Sent::Throttled | Sent::Failed => l.on_failure(&n.id),
            }
        }
        if self.lookups.get(&lid).is_some_and(Lookup::is_done) {
            let done = self.lookups.remove(&lid).expect("checked above");
            if done.scrape {
                self.finish_scrape(lid, &done);
            } else if let Some(hash) = done.info_hash {
                self.active_get_peers -= 1;
                let mut peers: Vec<SocketAddr> = done.peers.into_iter().collect();
                peers.sort_unstable();
                self.harvest_hash(hash, Source::Lookup, peers);
            }
        }
    }

    /// Self-lookups seeded with saved nodes and the bootstrap routers.
    fn start_bootstrap(&mut self, sockets: &[StdUdpSocket], saved: Vec<NodeInfo>) {
        self.last_bootstrap = Some(Instant::now());
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
    }

    fn maintain(&mut self, sockets: &[StdUdpSocket]) {
        let now_i = Instant::now();
        let now = self.now_secs();

        // Timeouts.
        let timeout = self.cfg.query_timeout;
        let expired: Vec<u16> = self
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
                self.sampler.sampled(p.to, SAMPLE_TIMEOUT_BACKOFF, now_i);
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
        if now_i.duration_since(self.last_prune) >= Duration::from_secs(60) {
            self.sampler.prune(now_i);
            self.last_prune = now_i;
        }
        while !self.paused && self.active_get_peers < MAX_GET_PEERS {
            let Some(hash) = self.get_peers_queue.pop_front() else {
                break;
            };
            self.start_get_peers(sockets, hash);
        }
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
            && now_i.duration_since(self.last_save) >= self.cfg.save_every
        {
            self.last_save = now_i;
            let _ = self.save();
        }

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
    fn scrape_tick(&mut self, sockets: &[StdUdpSocket], now_i: Instant) {
        for lid in std::mem::take(&mut self.stalled) {
            self.drive_lookup(sockets, lid);
        }
        let late: Vec<u32> = self
            .lookups
            .iter()
            .filter(|(_, l)| l.scrape && now_i.duration_since(l.started) >= SCRAPE_DEADLINE)
            .map(|(lid, _)| *lid)
            .collect();
        for lid in late {
            let lookup = self.lookups.remove(&lid).expect("listed above");
            self.finish_scrape(lid, &lookup);
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
    fn sample_tick(&mut self, sockets: &[StdUdpSocket], now_i: Instant) {
        if self.sampler.frontier_len() < LOW_FRONTIER {
            for f in 0..self.families.len() {
                let entries: Vec<NodeInfo> =
                    self.families[f].table.entries().map(|e| e.info).collect();
                for n in entries {
                    self.sampler.offer(n, now_i);
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
            if !self.query(sockets, f, n.addr, Some(n.id), method, Purpose::Sample) {
                self.sampler.offer(n, now_i);
                break;
            }
            Stats::inc(&self.stats.samples_sent);
        }
    }

    fn save(&self) -> Result<(), StateError> {
        let Some(path) = &self.cfg.state_path else {
            return Ok(());
        };
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
        .save(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ip_needs_a_quorum_of_distinct_reporters() {
        let mut e = ExternalIp::default();
        let me: IpAddr = "203.0.113.9".parse().unwrap();
        let at = |port| SocketAddr::new(me, port);
        // One reporter repeating itself never reaches quorum.
        for _ in 0..10 {
            assert_eq!(e.vote("1.1.1.1".parse().unwrap(), at(6881)), None);
        }
        for i in 2..=4u8 {
            assert_eq!(e.vote(IpAddr::from([1, 1, 1, i]), at(6881)), None);
        }
        assert_eq!(e.tally().ip, None, "4 reporters are short of quorum");
        assert_eq!(e.tally().voters, 4);
        assert_eq!(e.vote("1.1.1.5".parse().unwrap(), at(40000)), Some(me));
        // No change reported twice.
        assert_eq!(e.vote("1.1.1.6".parse().unwrap(), at(6881)), None);
        // A minority liar doesn't flip it.
        let liar: SocketAddr = "198.51.100.1:1".parse().unwrap();
        for i in 10..13u8 {
            assert_eq!(e.vote(IpAddr::from([2, 2, 2, i]), liar), None);
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
            let reporter = IpAddr::from(i.to_be_bytes());
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
