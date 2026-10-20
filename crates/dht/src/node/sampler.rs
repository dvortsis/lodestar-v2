//! BEP 51 traversal state: which nodes to sample next and when each node
//! may be sampled again (it says so in its `interval`). Bounded: the
//! frontier and the "next allowed" map have caps, so a flood of node
//! addresses can't grow memory without limit.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::compact::NodeInfo;

const MAX_FRONTIER: usize = 20_000;
const MAX_KNOWN: usize = 200_000;
/// For nodes that don't implement BEP 51 (error reply or no `samples`).
pub(crate) const UNSUPPORTED_BACKOFF: Duration = Duration::from_secs(6 * 3600);
pub(crate) const MIN_INTERVAL: Duration = Duration::from_secs(60);
pub(crate) const MAX_INTERVAL: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Default)]
pub(crate) struct Sampler {
    next_at: HashMap<SocketAddr, Instant>,
    frontier: VecDeque<NodeInfo>,
    queued: HashSet<SocketAddr>,
}

impl Sampler {
    /// Queues a node for sampling unless it's queued already or not due.
    pub fn offer(&mut self, node: NodeInfo, now: Instant) {
        if self.frontier.len() >= MAX_FRONTIER || self.queued.contains(&node.addr) {
            return;
        }
        if self.next_at.get(&node.addr).is_some_and(|t| *t > now) {
            return;
        }
        self.queued.insert(node.addr);
        self.frontier.push_back(node);
    }

    /// The next node due for sampling.
    pub fn next(&mut self, now: Instant) -> Option<NodeInfo> {
        while let Some(n) = self.frontier.pop_front() {
            self.queued.remove(&n.addr);
            if self.next_at.get(&n.addr).is_none_or(|t| *t <= now) {
                return Some(n);
            }
        }
        None
    }

    /// Records that a node was sampled (or refused) and when it may be
    /// sampled again.
    pub fn sampled(&mut self, addr: SocketAddr, wait: Duration, now: Instant) {
        if self.next_at.len() >= MAX_KNOWN && !self.next_at.contains_key(&addr) {
            self.prune(now);
            if self.next_at.len() >= MAX_KNOWN {
                return;
            }
        }
        self.next_at
            .insert(addr, now + wait.clamp(MIN_INTERVAL, MAX_INTERVAL));
    }

    /// Drops entries whose wait has passed; they're plain unknown nodes again.
    pub fn prune(&mut self, now: Instant) {
        self.next_at.retain(|_, t| *t > now);
    }

    pub fn frontier_len(&self) -> usize {
        self.frontier.len()
    }

    pub fn known(&self) -> usize {
        self.next_at.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::NodeId;

    fn n(i: u8) -> NodeInfo {
        NodeInfo {
            id: NodeId([i; 20]),
            addr: SocketAddr::from(([10, 0, 0, i], 6881)),
        }
    }

    #[test]
    fn honours_interval_and_dedups() {
        let mut s = Sampler::default();
        let t0 = Instant::now();
        s.offer(n(1), t0);
        s.offer(n(1), t0);
        s.offer(n(2), t0);
        assert_eq!(s.frontier_len(), 2);
        let first = s.next(t0).unwrap();
        s.sampled(first.addr, Duration::from_secs(600), t0);
        // Not offered again until its interval passes.
        s.offer(first, t0 + Duration::from_secs(10));
        assert_eq!(s.next(t0).unwrap(), n(2));
        assert!(s.next(t0).is_none());
        s.offer(first, t0 + Duration::from_secs(601));
        assert_eq!(s.next(t0 + Duration::from_secs(601)), Some(first));
    }

    #[test]
    fn intervals_are_clamped_and_pruned() {
        let mut s = Sampler::default();
        let t0 = Instant::now();
        s.sampled(n(1).addr, Duration::ZERO, t0);
        s.offer(n(1), t0 + Duration::from_secs(30));
        assert_eq!(s.frontier_len(), 0, "minimum interval applies");
        s.sampled(n(2).addr, Duration::from_secs(10 * 86_400), t0);
        assert_eq!(s.known(), 2);
        s.prune(t0 + MAX_INTERVAL);
        assert_eq!(s.known(), 0);
    }
}
