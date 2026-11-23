//! Iterative Kademlia lookups: ask the closest known nodes for even closer
//! ones, at most [`ALPHA`] queries in flight, until the [`K`] closest
//! nodes that are still answering have all answered.

use std::net::SocketAddr;

use tokio::time::Instant;

use super::limits::{Network, source};

use lodestar_metainfo::InfoHash;

use crate::bep33::Bloom;
use crate::compact::{NodeId, NodeInfo};
use crate::routing::K;

mod values;
pub(crate) use values::{MAX_VALUES, SentBy, Values};

pub(crate) const ALPHA: usize = 3;
/// Candidates kept per lookup; the far tail is never queried anyway.
const CAP: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Fresh,
    InFlight,
    /// CR-SOFT-TIMEOUT: asked, unanswered past the soft timeout. Its query
    /// is still out and still holds its [`ALPHA`] lane (a late answer
    /// settles it), but it gives its place among the [`K`] closest up: the
    /// next candidate is asked once a lane frees, and the leg can end once
    /// [`K`] others have answered.
    Slow,
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
    /// Queries out to candidates: [`State::InFlight`] and [`State::Slow`].
    in_flight: usize,
    /// CR-SOFT-TIMEOUT: of those, the [`State::Slow`] ones.
    slow: usize,
    /// The leg's values, shared out by source ([`Values`]).
    pub peers: Values,
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
    /// CR-1b: an exploration lookup whose prefix target hasn't been sent
    /// yet; its first send moves the explore cursor.
    pub prefix_pending: bool,
    /// The self-lookup of a bootstrap round ([`super::actor`]'s
    /// `start_bootstrap`): its end lets parked requests start and plans
    /// the join refresh. Marked explicitly, since BEP 42 can change our
    /// id, and so the target, while it runs.
    pub bootstrap: bool,
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
            slow: 0,
            peers: Values::default(),
            extra_in_flight: 0,
            info_hash: None,
            scrape: false,
            explore: false,
            prefix_pending: false,
            bootstrap: false,
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
        // Drop the farthest candidates whose queries aren't out, so a
        // late answer still finds its candidate.
        while self.candidates.len() > CAP {
            match self
                .candidates
                .iter()
                .rposition(|c| !matches!(c.state, State::InFlight | State::Slow))
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
    /// nodes that do; that bounds a scrape at 2K answers. A slow candidate
    /// (CR-SOFT-TIMEOUT) gives its slot up, as a failed one does: when
    /// choosing whom to ask, so the next candidate is asked once a lane
    /// frees, and in the end tests.
    fn slot_filter(scrape: bool) -> impl FnMut(&Candidate) -> bool {
        let mut skipped = 0;
        move |c: &Candidate| {
            if matches!(c.state, State::Failed | State::Slow) {
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

    /// Settles the query out to `id`, once: in flight, or (CR-SOFT-TIMEOUT)
    /// slow. Either way its lane frees now.
    fn settle(&mut self, id: &NodeId, state: State) -> bool {
        match self
            .candidates
            .iter_mut()
            .find(|c| c.info.id == *id && matches!(c.state, State::InFlight | State::Slow))
        {
            Some(c) => {
                if c.state == State::Slow {
                    self.slow -= 1;
                }
                self.in_flight -= 1;
                c.state = state;
                true
            }
            None => false,
        }
    }

    /// CR-SOFT-TIMEOUT: the query out to `id` has gone unanswered past the
    /// soft timeout. It keeps its [`ALPHA`] lane, so a leg never has more
    /// than [`ALPHA`] queries out, but it gives its place among the [`K`]
    /// closest up: the next candidate is asked once a lane frees, and the
    /// leg can end once [`K`] others have answered. Its answer, if one comes
    /// before the query timeout, still settles it. False when `id` has no
    /// query in flight.
    pub fn soft_expire(&mut self, id: &NodeId) -> bool {
        match self
            .candidates
            .iter_mut()
            .find(|c| c.info.id == *id && c.state == State::InFlight)
        {
            Some(c) => {
                c.state = State::Slow;
                self.slow += 1;
                true
            }
            None => false,
        }
    }

    /// CR-SOFT-TIMEOUT: queries out past the soft timeout, still awaiting
    /// an answer.
    #[cfg(test)]
    pub fn slow(&self) -> usize {
        self.slow
    }

    /// Answers among the K closest live candidates.
    #[cfg(test)]
    pub fn answered_live(&self) -> usize {
        let mut counts = Lookup::slot_filter(self.scrape);
        self.candidates
            .iter()
            .filter(|c| counts(c))
            .take(K)
            .filter(|c| c.state == State::Responded)
            .count()
    }

    /// A queried node answered from `from`. Returns whether it was one
    /// of ours.
    pub fn on_response(
        &mut self,
        id: &NodeId,
        from: SocketAddr,
        nodes: impl Iterator<Item = NodeInfo>,
        peers: &[SocketAddr],
    ) -> bool {
        let ours = self.settle(id, State::Responded);
        self.absorb(from, nodes, peers);
        ours
    }

    /// Takes a reply's nodes (by their own XOR distance) and peers without
    /// settling any candidate: for a query outside the candidate list. The
    /// peers are filed under `from`'s [`source`], which gets its share of
    /// the leg's [`MAX_VALUES`] ([`Values`]).
    pub fn absorb(
        &mut self,
        from: SocketAddr,
        nodes: impl Iterator<Item = NodeInfo>,
        peers: &[SocketAddr],
    ) {
        for n in nodes {
            self.add(n);
        }
        let net = source(from.ip());
        for peer in peers {
            self.peers.insert(net, *peer);
        }
    }

    pub fn on_failure(&mut self, id: &NodeId) {
        self.settle(id, State::Failed);
    }

    /// A query to `id` couldn't be sent for lack of budget: it goes back
    /// to fresh, to be asked on a later drive, rather than counting as a
    /// node that failed to answer.
    pub fn retry_later(&mut self, id: &NodeId) {
        // Only a query just handed out, never a slow one still out.
        if let Some(c) = self
            .candidates
            .iter_mut()
            .find(|c| c.info.id == *id && c.state == State::InFlight)
        {
            c.state = State::Fresh;
            self.in_flight -= 1;
        }
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
    /// have all answered. CR-SOFT-TIMEOUT: slow queries out don't hold it
    /// once the K closest candidates that are neither failed nor slow have
    /// all answered. With fewer than K such answers, the leg waits for the
    /// slow ones to settle (answer or reach the query timeout), as with the
    /// switch off.
    pub fn is_done(&self) -> bool {
        if self.in_flight > self.slow || self.extra_in_flight > 0 {
            return false;
        }
        let mut counts = Lookup::slot_filter(self.scrape);
        let mut answered = 0;
        for c in self.candidates.iter().filter(|c| counts(c)).take(K) {
            if c.state == State::Fresh {
                return false;
            }
            answered += 1;
        }
        self.slow == 0 || answered >= K
    }

    /// CR-LOOKUP-END's convergence test: the [`K`] closest candidates that
    /// haven't failed have all answered, so no closer node is left to ask
    /// (candidates are kept in distance order), and they answered from at
    /// least `networks` distinct sources (an IPv4 /24 or an IPv6 /48, one
    /// operator's usual allocation). Unlike
    /// [`Lookup::is_done`], it doesn't wait for queries still out to
    /// candidates outside that set. It does wait for a query outside the
    /// candidate list (a hint, a router): that's at most one per leg, and
    /// often the leg's best source of values. The network rule keeps one
    /// operator from ending a leg early by answering, at once, for made-up
    /// ids it lists just next to the target. Whatever it says,
    /// [`Lookup::is_done`] still ends the leg as before.
    pub fn closest_answered(&self, networks: usize) -> bool {
        if self.is_done() {
            return true;
        }
        if self.extra_in_flight > 0 {
            return false;
        }
        let mut seen: Vec<Network> = Vec::with_capacity(networks);
        let mut answered = 0;
        for c in self
            .candidates
            .iter()
            .filter(|c| !matches!(c.state, State::Failed | State::Slow))
            .take(K)
        {
            if c.state != State::Responded {
                return false;
            }
            let net = source(c.info.addr.ip());
            if seen.len() < networks && !seen.contains(&net) {
                seen.push(net);
            }
            answered += 1;
        }
        // CR-SOFT-TIMEOUT: with slow queries out, only K answers are a
        // closest set (as in [`Lookup::is_done`]).
        seen.len() >= networks && (self.slow == 0 || answered >= K)
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

    /// Where a test reply comes from, when that doesn't matter.
    const FROM: SocketAddr = SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 7, 0, 1)),
        1,
    );

    fn n(b: u8) -> NodeInfo {
        NodeInfo {
            id: NodeId([b; 20]),
            addr: SocketAddr::from(([10, 0, 0, b], 1)),
        }
    }

    /// A node in its own /24 (`10.0.<b>.1`), so every one is a network.
    fn spread(b: u8) -> NodeInfo {
        NodeInfo {
            id: NodeId([b; 20]),
            addr: SocketAddr::from(([10, 0, b, 1], 1)),
        }
    }

    /// CR-LOOKUP-END: converged once the K closest that haven't failed
    /// have answered from enough networks, whatever is still out to
    /// candidates beyond them; never while a query outside the list is out.
    #[test]
    fn closest_answered_ignores_queries_beyond_the_closest_set() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        // Three far nodes go out.
        for b in [0xF1, 0xF2, 0xF3] {
            l.add(spread(b));
        }
        assert_eq!(l.next().len(), 3);
        assert!(!l.closest_answered(2), "nobody has answered");
        // One answers with K closer nodes; they're asked one at a time.
        l.on_response(&NodeId([0xF1; 20]), FROM, (1..=K as u8).map(spread), &[]);
        while let Some(c) = l.next_limit(1).pop() {
            assert!(!l.closest_answered(2));
            l.on_response(&c.id, FROM, std::iter::empty(), &[]);
        }
        assert!(l.closest_answered(2));
        assert!(!l.is_done(), "two far queries are still out");
        // A failed node in the set gives its place to the next one.
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        l.add(spread(1));
        l.add(spread(2));
        let sent = l.next();
        l.on_failure(&sent[0].id);
        assert!(!l.closest_answered(2));
        l.on_response(&sent[1].id, FROM, std::iter::empty(), &[]);
        assert!(l.closest_answered(2) && l.is_done());
    }

    /// An IPv6 node in `2001:db8:<site>:<subnet>::1`.
    fn site(b: u8, site: u16, subnet: u16) -> NodeInfo {
        NodeInfo {
            id: NodeId([b; 20]),
            addr: SocketAddr::from((
                std::net::Ipv6Addr::new(0x2001, 0xdb8, site, subnet, 0, 0, 0, 1),
                1,
            )),
        }
    }

    /// On IPv6 a source is a /48: a site's many /64s count once, so they
    /// can't converge a leg early between them; a second site can.
    #[test]
    fn one_ipv6_site_is_one_source() {
        let early = |second_site: u16| {
            let mut l = Lookup::new(NodeId([0; 20]), 0);
            // Three far nodes go out.
            for (i, b) in [0xF1u8, 0xF2, 0xF3].into_iter().enumerate() {
                l.add(site(b, 9, i as u16));
            }
            assert_eq!(l.next().len(), 3);
            // One lists K closer nodes, each its own /64; all in one /48,
            // or the last in a second.
            let close = (1..=K as u8).map(|b| {
                let s = if b == K as u8 { second_site } else { 1 };
                site(b, s, u16::from(b))
            });
            l.on_response(&NodeId([0xF1; 20]), FROM, close, &[]);
            while let Some(c) = l.next_limit(1).pop() {
                l.on_response(&c.id, FROM, std::iter::empty(), &[]);
            }
            assert!(!l.is_done(), "two far queries are still out");
            l.closest_answered(2)
        };
        assert!(!early(1), "8 /64s in one /48");
        assert!(early(2), "a second /48");
    }

    /// A hint (or router) query out holds the leg, even with fewer than K
    /// candidates all answered.
    #[test]
    fn closest_answered_waits_for_a_query_outside_the_list() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        l.extra_in_flight = 1;
        assert!(!l.closest_answered(1));
        for b in 1..=3 {
            l.add(spread(b));
        }
        for c in l.next() {
            l.on_response(&c.id, FROM, std::iter::empty(), &[]);
        }
        assert!(!l.closest_answered(1), "the hint is still out");
        l.extra_in_flight = 0;
        assert!(l.closest_answered(1));
    }

    /// One network answering at once for made-up ids next to the target
    /// can't converge a leg early; the queries out to others still count.
    #[test]
    fn one_network_cannot_converge_a_leg_early() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for b in [0xF1, 0xF2, 0xF3] {
            l.add(spread(b));
        }
        l.next();
        // The hostile far node lists K ids at its own /24 on many ports.
        let fake = (1..=K as u8).map(|b| NodeInfo {
            id: NodeId([b; 20]),
            addr: SocketAddr::from(([10, 0, 0xF1, 1], u16::from(b))),
        });
        l.on_response(&NodeId([0xF1; 20]), FROM, fake, &[]);
        while let Some(c) = l.next_limit(1).pop() {
            l.on_response(&c.id, FROM, std::iter::empty(), &[]);
        }
        assert!(!l.closest_answered(2), "one network");
        assert!(!l.is_done(), "the honest queries are still out");
    }

    fn values(net: u8, n: u8) -> Vec<SocketAddr> {
        (0..n)
            .map(|i| SocketAddr::from(([10, 9, net, i], 6881)))
            .collect()
    }

    /// A leg's values are shared between sources, within its cap.
    #[test]
    fn leg_values_are_shared_across_sources() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        let first = SocketAddr::from(([10, 1, 0, 1], 1));
        let second = SocketAddr::from(([10, 2, 0, 1], 1));
        // Another address in the first's /24 is the same source.
        let first_again = SocketAddr::from(([10, 1, 0, 2], 1));
        l.absorb(first, std::iter::empty(), &values(1, 64));
        assert_eq!(l.peers.len(), MAX_VALUES);
        l.absorb(first_again, std::iter::empty(), &values(3, 64));
        assert!(values(3, 64).iter().all(|v| !l.peers.contains(v)));
        l.absorb(second, std::iter::empty(), &values(2, 5));
        assert_eq!(l.peers.len(), MAX_VALUES);
        assert!(values(2, 5).iter().all(|v| l.peers.contains(v)));
    }

    /// A responder answering alone still fills the whole leg.
    #[test]
    fn one_source_alone_fills_the_leg() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        l.add(n(1));
        l.next();
        l.on_response(&NodeId([1; 20]), FROM, std::iter::empty(), &values(1, 100));
        assert_eq!(l.peers.len(), MAX_VALUES);
    }

    /// CR-SOFT-TIMEOUT: a soft-expired query keeps its lane (nothing more
    /// is asked); its late answer settles it once, freeing the lane, and a
    /// repeat or a failure after that does nothing.
    #[test]
    fn a_slow_candidate_keeps_its_lane_and_settles_once() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for b in 1..=6 {
            l.add(spread(b));
        }
        let first = l.next();
        assert_eq!(first.len(), 3);
        assert!(l.soft_expire(&first[0].id));
        assert!(!l.soft_expire(&first[0].id), "already slow");
        assert!(l.soft_expire(&first[1].id) && l.soft_expire(&first[2].id));
        assert_eq!(l.slow(), 3);
        assert!(l.next().is_empty(), "slow queries keep their lanes");
        // The late answer settles once and frees its lane; a repeat and a
        // failure don't.
        assert!(l.on_response(&first[0].id, FROM, std::iter::empty(), &[]));
        assert_eq!(l.slow(), 2);
        assert_eq!(l.next().len(), 1, "its lane, freed by the answer");
        assert!(!l.on_response(&first[0].id, FROM, std::iter::empty(), &[]));
        l.on_failure(&first[0].id);
        assert_eq!(l.slow(), 2);
        // A slow one that times out fails once.
        l.on_failure(&first[1].id);
        assert_eq!(l.slow(), 1);
        l.on_failure(&first[1].id);
        assert_eq!(l.slow(), 1);
    }

    /// CR-SOFT-TIMEOUT: a slow candidate doesn't hold a closest set of K
    /// others that have answered (farther ones asked earlier in the walk),
    /// in either end test; with fewer than K answers the leg waits for it.
    /// Answering late, it counts again.
    #[test]
    fn a_slow_candidate_holds_only_a_short_closest_set() {
        // Three candidates: the slow one holds the end.
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for b in 1..=3 {
            l.add(spread(b));
        }
        let q = l.next();
        l.on_response(&q[1].id, FROM, std::iter::empty(), &[]);
        l.on_response(&q[2].id, FROM, std::iter::empty(), &[]);
        assert!(l.soft_expire(&q[0].id));
        assert!(!l.is_done() && !l.closest_answered(2), "only 2 answers");
        l.on_response(&q[0].id, FROM, std::iter::empty(), &[]);
        assert!(l.is_done());
        let closest: Vec<u8> = l.closest().map(|i| i.id.0[0]).collect();
        assert_eq!(closest, [1, 2, 3], "counts again once answered");
        // Three far nodes answer first, listing 8 closer ones; the closest
        // of those goes slow and the other 7 answer. With the far ones,
        // that's K answers without it.
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        for b in [0xF1, 0xF2, 0xF3] {
            l.add(spread(b));
        }
        for c in l.next() {
            let close = if c.id.0[0] == 0xF1 {
                (1..=8).map(spread).collect()
            } else {
                vec![]
            };
            l.on_response(&c.id, FROM, close.into_iter(), &[]);
        }
        let first = l.next();
        assert_eq!(first[0].id.0[0], 1);
        assert!(l.soft_expire(&first[0].id));
        for c in &first[1..] {
            l.on_response(&c.id, FROM, std::iter::empty(), &[]);
        }
        while let Some(c) = l.next_limit(1).pop() {
            assert!(!l.is_done());
            l.on_response(&c.id, FROM, std::iter::empty(), &[]);
        }
        assert_eq!(l.answered_live(), K);
        assert!(l.is_done() && l.closest_answered(2));
        // A late answer then still settles it.
        assert!(l.on_response(&first[0].id, FROM, std::iter::empty(), &[]));
        assert_eq!(l.slow(), 0);
    }

    /// CR-SOFT-TIMEOUT: crowding by closer candidates never drops a slow
    /// one, so its late answer still finds it.
    #[test]
    fn crowding_keeps_slow_candidates() {
        let mut l = Lookup::new(NodeId([0; 20]), 0);
        let far = NodeInfo {
            id: NodeId([0xff; 20]),
            addr: SocketAddr::from(([10, 3, 0, 1], 1)),
        };
        l.add(far);
        let q = l.next();
        assert!(l.soft_expire(&q[0].id));
        for b in 0..=255u8 {
            let mut id = [b; 20];
            id[0] = 0;
            l.add(NodeInfo {
                id: NodeId(id),
                addr: SocketAddr::from(([10, 4, 0, b], 1)),
            });
        }
        assert!(l.candidates.len() <= CAP + 1);
        assert!(l.on_response(&far.id, FROM, std::iter::empty(), &[]));
        assert_eq!(l.slow(), 0);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Add(u8),
        Next(usize),
        Soft(usize),
        Answer(usize, Vec<u8>),
        Fail(usize),
        Retry(usize),
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            any::<u8>().prop_map(Op::Add),
            (0usize..4).prop_map(Op::Next),
            any::<usize>().prop_map(Op::Soft),
            (any::<usize>(), prop::collection::vec(any::<u8>(), 0..8))
                .prop_map(|(i, n)| Op::Answer(i, n)),
            any::<usize>().prop_map(Op::Fail),
            any::<usize>().prop_map(Op::Retry),
        ]
    }

    proptest::proptest! {
        /// CR-SOFT-TIMEOUT: under any sequence of adds, sends, soft
        /// expiries, answers (repeated ones included), failures and
        /// retries, the counters match the states, at most ALPHA queries
        /// are out (slow ones included: they keep their lanes), the list
        /// stays bounded, and every id settles at most once per send.
        #[test]
        fn counters_match_states_under_any_sequence(ops in proptest::collection::vec(op(), 1..200)) {
            let mut l = Lookup::new(NodeId([0; 20]), 0);
            let mut sent: Vec<NodeId> = Vec::new();
            for op in ops {
                match op {
                    Op::Add(b) => l.add(spread(b)),
                    Op::Next(n) => sent.extend(l.next_limit(n).iter().map(|i| i.id)),
                    Op::Soft(i) if !sent.is_empty() => {
                        l.soft_expire(&sent[i % sent.len()]);
                    }
                    Op::Answer(i, nodes) if !sent.is_empty() => {
                        let id = sent[i % sent.len()];
                        let was_out = l.candidates.iter().any(|c| {
                            c.info.id == id && matches!(c.state, State::InFlight | State::Slow)
                        });
                        let ours = l.on_response(&id, FROM, nodes.into_iter().map(spread), &[]);
                        proptest::prop_assert_eq!(ours, was_out);
                    }
                    Op::Fail(i) if !sent.is_empty() => l.on_failure(&sent[i % sent.len()]),
                    Op::Retry(i) if !sent.is_empty() => l.retry_later(&sent[i % sent.len()]),
                    _ => {}
                }
                let count = |s: State| l.candidates.iter().filter(|c| c.state == s).count();
                proptest::prop_assert_eq!(l.in_flight, count(State::InFlight) + count(State::Slow));
                proptest::prop_assert_eq!(l.slow, count(State::Slow));
                proptest::prop_assert!(l.in_flight <= ALPHA);
                proptest::prop_assert!(l.candidates.len() <= CAP + ALPHA);
            }
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
        l.on_response(&NodeId([1; 20]), FROM, [n(2)].into_iter(), &[]);
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
                    l.on_response(&q.id, FROM, std::iter::empty(), &[]);
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
        l.on_response(&NodeId([1; 20]), FROM, std::iter::empty(), &[peer, peer]);
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
                l.on_response(&q.id, FROM, std::iter::empty(), &[]);
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
        l.on_response(&q[0].id, FROM, std::iter::empty(), &[]);
        assert!(l.is_done());
    }
}
