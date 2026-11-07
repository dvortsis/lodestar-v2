//! Placement v1 (architecture §19.5; workers plan WK-5, Q9): who runs
//! which part of the work.
//!
//! - **The DHT is sharded by external IP.** Members that share a public
//!   IP (the coordinator's own DHT counts as one member of its IP) split
//!   that IP's 8 BEP 42 id slots between them, so they never mint the same
//!   id prefix. A member alone on its IP keeps all 8. Across the fleet the
//!   256 target prefixes are split between every DHT member, so no two
//!   walk the same prefix.
//! - **Fetch and prepare run everywhere.**
//! - **No source plugin runs on a worker.** [`Assignment`] has no field
//!   for one; the coordinator keeps them all (plan Q9).
//! - **Same-IP politeness.** Members of one IP divide its per-remote
//!   limits ([`Politeness`]): connections per peer IP, and BEP 51 sampling
//!   intervals per remote node. A remote peer never sees more from one
//!   address than from a single install.
//!
//! **Reported addresses are hints.** A worker reports its public IPs in
//! `Hello` (its own word; an admin-joined worker over mTLS, so a lie only
//! makes its own address less polite). A worker that reports more than
//! one may leave through any of them, so it's planned in the
//! coordinator's group. Until the coordinator knows every member's
//! address it can't tell who shares one, so while any member's is unknown
//! the whole fleet is planned as one IP: polite always, slower until the
//! addresses are known.
//!
//! [`Planner`] keeps the membership and the per-worker settings, and makes
//! a new plan, with a new revision, on every join, leave, pin or setting.
//! With no worker joined, the coordinator's share is the full keyspace and
//! a single install's limits: exactly today's behaviour (§19.8).

use std::collections::BTreeMap;
use std::net::IpAddr;

use crate::WorkerId;
use crate::assign::{
    ALL_SLOTS, Assignment, Budget, DEFAULT_POOL_CAP, INSTALL_PER_PEER_IP, Intensity, Politeness,
    ShardSpec,
};

/// BEP 42 gives one address 8 id slots, so at most 8 DHT members per IP.
pub const MAX_DHT_PER_IP: usize = 8;
/// The 256 target prefixes allow at most 256 DHT members in a fleet.
pub const MAX_DHT_MEMBERS: usize = 256;

/// One member of the fleet, as the planner sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberId {
    /// The coordinator's own DHT and fetch pool.
    Coordinator,
    Worker(WorkerId),
}

/// Whether a member may run the DHT. Pinning it off keeps fetch and
/// prepare; `Auto` runs the DHT when its IP has a slot left.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DhtPin {
    #[default]
    Auto,
    Off,
}

/// A worker's own settings (§19.5: each worker has its own budget and
/// intensity; plan Q5's pool cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkerSettings {
    pub budget: Budget,
    pub intensity: Intensity,
    pub pool_cap: u32,
}

impl Default for WorkerSettings {
    fn default() -> WorkerSettings {
        WorkerSettings {
            budget: Budget::default(),
            intensity: Intensity::default(),
            pool_cap: DEFAULT_POOL_CAP,
        }
    }
}

/// One member's input to [`plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: MemberId,
    /// The public addresses it reported; empty when unknown.
    pub ips: Vec<IpAddr>,
    /// Whether it may run the DHT.
    pub dht: bool,
}

/// One member's share in a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Share {
    /// The DHT's shard, or `None` when it runs no DHT.
    pub dht: Option<ShardSpec>,
    pub politeness: Politeness,
}

/// Splits the work between `members` (in any order; ids distinct), given
/// one install's connections per peer IP (`per_peer_ip`, 1–2). The result
/// is in member order: the coordinator first, then by worker id.
pub fn plan(members: &[Member], per_peer_ip: u8) -> Vec<(MemberId, Share)> {
    let per_peer_ip = per_peer_ip.clamp(1, INSTALL_PER_PEER_IP);
    // Only a public address says where a remote peer sees a member from:
    // a LAN, loopback or CGNAT address says nothing, so it's unknown.
    let public: Vec<Member> = members
        .iter()
        .map(|m| Member {
            id: m.id,
            ips: m
                .ips
                .iter()
                .map(|i| i.to_canonical())
                .filter(|i| is_public(*i))
                .collect(),
            dht: m.dht,
        })
        .collect();
    let mut members: Vec<&Member> = public.iter().collect();
    members.sort_by_key(|m| m.id);
    members.dedup_by_key(|m| m.id);
    let groups = ip_groups(&members);

    // DHT members, group by group: the first 8 of each IP that may run it,
    // and no more than the fleet's 256.
    let mut dht: Vec<usize> = Vec::new();
    let mut dht_in_group = vec![0usize; groups.len()];
    for (g, group) in groups.iter().enumerate() {
        for &i in group {
            if members[i].dht && dht_in_group[g] < MAX_DHT_PER_IP && dht.len() < MAX_DHT_MEMBERS {
                dht.push(i);
                dht_in_group[g] += 1;
            }
        }
    }

    let mut shares: Vec<Share> = vec![
        Share {
            dht: None,
            politeness: Politeness::ALONE,
        };
        members.len()
    ];

    // Prefixes: contiguous, fleet-wide. Slots: round-robin within an IP.
    let total = dht.len();
    let mut rank_in_group = vec![0usize; groups.len()];
    let group_of = group_index(&groups, members.len());
    for (n, &i) in dht.iter().enumerate() {
        let g = group_of[i];
        let (k, j) = (dht_in_group[g], rank_in_group[g]);
        rank_in_group[g] += 1;
        let start = (n * 256 / total) as u16;
        let end = ((n + 1) * 256 / total) as u16;
        let slots = (0..8u8)
            .filter(|s| usize::from(*s) % k == j)
            .fold(0u8, |acc, s| acc | (1 << s));
        debug_assert!(k > 1 || slots == ALL_SLOTS);
        shares[i].dht = ShardSpec::new(start..end, slots);
        debug_assert!(shares[i].dht.is_some());
    }

    // Politeness: every member of an IP fetches, so all of them share its
    // per-peer-IP connections; only its DHT members share BEP 51 intervals.
    for (g, group) in groups.iter().enumerate() {
        let n = group.len();
        let limit = usize::from(per_peer_ip);
        let peer_groups = n.div_ceil(limit);
        for (j, &i) in group.iter().enumerate() {
            let pg = j % peer_groups;
            let size = (0..n).filter(|x| x % peer_groups == pg).count();
            let rank = j / peer_groups;
            let share = limit / size + usize::from(rank < limit % size);
            shares[i].politeness = Politeness {
                peer_ip_groups: peer_groups as u16,
                peer_ip_group: pg as u16,
                per_peer_ip: share as u8,
                bep51_interval_scale: dht_in_group[g].clamp(1, MAX_DHT_PER_IP) as u8,
            };
        }
    }

    members.iter().map(|m| m.id).zip(shares).collect()
}

/// Whether a remote peer could see a member at `ip`: not private, CGNAT,
/// loopback, link-local, unique-local, unspecified, multicast or
/// broadcast. (Documentation ranges count as public, for tests.)
pub fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(a) => {
            let [o0, o1, ..] = a.octets();
            !(a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || o0 == 0
                || (o0 == 100 && (64..128).contains(&o1)))
        }
        IpAddr::V6(a) => {
            let seg0 = a.segments()[0];
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (seg0 & 0xfe00) == 0xfc00
                || (seg0 & 0xffc0) == 0xfe80)
        }
    }
}

/// Members (indices into the sorted list) grouped by shared address,
/// each group in member order, the groups ordered by their first member.
/// Members sharing any address are in one group, and a worker that
/// reported more than one is in the coordinator's; an unknown address puts
/// everyone in one group.
fn ip_groups(members: &[&Member]) -> Vec<Vec<usize>> {
    let n = members.len();
    if members.iter().any(|m| m.ips.is_empty()) {
        return if n == 0 {
            vec![]
        } else {
            vec![(0..n).collect()]
        };
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut owner: BTreeMap<IpAddr, usize> = BTreeMap::new();
    for (i, m) in members.iter().enumerate() {
        for ip in &m.ips {
            match owner.get(&ip.to_canonical()) {
                Some(&o) => {
                    let (a, b) = (root(&mut parent, o), root(&mut parent, i));
                    let (lo, hi) = (a.min(b), a.max(b));
                    parent[hi] = lo;
                }
                None => {
                    owner.insert(ip.to_canonical(), i);
                }
            }
        }
    }
    // A member that reported several addresses may leave through any of
    // several exits, the coordinator's among them: it shares the
    // coordinator's group (runner, 2026-11-07).
    if members
        .first()
        .is_some_and(|m| m.id == MemberId::Coordinator)
    {
        for (i, m) in members.iter().enumerate().skip(1) {
            if m.ips.len() > 1 {
                let (a, b) = (root(&mut parent, 0), root(&mut parent, i));
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    // Roots are each group's smallest index, so the map's order is the
    // groups' first-member order.
    groups.into_values().collect()
}

fn group_index(groups: &[Vec<usize>], n: usize) -> Vec<usize> {
    let mut out = vec![0; n];
    for (g, group) in groups.iter().enumerate() {
        for &i in group {
            out[i] = g;
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
struct WorkerEntry {
    ips: Vec<IpAddr>,
    settings: WorkerSettings,
    pin: DhtPin,
    /// Finishing its leases before it stops (§19.6): no DHT and no new
    /// work, but it still fetches, so it keeps its politeness share.
    draining: bool,
}

/// The coordinator's planner: the fleet's membership and settings, and
/// the current plan.
#[derive(Debug, Clone)]
pub struct Planner {
    revision: u64,
    coordinator_ips: Vec<IpAddr>,
    coordinator_dht: bool,
    per_peer_ip: u8,
    dict_version: u64,
    filter_version: u64,
    workers: BTreeMap<WorkerId, WorkerEntry>,
    coordinator_share: Share,
    assignments: BTreeMap<WorkerId, Assignment>,
}

/// A new plan: every remote worker's assignment (all at the new revision)
/// and the coordinator's own share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replan {
    pub revision: u64,
    pub coordinator: Share,
    pub workers: Vec<(WorkerId, Assignment)>,
}

impl Planner {
    /// A planner for a coordinator that runs the DHT when `coordinator_dht`
    /// is set. No worker has joined: the coordinator's share is the whole
    /// keyspace and one install's limits.
    pub fn new(coordinator_dht: bool) -> Planner {
        Planner::starting_at(coordinator_dht, 0)
    }

    /// As [`new`](Self::new), with plans numbered after `revision`. The
    /// coordinator seeds it from its clock, so a restarted coordinator's
    /// plans are still newer than the last one a worker applied.
    pub fn starting_at(coordinator_dht: bool, revision: u64) -> Planner {
        Planner {
            revision,
            coordinator_ips: Vec::new(),
            coordinator_dht,
            per_peer_ip: INSTALL_PER_PEER_IP,
            dict_version: 0,
            filter_version: 0,
            workers: BTreeMap::new(),
            coordinator_share: Share {
                dht: coordinator_dht.then_some(ShardSpec::FULL),
                politeness: Politeness::ALONE,
            },
            assignments: BTreeMap::new(),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The coordinator's own share under the current plan.
    pub fn coordinator(&self) -> Share {
        self.coordinator_share
    }

    /// A worker's current assignment.
    pub fn assignment(&self, worker: WorkerId) -> Option<&Assignment> {
        self.assignments.get(&worker)
    }

    pub fn workers(&self) -> impl Iterator<Item = WorkerId> + '_ {
        self.workers.keys().copied()
    }

    /// A worker joined (or reconnected) with the addresses it reported.
    /// The built-in worker is the coordinator itself and is ignored.
    pub fn join(&mut self, worker: WorkerId, ips: Vec<IpAddr>) -> Option<Replan> {
        if worker == WorkerId::LOCAL {
            return None;
        }
        self.workers.entry(worker).or_default().ips = ips;
        Some(self.replan())
    }

    /// A worker left (disconnected, drained, revoked).
    pub fn leave(&mut self, worker: WorkerId) -> Option<Replan> {
        self.workers.remove(&worker)?;
        self.assignments.remove(&worker);
        Some(self.replan())
    }

    /// A worker started or stopped draining. While it drains it runs no
    /// DHT but stays a member of its IP for politeness: it's still
    /// fetching the keys it holds.
    pub fn set_draining(&mut self, worker: WorkerId, draining: bool) -> Option<Replan> {
        let w = self.workers.get_mut(&worker)?;
        if w.draining == draining {
            return None;
        }
        w.draining = draining;
        Some(self.replan())
    }

    /// Pins a worker's DHT on (`Auto`) or off.
    pub fn pin(&mut self, worker: WorkerId, pin: DhtPin) -> Option<Replan> {
        self.workers.get_mut(&worker)?.pin = pin;
        Some(self.replan())
    }

    /// A worker's budget, intensity and pool cap (its pin is kept). A pool
    /// cap of 0 is taken as 1, one over the maximum as the maximum, and the
    /// CPU share as 1–1000 ‰.
    pub fn set_settings(&mut self, worker: WorkerId, mut s: WorkerSettings) -> Option<Replan> {
        s.pool_cap = s.pool_cap.clamp(1, crate::assign::MAX_POOL_CAP);
        s.budget.cpu_permille = s.budget.cpu_permille.clamp(1, 1000);
        self.workers.get_mut(&worker)?.settings = s;
        Some(self.replan())
    }

    /// The coordinator's own public addresses (its DHT's address vote).
    pub fn set_coordinator_ips(&mut self, ips: Vec<IpAddr>) -> Option<Replan> {
        if self.coordinator_ips == ips {
            return None;
        }
        self.coordinator_ips = ips;
        (!self.workers.is_empty()).then(|| self.replan())
    }

    /// New dictionary or filter-snapshot versions to announce.
    pub fn set_versions(&mut self, dict: u64, filter: u64) -> Option<Replan> {
        if (self.dict_version, self.filter_version) == (dict, filter) {
            return None;
        }
        (self.dict_version, self.filter_version) = (dict, filter);
        (!self.workers.is_empty()).then(|| self.replan())
    }

    fn replan(&mut self) -> Replan {
        self.revision += 1;
        let mut members = vec![Member {
            id: MemberId::Coordinator,
            ips: self.coordinator_ips.clone(),
            dht: self.coordinator_dht,
        }];
        members.extend(self.workers.iter().map(|(id, w)| Member {
            id: MemberId::Worker(*id),
            ips: w.ips.clone(),
            dht: w.pin == DhtPin::Auto && !w.draining,
        }));
        let mut out = Vec::with_capacity(self.workers.len());
        self.assignments.clear();
        for (id, share) in plan(&members, self.per_peer_ip) {
            match id {
                MemberId::Coordinator => self.coordinator_share = share,
                MemberId::Worker(w) => {
                    let s = self.workers[&w].settings;
                    let a = Assignment {
                        revision: self.revision,
                        dht: share.dht,
                        fetch: true,
                        prepare: true,
                        intensity: s.intensity,
                        budget: s.budget,
                        pool_cap: s.pool_cap,
                        dict_version: self.dict_version,
                        filter_version: self.filter_version,
                        politeness: share.politeness,
                    };
                    debug_assert!(a.validate().is_ok());
                    self.assignments.insert(w, a);
                    out.push((w, a));
                }
            }
        }
        Replan {
            revision: self.revision,
            coordinator: self.coordinator_share,
            workers: out,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, n))
    }

    #[test]
    fn alone_the_coordinator_keeps_everything() {
        let p = Planner::new(true);
        assert_eq!(p.coordinator().dht, Some(ShardSpec::FULL));
        assert_eq!(p.coordinator().politeness, Politeness::ALONE);
        let out = plan(
            &[Member {
                id: MemberId::Coordinator,
                ips: vec![ip(1)],
                dht: true,
            }],
            2,
        );
        assert_eq!(out[0].1.dht, Some(ShardSpec::FULL));
        assert_eq!(out[0].1.politeness, Politeness::ALONE);
    }

    #[test]
    fn same_ip_splits_slots_different_ips_keep_them() {
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![ip(1)]);
        let r = p.join(WorkerId(1), vec![ip(1)]).unwrap();
        assert_eq!(r.revision, 1);
        let c = r.coordinator.dht.unwrap();
        let w = r.workers[0].1.dht.unwrap();
        assert_eq!((c.prefixes(), w.prefixes()), (0..128, 128..256));
        assert_eq!(c.id_slots() & w.id_slots(), 0);
        assert_eq!(c.id_slots() | w.id_slots(), ALL_SLOTS);
        assert_eq!(r.workers[0].1.politeness.bep51_interval_scale, 2);
        assert_eq!(r.workers[0].1.politeness.per_peer_ip, 1);

        let r = p.join(WorkerId(2), vec![ip(2)]).unwrap();
        assert_eq!(r.revision, 2);
        let w2 = r.workers[1].1;
        assert_eq!(w2.dht.unwrap().id_slots(), ALL_SLOTS);
        assert_eq!(w2.politeness, Politeness::ALONE);
        assert_eq!(w2.dht.unwrap().prefixes(), 170..256);
    }

    #[test]
    fn several_addresses_join_the_coordinators_group() {
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![ip(1)]);
        p.join(WorkerId(1), vec![ip(3)]);
        let r = p.join(WorkerId(2), vec![ip(2), ip(4)]).unwrap();
        let c = r.coordinator.dht.unwrap();
        let w2 = r.workers[1].1;
        assert_eq!(c.id_slots() & w2.dht.unwrap().id_slots(), 0);
        assert_eq!(w2.politeness.bep51_interval_scale, 2);
        assert_eq!(r.workers[0].1.politeness, Politeness::ALONE);
    }

    #[test]
    fn an_unknown_address_plans_everyone_as_one_ip() {
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![ip(1)]);
        p.join(WorkerId(1), vec![ip(2)]);
        let r = p.join(WorkerId(2), vec![]).unwrap();
        for (_, a) in &r.workers {
            assert_eq!(a.politeness.bep51_interval_scale, 3);
            assert_ne!(a.dht.unwrap().id_slots(), ALL_SLOTS);
        }
    }

    #[test]
    fn pins_leave_and_settings_replan_with_new_revisions() {
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![ip(1)]);
        p.join(WorkerId(1), vec![ip(1)]);
        let r = p.pin(WorkerId(1), DhtPin::Off).unwrap();
        assert_eq!(r.revision, 2);
        assert_eq!(r.workers[0].1.dht, None);
        assert!(r.workers[0].1.fetch && r.workers[0].1.prepare);
        assert_eq!(r.coordinator.dht, Some(ShardSpec::FULL));
        // It still fetches from the same IP, so connections stay shared.
        assert_eq!(r.workers[0].1.politeness.per_peer_ip, 1);
        let r = p
            .set_settings(
                WorkerId(1),
                WorkerSettings {
                    pool_cap: 0,
                    ..WorkerSettings::default()
                },
            )
            .unwrap();
        assert_eq!(r.workers[0].1.pool_cap, 1);
        assert_eq!(r.workers[0].1.dht, None, "still pinned off");
        let r = p.leave(WorkerId(1)).unwrap();
        assert_eq!(r.revision, 4);
        assert!(r.workers.is_empty());
        assert_eq!(r.coordinator.dht, Some(ShardSpec::FULL));
        assert_eq!(r.coordinator.politeness, Politeness::ALONE);
        assert!(p.leave(WorkerId(1)).is_none());
        assert!(p.join(WorkerId::LOCAL, vec![ip(1)]).is_none());
    }

    #[test]
    fn a_draining_worker_keeps_its_share_of_connections() {
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![ip(1)]);
        p.join(WorkerId(1), vec![ip(1)]);
        let r = p.set_draining(WorkerId(1), true).unwrap();
        assert_eq!(r.workers[0].1.dht, None, "no DHT while draining");
        assert_eq!(r.coordinator.dht, Some(ShardSpec::FULL));
        assert_eq!(r.coordinator.politeness.per_peer_ip, 1, "still shared");
        assert_eq!(r.workers[0].1.politeness.per_peer_ip, 1);
        assert!(p.set_draining(WorkerId(1), true).is_none());
    }

    #[test]
    fn non_public_addresses_count_as_unknown() {
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        for a in [
            lan,
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            "fd00::1".parse().unwrap(),
            "fe80::1".parse().unwrap(),
            "::ffff:10.0.0.1".parse().unwrap(),
        ] {
            assert!(!is_public(a), "{a}");
        }
        assert!(is_public(ip(1)) && is_public("2001:db8::1".parse().unwrap()));
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![ip(1)]);
        let r = p.join(WorkerId(1), vec![lan]).unwrap();
        assert_eq!(
            r.workers[0].1.politeness.per_peer_ip, 1,
            "planned as shared"
        );
    }

    #[test]
    fn revisions_continue_from_the_seed() {
        let mut p = Planner::starting_at(true, 1_000);
        assert_eq!(p.join(WorkerId(1), vec![ip(2)]).unwrap().revision, 1_001);
    }

    #[test]
    fn versions_reach_every_assignment() {
        let mut p = Planner::new(false);
        assert!(p.set_versions(1, 1).is_none(), "nothing to tell");
        p.join(WorkerId(5), vec![ip(5)]);
        let r = p.set_versions(4, 6).unwrap();
        assert_eq!(
            (r.workers[0].1.dict_version, r.workers[0].1.filter_version),
            (4, 6)
        );
        // A coordinator without a DHT leaves the keyspace to the worker.
        assert_eq!(r.coordinator.dht, None);
        assert_eq!(r.workers[0].1.dht, Some(ShardSpec::FULL));
    }
}
