//! Kademlia routing table (BEP 5) for one node id and one address family.
//!
//! Buckets hold up to [`K`] nodes. Only the bucket covering our own id
//! splits, so the table is detailed near us and coarse far away. Node
//! health follows BEP 5:
//!
//! * **good**: answered one of our queries in the last 15 minutes, or has
//!   answered before and queried us in the last 15 minutes;
//! * **questionable**: otherwise;
//! * **bad**: failed to answer [`MAX_FAILURES`] queries in a row.
//!
//! Sybil and hijack resistance: only nodes that have answered us enter the
//! main buckets (nodes that merely queried us wait in a replacement
//! cache); each IP holds at most one entry; and an existing id can't be
//! moved to a new address, because that's exactly what a spoofed packet
//! would try.
//!
//! Time is passed in as monotonic seconds so the logic is deterministic
//! and testable.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;

use crate::compact::{NodeId, NodeInfo};

pub const K: usize = 8;
/// How long a response (or a query from a verified node) keeps a node good.
pub const GOOD_FOR_SECS: u64 = 15 * 60;
pub const MAX_FAILURES: u8 = 2;
/// Buckets untouched for this long should be refreshed with a lookup.
pub const REFRESH_AFTER_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Good,
    Questionable,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub info: NodeInfo,
    pub last_response: Option<u64>,
    pub last_query: Option<u64>,
    pub failures: u8,
}

impl Entry {
    fn new(info: NodeInfo) -> Entry {
        Entry {
            info,
            last_response: None,
            last_query: None,
            failures: 0,
        }
    }

    pub fn status(&self, now: u64) -> Status {
        let recent = |t: Option<u64>| t.is_some_and(|t| now.saturating_sub(t) < GOOD_FOR_SECS);
        if self.failures >= MAX_FAILURES {
            Status::Bad
        } else if recent(self.last_response)
            || (self.last_response.is_some() && recent(self.last_query))
        {
            Status::Good
        } else {
            Status::Questionable
        }
    }

    fn touch(&mut self, contact: Contact, now: u64) {
        match contact {
            Contact::Response => {
                self.last_response = Some(now);
                self.failures = 0;
            }
            Contact::Query => self.last_query = Some(now),
        }
    }
}

/// How we heard from a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contact {
    /// It answered one of our queries: its address is verified.
    Response,
    /// It sent us a query: its address may be spoofed.
    Query,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inserted {
    Added,
    Updated,
    /// Took the slot of a bad node, which was dropped.
    Replaced(NodeInfo),
    /// The bucket is full but holds questionable nodes: ping these. The new
    /// node waits in the replacement cache and takes over if one fails.
    Pending(Vec<NodeInfo>),
    /// Kept in the replacement cache only.
    Cached,
    Rejected(Reject),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    OwnId,
    PortZero,
    /// Another id already lives at this IP.
    IpTaken,
    /// This id is known at a different address.
    AddressMismatch,
    WrongFamily,
}

#[derive(Debug, Default)]
struct Bucket {
    entries: Vec<Entry>,
    replacements: VecDeque<Entry>,
    last_changed: u64,
}

#[derive(Debug)]
pub struct RoutingTable {
    own: NodeId,
    ipv6: bool,
    buckets: Vec<Bucket>,
    /// Which id occupies each IP, across entries and replacements.
    ips: HashMap<IpAddr, NodeId>,
}

impl RoutingTable {
    pub fn new(own: NodeId, ipv6: bool) -> RoutingTable {
        RoutingTable {
            own,
            ipv6,
            buckets: vec![Bucket::default()],
            ips: HashMap::new(),
        }
    }

    pub fn own_id(&self) -> NodeId {
        self.own
    }

    /// Number of nodes in the main buckets.
    pub fn len(&self) -> usize {
        self.buckets.iter().map(|b| b.entries.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.buckets.iter().flat_map(|b| b.entries.iter())
    }

    pub fn get(&self, id: &NodeId) -> Option<&Entry> {
        let b = &self.buckets[self.bucket_index(id)];
        b.entries.iter().find(|e| e.info.id == *id)
    }

    /// Shared prefix length with our id, capped to the last bucket.
    fn bucket_index(&self, id: &NodeId) -> usize {
        prefix_len(&self.own, id).min(self.buckets.len() - 1)
    }

    /// Records contact with a node and decides whether it enters the table.
    pub fn insert(&mut self, info: NodeInfo, contact: Contact, now: u64) -> Inserted {
        if info.id == self.own {
            return Inserted::Rejected(Reject::OwnId);
        }
        if info.addr.port() == 0 {
            return Inserted::Rejected(Reject::PortZero);
        }
        if info.addr.is_ipv6() != self.ipv6 {
            return Inserted::Rejected(Reject::WrongFamily);
        }
        let idx = self.bucket_index(&info.id);
        let bucket = &mut self.buckets[idx];
        // Known id: refresh it, but never move it to a new address.
        if let Some(e) = bucket.entries.iter_mut().find(|e| e.info.id == info.id) {
            if e.info.addr != info.addr {
                return Inserted::Rejected(Reject::AddressMismatch);
            }
            e.touch(contact, now);
            bucket.last_changed = now;
            return Inserted::Updated;
        }
        // Known only from the replacement cache.
        let cached = bucket
            .replacements
            .iter()
            .position(|r| r.info.id == info.id);
        let mut promoted = None;
        if let Some(pos) = cached {
            let r = &mut bucket.replacements[pos];
            if r.info.addr == info.addr {
                r.touch(contact, now);
                if contact == Contact::Query {
                    return Inserted::Cached;
                }
                // Now verified: move it out of the cache, keeping its history.
                promoted = self.remove(&info.id);
            } else if contact == Contact::Response && r.last_response.is_none() {
                // An unverified (possibly spoofed) address loses to a
                // verified one.
                self.remove(&info.id);
            } else {
                return Inserted::Rejected(Reject::AddressMismatch);
            }
        }
        // One entry per IP, unless the previous holder has gone bad.
        if let Some(holder) = self.ips.get(&info.addr.ip()).copied() {
            if self
                .get(&holder)
                .is_some_and(|e| e.status(now) == Status::Bad)
            {
                self.remove(&holder);
            } else {
                return Inserted::Rejected(Reject::IpTaken);
            }
        }

        let mut entry = promoted.unwrap_or_else(|| Entry::new(info));
        entry.touch(contact, now);
        if contact == Contact::Query {
            self.cache(entry);
            return Inserted::Cached;
        }
        loop {
            let idx = self.bucket_index(&info.id);
            let count = self.buckets.len();
            let bucket = &mut self.buckets[idx];
            if bucket.entries.len() < K {
                bucket.entries.push(entry);
                bucket.last_changed = now;
                self.ips.insert(info.addr.ip(), info.id);
                return Inserted::Added;
            }
            if idx == count - 1 && count < 160 {
                self.split();
                continue;
            }
            if let Some(pos) = bucket
                .entries
                .iter()
                .position(|e| e.status(now) == Status::Bad)
            {
                let old = std::mem::replace(&mut bucket.entries[pos], entry);
                bucket.last_changed = now;
                self.ips.remove(&old.info.addr.ip());
                self.ips.insert(info.addr.ip(), info.id);
                return Inserted::Replaced(old.info);
            }
            let questionable: Vec<NodeInfo> = bucket
                .entries
                .iter()
                .filter(|e| e.status(now) == Status::Questionable)
                .map(|e| e.info)
                .collect();
            self.cache(entry);
            return if questionable.is_empty() {
                Inserted::Cached
            } else {
                Inserted::Pending(questionable)
            };
        }
    }

    /// Adds to the bucket's replacement cache (newest kept, at most K).
    /// Caching doesn't count as a bucket change for refresh purposes.
    fn cache(&mut self, entry: Entry) {
        let idx = self.bucket_index(&entry.info.id);
        let bucket = &mut self.buckets[idx];
        let mut freed = Vec::new();
        if let Some(pos) = bucket
            .replacements
            .iter()
            .position(|r| r.info.id == entry.info.id)
        {
            freed.extend(bucket.replacements.remove(pos));
        }
        if bucket.replacements.len() == K {
            freed.extend(bucket.replacements.pop_front());
        }
        bucket.replacements.push_back(entry.clone());
        for old in freed {
            self.ips.remove(&old.info.addr.ip());
        }
        self.ips.insert(entry.info.addr.ip(), entry.info.id);
    }

    /// Records a query the node didn't answer. Once it turns bad, the
    /// freshest verified replacement takes its slot.
    pub fn failed(&mut self, id: &NodeId, now: u64) {
        let idx = self.bucket_index(id);
        let bucket = &mut self.buckets[idx];
        let Some(pos) = bucket.entries.iter().position(|e| e.info.id == *id) else {
            if let Some(r) = bucket.replacements.iter().position(|r| r.info.id == *id) {
                let gone = bucket.replacements.remove(r).expect("position is valid");
                self.ips.remove(&gone.info.addr.ip());
            }
            return;
        };
        bucket.entries[pos].failures = bucket.entries[pos].failures.saturating_add(1);
        if bucket.entries[pos].status(now) != Status::Bad {
            return;
        }
        let verified = bucket
            .replacements
            .iter()
            .rposition(|r| r.last_response.is_some());
        if let Some(r) = verified {
            let replacement = bucket.replacements.remove(r).expect("position is valid");
            let old = std::mem::replace(&mut bucket.entries[pos], replacement);
            bucket.last_changed = now;
            self.ips.remove(&old.info.addr.ip());
        }
    }

    pub fn remove(&mut self, id: &NodeId) -> Option<Entry> {
        let idx = self.bucket_index(id);
        let bucket = &mut self.buckets[idx];
        let removed = if let Some(pos) = bucket.entries.iter().position(|e| e.info.id == *id) {
            Some(bucket.entries.remove(pos))
        } else if let Some(pos) = bucket.replacements.iter().position(|e| e.info.id == *id) {
            bucket.replacements.remove(pos)
        } else {
            None
        };
        if let Some(e) = &removed {
            self.ips.remove(&e.info.addr.ip());
        }
        removed
    }

    /// The `n` closest non-bad nodes to `target`, closest first.
    pub fn closest(&self, target: &NodeId, n: usize, now: u64) -> Vec<NodeInfo> {
        let mut nodes: Vec<&Entry> = self
            .entries()
            .filter(|e| e.status(now) != Status::Bad)
            .collect();
        nodes.sort_unstable_by_key(|e| e.info.id.distance(target));
        nodes.into_iter().take(n).map(|e| e.info).collect()
    }

    /// Questionable nodes that should be pinged to learn whether they're alive.
    pub fn questionable(&self, now: u64) -> Vec<NodeInfo> {
        self.entries()
            .filter(|e| e.status(now) == Status::Questionable)
            .map(|e| e.info)
            .collect()
    }

    /// One random-looking target per stale bucket, to be looked up so the
    /// bucket refills (BEP 5 refresh). `fill` supplies the random bits.
    pub fn refresh_targets(&self, now: u64, mut fill: impl FnMut() -> [u8; 20]) -> Vec<NodeId> {
        self.buckets
            .iter()
            .enumerate()
            .filter(|(_, b)| now.saturating_sub(b.last_changed) >= REFRESH_AFTER_SECS)
            .map(|(i, _)| id_in_bucket(&self.own, i, fill()))
            .collect()
    }

    fn split(&mut self) {
        let last = self.buckets.len() - 1;
        let old = std::mem::take(&mut self.buckets[last]);
        let mut near = Bucket {
            last_changed: old.last_changed,
            ..Bucket::default()
        };
        let mut far = Bucket {
            last_changed: old.last_changed,
            ..Bucket::default()
        };
        for e in old.entries {
            if prefix_len(&self.own, &e.info.id) > last {
                near.entries.push(e);
            } else {
                far.entries.push(e);
            }
        }
        for r in old.replacements {
            if prefix_len(&self.own, &r.info.id) > last {
                near.replacements.push_back(r);
            } else {
                far.replacements.push_back(r);
            }
        }
        self.buckets[last] = far;
        self.buckets.push(near);
    }
}

/// Number of leading bits `a` and `b` share (160 when equal).
pub fn prefix_len(a: &NodeId, b: &NodeId) -> usize {
    let d = a.distance(b);
    for (i, byte) in d.iter().enumerate() {
        if *byte != 0 {
            return i * 8 + byte.leading_zeros() as usize;
        }
    }
    160
}

/// An id sharing exactly `bucket` leading bits with `own` (random below).
fn id_in_bucket(own: &NodeId, bucket: usize, random: [u8; 20]) -> NodeId {
    let mut id = random;
    let bucket = bucket.min(159);
    for bit in 0..=bucket {
        let byte = bit / 8;
        let mask = 0x80u8 >> (bit % 8);
        let own_bit = own.0[byte] & mask;
        // Copy our bits for the shared prefix, then flip the next one.
        let want = if bit < bucket {
            own_bit
        } else {
            own_bit ^ mask
        };
        id[byte] = (id[byte] & !mask) | want;
    }
    NodeId(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn node(id: [u8; 20], ip: &str) -> NodeInfo {
        NodeInfo {
            id: NodeId(id),
            addr: format!("{ip}:6881").parse::<SocketAddr>().unwrap(),
        }
    }

    /// An id whose first byte is `b` and the rest `rest`.
    fn id(b: u8, rest: u8) -> [u8; 20] {
        let mut x = [rest; 20];
        x[0] = b;
        x
    }

    #[test]
    fn prefix_lengths() {
        let a = NodeId([0; 20]);
        assert_eq!(prefix_len(&a, &a), 160);
        assert_eq!(prefix_len(&a, &NodeId(id(0x80, 0))), 0);
        assert_eq!(prefix_len(&a, &NodeId(id(0x01, 0))), 7);
        let mut last = [0u8; 20];
        last[19] = 1;
        assert_eq!(prefix_len(&a, &NodeId(last)), 159);
    }

    #[test]
    fn rejects_bad_inputs() {
        let mut t = RoutingTable::new(NodeId([0; 20]), false);
        assert_eq!(
            t.insert(node([0; 20], "1.1.1.1"), Contact::Response, 0),
            Inserted::Rejected(Reject::OwnId)
        );
        let zero = NodeInfo {
            id: NodeId([1; 20]),
            addr: "1.1.1.1:0".parse().unwrap(),
        };
        assert_eq!(
            t.insert(zero, Contact::Response, 0),
            Inserted::Rejected(Reject::PortZero)
        );
        let v6 = NodeInfo {
            id: NodeId([1; 20]),
            addr: "[::2]:1".parse().unwrap(),
        };
        assert_eq!(
            t.insert(v6, Contact::Response, 0),
            Inserted::Rejected(Reject::WrongFamily)
        );
    }

    #[test]
    fn queries_alone_never_enter_main_buckets() {
        let mut t = RoutingTable::new(NodeId([0; 20]), false);
        let n = node(id(0x80, 1), "1.1.1.1");
        assert_eq!(t.insert(n, Contact::Query, 0), Inserted::Cached);
        assert_eq!(t.len(), 0);
        // A response verifies it.
        assert_eq!(t.insert(n, Contact::Response, 1), Inserted::Added);
        assert_eq!(t.len(), 1);
        assert_eq!(t.insert(n, Contact::Query, 2), Inserted::Updated);
    }

    #[test]
    fn ids_cannot_be_hijacked_and_ips_hold_one_id() {
        let mut t = RoutingTable::new(NodeId([0; 20]), false);
        let n = node(id(0x80, 1), "1.1.1.1");
        t.insert(n, Contact::Response, 0);
        let moved = node(id(0x80, 1), "2.2.2.2");
        assert_eq!(
            t.insert(moved, Contact::Response, 1),
            Inserted::Rejected(Reject::AddressMismatch)
        );
        let squatter = node(id(0x80, 2), "1.1.1.1");
        assert_eq!(
            t.insert(squatter, Contact::Response, 1),
            Inserted::Rejected(Reject::IpTaken)
        );
        // Once the holder is bad, the IP frees up.
        t.failed(&n.id, 2);
        t.failed(&n.id, 3);
        assert_eq!(t.get(&n.id).unwrap().status(3), Status::Bad);
        assert_eq!(t.insert(squatter, Contact::Response, 4), Inserted::Added);
        assert!(t.get(&n.id).is_none());
    }

    #[test]
    fn verified_response_replaces_unverified_cached_address() {
        // Found by the invariant property test: the cached copy used to
        // linger, leaving the id in the table twice.
        let mut t = RoutingTable::new(NodeId([0; 20]), false);
        let spoofed = node(id(0x80, 1), "1.1.1.1");
        let real = node(id(0x80, 1), "2.2.2.2");
        assert_eq!(t.insert(spoofed, Contact::Query, 0), Inserted::Cached);
        assert_eq!(t.insert(real, Contact::Response, 1), Inserted::Added);
        assert_eq!(t.get(&real.id).unwrap().info.addr, real.addr);
        // The spoofed IP is free again.
        assert_eq!(
            t.insert(node(id(0x80, 9), "1.1.1.1"), Contact::Response, 2),
            Inserted::Added
        );
        // A verified address is never displaced by a different one.
        let other = node(id(0x80, 1), "3.3.3.3");
        assert_eq!(
            t.insert(other, Contact::Response, 3),
            Inserted::Rejected(Reject::AddressMismatch)
        );
    }

    #[test]
    fn health_follows_bep5() {
        let mut e = Entry::new(node([1; 20], "1.1.1.1"));
        assert_eq!(e.status(0), Status::Questionable);
        e.touch(Contact::Response, 100);
        assert_eq!(e.status(100 + GOOD_FOR_SECS - 1), Status::Good);
        assert_eq!(e.status(100 + GOOD_FOR_SECS), Status::Questionable);
        e.touch(Contact::Query, 2000);
        assert_eq!(
            e.status(2000),
            Status::Good,
            "verified node that queried us recently"
        );
        e.failures = MAX_FAILURES;
        assert_eq!(e.status(2000), Status::Bad);
    }

    #[test]
    fn splits_near_own_id_and_stays_bounded_far_away() {
        let own = NodeId([0; 20]);
        let mut t = RoutingTable::new(own, false);
        // 20 far nodes (first bit set) share bucket 0 after splits: only K fit.
        for i in 0..20u8 {
            t.insert(
                node(id(0x80 | i, i), &format!("1.0.0.{i}")),
                Contact::Response,
                0,
            );
        }
        assert_eq!(
            t.entries()
                .filter(|e| prefix_len(&own, &e.info.id) == 0)
                .count(),
            K
        );
        // Nodes ever closer to us keep splitting the last bucket.
        for bits in 1..40u8 {
            let mut x = [0u8; 20];
            let bit = bits as usize;
            x[bit / 8] = 0x80 >> (bit % 8);
            t.insert(node(x, &format!("2.0.0.{bits}")), Contact::Response, 0);
        }
        assert!(t.bucket_count() > 30);
        for (i, b) in t.buckets.iter().enumerate() {
            assert!(b.entries.len() <= K);
            for e in &b.entries {
                let p = prefix_len(&own, &e.info.id);
                if i + 1 < t.bucket_count() {
                    assert_eq!(p, i, "entry in the wrong bucket");
                } else {
                    assert!(p >= i);
                }
            }
        }
    }

    #[test]
    fn full_bucket_asks_for_pings_then_promotes_replacement() {
        let own = NodeId([0; 20]);
        let mut t = RoutingTable::new(own, false);
        // Fill the far bucket (and force splits past it with one near node).
        t.insert(node(id(0x00, 1), "9.9.9.9"), Contact::Response, 0);
        let far: Vec<NodeInfo> = (0..K as u8)
            .map(|i| node(id(0x80 | i, 7), &format!("1.0.0.{i}")))
            .collect();
        for n in &far {
            t.insert(*n, Contact::Response, 0);
        }
        let newcomer = node(id(0xf0, 7), "3.3.3.3");
        // While everyone is good, the newcomer is only cached.
        assert_eq!(t.insert(newcomer, Contact::Response, 10), Inserted::Cached);
        // Later, the old nodes are questionable: ping them.
        let later = 10 + GOOD_FOR_SECS;
        let Inserted::Pending(ping) = t.insert(newcomer, Contact::Response, later) else {
            panic!("expected pings");
        };
        assert_eq!(ping.len(), K);
        // One fails twice: the newcomer takes its slot.
        t.failed(&far[0].id, later + 1);
        t.failed(&far[0].id, later + 2);
        assert!(t.get(&far[0].id).is_none());
        assert!(t.get(&newcomer.id).is_some());
    }

    #[test]
    fn closest_orders_by_xor_distance_and_skips_bad() {
        let own = NodeId([0; 20]);
        let mut t = RoutingTable::new(own, false);
        let nodes: Vec<NodeInfo> = [0x10u8, 0x20, 0x30, 0x40]
            .iter()
            .enumerate()
            .map(|(i, b)| node(id(*b, 0), &format!("5.0.0.{i}")))
            .collect();
        for n in &nodes {
            t.insert(*n, Contact::Response, 0);
        }
        let target = NodeId(id(0x31, 0));
        let got: Vec<u8> = t.closest(&target, 3, 0).iter().map(|n| n.id.0[0]).collect();
        assert_eq!(got, [0x30, 0x20, 0x10]);
        t.failed(&nodes[2].id, 1);
        t.failed(&nodes[2].id, 2);
        let got: Vec<u8> = t.closest(&target, 3, 2).iter().map(|n| n.id.0[0]).collect();
        assert_eq!(got, [0x20, 0x10, 0x40]);
    }

    #[test]
    fn refresh_targets_land_in_their_buckets() {
        let own = NodeId([0x5a; 20]);
        let mut t = RoutingTable::new(own, false);
        for i in 0..40u8 {
            let mut x = own.0;
            x[(i / 8) as usize] ^= 0x80 >> (i % 8);
            t.insert(node(x, &format!("6.0.0.{i}")), Contact::Response, 0);
        }
        let targets = t.refresh_targets(REFRESH_AFTER_SECS, || [0xff; 20]);
        assert_eq!(targets.len(), t.bucket_count());
        for (i, target) in targets.iter().enumerate() {
            if i + 1 < t.bucket_count() {
                assert_eq!(prefix_len(&own, target), i);
            }
        }
        assert!(
            t.refresh_targets(REFRESH_AFTER_SECS - 1, || [0; 20])
                .is_empty()
        );
    }

    /// Checks every structural invariant of the table.
    fn assert_invariants(t: &RoutingTable) {
        let mut ids = std::collections::HashSet::new();
        let mut ips = std::collections::HashMap::new();
        for (i, b) in t.buckets.iter().enumerate() {
            assert!(b.entries.len() <= K, "bucket {i} overfull");
            assert!(b.replacements.len() <= K, "replacements {i} overfull");
            for e in b.entries.iter().chain(b.replacements.iter()) {
                let p = prefix_len(&t.own, &e.info.id);
                if i + 1 < t.buckets.len() {
                    assert_eq!(p, i, "entry in the wrong bucket");
                } else {
                    assert!(p >= i, "entry in the wrong bucket");
                }
                assert!(ids.insert(e.info.id), "duplicate id");
                assert!(
                    ips.insert(e.info.addr.ip(), e.info.id).is_none(),
                    "duplicate ip"
                );
                assert_ne!(e.info.id, t.own);
            }
        }
        assert_eq!(ips, t.ips, "ip index out of sync");
    }

    #[derive(Debug, Clone)]
    enum Op {
        Insert {
            id: u8,
            ip: u8,
            port: u16,
            response: bool,
        },
        Fail {
            id: u8,
        },
        Remove {
            id: u8,
        },
        Tick(u16),
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            6 => (any::<u8>(), 0u8..40, 1u16..4, any::<bool>())
                .prop_map(|(id, ip, port, response)| Op::Insert { id, ip, port, response }),
            2 => any::<u8>().prop_map(|id| Op::Fail { id }),
            1 => any::<u8>().prop_map(|id| Op::Remove { id }),
            1 => any::<u16>().prop_map(Op::Tick),
        ]
    }

    /// Spreads a small id seed over the keyspace, with some ids close to
    /// our own so buckets split.
    fn spread(seed: u8) -> NodeId {
        let mut x = [seed.wrapping_mul(97); 20];
        x[0] = seed >> (seed % 8);
        NodeId(x)
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(512))]

        #[test]
        fn invariants_hold_under_random_operations(ops in proptest::collection::vec(op(), 1..300)) {
            let mut t = RoutingTable::new(NodeId([0; 20]), false);
            let mut now = 0u64;
            for op in ops {
                match op {
                    Op::Insert { id, ip, port, response } => {
                        let info = NodeInfo {
                            id: spread(id),
                            addr: SocketAddr::from(([7, 0, 0, ip], port)),
                        };
                        let contact = if response { Contact::Response } else { Contact::Query };
                        t.insert(info, contact, now);
                    }
                    Op::Fail { id } => t.failed(&spread(id), now),
                    Op::Remove { id } => {
                        t.remove(&spread(id));
                    }
                    Op::Tick(s) => now += u64::from(s),
                }
                assert_invariants(&t);
            }
        }
    }
}
