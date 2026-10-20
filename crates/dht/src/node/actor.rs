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
use tokio::sync::{mpsc, watch};

use super::lookup::Lookup;
use super::tokens::Tokens;
use super::{Command, Config, Snapshot, Stats};
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

#[derive(Debug, Clone, Copy)]
enum Purpose {
    Ping,
    Lookup(u32),
}

#[derive(Debug)]
struct Pending {
    to: SocketAddr,
    id: Option<NodeId>,
    sent: Instant,
    family: usize,
    purpose: Purpose,
}

struct TokenBucket {
    rate: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(per_sec: u32, now: Instant) -> TokenBucket {
        TokenBucket {
            rate: f64::from(per_sec),
            tokens: f64::from(per_sec),
            last: now,
        }
    }

    fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.rate);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Majority vote over the external address other nodes report (BEP 42).
#[derive(Default)]
struct ExternalIp {
    votes: VecDeque<(IpAddr, IpAddr)>,
    current: Option<IpAddr>,
}

impl ExternalIp {
    const WINDOW: usize = 32;
    const QUORUM: usize = 5;

    /// Records a report; returns the new address when the majority changes.
    fn vote(&mut self, reporter: IpAddr, reported: IpAddr) -> Option<IpAddr> {
        self.votes.retain(|(r, _)| *r != reporter);
        self.votes.push_back((reporter, reported));
        if self.votes.len() > Self::WINDOW {
            self.votes.pop_front();
        }
        let mut counts: HashMap<IpAddr, usize> = HashMap::new();
        for (_, ip) in &self.votes {
            *counts.entry(*ip).or_default() += 1;
        }
        let (best, n) = counts.into_iter().max_by_key(|(_, n)| *n)?;
        if n >= Self::QUORUM && n * 2 > self.votes.len() && self.current != Some(best) {
            self.current = Some(best);
            return Some(best);
        }
        None
    }
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
}

pub(crate) async fn run(
    cfg: Config,
    sockets: Vec<(UdpSocket, StdUdpSocket)>,
    stats: Arc<Stats>,
    mut commands: mpsc::Receiver<Command>,
    mut shutdown: watch::Receiver<bool>,
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
        query_rate: TokenBucket::new(cfg.intensity.queries_per_sec(), now),
        // Replies are capped too, so we can't be used as an amplifier.
        reply_rate: TokenBucket::new(cfg.intensity.queries_per_sec() * 2, now),
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
                });
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
        let now = Instant::now();
        if self.pending.len() >= self.cfg.intensity.max_outstanding()
            || !self.query_rate.try_take(now)
        {
            Stats::inc(&self.stats.throttled);
            return false;
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
            return false;
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
        true
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
        let Ok(msg) = krpc::parse(data) else {
            Stats::inc(&self.stats.malformed_in);
            return;
        };
        match &msg.body {
            Body::Query(q) => {
                Stats::inc(&self.stats.queries_in);
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
                num: Some(0),
                ..Response::default()
            }),
            Method::Unknown(_) => Body::Error(ErrorBody {
                code: error_code::METHOD_UNKNOWN,
                message: b"Method Unknown",
            }),
        };
        let reply = Message {
            transaction_id: msg.transaction_id,
            version: Some(VERSION),
            ip: Some(from),
            read_only: false,
            body,
        };
        self.send(sockets, family, from, &reply);
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
            self.on_external_ip(from.ip(), ip.ip());
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

    fn on_external_ip(&mut self, reporter: IpAddr, reported: IpAddr) {
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

    /// Starts a find_node lookup seeded from the routing table, `known`
    /// nodes, and `seeds` (addresses whose ids we don't know yet).
    fn start_lookup(
        &mut self,
        sockets: &[StdUdpSocket],
        family: usize,
        target: NodeId,
        known: &[NodeInfo],
        seeds: &[SocketAddr],
    ) {
        if self.lookups.len() >= MAX_LOOKUPS {
            return;
        }
        let lid = self.next_lookup;
        self.next_lookup = self.next_lookup.wrapping_add(1);
        let mut lookup = Lookup::new(target, family);
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
        let (family, target) = (lookup.family, lookup.target);
        let next = lookup.next();
        for n in next {
            let sent = self.query(
                sockets,
                family,
                n.addr,
                Some(n.id),
                Method::FindNode {
                    target,
                    want: Want::default(),
                },
                Purpose::Lookup(lid),
            );
            if !sent {
                if let Some(l) = self.lookups.get_mut(&lid) {
                    l.on_failure(&n.id);
                }
            }
        }
        if self.lookups.get(&lid).is_some_and(Lookup::is_done) {
            self.lookups.remove(&lid);
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
            self.start_lookup(sockets, f, target, &known, &routers);
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
                self.start_lookup(sockets, f, target, &[], &[]);
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

        let thin = self.families.iter().any(|f| f.table.len() < HEALTHY_NODES);
        let due = self
            .last_bootstrap
            .is_none_or(|t| now_i.duration_since(t) >= self.cfg.tick * REBOOTSTRAP_TICKS);
        if thin && due && self.lookups.is_empty() {
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
        // One reporter repeating itself never reaches quorum.
        for _ in 0..10 {
            assert_eq!(e.vote("1.1.1.1".parse().unwrap(), me), None);
        }
        for i in 2..=4u8 {
            assert_eq!(e.vote(IpAddr::from([1, 1, 1, i]), me), None);
        }
        assert_eq!(e.vote("1.1.1.5".parse().unwrap(), me), Some(me));
        // No change reported twice.
        assert_eq!(e.vote("1.1.1.6".parse().unwrap(), me), None);
        // A minority liar doesn't flip it.
        let liar: IpAddr = "198.51.100.1".parse().unwrap();
        for i in 10..13u8 {
            assert_eq!(e.vote(IpAddr::from([2, 2, 2, i]), liar), None);
        }
        assert_eq!(e.current, Some(me));
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
}
