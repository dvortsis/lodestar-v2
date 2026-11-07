//! The coordinator's placement (architecture §19.5; workers plan WK-5,
//! Q9): `lodestar_cluster::placement`'s planner, fed with the fleet the
//! cluster server sees, and its assignments sent to the workers.
//!
//! - **Membership.** A worker is a member while it's connected
//!   ([`Placement::sync`] diffs a `FleetSnapshot` against the planner). A
//!   draining one runs no DHT but keeps its politeness share, since it's
//!   still fetching. Joining, leaving, draining, a new address, a pin or a
//!   setting re-plans with a new revision, numbered from the clock.
//! - **Sending.** Each sync queues the current `Assign` to every member
//!   (56 bytes; a worker applies only a newer revision). That also covers a
//!   worker that reconnected between two syncs and was greeted with the
//!   server's empty `Assign`. A full queue is retried on the next sync.
//! - **Plugins.** [`runs_on_worker`] refuses every source plugin, whatever
//!   its `Distribution` (Q9). Only the built-in DHT, fetch and prepare run
//!   on workers, and an `Assignment` has no field to place anything else.
//! - **Single machine (§19.8).** With no worker joined nothing is planned
//!   or sent, and the coordinator's share is the full keyspace with one
//!   install's limits. Nothing here opens the cluster PKI.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};

use lodestar_cluster::assign::{Assignment, Intensity, ShardSpec};
use lodestar_cluster::placement::{DhtPin, Planner, Replan, Share, WorkerSettings};
use lodestar_cluster::proto::ToWorker;
use lodestar_cluster::{ClusterServer, FleetSnapshot, SendError, WorkerId, WorkerStatus};
use lodestar_dht::node::shard::Shard;
use lodestar_sources::Distribution;

/// Whether a source plugin may run on a remote worker. Never, in v1 (plan
/// Q9): `ShardedKeyspace` is the DHT's, and the DHT is built in, not a
/// plugin; `sharded(unit)`, singleton and account placement wait for v0.8
/// (build-plan P6). A new variant has to be decided here.
// The sources' placement (PL-3) asks it; its tests do today.
#[cfg_attr(not(test), allow(dead_code))]
pub fn runs_on_worker(d: Distribution) -> bool {
    match d {
        Distribution::Everywhere
        | Distribution::ShardedKeyspace
        | Distribution::ShardedUnit
        | Distribution::Singleton
        | Distribution::Coordinator => false,
    }
}

/// The DHT config's shard for an assigned one: `None` for the full
/// keyspace, which the DHT treats as no shard (WK-4).
#[cfg_attr(not(test), allow(dead_code))]
pub fn dht_shard(spec: ShardSpec) -> Option<Shard> {
    if spec.is_full() {
        return None;
    }
    // `ShardSpec` checks what `Shard::new` checks, so this never fails.
    Shard::new(spec.prefixes(), spec.id_slots()).ok()
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn dht_intensity(i: Intensity) -> lodestar_dht::node::Intensity {
    match i {
        Intensity::Low => lodestar_dht::node::Intensity::Low,
        Intensity::Balanced => lodestar_dht::node::Intensity::Balanced,
        Intensity::Aggressive => lodestar_dht::node::Intensity::Aggressive,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX / 2))
}

#[derive(Debug)]
struct State {
    planner: Planner,
    /// Each member's last reported addresses.
    addrs: BTreeMap<WorkerId, Vec<IpAddr>>,
}

/// The coordinator's planner and what it last sent.
#[derive(Debug)]
pub struct Placement(Mutex<State>);

/// What one [`Placement::sync`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// The new plan, if membership changed.
    pub replan: Option<Replan>,
    /// Members whose queue was full; tried again on the next sync.
    pub full: Vec<WorkerId>,
}

// `serve` wires these in when the worker runtime goes live (WK-10) and
// Settings → Workers sets pins and budgets (WK-14); tests do today.
#[cfg_attr(not(test), allow(dead_code))]
impl Placement {
    /// A coordinator that runs its own DHT when `coordinator_dht` is set.
    pub fn new(coordinator_dht: bool) -> Placement {
        Placement(Mutex::new(State {
            // Plans are numbered from the clock, so after a restart they're
            // still newer than any a worker applied before it.
            planner: Planner::starting_at(coordinator_dht, now_ms()),
            addrs: BTreeMap::new(),
        }))
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn revision(&self) -> u64 {
        self.state().planner.revision()
    }

    /// The coordinator's own share: the shard its DHT runs and its part of
    /// its IP's per-remote limits.
    pub fn coordinator(&self) -> Share {
        self.state().planner.coordinator()
    }

    pub fn assignment(&self, worker: WorkerId) -> Option<Assignment> {
        self.state().planner.assignment(worker).copied()
    }

    /// The coordinator's public addresses (its DHT's address vote).
    pub fn set_coordinator_ips(&self, ips: Vec<IpAddr>) -> Option<Replan> {
        self.state().planner.set_coordinator_ips(ips)
    }

    pub fn pin(&self, worker: WorkerId, pin: DhtPin) -> Option<Replan> {
        self.state().planner.pin(worker, pin)
    }

    pub fn set_settings(&self, worker: WorkerId, s: WorkerSettings) -> Option<Replan> {
        self.state().planner.set_settings(worker, s)
    }

    pub fn set_versions(&self, dict: u64, filter: u64) -> Option<Replan> {
        self.state().planner.set_versions(dict, filter)
    }

    /// Brings the membership in line with `fleet` (with each worker's
    /// reported public addresses), then queues every member's current
    /// assignment on `server`.
    // The serve tick calls it once the worker runtime goes live (WK-10).
    #[allow(dead_code)]
    pub fn sync(&self, fleet: &FleetSnapshot, server: &ClusterServer) -> Synced {
        self.sync_with(&fleet.workers, |w, m| server.send(w, m))
    }

    /// [`sync`](Self::sync) with the send step given.
    fn sync_with(
        &self,
        workers: &[WorkerStatus],
        mut send: impl FnMut(WorkerId, ToWorker) -> Result<(), SendError>,
    ) -> Synced {
        let replan = self.update(workers);
        let assigned: Vec<(WorkerId, Assignment)> = {
            let st = self.state();
            st.addrs
                .keys()
                .filter_map(|w| Some((*w, *st.planner.assignment(*w)?)))
                .collect()
        };
        let mut full = Vec::new();
        for (w, a) in assigned {
            let Some(msg) = a.to_message() else { continue };
            match send(w, ToWorker::Assign(msg)) {
                Ok(()) | Err(SendError::NotConnected) => {}
                Err(SendError::Full) => full.push(w),
                // An assignment always validates; nothing to retry.
                Err(SendError::Invalid) => debug_assert!(false, "assignment refused"),
            }
        }
        Synced { replan, full }
    }

    /// The membership half of [`sync`](Self::sync): the last plan made,
    /// if anything changed.
    fn update(&self, workers: &[WorkerStatus]) -> Option<Replan> {
        let mut st = self.state();
        // A draining worker stays a member: it's still fetching the keys it
        // holds, so its IP's per-peer connections stay divided (§19.6).
        let now: BTreeMap<WorkerId, (Vec<IpAddr>, bool)> = workers
            .iter()
            .filter(|w| w.connected && w.worker != WorkerId::LOCAL)
            .map(|w| (w.worker, (w.external_ips.clone(), w.draining)))
            .collect();
        let mut last = None;
        let gone: Vec<WorkerId> = st
            .addrs
            .keys()
            .filter(|w| !now.contains_key(w))
            .copied()
            .collect();
        for w in gone {
            st.addrs.remove(&w);
            last = st.planner.leave(w).or(last);
        }
        for (w, (ips, draining)) in now {
            if st.addrs.get(&w) != Some(&ips) {
                st.addrs.insert(w, ips.clone());
                last = st.planner.join(w, ips).or(last);
            }
            last = st.planner.set_draining(w, draining).or(last);
        }
        last
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use lodestar_cluster::assign::{ALL_SLOTS, Politeness};
    use lodestar_cluster::proto::Stats;

    use super::*;

    fn status(id: u32, connected: bool, draining: bool, ips: Vec<IpAddr>) -> WorkerStatus {
        WorkerStatus {
            worker: WorkerId(id),
            connected,
            draining,
            offset_ms: 0,
            skewed: false,
            leases: 0,
            last_seen_ms: 0,
            stats: Stats::default(),
            external_ips: ips,
        }
    }

    const HOME: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));
    const AWAY: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2));
    const HOME_V6: IpAddr = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));

    #[test]
    fn no_source_plugin_runs_on_a_worker() {
        // The structural guarantee is that `Assignment` has no field for a
        // plugin, so nothing can place one. This checks the rule PL-3 will
        // ask, and `runs_on_worker`'s exhaustive match makes a new
        // `Distribution` variant a compile error until it's decided.
        for d in [
            Distribution::Everywhere,
            Distribution::ShardedKeyspace,
            Distribution::ShardedUnit,
            Distribution::Singleton,
            Distribution::Coordinator,
        ] {
            assert!(!runs_on_worker(d), "{d:?}");
        }
    }

    #[test]
    fn shards_convert_and_the_full_one_is_no_shard() {
        assert_eq!(dht_shard(ShardSpec::FULL), None);
        let s = ShardSpec::new(0..128, 0x0f).unwrap();
        let d = dht_shard(s).unwrap();
        assert_eq!((d.prefixes(), d.id_slots()), (0..128, 0x0f));
        assert_eq!(
            dht_intensity(Intensity::Aggressive),
            lodestar_dht::node::Intensity::Aggressive
        );
    }

    #[test]
    fn membership_follows_the_fleet() {
        let p = Placement::new(true);
        p.set_coordinator_ips(vec![HOME]);
        // Single machine: nothing planned.
        let base = p.revision();
        assert!(base > 1_000_000_000_000, "seeded from the clock");
        assert_eq!(p.update(&[status(0, true, false, vec![HOME])]), None);
        assert_eq!(p.revision(), base);
        assert_eq!(p.coordinator().dht, Some(ShardSpec::FULL));
        assert_eq!(p.coordinator().politeness, Politeness::ALONE);

        let both = |d1, c2| {
            vec![
                status(1, true, d1, vec![HOME]),
                status(2, c2, false, vec![AWAY]),
            ]
        };
        let r = p.update(&both(false, true)).unwrap();
        assert_eq!(r.revision, base + 2, "two joins, two revisions");
        let c = p.coordinator().dht.unwrap();
        let w1 = p.assignment(WorkerId(1)).unwrap().dht.unwrap();
        assert_eq!(c.id_slots() & w1.id_slots(), 0, "same IP, disjoint slots");
        let w2 = p.assignment(WorkerId(2)).unwrap();
        assert_eq!(w2.dht.unwrap().id_slots(), ALL_SLOTS);
        assert_eq!(w2.politeness, Politeness::ALONE);

        // Nothing changed: no new plan.
        assert_eq!(p.update(&both(false, true)), None);
        // Worker 1 drains, worker 2 disconnects: the coordinator runs the
        // whole DHT again, but worker 1 is still fetching from its IP, so
        // per-peer connections stay divided.
        let r = p.update(&both(true, false)).unwrap();
        assert_eq!(r.workers.len(), 1);
        assert_eq!(r.workers[0].1.dht, None);
        assert_eq!(p.coordinator().dht, Some(ShardSpec::FULL));
        assert_eq!(p.coordinator().politeness.per_peer_ip, 1);
        // It finished and disconnected: the coordinator is alone.
        let r = p.update(&[]).unwrap();
        assert!(r.workers.is_empty());
        assert_eq!(p.coordinator().politeness, Politeness::ALONE);
        assert_eq!(p.assignment(WorkerId(1)), None);
    }

    #[test]
    fn a_new_address_replans() {
        let p = Placement::new(true);
        p.set_coordinator_ips(vec![HOME]);
        p.update(&[status(3, true, false, vec![])]).unwrap();
        // Unknown address: planned as sharing the coordinator's IP.
        assert_eq!(p.assignment(WorkerId(3)).unwrap().politeness.per_peer_ip, 1);
        let r = p.update(&[status(3, true, false, vec![AWAY])]).unwrap();
        assert_eq!(r.workers[0].1.politeness, Politeness::ALONE);
        // Several addresses: planned with the coordinator again.
        let r = p
            .update(&[status(3, true, false, vec![AWAY, HOME_V6])])
            .unwrap();
        assert_eq!(r.workers[0].1.politeness.bep51_interval_scale, 2);
    }

    #[test]
    fn sync_sends_every_member_its_assignment_and_reports_full_queues() {
        let p = Placement::new(true);
        p.set_coordinator_ips(vec![HOME]);
        let ws = vec![
            status(1, true, false, vec![AWAY]),
            status(2, true, false, vec![AWAY]),
        ];
        let mut sent = Vec::new();
        let s = p.sync_with(&ws, |w, m| {
            sent.push((w, m));
            if w.0 == 2 {
                Err(SendError::Full)
            } else {
                Ok(())
            }
        });
        let rev = p.revision();
        assert_eq!(s.replan.as_ref().map(|r| r.revision), Some(rev));
        assert_eq!(s.full, vec![WorkerId(2)]);
        for (w, m) in &sent {
            let ToWorker::Assign(m) = m else {
                panic!("{m:?}")
            };
            let a = Assignment::from_message(m).unwrap().unwrap();
            assert_eq!(a.revision, rev);
            assert_eq!(Some(a), p.assignment(*w));
            assert!(a.fetch && a.prepare);
        }
        assert_eq!(sent.len(), 2);
        // Nothing changed: the same revision is sent again (a reconnected
        // worker was greeted with an empty one), with no new plan.
        let mut again = 0;
        let s = p.sync_with(&ws, |_, _| {
            again += 1;
            Ok(())
        });
        assert_eq!((s.replan, s.full, again), (None, vec![], 2));
    }

    #[test]
    fn pins_settings_and_versions_reach_the_assignment() {
        let p = Placement::new(true);
        p.set_coordinator_ips(vec![HOME]);
        p.update(&[status(1, true, false, vec![HOME])]);
        let r = p.pin(WorkerId(1), DhtPin::Off).unwrap();
        assert_eq!(r.workers[0].1.dht, None);
        assert_eq!(p.coordinator().dht, Some(ShardSpec::FULL));
        let s = WorkerSettings {
            intensity: Intensity::Low,
            pool_cap: 10_000,
            ..WorkerSettings::default()
        };
        let r = p.set_settings(WorkerId(1), s).unwrap();
        let a = r.workers[0].1;
        assert_eq!(
            (a.intensity, a.pool_cap, a.dht),
            (Intensity::Low, 10_000, None)
        );
        let r = p.set_versions(2, 5).unwrap();
        assert_eq!(
            (r.workers[0].1.dict_version, r.workers[0].1.filter_version),
            (2, 5)
        );
        assert!(p.pin(WorkerId(9), DhtPin::Off).is_none(), "not a member");
    }
}
