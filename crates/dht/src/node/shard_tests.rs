//! WK-4's keyspace shard against the real actor: the full shard is no
//! shard, a sub-range shard's targets stay in its range and its ids in its
//! slots (at start, from saved state and on the re-id after an address
//! change), and disjoint shards never target the same prefix in a pass or
//! mint the same BEP 42 prefix.

use super::tests::{UNBOUND, unbound};
use super::*;
use crate::node::shard::ALL_SLOTS;
use crate::state::State;
use proptest::prelude::*;
use rand::SeedableRng;

const DEADLINE: Duration = Duration::from_secs(10);

fn config(
    shard: Option<Shard>,
    target_prefixes: bool,
    virtual_ids: usize,
    bind: SocketAddr,
) -> Config {
    Config {
        bind: vec![bind],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        target_prefixes,
        virtual_ids,
        shard,
        ..Default::default()
    }
}

/// An actor that never sends (its address is unbound): for ids and
/// cursors.
fn idle(
    shard: Option<Shard>,
    target_prefixes: bool,
    virtual_ids: usize,
    saved: Option<&State>,
    seed: u64,
) -> Actor {
    let addr = unbound(false, UNBOUND);
    Actor::new(
        config(shard, target_prefixes, virtual_ids, addr),
        Arc::new(Stats::default()),
        vec![addr],
        saved,
        vec![],
        None,
        Instant::now(),
        StdRng::seed_from_u64(seed),
    )
}

/// An actor with a real socket, so its samples go out.
fn sending(shard: Option<Shard>, target_prefixes: bool, seed: u64) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let actor = Actor::new(
        config(shard, target_prefixes, 4, addr),
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![],
        None,
        Instant::now(),
        StdRng::seed_from_u64(seed),
    );
    (actor, vec![SendSocket::Udp(socket)])
}

fn peer(a: &mut Actor) -> std::net::UdpSocket {
    let p = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    p.set_read_timeout(Some(DEADLINE)).unwrap();
    let node = NodeInfo {
        id: NodeId([42; 20]),
        addr: p.local_addr().unwrap(),
    };
    assert!(a.sampler.offer(node, Instant::now()));
    p
}

fn sample_target(p: &std::net::UdpSocket, from: SocketAddr) -> [u8; 20] {
    let mut buf = [0u8; 2048];
    loop {
        let (len, src) = p.recv_from(&mut buf).expect("a sample within the deadline");
        if let Some(target) = super::tests::sample_from(&buf[..len], src, from) {
            return target;
        }
    }
}

/// The 21 bits BEP 42 derives from the address.
fn bep42_prefix(id: &NodeId) -> u32 {
    u32::from_be_bytes([id.0[0], id.0[1], id.0[2] & 0xf8, 0])
}

const PUBLIC: &str = "93.184.216.34";

fn public() -> IpAddr {
    PUBLIC.parse().unwrap()
}

/// Five votes from five networks: enough for a quorum on `ip`.
fn vote_for(a: &mut Actor, ip: IpAddr) {
    for n in 1..=5u8 {
        let reporter = IpAddr::from([n, n, n, n]);
        a.on_external_ip(reporter, SocketAddr::new(ip, 6881));
    }
    assert_eq!(a.external.current, Some(ip), "the vote reached a quorum");
}

#[test]
fn the_full_shard_is_no_shard() {
    let saved_public = State {
        ids: secure_id::virtual_ids(&public(), 4, &mut StdRng::seed_from_u64(1)),
        external_ip: Some(public()),
        nodes: vec![],
    };
    let saved_stale = State {
        ids: vec![NodeId([7; 20]); 3],
        external_ip: Some(public()),
        nodes: vec![],
    };
    for saved in [None, Some(&saved_public), Some(&saved_stale)] {
        for prefixes in [false, true] {
            // Above 8 ids too: the full shard doesn't cap.
            for want in [1, 4, 16] {
                let mut a = idle(None, prefixes, want, saved, 11);
                let mut b = idle(Some(Shard::FULL), prefixes, want, saved, 11);
                assert!(b.shard.is_none(), "the full shard is held as none");
                assert_eq!(a.ids, b.ids);
                assert_eq!(a.ids.len(), want);
                assert_eq!(a.prefixes.is_some(), b.prefixes.is_some());
                assert_eq!(a.prefixes.is_some(), prefixes, "the switch alone decides");
                // The re-id after an address change, and the next draws.
                let moved: IpAddr = "203.0.113.9".parse().unwrap();
                vote_for(&mut a, moved);
                vote_for(&mut b, moved);
                assert_eq!(a.ids, b.ids);
                assert_eq!(a.rng.random::<u64>(), b.rng.random::<u64>());
                for kind in TargetKind::ALL {
                    assert_eq!(a.routing_target(0, kind), b.routing_target(0, kind));
                }
            }
        }
    }
}

#[test]
fn the_full_shard_sends_the_same_samples_as_no_shard() {
    for prefixes in [false, true] {
        let (mut a, sa) = sending(None, prefixes, 3);
        let (mut b, sb) = sending(Some(Shard::FULL), prefixes, 3);
        let pa: Vec<_> = (0..3).map(|_| peer(&mut a)).collect();
        let pb: Vec<_> = (0..3).map(|_| peer(&mut b)).collect();
        a.sample_tick(&sa, Instant::now());
        b.sample_tick(&sb, Instant::now());
        for (x, y) in pa.iter().zip(&pb) {
            assert_eq!(
                sample_target(x, a.cfg.bind[0]),
                sample_target(y, b.cfg.bind[0])
            );
        }
        assert_eq!(
            a.stats.snapshot().target_prefix_queries,
            b.stats.snapshot().target_prefix_queries
        );
    }
}

#[test]
fn a_sub_range_shard_samples_only_its_prefixes_with_the_switch_off() {
    let shard = Shard::new(64..96, 0b11).unwrap();
    let (mut a, sockets) = sending(Some(shard), false, 5);
    assert!(a.prefixes.is_some(), "the shard brings its own cursor");
    // Two passes' worth of samples, a few per tick.
    let mut seen = Vec::new();
    let mut peers = Vec::new();
    while seen.len() < 64 {
        // Every peer stays bound to the end, so no later one reuses a port
        // the sampler already holds.
        let start = peers.len();
        peers.extend((0..4).map(|_| peer(&mut a)));
        a.sample_tick(&sockets, Instant::now());
        for p in &peers[start..] {
            seen.push(sample_target(p, a.cfg.bind[0])[0]);
        }
    }
    assert!(seen.iter().all(|&p| shard.has_prefix(p)), "{seen:?}");
    let mut first: Vec<u8> = seen[..32].to_vec();
    first.sort_unstable();
    assert_eq!(
        first,
        (64..96).collect::<Vec<u8>>(),
        "each prefix once per pass"
    );
    let st = a.stats.snapshot();
    assert_eq!(st.target_prefix_queries[TargetKind::Sample as usize], 64);
    assert_eq!(st.target_prefix_cycles, 2);
}

#[test]
fn a_sub_range_shard_caps_its_ids_at_its_slots() {
    let shard = Shard::new(0..128, 0b1010_0000).unwrap();
    for want in [1, 2, 4, 16] {
        let a = idle(Some(shard), false, want, None, 2);
        assert_eq!(a.ids.len(), want.min(2), "{want}");
    }
    // Every slot but half the prefixes is still a sub-range shard.
    let wide = Shard::new(0..128, ALL_SLOTS).unwrap();
    assert_eq!(idle(Some(wide), false, 16, None, 2).ids.len(), 8);
}

#[test]
fn saved_ids_outside_the_shard_are_regenerated() {
    let ip = public();
    let shard = Shard::new(128..256, 0b0000_1100).unwrap();
    // Saved ids in slots 0..4: two of them outside the shard.
    let saved = State {
        ids: secure_id::virtual_ids(&ip, 4, &mut StdRng::seed_from_u64(1)),
        external_ip: Some(ip),
        nodes: vec![],
    };
    let a = idle(Some(shard), false, 4, Some(&saved), 3);
    assert_eq!(a.ids.len(), 2);
    let slots: Vec<u8> = a.ids.iter().map(secure_id::slot).collect();
    assert_eq!(slots, [2, 3], "regenerated in the shard's slots");
    assert!(a.ids.iter().all(|id| secure_id::is_valid(id, &ip)));
    assert!(
        a.ids.iter().all(|id| !saved.ids.contains(id)),
        "all regenerated"
    );

    // Saved ids already in the shard and valid are kept.
    let kept = State {
        ids: secure_id::virtual_ids_in(&ip, 0b0000_1100, 2, &mut StdRng::seed_from_u64(4)),
        ..saved.clone()
    };
    let b = idle(Some(shard), false, 4, Some(&kept), 3);
    assert_eq!(b.ids, kept.ids);

    // Valid and in the shard, but too few for the cap: regenerated.
    let short = State {
        ids: kept.ids[..1].to_vec(),
        ..saved.clone()
    };
    let c = idle(Some(shard), false, 4, Some(&short), 3);
    assert_eq!(c.ids.len(), 2);
    assert!(c.ids.iter().all(|id| shard.has_slot(secure_id::slot(id))));

    // In the shard's slots but for another address: regenerated.
    let other = State {
        ids: kept.ids.clone(),
        external_ip: Some("203.0.113.9".parse().unwrap()),
        nodes: vec![],
    };
    let d = idle(Some(shard), false, 4, Some(&other), 3);
    assert!(
        d.ids
            .iter()
            .all(|id| secure_id::is_valid(id, &"203.0.113.9".parse().unwrap()))
    );
    assert_ne!(d.ids, kept.ids);
}

#[test]
fn without_a_public_address_saved_ids_wait_for_the_vote() {
    // No address saved (or a private one): ids aren't BEP 42 yet, so the
    // saved ones are kept, capped, and the vote re-ids in the shard.
    let shard = Shard::new(0..64, 0b0100_0000).unwrap();
    for external_ip in [None, Some("192.168.1.2".parse().unwrap())] {
        let saved = State {
            ids: vec![NodeId([9; 20]), NodeId([8; 20])],
            external_ip,
            nodes: vec![],
        };
        let mut a = idle(Some(shard), false, 4, Some(&saved), 6);
        assert_eq!(a.ids, [NodeId([9; 20])]);
        vote_for(&mut a, public());
        assert_eq!(a.ids.len(), 1);
        assert_eq!(secure_id::slot(&a.ids[0]), 6);
        assert!(secure_id::is_valid(&a.ids[0], &public()));
    }
}

#[test]
fn the_re_id_on_an_address_change_honours_the_shard() {
    let shard = Shard::new(0..16, 0b0001_0010).unwrap();
    let mut a = idle(Some(shard), false, 4, None, 7);
    vote_for(&mut a, public());
    let slots: Vec<u8> = a.ids.iter().map(secure_id::slot).collect();
    assert_eq!(slots, [1, 4]);
    assert!(a.ids.iter().all(|id| secure_id::is_valid(id, &public())));
    for f in &a.families {
        assert_eq!(
            f.table.own_id(),
            a.ids[0],
            "the tables follow the new primary id"
        );
    }
    // Ids valid for the new address but outside the shard are replaced.
    let moved: IpAddr = "203.0.113.9".parse().unwrap();
    a.ids = secure_id::virtual_ids_in(&moved, 0b0000_0001, 2, &mut StdRng::seed_from_u64(8));
    vote_for(&mut a, moved);
    let slots: Vec<u8> = a.ids.iter().map(secure_id::slot).collect();
    assert_eq!(slots, [1, 4]);
    assert!(a.ids.iter().all(|id| secure_id::is_valid(id, &moved)));
}

/// Two disjoint shards: prefixes `0..split` and `split..256`, and two
/// disjoint, non-empty slot sets.
fn disjoint() -> impl Strategy<Value = (Shard, Shard)> {
    (1..256u16, 1..=255u8, any::<u8>()).prop_filter_map(
        "two non-empty slot sets",
        |(split, m, r)| {
            let other = r & !m;
            (other != 0).then(|| {
                (
                    Shard::new(0..split, m).unwrap(),
                    Shard::new(split..256, other).unwrap(),
                )
            })
        },
    )
}

/// The prefixes a shard's actor targets in one pass of `kind`.
fn one_pass(a: &mut Actor, kind: TargetKind) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        out.push(a.routing_target(0, kind).0[0]);
        let p = a.prefixes.as_mut().unwrap();
        if p.advance(0, kind) {
            return out;
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn disjoint_shards_never_target_the_same_prefix_in_a_pass(
        (s1, s2) in disjoint(),
        switch: bool,
        seed: u64,
    ) {
        for kind in TargetKind::ALL {
            let p1 = one_pass(&mut idle(Some(s1), switch, 4, None, seed), kind);
            let p2 = one_pass(&mut idle(Some(s2), switch, 4, None, seed), kind);
            prop_assert!(p1.iter().all(|&p| s1.has_prefix(p)));
            prop_assert!(p2.iter().all(|&p| s2.has_prefix(p)));
            prop_assert!(p1.iter().all(|p| !p2.contains(p)));
            prop_assert_eq!(p1.len() + p2.len(), 256, "between them, every prefix once");
        }
    }

    #[test]
    fn disjoint_shards_never_mint_the_same_bep42_prefix(
        (s1, s2) in disjoint(),
        v4: u32,
        v6: u128,
        want in 1..=16usize,
        seed: u64,
    ) {
        for ip in [IpAddr::from(v4.to_be_bytes()), IpAddr::from(v6.to_be_bytes())] {
            prop_assume!(!secure_id::is_exempt(&ip));
            // At start from a stale saved state, and on the re-id.
            let stale = State { ids: vec![NodeId([0; 20])], external_ip: Some(ip), nodes: vec![] };
            let a = idle(Some(s1), false, want, Some(&stale), seed);
            let b = idle(Some(s2), false, want, Some(&stale), seed);
            let mut c = idle(Some(s1), false, want, None, seed);
            let mut d = idle(Some(s2), false, want, None, seed);
            vote_for(&mut c, ip);
            vote_for(&mut d, ip);
            for (x, y, s) in [(&a.ids, &b.ids, s1), (&c.ids, &d.ids, s1)] {
                prop_assert!(x.iter().all(|id| secure_id::is_valid(id, &ip)));
                prop_assert!(x.iter().all(|id| s.has_slot(secure_id::slot(id))));
                prop_assert!(y.iter().all(|id| s2.has_slot(secure_id::slot(id))));
                prop_assert!(x.len() <= s.slot_count() && y.len() <= s2.slot_count());
                let px: Vec<u32> = x.iter().map(bep42_prefix).collect();
                prop_assert!(y.iter().all(|id| !px.contains(&bep42_prefix(id))), "{}", ip);
            }
        }
    }
}
