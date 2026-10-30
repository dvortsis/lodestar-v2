//! The scripted DHT network the real node talks to.
//!
//! Every simulated node is a row in one table, not a task: the network
//! task takes each datagram the node sends, decides (from the seeded RNG)
//! whether it's lost, rate-limited or answered, and schedules the reply on
//! a virtual-time heap.
//!
//! Routing is modelled on node ids sorted numerically. A node far from a
//! target (by rank) answers with nodes about an eighth of the way closer,
//! so lookups take about log₈(N) hops; a node within the target's
//! neighbourhood answers with the 8 XOR-closest ids there. The 8 closest
//! to a live hash hold its swarm and return it as `values`.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::net::{Ipv4Addr, SocketAddr};

use lodestar_dht::NodeId;
use lodestar_dht::compact::{CompactNodes, NodeInfo, encode_nodes};
use lodestar_dht::krpc::{self, Body, ErrorBody, Message, Method, Response, Samples, error_code};
use lodestar_dht::node::testing::SimPort;
use lodestar_metainfo::InfoHash;
use tokio::sync::watch;
use tokio::time::{Duration, Instant, sleep_until};

use crate::rng::Rng;
use crate::scenario::{NetConfig, SwarmConfig};

/// Nodes a reply carries, and the holders of a hash (BEP 5's K).
const K: usize = 8;
/// Ranks either side of a target searched for the XOR-closest.
const WINDOW: usize = 16;

/// Public /8s the simulator draws addresses from (outside every special
/// range, so the node's real endpoint policy applies unchanged).
const PREFIXES: [u8; 12] = [24, 31, 37, 46, 62, 77, 80, 85, 91, 109, 178, 212];

struct SimNode {
    id: NodeId,
    addr: SocketAddr,
    alive: bool,
    bep51: bool,
    held: Vec<InfoHash>,
    cursor: usize,
    /// (virtual second, queries answered in it).
    rate: (u64, u32),
}

#[derive(Debug, Default, Clone)]
pub struct NetStats {
    pub queries: u64,
    pub ping: u64,
    pub find_node: u64,
    pub get_peers: u64,
    pub sample: u64,
    pub other: u64,
    pub to_dead: u64,
    pub to_unknown: u64,
    pub lost: u64,
    pub rate_limited: u64,
    pub errors: u64,
    pub replies: u64,
    pub values_replies: u64,
}

/// A reply on its way: (due, tie-break, bytes, from).
type InFlight = Reverse<(Instant, u64, Vec<u8>, SocketAddr)>;

pub struct Net {
    cfg: NetConfig,
    rng: Rng,
    start: Instant,
    /// Sorted by id.
    nodes: Vec<SimNode>,
    by_addr: HashMap<SocketAddr, usize>,
    swarms: HashMap<InfoHash, Vec<SocketAddr>>,
    me: SocketAddr,
    heap: BinaryHeap<InFlight>,
    seq: u64,
    pub stats: NetStats,
}

/// A fresh public address no one else has.
pub fn address(rng: &mut Rng, taken: &mut HashMap<SocketAddr, usize>, slot: usize) -> SocketAddr {
    loop {
        let ip = Ipv4Addr::new(
            PREFIXES[rng.below(PREFIXES.len() as u64) as usize],
            rng.below(256) as u8,
            rng.below(256) as u8,
            1 + rng.below(254) as u8,
        );
        let addr = SocketAddr::from((ip, 1024 + rng.below(64_000) as u16));
        if lodestar_network_policy::allows(addr, false) && !taken.contains_key(&addr) {
            taken.insert(addr, slot);
            return addr;
        }
    }
}

fn xor(a: &[u8; 20], b: &[u8; 20]) -> [u8; 20] {
    std::array::from_fn(|i| a[i] ^ b[i])
}

impl Net {
    /// Builds the network and the swarms; returns it with the addresses
    /// to bootstrap from.
    pub fn build(
        cfg: &NetConfig,
        swarm: &SwarmConfig,
        seed: u64,
        me: SocketAddr,
        start: Instant,
    ) -> (Net, Vec<SocketAddr>) {
        assert!(
            cfg.nodes >= 2 * WINDOW,
            "a network needs at least {} nodes",
            2 * WINDOW
        );
        let mut rng = Rng::new(seed);
        let mut taken = HashMap::from([(me, usize::MAX)]);
        let catalogue: Vec<InfoHash> = (0..swarm.catalogue)
            .map(|_| InfoHash(rng.bytes20()))
            .collect();
        let mut nodes: Vec<SimNode> = (0..cfg.nodes)
            .map(|i| {
                let alive = !rng.chance(cfg.dead_permille);
                let bep51 = alive && rng.chance(cfg.bep51_permille);
                let held = if bep51 && !catalogue.is_empty() {
                    (0..cfg.held_per_node)
                        .map(|_| catalogue[rng.below(catalogue.len() as u64) as usize])
                        .collect()
                } else {
                    Vec::new()
                };
                SimNode {
                    id: NodeId(rng.bytes20()),
                    addr: address(&mut rng, &mut taken, i),
                    alive,
                    bep51,
                    held,
                    cursor: 0,
                    rate: (u64::MAX, 0),
                }
            })
            .collect();
        nodes.sort_by_key(|n| n.id.0);
        let by_addr = nodes.iter().enumerate().map(|(i, n)| (n.addr, i)).collect();
        let mut peer_addrs = HashMap::new();
        let mut swarms = HashMap::new();
        for h in &catalogue {
            if rng.chance(swarm.live_permille) && !swarms.contains_key(h) {
                let n = 1 + rng.below(swarm.max_peers.max(1) as u64) as usize;
                let peers = (0..n)
                    .map(|_| address(&mut rng, &mut peer_addrs, 0))
                    .collect();
                swarms.insert(*h, peers);
            }
        }
        let bootstrap = nodes
            .iter()
            .filter(|n| n.alive)
            .step_by((cfg.nodes / 16).max(1))
            .take(8)
            .map(|n| n.addr)
            .collect();
        let net = Net {
            cfg: cfg.clone(),
            rng,
            start,
            nodes,
            by_addr,
            swarms,
            me,
            heap: BinaryHeap::new(),
            seq: 0,
            stats: NetStats::default(),
        };
        (net, bootstrap)
    }

    /// Hashes with a swarm, for the report.
    pub fn live_hashes(&self) -> usize {
        self.swarms.len()
    }

    /// Serves the node until `stop` fires.
    pub async fn run(mut self, mut port: SimPort, mut stop: watch::Receiver<bool>) -> NetStats {
        loop {
            let next = self.heap.peek().map(|Reverse((at, ..))| *at);
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                Some((to, bytes)) = port.outbound.recv() => self.on_packet(to, &bytes),
                _ = sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                    let now = Instant::now();
                    while self.heap.peek().is_some_and(|Reverse((at, ..))| *at <= now) {
                        let Reverse((_, _, bytes, from)) = self.heap.pop().expect("peeked");
                        let _ = port.inbound.send((bytes, from));
                    }
                }
            }
        }
        self.stats
    }

    fn on_packet(&mut self, to: SocketAddr, bytes: &[u8]) {
        let Ok(msg) = krpc::parse(bytes) else {
            return;
        };
        let Body::Query(query) = &msg.body else {
            // The node answering a simulated node's query: none are sent.
            return;
        };
        self.stats.queries += 1;
        match query.method {
            Method::Ping => self.stats.ping += 1,
            Method::FindNode { .. } => self.stats.find_node += 1,
            Method::GetPeers { .. } => self.stats.get_peers += 1,
            Method::SampleInfohashes { .. } => self.stats.sample += 1,
            _ => self.stats.other += 1,
        }
        let Some(&i) = self.by_addr.get(&to) else {
            self.stats.to_unknown += 1;
            return;
        };
        if !self.nodes[i].alive {
            self.stats.to_dead += 1;
            return;
        }
        if self.rng.chance(self.cfg.loss_permille) {
            self.stats.lost += 1;
            return;
        }
        let second = Instant::now().duration_since(self.start).as_secs();
        if let Some(limit) = self.cfg.rate_limit_qps {
            let rate = &mut self.nodes[i].rate;
            if rate.0 != second {
                *rate = (second, 0);
            }
            if rate.1 >= limit {
                self.stats.rate_limited += 1;
                return;
            }
            rate.1 += 1;
        }
        let reply = if self.rng.chance(self.cfg.error_permille) {
            self.stats.errors += 1;
            error(msg.transaction_id, error_code::SERVER, b"busy")
        } else {
            self.answer(i, msg.transaction_id, &query.method)
        };
        // The reply can be lost on the way back too.
        if self.rng.chance(self.cfg.loss_permille) {
            self.stats.lost += 1;
            return;
        }
        let l = self.cfg.latency;
        let ms = if self.rng.chance(l.slow_permille) {
            l.slow_ms
        } else {
            l.base_ms + self.rng.below(l.jitter_ms + 1)
        };
        self.seq += 1;
        self.stats.replies += 1;
        let at = Instant::now() + Duration::from_millis(ms);
        self.heap.push(Reverse((at, self.seq, reply, to)));
    }

    fn answer(&mut self, i: usize, tid: &[u8], method: &Method<'_>) -> Vec<u8> {
        let (nodes, values, samples);
        let mut response = Response {
            id: self.nodes[i].id,
            ..Default::default()
        };
        match method {
            Method::Ping | Method::AnnouncePeer { .. } => {}
            Method::FindNode { target, .. } => {
                nodes = encode_nodes(&self.closer(&target.0, i), false);
                response.nodes = CompactNodes::v4(&nodes).expect("whole entries");
            }
            Method::GetPeers { info_hash, .. } => {
                response.token = Some(b"simtoken");
                let holder = self.is_holder(&info_hash.0, i);
                values = if holder {
                    self.swarms.get(info_hash).cloned().unwrap_or_default()
                } else {
                    Vec::new()
                };
                if values.is_empty() {
                    nodes = encode_nodes(&self.closer(&info_hash.0, i), false);
                    response.nodes = CompactNodes::v4(&nodes).expect("whole entries");
                } else {
                    self.stats.values_replies += 1;
                    response.values = values;
                }
            }
            Method::SampleInfohashes { target, .. } => {
                if !self.nodes[i].bep51 {
                    self.stats.errors += 1;
                    return error(tid, error_code::METHOD_UNKNOWN, b"unknown method");
                }
                let node = &mut self.nodes[i];
                let take = self.cfg.sample_size.min(node.held.len());
                samples = (0..take)
                    .flat_map(|k| node.held[(node.cursor + k) % node.held.len()].0)
                    .collect::<Vec<u8>>();
                node.cursor = (node.cursor + take) % node.held.len().max(1);
                response.samples = Samples::new(&samples);
                response.interval = Some(self.cfg.sample_interval_s);
                response.num = Some(node.held.len() as u32);
                nodes = encode_nodes(&self.closer(&target.0, i), false);
                response.nodes = CompactNodes::v4(&nodes).expect("whole entries");
            }
            _ => return error(tid, error_code::METHOD_UNKNOWN, b"unknown method"),
        }
        Message {
            transaction_id: tid,
            version: Some(b"SM01"),
            ip: Some(self.me),
            read_only: false,
            body: Body::Response(response),
        }
        .encode()
    }

    /// The rank a target would have among the sorted ids.
    fn rank(&self, target: &[u8; 20]) -> usize {
        self.nodes.partition_point(|n| n.id.0 < *target)
    }

    /// The K XOR-closest ids in the target's neighbourhood.
    fn closest(&self, target: &[u8; 20]) -> Vec<usize> {
        let pos = self.rank(target);
        let lo = pos.saturating_sub(WINDOW);
        let hi = (pos + WINDOW).min(self.nodes.len());
        let mut near: Vec<usize> = (lo..hi).collect();
        near.sort_by_key(|&j| xor(&self.nodes[j].id.0, target));
        near.truncate(K);
        near
    }

    fn is_holder(&self, target: &[u8; 20], i: usize) -> bool {
        self.closest(target).contains(&i)
    }

    /// What node `i` knows nearer `target`.
    fn closer(&self, target: &[u8; 20], i: usize) -> Vec<NodeInfo> {
        let pos = self.rank(target);
        let d = i.abs_diff(pos);
        let picks = if d <= WINDOW {
            self.closest(target)
        } else {
            let step = d / 8;
            let centre = if i >= pos { pos + step } else { pos - step };
            let lo = centre.saturating_sub(K / 2).min(self.nodes.len() - K);
            (lo..lo + K).collect()
        };
        picks
            .into_iter()
            .map(|j| NodeInfo {
                id: self.nodes[j].id,
                addr: self.nodes[j].addr,
            })
            .collect()
    }

    /// Hashes held per node, for a sanity line in the report.
    pub fn sample_overlap(&self) -> (usize, usize) {
        let mut distinct = BTreeMap::new();
        let mut held = 0;
        for n in &self.nodes {
            held += n.held.len();
            for h in &n.held {
                distinct.insert(h.0, ());
            }
        }
        (held, distinct.len())
    }
}

fn error(tid: &[u8], code: i64, message: &[u8]) -> Vec<u8> {
    Message {
        transaction_id: tid,
        version: Some(b"SM01"),
        ip: None,
        read_only: false,
        body: Body::Error(ErrorBody { code, message }),
    }
    .encode()
}
