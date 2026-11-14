//! Bounded BEP 51 admission. Capacity belongs to a request before it is sent.
//! An indexed expiry set has exactly one record per cooldown: no stale heap
//! records, compaction pauses, or per-packet scans of the address map.

use std::collections::{BTreeSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::time::Instant;

use super::limits::{Network, allocation, network};
use super::transport::{Map, Set};

use crate::compact::NodeInfo;

const MAX_FRONTIER: usize = 20_000;
const MAX_KNOWN: usize = crate::restart::MAX_ENTRIES;
const RECLAIM_PER_TICK: usize = 1024;
/// The most addresses one IP may hold across the frontier and the table:
/// the fleet's DHT nodes per IP (`MAX_DHT_PER_IP`), so a host running
/// several nodes still has each sampled.
pub(crate) const MAX_PER_IP: u32 = 8;
/// The most addresses one IPv4 /24 may hold across the frontier and the
/// table, so no block takes more than a small share of either.
pub(crate) const MAX_PER_V4_NETWORK: u32 = 64;
/// The same for one IPv6 /64...
pub(crate) const MAX_PER_V6_NETWORK: u32 = 16;
/// ...and for one IPv6 /48, an ordinary site's allocation, so filling the
/// table takes as many allocations on IPv6 as on IPv4.
pub(crate) const MAX_PER_V6_ALLOCATION: u32 = 64;
pub(crate) const UNSUPPORTED_BACKOFF: Duration = Duration::from_secs(6 * 3600);
pub const MIN_INTERVAL: Duration = Duration::from_secs(60);
pub const MAX_INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// The largest BEP 51 share a node takes (WK-10e): the fleet planner's
/// `MAX_DHT_PER_IP`, so a cooldown is at most `MAX_INTERVAL` times this
/// (48 h).
pub const MAX_SCALE: u8 = 8;
/// Tokens for records the restart proof made (CR-0d): restored at start or
/// sealed at shutdown. Wire transaction ids start in the lower half and
/// only count up, so they never reach this bit.
const PROOF_TOKEN: u64 = 1 << 63;
/// Within proof tokens: restored from a snapshot (for the counters).
const RESTORED: u64 = 1 << 62;

#[derive(Debug, Clone, Copy)]
enum State {
    InFlight(u64),
    Cooling { until: Instant, token: u64 },
}

#[derive(Debug)]
pub(crate) struct Sampler {
    states: Map<SocketAddr, State>,
    expiry: BTreeSet<(Instant, u64, SocketAddr)>,
    frontier: VecDeque<NodeInfo>,
    queued: Set<SocketAddr>,
    /// How many addresses each IP and each block holds, queued or in
    /// `states` (an address in both counts once).
    per_ip: Map<IpAddr, u32>,
    per_network: Map<Network, u32>,
    /// IPv6 /48s ([`allocation`]).
    per_allocation: Map<Network, u32>,
    capacity: usize,
    next_proof: u64,
    /// Restored records still cooling, and the running totals of
    /// candidates they turned away and of those that expired unsampled.
    restored_live: usize,
    restored_refused: u64,
    restored_honoured: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self {
            states: Map::default(),
            expiry: BTreeSet::new(),
            frontier: VecDeque::new(),
            queued: Set::default(),
            per_ip: Map::default(),
            per_network: Map::default(),
            per_allocation: Map::default(),
            capacity: MAX_KNOWN,
            next_proof: 0,
            restored_live: 0,
            restored_refused: 0,
            restored_honoured: 0,
        }
    }
}

impl Sampler {
    #[cfg(test)]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Self::default()
        }
    }

    fn eligible(&self, addr: &SocketAddr, now: Instant) -> bool {
        match self.states.get(addr) {
            None => true,
            Some(State::Cooling { until, .. }) => *until <= now,
            Some(State::InFlight(_)) => false,
        }
    }

    fn tracked(&self, addr: &SocketAddr) -> bool {
        self.queued.contains(addr) || self.states.contains_key(addr)
    }

    /// Whether a new address fits its IP's and its block's shares.
    /// Loopback is never a remote node (the public policy refuses it), so
    /// it has no share: a local test network runs many nodes on it.
    fn has_room(&self, addr: &SocketAddr) -> bool {
        let ip = addr.ip().to_canonical();
        let net_cap = if ip.is_ipv4() {
            MAX_PER_V4_NETWORK
        } else {
            MAX_PER_V6_NETWORK
        };
        ip.is_loopback()
            || (count(&self.per_ip, ip) < MAX_PER_IP
                && count(&self.per_network, network(ip)) < net_cap
                && allocation(ip)
                    .is_none_or(|a| count(&self.per_allocation, a) < MAX_PER_V6_ALLOCATION))
    }

    /// Updates the shares after `addr` was (`was`) or wasn't tracked
    /// before a change.
    fn recount(&mut self, addr: &SocketAddr, was: bool) {
        let now = self.tracked(addr);
        if was == now {
            return;
        }
        let ip = addr.ip().to_canonical();
        let net = network(ip);
        let alloc = allocation(ip);
        if now {
            *self.per_ip.entry(ip).or_default() += 1;
            *self.per_network.entry(net).or_default() += 1;
            if let Some(a) = alloc {
                *self.per_allocation.entry(a).or_default() += 1;
            }
        } else {
            decrement(&mut self.per_ip, ip);
            decrement(&mut self.per_network, net);
            if let Some(a) = alloc {
                decrement(&mut self.per_allocation, a);
            }
        }
    }

    /// Queues `node` on the frontier if it's eligible, not already queued,
    /// and a new address fits its IP's and block's shares. True when it was
    /// queued (CR-1b counts these as new).
    pub fn offer(&mut self, mut node: NodeInfo, now: Instant) -> bool {
        node.addr = lodestar_network_policy::normalize(node.addr);
        if matches!(self.states.get(&node.addr), Some(State::Cooling { until, token })
            if *until > now && token & (PROOF_TOKEN | RESTORED) == PROOF_TOKEN | RESTORED)
        {
            self.restored_refused += 1;
        }
        let was = self.tracked(&node.addr);
        if self.frontier.len() < MAX_FRONTIER
            && !self.queued.contains(&node.addr)
            && self.eligible(&node.addr, now)
            && (was || self.has_room(&node.addr))
        {
            self.queued.insert(node.addr);
            self.frontier.push_back(node);
            self.recount(&node.addr, was);
            return true;
        }
        false
    }

    pub fn next(&mut self, now: Instant) -> Option<NodeInfo> {
        // Saturation leaves candidates queued. A later maintenance tick
        // reclaims capacity; callers never spin looking for a free slot.
        if self.states.len() >= self.capacity {
            return None;
        }
        while let Some(n) = self.frontier.pop_front() {
            self.queued.remove(&n.addr);
            self.recount(&n.addr, true);
            if self.eligible(&n.addr, now) {
                return Some(n);
            }
        }
        None
    }

    pub fn reserve(&mut self, addr: SocketAddr, token: u64, now: Instant) -> bool {
        if !self.eligible(&addr, now) {
            return false;
        }
        let was = self.tracked(&addr);
        if let Some(State::Cooling { until, token }) = self.states.remove(&addr) {
            self.expiry.remove(&(until, token, addr));
            self.retire(token);
        }
        if self.states.len() >= self.capacity || !(was || self.has_room(&addr)) {
            self.recount(&addr, was);
            return false;
        }
        self.states.insert(addr, State::InFlight(token));
        self.recount(&addr, was);
        true
    }

    pub fn abort(&mut self, addr: SocketAddr, token: u64) -> bool {
        if matches!(self.states.get(&addr), Some(State::InFlight(t)) if *t == token) {
            self.states.remove(&addr);
            self.recount(&addr, true);
            true
        } else {
            false
        }
    }

    pub fn complete(&mut self, addr: SocketAddr, token: u64, wait: Duration, now: Instant) -> bool {
        self.complete_scaled(addr, token, wait, 1, now)
    }

    /// [`complete`](Self::complete), cooling `scale` times as long (WK-10e,
    /// plan Q9): the node's own interval is bounded first, then multiplied,
    /// so N same-IP members together sample it no faster than one install.
    /// `scale` is taken as `1..=MAX_SCALE`; the multiply saturates.
    pub fn complete_scaled(
        &mut self,
        addr: SocketAddr,
        token: u64,
        wait: Duration,
        scale: u8,
        now: Instant,
    ) -> bool {
        // A sample's token is a wire transaction id, always below the
        // proof's bit (the actor never hands out one at or past it), which
        // is how a snapshot tells a sample's cooldown from a seal.
        debug_assert!(token & PROOF_TOKEN == 0, "a wire token below PROOF_TOKEN");
        if !matches!(self.states.get(&addr), Some(State::InFlight(t)) if *t == token) {
            return false;
        }
        let scale = scale.clamp(1, MAX_SCALE);
        let until = now
            + wait
                .clamp(MIN_INTERVAL, MAX_INTERVAL)
                .saturating_mul(u32::from(scale));
        self.states.insert(addr, State::Cooling { until, token });
        self.expiry.insert((until, token, addr));
        true
    }

    pub fn prune(&mut self, now: Instant) {
        for _ in 0..RECLAIM_PER_TICK {
            let Some(&(until, token, addr)) = self.expiry.first() else {
                break;
            };
            if until > now {
                break;
            }
            self.expiry.pop_first();
            if matches!(self.states.get(&addr), Some(State::Cooling { token: t, .. }) if *t == token)
            {
                self.states.remove(&addr);
                self.recount(&addr, true);
                self.retire(token);
            }
        }
    }

    fn proof_token(&mut self, restored: bool) -> u64 {
        self.next_proof += 1;
        PROOF_TOKEN | if restored { RESTORED } else { 0 } | self.next_proof
    }

    /// An expired record left the table: count a restored one as honoured.
    fn retire(&mut self, token: u64) {
        if token & (PROOF_TOKEN | RESTORED) == PROOF_TOKEN | RESTORED {
            self.restored_live -= 1;
            self.restored_honoured += 1;
        }
    }

    /// Seeds a restriction restored by the restart proof (S2). It is never
    /// refused for capacity: dropping one would shorten it, and a snapshot
    /// holds at most the table's bound. An existing later record wins.
    /// Sampling is gated until seeding is done, so nothing is in flight.
    pub fn restore(&mut self, addr: SocketAddr, until: Instant) {
        let addr = lodestar_network_policy::normalize(addr);
        match self.states.get(&addr).copied() {
            // Can't happen (sampling waits for seeding), but if it did, the
            // restored restriction wins: the in-flight sample's completion
            // then fails its token check and can't shorten it.
            Some(State::InFlight(_)) => {
                debug_assert!(false, "a sample was in flight before seeding");
            }
            Some(State::Cooling { until: u, .. }) if u >= until => return,
            Some(State::Cooling { until: u, token }) => {
                self.expiry.remove(&(u, token, addr));
                if token & (PROOF_TOKEN | RESTORED) == PROOF_TOKEN | RESTORED {
                    self.restored_live -= 1;
                }
            }
            None => {}
        }
        let was = self.tracked(&addr);
        let token = self.proof_token(true);
        self.states.insert(addr, State::Cooling { until, token });
        self.expiry.insert((until, token, addr));
        self.restored_live += 1;
        // Never refused for its shares, as for capacity; it still counts.
        self.recount(&addr, was);
    }

    /// S8: every sample still in flight becomes a cooldown ending at
    /// `until` under a fresh token, so a late reply or timeout for the old
    /// transaction fails `complete` and can't shorten it (P6).
    pub fn seal_in_flight(&mut self, until: Instant) {
        let flying: Vec<SocketAddr> = self
            .states
            .iter()
            .filter(|(_, s)| matches!(s, State::InFlight(_)))
            .map(|(a, _)| *a)
            .collect();
        for addr in flying {
            let token = self.proof_token(false);
            self.states.insert(addr, State::Cooling { until, token });
            self.expiry.insert((until, token, addr));
        }
    }

    /// Every live cooldown, for the snapshot (S10). Call after
    /// [`Sampler::seal_in_flight`]: in-flight records aren't listed.
    ///
    /// A cooldown from a sample's answer (a wire token, below
    /// `PROOF_TOKEN`) is listed as at most `MAX_INTERVAL` from `now`,
    /// within the restart proof's horizon (WK-10e). Unscaled, it ends by
    /// then anyway; scaled (up to 48 h), only this member's extra share is
    /// cut, since the node's own interval (≤ `MAX_INTERVAL`) ran from a
    /// sample before `now`. The live cooldown keeps its full length, and a
    /// sealed or restored record (a proof token) keeps its own end.
    pub fn restrictions(&self, now: Instant) -> impl Iterator<Item = (SocketAddr, Instant)> + '_ {
        let bound = now + MAX_INTERVAL;
        self.states.iter().filter_map(move |(addr, s)| match s {
            State::Cooling { until, token } if *until > now => {
                let until = if token & PROOF_TOKEN == 0 {
                    (*until).min(bound)
                } else {
                    *until
                };
                Some((*addr, until))
            }
            _ => None,
        })
    }

    /// Restored records still cooling, candidates they turned away, and
    /// those that expired unsampled. Monitoring only (P5).
    pub fn restored_counts(&self) -> (usize, u64, u64) {
        (
            self.restored_live,
            self.restored_refused,
            self.restored_honoured,
        )
    }

    pub fn frontier_len(&self) -> usize {
        self.frontier.len()
    }
    pub fn known(&self) -> usize {
        self.states.len()
    }
    pub fn cooling(&self) -> usize {
        self.expiry.len()
    }
    pub fn inflight(&self) -> usize {
        self.known() - self.cooling()
    }
    pub fn saturated(&self) -> bool {
        self.known() >= self.capacity
    }
}

/// A share's count; zero when the key holds nothing.
fn count<K: Ord + std::hash::Hash>(map: &Map<K, u32>, key: K) -> u32 {
    map.get(&key).copied().unwrap_or(0)
}

/// Lowers a share's count, dropping the key at zero so the maps hold only
/// live IPs and blocks.
fn decrement<K: Ord + std::hash::Hash>(map: &mut Map<K, u32>, key: K) {
    let Some(n) = map.get_mut(&key) else {
        debug_assert!(false, "a share released that was never counted");
        return;
    };
    *n -= 1;
    if *n == 0 {
        map.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::NodeId;

    fn n(i: u32) -> NodeInfo {
        NodeInfo {
            id: NodeId([1; 20]),
            addr: SocketAddr::from((i.to_be_bytes(), 6881)),
        }
    }

    #[test]
    fn reserve_complete_duplicate_and_late_reply() {
        let mut s = Sampler::default();
        let now = Instant::now();
        s.offer(n(1), now);
        assert_eq!(s.next(now), Some(n(1)));
        assert!(s.reserve(n(1).addr, 1, now));
        s.offer(n(1), now);
        assert!(s.next(now).is_none());
        assert!(!s.reserve(n(1).addr, 2, now));
        assert!(!s.complete(n(1).addr, 2, MIN_INTERVAL, now));
        assert!(s.complete(n(1).addr, 1, Duration::ZERO, now));
        assert!(!s.complete(n(1).addr, 1, MAX_INTERVAL, now));
        assert!(!s.reserve(n(1).addr, 2, now + Duration::from_secs(59)));
        assert!(s.reserve(n(1).addr, 2, now + MIN_INTERVAL));
        assert!(!s.abort(n(1).addr, 1));
        assert!(s.abort(n(1).addr, 2));
        assert_eq!((s.known(), s.cooling()), (0, 0));
    }

    #[test]
    fn saturation_preserves_frontier_and_reclaims_in_bounded_steps() {
        // One address per /24, so the blocks' shares don't bind first.
        let wide = |i: u32| n(i << 8);
        let mut s = Sampler::default();
        let now = Instant::now();
        for i in 0..MAX_KNOWN as u32 {
            assert!(s.reserve(wide(i).addr, u64::from(i), now));
            assert!(s.complete(wide(i).addr, u64::from(i), MAX_INTERVAL, now));
        }
        s.offer(wide(MAX_KNOWN as u32), now);
        assert!(s.next(now).is_none());
        assert!(!s.reserve(wide(MAX_KNOWN as u32).addr, 999999, now));
        assert_eq!(
            (s.known(), s.cooling(), s.frontier_len()),
            (MAX_KNOWN, MAX_KNOWN, 1)
        );
        s.prune(now + MAX_INTERVAL);
        assert_eq!(s.known(), MAX_KNOWN - RECLAIM_PER_TICK);
        assert_eq!(s.next(now + MAX_INTERVAL), Some(wide(MAX_KNOWN as u32)));
    }

    #[test]
    fn interval_bounds_and_no_stale_expiries() {
        for wait in [0, 60, 900, 1800, 21600, 21601] {
            let mut s = Sampler::default();
            let now = Instant::now();
            assert!(s.reserve(n(1).addr, 1, now));
            assert!(s.complete(n(1).addr, 1, Duration::from_secs(wait), now));
            let due = now + Duration::from_secs(wait.clamp(60, 21600));
            s.prune(due - Duration::from_nanos(1));
            assert_eq!(s.known(), 1);
            s.prune(due);
            assert_eq!(s.known(), 0);
            assert!(s.reserve(n(1).addr, 2, due));
            s.prune(due + MAX_INTERVAL);
            assert_eq!(s.inflight(), 1);
        }
    }

    #[test]
    fn repeated_reuse_and_stale_completion_cannot_shorten_new_cooldown() {
        let mut s = Sampler::with_capacity(1);
        let start = Instant::now();
        let addr = n(1).addr;
        for token in 1..=100u64 {
            let now = start + MAX_INTERVAL * (token - 1) as u32;
            // Reserve directly at expiry, before the maintenance prune. This
            // must retire the old expiry index without freeing the new owner.
            assert!(s.reserve(addr, token, now));
            assert!(!s.complete(addr, token - 1, MIN_INTERVAL, now));
            assert!(!s.abort(addr, token - 1));
            s.prune(now);
            assert_eq!((s.known(), s.inflight(), s.cooling()), (1, 1, 0));
            assert!(s.complete(addr, token, MAX_INTERVAL, now));
            s.prune(now + MIN_INTERVAL);
            assert_eq!((s.known(), s.inflight(), s.cooling()), (1, 0, 1));
            assert!(!s.reserve(addr, token + 1, now + MIN_INTERVAL));
        }
        s.prune(start + MAX_INTERVAL * 100);
        assert_eq!((s.known(), s.inflight(), s.cooling()), (0, 0, 0));
    }

    /// WK-10e: a scaled cooldown is the node's bounded interval times the
    /// scale, which is taken as 1..=MAX_SCALE: at most 48 h.
    #[test]
    fn a_scaled_cooldown_is_the_interval_times_the_scale() {
        for (wait, scale, want) in [
            (3600, 1, 3600),
            (3600, 3, 3 * 3600),
            (21_600, 8, 8 * 21_600),
            // The node's own interval is bounded first.
            (30, 2, 120),
            (99_999, 8, 8 * 21_600),
            // 0 reads as 1; past MAX_SCALE reads as MAX_SCALE.
            (3600, 0, 3600),
            (21_600, 200, 8 * 21_600),
        ] {
            let mut s = Sampler::default();
            let now = Instant::now();
            assert!(s.reserve(n(1).addr, 1, now));
            assert!(s.complete_scaled(n(1).addr, 1, Duration::from_secs(wait), scale, now));
            let due = now + Duration::from_secs(want);
            assert!(
                !s.reserve(n(1).addr, 2, due - Duration::from_nanos(1)),
                "{wait} × {scale}"
            );
            assert!(s.reserve(n(1).addr, 2, due), "{wait} × {scale}");
        }
        assert_eq!(
            MAX_INTERVAL * u32::from(MAX_SCALE),
            Duration::from_secs(48 * 3600)
        );
    }

    /// WK-10e: with a full table of 48 h cooldowns, a new node waits on the
    /// frontier, isn't lost, and is sampled as soon as a cooldown ends.
    #[test]
    fn a_full_table_of_scaled_cooldowns_holds_new_nodes_until_one_ends() {
        let mut s = Sampler::with_capacity(4);
        let now = Instant::now();
        for i in 1..=4 {
            assert!(s.reserve(n(i).addr, u64::from(i), now));
            assert!(s.complete_scaled(n(i).addr, u64::from(i), MAX_INTERVAL, MAX_SCALE, now));
        }
        assert!(s.saturated());
        assert!(s.offer(n(9), now));
        assert_eq!(s.next(now + MAX_INTERVAL), None, "full: it waits");
        assert_eq!(s.frontier_len(), 1, "still queued");
        let end = now + MAX_INTERVAL * u32::from(MAX_SCALE);
        s.prune(end);
        assert!(!s.saturated());
        assert_eq!(s.next(end), Some(n(9)));
    }

    /// WK-10e: the snapshot lists a scaled cooldown as at most
    /// `MAX_INTERVAL` from now; the live one keeps its full length, and an
    /// unscaled or sealed record its own end.
    #[test]
    fn a_snapshot_bounds_only_scaled_cooldowns() {
        let mut s = Sampler::default();
        let now = Instant::now();
        assert!(s.reserve(n(1).addr, 1, now));
        assert!(s.complete_scaled(n(1).addr, 1, MAX_INTERVAL, MAX_SCALE, now));
        // Wire ids start anywhere below bit 63, bits 61 and 62 included.
        let high = (1 << 62) | (1 << 61) | 2;
        assert!(s.reserve(n(2).addr, high, now));
        assert!(s.complete_scaled(n(2).addr, high, Duration::from_secs(600), 4, now));
        assert!(s.reserve(n(4).addr, 4, now));
        assert!(s.complete(n(4).addr, 4, Duration::from_secs(900), now));
        assert!(s.reserve(n(3).addr, 3, now));
        let sealed = now + MAX_INTERVAL + Duration::from_secs(4);
        s.seal_in_flight(sealed);
        let listed: std::collections::HashMap<SocketAddr, Instant> = s.restrictions(now).collect();
        assert_eq!(listed[&n(1).addr], now + MAX_INTERVAL, "bounded");
        assert_eq!(listed[&n(2).addr], now + Duration::from_secs(2400));
        assert_eq!(listed[&n(4).addr], now + Duration::from_secs(900));
        assert_eq!(listed[&n(3).addr], sealed, "a seal keeps the horizon");
        // Live, the scaled one still cools for 48 h.
        let end = now + MAX_INTERVAL * u32::from(MAX_SCALE);
        assert!(!s.reserve(n(1).addr, 9, end - Duration::from_nanos(1)));
        assert!(s.reserve(n(1).addr, 9, end));
    }

    /// A sample's token is always a wire id below the proof's bit; one at
    /// it is a bug the snapshot would misread as a seal (debug builds).
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a wire token below PROOF_TOKEN")]
    fn a_proof_bit_on_a_samples_token_is_refused() {
        let mut s = Sampler::default();
        let now = Instant::now();
        let _ = s.complete_scaled(n(1).addr, PROOF_TOKEN | 1, MAX_INTERVAL, 1, now);
    }

    type Counts = (Map<IpAddr, u32>, Map<Network, u32>, Map<Network, u32>);

    /// The shares, recomputed from scratch.
    fn recounted(s: &Sampler) -> Counts {
        let mut tracked: Set<SocketAddr> = s.queued.clone();
        tracked.extend(s.states.keys().copied());
        let (mut ips, mut nets, mut allocs) = (Map::default(), Map::default(), Map::default());
        for a in tracked {
            let ip = a.ip().to_canonical();
            *ips.entry(ip).or_default() += 1;
            *nets.entry(network(ip)).or_default() += 1;
            if let Some(al) = allocation(ip) {
                *allocs.entry(al).or_default() += 1;
            }
        }
        (ips, nets, allocs)
    }

    fn node(addr: SocketAddr) -> NodeInfo {
        NodeInfo {
            id: NodeId([1; 20]),
            addr,
        }
    }

    /// Offers every address, sampling half of what's queued as it goes so
    /// the table fills as well as the frontier, and returns how many of
    /// them the sampler holds at the end.
    fn push(s: &mut Sampler, addrs: impl Iterator<Item = SocketAddr>, now: Instant) -> usize {
        let mut token = 0;
        let mine: Vec<SocketAddr> = addrs.collect();
        for (i, a) in mine.iter().enumerate() {
            s.offer(node(*a), now);
            if i % 2 == 1
                && let Some(n) = s.next(now)
            {
                token += 1;
                if s.reserve(n.addr, token, now) {
                    s.complete(n.addr, token, MAX_INTERVAL, now);
                }
            }
        }
        mine.iter().filter(|a| s.tracked(a)).count()
    }

    #[test]
    fn one_ip_holds_at_most_its_share() {
        for ip in ["10.0.0.1", "2001:db8::1"] {
            let ip: IpAddr = ip.parse().unwrap();
            let mut s = Sampler::default();
            let held = push(
                &mut s,
                (1..=2000).map(|p| SocketAddr::new(ip, p)),
                Instant::now(),
            );
            assert_eq!(held, MAX_PER_IP as usize, "{ip}");
        }
    }

    #[test]
    fn one_v4_block_holds_at_most_its_share() {
        let mut s = Sampler::default();
        let addrs =
            (0..=255u8).flat_map(|h| (1..=8u16).map(move |p| SocketAddr::from(([10, 0, 0, h], p))));
        assert_eq!(
            push(&mut s, addrs, Instant::now()),
            MAX_PER_V4_NETWORK as usize
        );
        // Another block isn't affected.
        assert!(s.offer(node(SocketAddr::from(([10, 0, 1, 1], 1))), Instant::now()));
    }

    #[test]
    fn one_v6_block_holds_at_most_its_share() {
        let mut s = Sampler::default();
        let addrs =
            (1..=5000u16).map(|h| SocketAddr::from(([0x2001, 0xdb8, 0, 1, 0, 0, h, h], 6881)));
        assert_eq!(
            push(&mut s, addrs, Instant::now()),
            MAX_PER_V6_NETWORK as usize
        );
    }

    #[test]
    fn one_v6_allocation_holds_at_most_its_share_across_its_blocks() {
        let mut s = Sampler::default();
        // Many /64s of one /48, a few hosts each.
        let addrs = (0..=4000u16).flat_map(|net| {
            (1..=4u16).map(move |h| SocketAddr::from(([0x2001, 0xdb8, 7, net, 0, 0, 0, h], 6881)))
        });
        assert_eq!(
            push(&mut s, addrs, Instant::now()),
            MAX_PER_V6_ALLOCATION as usize
        );
        // Another /48 isn't affected.
        assert!(s.offer(
            node(SocketAddr::from(([0x2001, 0xdb8, 8, 0, 0, 0, 0, 1], 6881))),
            Instant::now()
        ));
    }

    #[test]
    fn a_place_comes_back_when_its_record_leaves() {
        let mut s = Sampler::default();
        let now = Instant::now();
        let at = |h: u8, p| SocketAddr::from(([10, 0, 0, h], p));
        for h in 1..=255u8 {
            s.offer(node(at(h, 1)), now);
        }
        assert!(!s.offer(node(at(1, 99)), now));
        // Sampling one frees nothing while it's in flight or cooling...
        let first = s.next(now).unwrap();
        assert!(s.reserve(first.addr, 1, now));
        assert!(!s.offer(node(at(1, 99)), now));
        // ...and an expired cooldown gives its place back.
        assert!(s.complete(first.addr, 1, MIN_INTERVAL, now));
        s.prune(now + MIN_INTERVAL);
        assert!(s.offer(node(at(1, 99)), now + MIN_INTERVAL));
    }

    #[derive(Debug, Clone)]
    enum Op {
        Offer(SocketAddr),
        Next,
        Complete(u8),
        Abort(u8),
        Restore(SocketAddr),
        Advance(u16),
    }

    /// Addresses crowded into a few IPv4 /24s, IPv6 /64s and /48s.
    fn arb_addr() -> impl Strategy<Value = SocketAddr> {
        prop_oneof![
            (0..3u8, any::<u8>(), 1..20u16)
                .prop_map(|(b, h, p)| SocketAddr::from(([10, 0, b, h], p))),
            (0..2u16, 0..40u16, 0..20u16, 1..4u16)
                .prop_map(|(a, n, h, p)| SocketAddr::from(([0x2001, 0xdb8, a, n, 0, 0, 0, h], p))),
        ]
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        prop_oneof![
            6 => arb_addr().prop_map(Op::Offer),
            3 => Just(Op::Next),
            1 => any::<u8>().prop_map(Op::Complete),
            1 => any::<u8>().prop_map(Op::Abort),
            1 => arb_addr().prop_map(Op::Restore),
            1 => (0..400u16).prop_map(Op::Advance),
        ]
    }

    use proptest::prelude::*;

    proptest! {
        /// Whatever the sequence, the share counts match the frontier and
        /// table, and outside restored records no IP, block or IPv6
        /// allocation holds more than its share.
        #[test]
        fn shares_hold_under_any_sequence(ops in proptest::collection::vec(arb_op(), 1..600)) {
            let mut s = Sampler::default();
            let mut now = Instant::now();
            let mut token = 0u64;
            let mut flying: Vec<(SocketAddr, u64)> = vec![];
            let mut restored = false;
            for op in ops {
                match op {
                    Op::Offer(a) => {
                        s.offer(node(a), now);
                    }
                    Op::Next => {
                        if let Some(n) = s.next(now) {
                            token += 1;
                            if s.reserve(n.addr, token, now) {
                                flying.push((n.addr, token));
                            }
                        }
                    }
                    Op::Complete(i) if !flying.is_empty() => {
                        let (a, t) = flying.remove(usize::from(i) % flying.len());
                        s.complete(a, t, MIN_INTERVAL, now);
                    }
                    Op::Abort(i) if !flying.is_empty() => {
                        let (a, t) = flying.remove(usize::from(i) % flying.len());
                        s.abort(a, t);
                    }
                    Op::Restore(a) if flying.is_empty() => {
                        restored = true;
                        s.restore(a, now + MIN_INTERVAL);
                    }
                    Op::Advance(secs) => {
                        now += Duration::from_secs(u64::from(secs));
                        s.prune(now);
                    }
                    _ => {}
                }
                let (ips, nets, allocs) = recounted(&s);
                prop_assert_eq!(&s.per_ip, &ips);
                prop_assert_eq!(&s.per_network, &nets);
                prop_assert_eq!(&s.per_allocation, &allocs);
                if !restored {
                    prop_assert!(ips.values().all(|n| *n <= MAX_PER_IP));
                    let cap = |k: &Network| if k.0 == 4 { MAX_PER_V4_NETWORK } else { MAX_PER_V6_NETWORK };
                    prop_assert!(nets.iter().all(|(k, n)| *n <= cap(k)));
                    prop_assert!(allocs.values().all(|n| *n <= MAX_PER_V6_ALLOCATION));
                }
            }
        }
    }
}
