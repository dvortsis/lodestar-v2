//! Iterative Kademlia lookups: ask the closest known nodes for even closer
//! ones, at most [`ALPHA`] queries in flight, until the [`K`] closest
//! nodes that are still answering have all answered.

use std::collections::HashSet;
use std::net::SocketAddr;

use crate::compact::{NodeId, NodeInfo};
use crate::routing::K;

pub(crate) const ALPHA: usize = 3;
/// Candidates kept per lookup; the far tail is never queried anyway.
const CAP: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Fresh,
    InFlight,
    Responded,
    Failed,
}

#[derive(Debug)]
struct Candidate {
    info: NodeInfo,
    distance: [u8; 20],
    state: State,
}

#[derive(Debug)]
pub(crate) struct Lookup {
    pub target: NodeId,
    /// Index of the address family (socket) this lookup runs on.
    pub family: usize,
    candidates: Vec<Candidate>,
    in_flight: usize,
    pub peers: HashSet<SocketAddr>,
    /// Queries sent outside the candidate list (e.g. to bootstrap
    /// routers, whose ids we don't know yet).
    pub extra_in_flight: usize,
}

impl Lookup {
    pub fn new(target: NodeId, family: usize) -> Lookup {
        Lookup {
            target,
            family,
            candidates: Vec::new(),
            in_flight: 0,
            peers: HashSet::new(),
            extra_in_flight: 0,
        }
    }

    /// Adds a candidate (from a routing table or a response's `nodes`).
    pub fn add(&mut self, info: NodeInfo) {
        if self.candidates.iter().any(|c| c.info.id == info.id) {
            return;
        }
        let distance = info.id.distance(&self.target);
        let pos = self.candidates.partition_point(|c| c.distance < distance);
        if pos >= CAP {
            return;
        }
        self.candidates.insert(
            pos,
            Candidate {
                info,
                distance,
                state: State::Fresh,
            },
        );
        // Drop the farthest candidates that aren't in flight.
        while self.candidates.len() > CAP {
            match self
                .candidates
                .iter()
                .rposition(|c| c.state != State::InFlight)
            {
                Some(i) => {
                    self.candidates.remove(i);
                }
                None => break,
            }
        }
    }

    /// Candidates to query now, marked in flight.
    pub fn next(&mut self) -> Vec<NodeInfo> {
        let mut out = Vec::new();
        let mut live = 0;
        for c in &mut self.candidates {
            if self.in_flight >= ALPHA || live >= K {
                break;
            }
            if c.state == State::Failed {
                continue;
            }
            live += 1;
            if c.state == State::Fresh {
                c.state = State::InFlight;
                self.in_flight += 1;
                out.push(c.info);
            }
        }
        out
    }

    fn settle(&mut self, id: &NodeId, state: State) -> bool {
        match self
            .candidates
            .iter_mut()
            .find(|c| c.info.id == *id && c.state == State::InFlight)
        {
            Some(c) => {
                c.state = state;
                self.in_flight -= 1;
                true
            }
            None => false,
        }
    }

    /// A queried node answered. Returns whether it was one of ours.
    pub fn on_response(
        &mut self,
        id: &NodeId,
        nodes: impl Iterator<Item = NodeInfo>,
        peers: &[SocketAddr],
    ) -> bool {
        let ours = self.settle(id, State::Responded);
        for n in nodes {
            self.add(n);
        }
        self.peers.extend(peers.iter().copied());
        ours
    }

    pub fn on_failure(&mut self, id: &NodeId) {
        self.settle(id, State::Failed);
    }

    /// Done when nothing is in flight and the K closest live candidates
    /// have all answered.
    pub fn is_done(&self) -> bool {
        if self.in_flight > 0 || self.extra_in_flight > 0 {
            return false;
        }
        !self
            .candidates
            .iter()
            .filter(|c| c.state != State::Failed)
            .take(K)
            .any(|c| c.state == State::Fresh)
    }

    /// The closest nodes that answered.
    #[cfg(test)]
    pub fn closest(&self) -> impl Iterator<Item = NodeInfo> + '_ {
        self.candidates
            .iter()
            .filter(|c| c.state == State::Responded)
            .map(|c| c.info)
            .take(K)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(b: u8) -> NodeInfo {
        NodeInfo {
            id: NodeId([b; 20]),
            addr: SocketAddr::from(([10, 0, 0, b], 1)),
        }
    }

    #[test]
    fn queries_closest_first_with_bounded_parallelism() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for b in [9, 3, 7, 1, 5] {
            l.add(n(b));
        }
        let first: Vec<u8> = l.next().iter().map(|i| i.id.0[0]).collect();
        assert_eq!(first, [1, 3, 5]);
        assert!(l.next().is_empty(), "alpha in flight");
        l.on_response(&NodeId([1; 20]), [n(2)].into_iter(), &[]);
        let second: Vec<u8> = l.next().iter().map(|i| i.id.0[0]).collect();
        assert_eq!(second, [2], "newly learned closer node goes first");
    }

    #[test]
    fn finishes_when_k_closest_answered_and_skips_failures() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for b in 1..=12 {
            l.add(n(b));
        }
        while !l.is_done() {
            let batch = l.next();
            assert!(!batch.is_empty(), "stuck lookup");
            for q in batch {
                if q.id.0[0] == 2 {
                    l.on_failure(&q.id);
                } else {
                    l.on_response(&q.id, std::iter::empty(), &[]);
                }
            }
        }
        let closest: Vec<u8> = l.closest().map(|i| i.id.0[0]).collect();
        assert_eq!(closest, [1, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn duplicates_and_cap() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for _ in 0..3 {
            l.add(n(4));
        }
        assert_eq!(l.candidates.len(), 1);
        for b in 0..=255u8 {
            let mut id = [b; 20];
            id[19] = 0xee;
            l.add(NodeInfo {
                id: NodeId(id),
                addr: SocketAddr::from(([10, 1, 0, b], 1)),
            });
        }
        assert_eq!(l.candidates.len(), CAP);
        assert!(
            l.candidates
                .windows(2)
                .all(|w| w[0].distance <= w[1].distance)
        );
    }

    #[test]
    fn collects_peers() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        l.add(n(1));
        l.next();
        let peer = SocketAddr::from(([1, 2, 3, 4], 5));
        l.on_response(&NodeId([1; 20]), std::iter::empty(), &[peer, peer]);
        assert_eq!(l.peers.len(), 1);
        assert!(l.is_done());
    }
}
