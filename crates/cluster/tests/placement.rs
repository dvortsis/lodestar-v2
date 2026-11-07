//! WK-5's placement gates (workers plan §4.3): over arbitrary fleets, the
//! DHT shards never overlap and cover the keyspace, members sharing an IP
//! never share a BEP 42 slot, and per remote address the members' limits
//! sum to no more than one install's.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use lodestar_cluster::WorkerId;
use lodestar_cluster::assign::{ALL_SLOTS, Assignment, INSTALL_PER_PEER_IP};
use lodestar_cluster::placement::{
    DhtPin, MAX_DHT_PER_IP, Member, MemberId, Planner, Share, WorkerSettings, plan,
};
use proptest::prelude::*;

/// Addresses from a small pool, so members often share one; index 6 is an
/// IPv4-mapped form of index 0, and index 7 a LAN address, which says
/// nothing about where a remote peer sees the member from.
fn addr(i: u8) -> IpAddr {
    match i {
        6 => IpAddr::V6(Ipv4Addr::new(192, 0, 2, 0).to_ipv6_mapped()),
        7 => IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)),
        n => IpAddr::V4(Ipv4Addr::new(192, 0, 2, n)),
    }
}

/// The members as a remote peer could tell them apart: LAN addresses
/// dropped, so a member left with none is one whose address is unknown.
fn seen_as(members: &[Member]) -> Vec<Member> {
    let lan = addr(7);
    members
        .iter()
        .map(|m| Member {
            id: m.id,
            ips: m.ips.iter().copied().filter(|i| *i != lan).collect(),
            dht: m.dht,
        })
        .collect()
}

fn fleet() -> impl Strategy<Value = (Vec<Member>, u8)> {
    let member = (
        proptest::collection::vec(0u8..8, 0..3),
        any::<bool>(),
        // Mostly known addresses: an unknown one plans everything as one IP.
        prop::bool::weighted(0.9),
    );
    (
        proptest::collection::vec(member.clone(), 0..40),
        member,
        1u8..=INSTALL_PER_PEER_IP,
    )
        .prop_map(|(workers, coord, per_ip)| {
            let to_member = |id, (ips, dht, known): (Vec<u8>, bool, bool)| Member {
                id,
                ips: if known && !ips.is_empty() {
                    ips.into_iter().map(addr).collect()
                } else {
                    vec![]
                },
                dht,
            };
            let mut ms = vec![to_member(MemberId::Coordinator, coord)];
            ms.extend(
                workers
                    .into_iter()
                    .enumerate()
                    .map(|(i, w)| to_member(MemberId::Worker(WorkerId(i as u32 + 1)), w)),
            );
            (ms, per_ip)
        })
}

/// Whether two members may be behind one public address: they reported
/// a common one, either address is unknown, or one reported several and
/// the other is the coordinator.
fn may_share(a: &Member, b: &Member) -> bool {
    let several_and_coordinator =
        |x: &Member, y: &Member| x.ips.len() > 1 && y.id == MemberId::Coordinator;
    a.ips.is_empty()
        || b.ips.is_empty()
        || several_and_coordinator(a, b)
        || several_and_coordinator(b, a)
        || a.ips
            .iter()
            .any(|x| b.ips.iter().any(|y| x.to_canonical() == y.to_canonical()))
}

fn peers() -> Vec<IpAddr> {
    (0..64u8)
        .map(|n| IpAddr::V4(Ipv4Addr::new(203, 0, 113, n)))
        .chain((0..16u16).map(|n| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, n))))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn shards_never_overlap_and_cover_the_keyspace((members, per_ip) in fleet()) {
        let out = plan(&members, per_ip);
        prop_assert_eq!(out.len(), members.len());
        let shards: Vec<_> = out.iter().filter_map(|(_, s)| s.dht).collect();
        let mut seen = [false; 256];
        for s in &shards {
            for p in s.prefixes() {
                prop_assert!(!seen[usize::from(p)], "prefix {} twice", p);
                seen[usize::from(p)] = true;
            }
        }
        if members.iter().any(|m| m.dht) {
            prop_assert!(seen.iter().all(|x| *x), "every prefix is walked");
        } else {
            prop_assert!(shards.is_empty());
        }
        // Nobody pinned off runs a DHT.
        for (id, s) in &out {
            let m = members.iter().find(|m| m.id == *id).unwrap();
            prop_assert!(m.dht || s.dht.is_none());
        }
    }

    #[test]
    fn members_that_may_share_an_ip_never_share_a_slot((members, per_ip) in fleet()) {
        let out = plan(&members, per_ip);
        let members = seen_as(&members);
        let share = |id: MemberId| -> Share { out.iter().find(|(i, _)| *i == id).unwrap().1 };
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                if let (Some(x), Some(y)) = (share(a.id).dht, share(b.id).dht)
                    && may_share(a, b)
                {
                    prop_assert_eq!(x.id_slots() & y.id_slots(), 0, "{:?} {:?}", a.id, b.id);
                }
            }
            // A member alone on its addresses keeps all 8 slots.
            if let Some(s) = share(a.id).dht
                && members.iter().all(|b| b.id == a.id || !may_share(a, b))
            {
                prop_assert_eq!(s.id_slots(), ALL_SLOTS);
            }
        }
    }

    #[test]
    fn per_remote_limits_never_exceed_one_install((members, per_ip) in fleet()) {
        let out = plan(&members, per_ip);
        let members = seen_as(&members);
        let share = |id: MemberId| -> Share { out.iter().find(|(i, _)| *i == id).unwrap().1 };
        // Every address any member reported, and "unknown".
        let mut addrs: BTreeSet<Option<IpAddr>> =
            members.iter().flat_map(|m| m.ips.iter().map(|i| Some(i.to_canonical()))).collect();
        addrs.insert(None);
        let peers = peers();
        let coord_ips: Vec<IpAddr> =
            members[0].ips.iter().map(|i| i.to_canonical()).collect();
        for a in addrs {
            // Everyone who might appear to a remote peer as `a`.
            let behind: Vec<&Member> = members
                .iter()
                .filter(|m| {
                    m.ips.is_empty()
                        || a.is_none_or(|a| m.ips.iter().any(|i| i.to_canonical() == a))
                        || (m.ips.len() > 1 && a.is_some_and(|a| coord_ips.contains(&a)))
                })
                .collect();
            if a.is_none() && !members.iter().any(|m| m.ips.is_empty()) {
                continue;
            }
            for p in &peers {
                let conns: u32 = behind
                    .iter()
                    .map(|m| u32::from(share(m.id).politeness.connections_to(*p)))
                    .sum();
                prop_assert!(conns <= u32::from(per_ip), "{} connections to {} from {:?}", conns, p, a);
            }
            // BEP 51: the members' sampling rates of one remote node sum
            // to at most one install's (1 / interval).
            let rate: f64 = behind
                .iter()
                .filter(|m| share(m.id).dht.is_some())
                .map(|m| 1.0 / f64::from(share(m.id).politeness.bep51_interval_scale))
                .sum();
            prop_assert!(rate <= 1.0 + 1e-9, "BEP 51 rate {} from {:?}", rate, a);
            let dht = behind.iter().filter(|m| share(m.id).dht.is_some()).count();
            prop_assert!(dht <= MAX_DHT_PER_IP);
        }
        // Every member may dial the peers in its own bucket. (With 3 or
        // more on one IP a member can't dial peers outside it: the cost
        // of the split, recorded in the WK-5 decisions.)
        for (_, s) in &out {
            prop_assert!(s.politeness.per_peer_ip >= 1);
        }
    }

    #[test]
    fn replans_carry_new_revisions_and_valid_assignments(
        ops in proptest::collection::vec((0u8..9, 1u32..12, proptest::collection::vec(0u8..7, 0..2)), 1..60),
    ) {
        let mut p = Planner::new(true);
        p.set_coordinator_ips(vec![addr(0)]);
        let mut last = p.revision();
        for (op, w, ips) in ops {
            let w = WorkerId(w);
            let r = match op {
                0 => p.join(w, ips.into_iter().map(addr).collect()),
                1 => p.leave(w),
                2 => p.pin(w, DhtPin::Off),
                3 => p.pin(w, DhtPin::Auto),
                4 => p.set_draining(w, w.0.is_multiple_of(2)),
                5 => p.set_settings(w, WorkerSettings { pool_cap: w.0 * 1000, ..WorkerSettings::default() }),
                6 => p.set_versions(u64::from(w.0), u64::from(w.0) + 1),
                7 => p.set_coordinator_ips(ips.into_iter().map(addr).collect()),
                _ => p.leave(w),
            };
            if let Some(r) = r {
                prop_assert!(r.revision > last);
                last = r.revision;
                prop_assert_eq!(r.workers.len(), p.workers().count());
                for (id, a) in &r.workers {
                    prop_assert_eq!(a.revision, r.revision);
                    prop_assert!(a.fetch && a.prepare, "fetch and prepare run everywhere");
                    let m = a.to_message().unwrap();
                    prop_assert_eq!(Assignment::from_message(&m).unwrap(), Some(*a));
                    prop_assert_eq!(p.assignment(*id), Some(a));
                }
            }
        }
        // Everyone gone: the coordinator is a single install again.
        for w in p.workers().collect::<Vec<_>>() {
            p.leave(w);
        }
        prop_assert!(p.coordinator().dht.is_some_and(|s| s.is_full()));
        prop_assert_eq!(p.coordinator().politeness, lodestar_cluster::assign::Politeness::ALONE);
    }
}

#[test]
fn hello_caps_the_reported_addresses() {
    use lodestar_cluster::proto::{Capabilities, Hello, MAX_EXTERNAL_IPS, ToCoordinator};
    let hello = |n: usize| {
        ToCoordinator::Hello(Hello::new(
            1,
            Capabilities {
                external_ips: (0..n).map(|i| addr(i as u8)).collect(),
                ..Capabilities::default()
            },
        ))
    };
    assert!(hello(MAX_EXTERNAL_IPS).validate());
    assert!(!hello(MAX_EXTERNAL_IPS + 1).validate());
}
