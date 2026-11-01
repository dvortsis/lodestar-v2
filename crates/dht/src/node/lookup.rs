//! Iterative Kademlia lookups: ask the closest known nodes for even closer
//! ones, at most [`ALPHA`] queries in flight, until the [`K`] closest
//! nodes that are still answering have all answered.

use std::net::SocketAddr;

use tokio::time::Instant;

use super::transport::Set;

use lodestar_metainfo::InfoHash;

use crate::bep33::Bloom;
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
    /// BEP 33 `(BFsd, BFpe)` from a scrape answer.
    blooms: Option<Box<(Bloom, Bloom)>>,
}

#[derive(Debug)]
pub(crate) struct Lookup {
    pub target: NodeId,
    pub owner: Option<u32>,
    /// Index of the address family (socket) this lookup runs on.
    pub family: usize,
    candidates: Vec<Candidate>,
    in_flight: usize,
    pub peers: Set<SocketAddr>,
    /// Queries sent outside the candidate list (e.g. to bootstrap
    /// routers, whose ids we don't know yet).
    pub extra_in_flight: usize,
    /// `Some` for a get_peers lookup, `None` for find_node.
    pub info_hash: Option<InfoHash>,
    /// A BEP 33 scrape: get_peers with `scrape=1`, answered with counts.
    pub scrape: bool,
    /// CR-1b: the sampler's exploration `find_node`, whose new contacts
    /// count as `dht_target_novel_nodes_total{kind="explore"}`.
    pub explore: bool,
    pub started: Instant,
}

impl Lookup {
    pub fn new(target: NodeId, family: usize) -> Lookup {
        Lookup {
            target,
            owner: None,
            family,
            candidates: Vec::new(),
            in_flight: 0,
            peers: Set::default(),
            extra_in_flight: 0,
            info_hash: None,
            scrape: false,
            explore: false,
            started: Instant::now(),
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
                blooms: None,
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

    /// Whether a candidate takes one of the K slots. In a scrape, the
    /// first K responders that sent no filters (they don't store the hash,
    /// or don't speak BEP 33) give their slot up, so the lookup reaches the
    /// nodes that do; that bounds a scrape at 2K answers.
    fn slot_filter(scrape: bool) -> impl FnMut(&Candidate) -> bool {
        let mut skipped = 0;
        move |c: &Candidate| {
            if c.state == State::Failed {
                return false;
            }
            if scrape && c.state == State::Responded && c.blooms.is_none() && skipped < K {
                skipped += 1;
                return false;
            }
            true
        }
    }

    /// Candidates to query now, marked in flight.
    #[cfg(test)]
    pub fn next(&mut self) -> Vec<NodeInfo> {
        self.next_limit(ALPHA)
    }

    pub fn next_limit(&mut self, limit: usize) -> Vec<NodeInfo> {
        let mut out = Vec::new();
        let mut live = 0;
        let mut counts = Lookup::slot_filter(self.scrape);
        for c in &mut self.candidates {
            if self.in_flight >= ALPHA || out.len() >= limit || live >= K {
                break;
            }
            if !counts(c) {
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
        self.absorb(nodes, peers);
        ours
    }

    /// Takes a reply's nodes (by their own XOR distance) and peers without
    /// settling any candidate: for a query outside the candidate list.
    pub fn absorb(&mut self, nodes: impl Iterator<Item = NodeInfo>, peers: &[SocketAddr]) {
        for n in nodes {
            self.add(n);
        }
        for peer in peers {
            if self.peers.len() >= 64 {
                break;
            }
            self.peers.insert(*peer);
        }
    }

    pub fn on_failure(&mut self, id: &NodeId) {
        self.settle(id, State::Failed);
    }

    /// A query to `id` couldn't be sent for lack of budget: it goes back
    /// to fresh, to be asked on a later drive, rather than counting as a
    /// node that failed to answer.
    pub fn retry_later(&mut self, id: &NodeId) {
        self.settle(id, State::Fresh);
    }

    /// Keeps the BEP 33 filters an in-flight node answered with. Call
    /// before [`Lookup::on_response`] settles it.
    pub fn record_blooms(
        &mut self,
        id: &NodeId,
        seeds: Option<&[u8; 256]>,
        peers: Option<&[u8; 256]>,
    ) {
        if seeds.is_none() && peers.is_none() {
            return;
        }
        if let Some(c) = self
            .candidates
            .iter_mut()
            .find(|c| c.info.id == *id && c.state == State::InFlight)
        {
            let copy = |b: Option<&[u8; 256]>| b.map_or_else(Bloom::default, |b| Bloom(*b));
            c.blooms = Some(Box::new((copy(seeds), copy(peers))));
        }
    }

    /// The union of the filters from the [`K`] closest responders that
    /// sent any, and how many that was. Nodes that don't store the hash
    /// (or don't speak BEP 33) send none and so don't take a slot.
    pub fn scrape_blooms(&self) -> (Bloom, Bloom, u8) {
        let (mut seeds, mut peers, mut n) = (Bloom::default(), Bloom::default(), 0u8);
        for (sd, pe) in self
            .candidates
            .iter()
            .filter(|c| c.state == State::Responded)
            .filter_map(|c| c.blooms.as_deref())
            .take(K)
        {
            seeds.union(sd);
            peers.union(pe);
            n += 1;
        }
        (seeds, peers, n)
    }

    /// Done when nothing is in flight and the K closest live candidates
    /// have all answered.
    pub fn is_done(&self) -> bool {
        if self.in_flight > 0 || self.extra_in_flight > 0 {
            return false;
        }
        let mut counts = Lookup::slot_filter(self.scrape);
        !self
            .candidates
            .iter()
            .filter(|c| counts(c))
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

    /// One filter per IP in `ips`, as a node storing those peers sends.
    fn bloom(ips: &[u8]) -> [u8; 256] {
        let mut b = Bloom::default();
        for i in ips {
            b.insert(std::net::IpAddr::from([10, 9, 0, *i]));
        }
        b.0
    }

    #[test]
    fn merges_filters_of_the_k_closest_responders_that_sent_one() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        l.scrape = true;
        for b in 1..=12 {
            l.add(n(b));
        }
        while !l.is_done() {
            for q in l.next() {
                let b = q.id.0[0];
                match b {
                    // No BEP 33 support: doesn't take one of the K slots.
                    1 | 2 => {}
                    // Nodes 3..=10 are the K closest with filters; each
                    // knows seed `b` and the shared leecher 200.
                    3..=10 => l.record_blooms(&q.id, Some(&bloom(&[b])), Some(&bloom(&[200]))),
                    // Farther than the K: its peers must not count.
                    _ => l.record_blooms(&q.id, Some(&bloom(&[100, 101])), None),
                }
                l.on_response(&q.id, std::iter::empty(), &[]);
            }
        }
        let (seeds, peers, n) = l.scrape_blooms();
        assert_eq!(n, K as u8);
        assert_eq!(seeds.count(), 8);
        assert_eq!(peers.count(), 1, "the same leecher counts once");
    }

    #[test]
    fn throttled_candidates_wait_instead_of_failing() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        l.add(n(1));
        let q = l.next();
        l.retry_later(&q[0].id);
        assert!(!l.is_done(), "still has a fresh close node");
        assert_eq!(l.next(), q, "asked again on the next drive");
        l.on_response(&q[0].id, std::iter::empty(), &[]);
        assert!(l.is_done());
    }
}
