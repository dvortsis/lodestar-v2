//! Bounded BEP 51 admission. Capacity belongs to a request before it is sent.
//! An indexed expiry set has exactly one record per cooldown: no stale heap
//! records, compaction pauses, or per-packet scans of the address map.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::compact::NodeInfo;

const MAX_FRONTIER: usize = 20_000;
const MAX_KNOWN: usize = crate::restart::MAX_ENTRIES;
const RECLAIM_PER_TICK: usize = 1024;
pub(crate) const UNSUPPORTED_BACKOFF: Duration = Duration::from_secs(6 * 3600);
pub(crate) const MIN_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const MAX_INTERVAL: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, Copy)]
enum State {
    InFlight(u64),
    Cooling { until: Instant, token: u64 },
}

#[derive(Debug)]
pub(crate) struct Sampler {
    states: HashMap<SocketAddr, State>,
    expiry: BTreeSet<(Instant, u64, SocketAddr)>,
    frontier: VecDeque<NodeInfo>,
    queued: HashSet<SocketAddr>,
    capacity: usize,
}

impl Default for Sampler {
    fn default() -> Self {
        Self {
            states: HashMap::new(),
            expiry: BTreeSet::new(),
            frontier: VecDeque::new(),
            queued: HashSet::new(),
            capacity: MAX_KNOWN,
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

    pub fn offer(&mut self, mut node: NodeInfo, now: Instant) {
        node.addr = lodestar_network_policy::normalize(node.addr);
        if self.frontier.len() < MAX_FRONTIER
            && !self.queued.contains(&node.addr)
            && self.eligible(&node.addr, now)
        {
            self.queued.insert(node.addr);
            self.frontier.push_back(node);
        }
    }

    pub fn next(&mut self, now: Instant) -> Option<NodeInfo> {
        // Saturation leaves candidates queued. A later maintenance tick
        // reclaims capacity; callers never spin looking for a free slot.
        if self.states.len() >= self.capacity {
            return None;
        }
        while let Some(n) = self.frontier.pop_front() {
            self.queued.remove(&n.addr);
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
        if let Some(State::Cooling { until, token }) = self.states.remove(&addr) {
            self.expiry.remove(&(until, token, addr));
        }
        if self.states.len() >= self.capacity {
            return false;
        }
        self.states.insert(addr, State::InFlight(token));
        true
    }

    pub fn abort(&mut self, addr: SocketAddr, token: u64) -> bool {
        if matches!(self.states.get(&addr), Some(State::InFlight(t)) if *t == token) {
            self.states.remove(&addr);
            true
        } else {
            false
        }
    }

    pub fn complete(&mut self, addr: SocketAddr, token: u64, wait: Duration, now: Instant) -> bool {
        if !matches!(self.states.get(&addr), Some(State::InFlight(t)) if *t == token) {
            return false;
        }
        let until = now + wait.clamp(MIN_INTERVAL, MAX_INTERVAL);
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
            }
        }
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
        let mut s = Sampler::default();
        let now = Instant::now();
        for i in 0..MAX_KNOWN as u32 {
            assert!(s.reserve(n(i).addr, u64::from(i), now));
            assert!(s.complete(n(i).addr, u64::from(i), MAX_INTERVAL, now));
        }
        s.offer(n(MAX_KNOWN as u32), now);
        assert!(s.next(now).is_none());
        assert!(!s.reserve(n(MAX_KNOWN as u32).addr, 999999, now));
        assert_eq!(
            (s.known(), s.cooling(), s.frontier_len()),
            (MAX_KNOWN, MAX_KNOWN, 1)
        );
        s.prune(now + MAX_INTERVAL);
        assert_eq!(s.known(), MAX_KNOWN - RECLAIM_PER_TICK);
        assert_eq!(s.next(now + MAX_INTERVAL), Some(n(MAX_KNOWN as u32)));
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
}
