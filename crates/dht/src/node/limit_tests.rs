//! Per-reply and per-source limits on intake and replies, against the real
//! actor: what one reply, one host or one block can make the node do.

use super::*;
use crate::krpc::Samples;
use proptest::prelude::*;
use rand::SeedableRng;

fn actor() -> (Actor, Vec<SendSocket>, mpsc::Receiver<Harvest>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel(100_000);
    let a = Actor::new(
        cfg,
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![],
        Some(tx),
        Instant::now(),
        StdRng::seed_from_u64(5),
    );
    (a, vec![SendSocket::Udp(socket)], rx)
}

/// A pending query of `purpose` to `to`, as if the actor had sent it.
fn pretend_sent(a: &mut Actor, to: SocketAddr, purpose: Purpose) -> u64 {
    let tid = a.tids.next_id().unwrap();
    a.pending.insert(
        tid,
        Pending {
            to,
            id: None,
            sent: Instant::now(),
            family: 0,
            purpose,
            hinted: false,
        },
    );
    *a.pending_addrs.entry(to).or_default() += 1;
    tid
}

fn respond(a: &mut Actor, sockets: &[SendSocket], from: SocketAddr, tid: u64, r: &Response<'_>) {
    let bytes = tid.to_be_bytes();
    let msg = Message {
        transaction_id: &bytes,
        version: None,
        ip: None,
        read_only: false,
        body: Body::Response(r.clone()),
    };
    a.on_response(sockets, 0, &msg, Some(r), from);
}

/// `n` distinct public-looking contacts in distinct /24s.
fn contacts(n: usize) -> Vec<u8> {
    let nodes: Vec<NodeInfo> = (0..n as u32)
        .map(|i| NodeInfo {
            id: NodeId([i as u8; 20]),
            addr: SocketAddr::from(((0x0a00_0000 | (i << 8) | 1).to_be_bytes(), 6881)),
        })
        .collect();
    compact::encode_nodes(&nodes, false)
}

fn hashes(n: usize, salt: u32) -> Vec<u8> {
    (0..n as u32)
        .flat_map(|i| {
            let mut h = [0u8; 20];
            h[..4].copy_from_slice(&salt.to_be_bytes());
            h[4..8].copy_from_slice(&i.to_be_bytes());
            h
        })
        .collect()
}

fn drain(rx: &mut mpsc::Receiver<Harvest>) -> usize {
    let mut n = 0;
    while rx.try_recv().is_ok() {
        n += 1;
    }
    n
}

fn query(a: &mut Actor, sockets: &[SendSocket], from: SocketAddr, method: Method<'_>) {
    let msg = Message {
        transaction_id: b"qq",
        version: None,
        ip: None,
        read_only: true,
        body: Body::Query(Query {
            id: NodeId([9; 20]),
            method,
        }),
    };
    let Body::Query(q) = &msg.body else {
        unreachable!()
    };
    a.on_query(sockets, 0, &msg, q, from);
}

fn src(block: u8, host: u8) -> SocketAddr {
    SocketAddr::from(([10, 9, block, host], 9))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// One reply offers at most a few contacts to sampling, whatever it
    /// carries, and a ping's reply offers none.
    #[test]
    fn one_reply_offers_a_bounded_number_of_contacts(n in 0usize..120, ping: bool) {
        let (mut a, sockets, _rx) = actor();
        let from = src(1, 1);
        let purpose = if ping { Purpose::Ping } else { Purpose::Sample };
        let tid = pretend_sent(&mut a, from, purpose);
        let raw = contacts(n);
        let r = Response {
            id: NodeId([7; 20]),
            nodes: CompactNodes::v4(&raw).unwrap(),
            ..Default::default()
        };
        let before = a.sampler.frontier_len();
        respond(&mut a, &sockets, from, tid, &r);
        let offered = a.sampler.frontier_len() - before;
        let most = if ping { 0 } else { n.min(NODES_PER_REPLY) };
        prop_assert!(offered <= most, "{offered} > {most}");
        if !ping {
            prop_assert_eq!(offered, n.min(NODES_PER_REPLY));
        }
    }

    /// One reply hands the pipeline at most a bounded number of hashes,
    /// and one block, over many replies, no more than its share.
    #[test]
    fn samples_are_bounded_per_reply_and_per_block(
        sizes in proptest::collection::vec(0usize..300, 1..12)
    ) {
        let (mut a, sockets, mut rx) = actor();
        let t0 = Instant::now();
        let mut total = 0;
        for (i, n) in sizes.iter().enumerate() {
            // Each reply from another host of the same block.
            let from = src(2, i as u8 + 1);
            let tid = pretend_sent(&mut a, from, Purpose::Sample);
            let raw = hashes(*n, i as u32);
            let r = Response {
                id: NodeId([7; 20]),
                samples: Samples::new(&raw),
                interval: Some(60),
                ..Default::default()
            };
            respond(&mut a, &sockets, from, tid, &r);
            let got = drain(&mut rx);
            prop_assert!(got <= SAMPLES_PER_REPLY, "{got} from one reply");
            if i == 0 {
                prop_assert_eq!(got, (*n).min(SAMPLES_PER_REPLY), "the first reply is taken");
            }
            total += got;
        }
        let elapsed = Instant::now().duration_since(t0).as_secs_f64();
        let share = SAMPLES_PER_NETWORK_BURST + SAMPLES_PER_NETWORK_RATE * elapsed;
        prop_assert!(total as f64 <= share + 1.0, "{total} > {share}");
    }

    /// However queries from different blocks interleave, each block is
    /// answered at most its share, and a block that hasn't used its share
    /// is answered while the global budget lasts.
    #[test]
    fn each_block_gets_its_share_of_replies(
        order in proptest::collection::vec(0u8..4, 1..600)
    ) {
        let (mut a, sockets, _rx) = actor();
        let t0 = Instant::now();
        let (rate, burst) = {
            let qps = f64::from(a.cfg.intensity.queries_per_sec());
            let rate = (qps * 2.0 / 32.0).max(2.0);
            (rate, (rate * 2.0).max(8.0))
        };
        let mut answered = [0u64; 4];
        for b in order {
            let before = a.stats.snapshot().replies_dropped;
            query(&mut a, &sockets, src(b, 1), Method::Ping);
            if a.stats.snapshot().replies_dropped == before {
                answered[usize::from(b)] += 1;
            }
        }
        let share = burst + rate * Instant::now().duration_since(t0).as_secs_f64();
        for n in answered {
            prop_assert!(n as f64 <= share + 1.0, "{n} > {share}");
        }
        // A fresh block still gets an answer.
        let before = a.stats.snapshot().replies_dropped;
        query(&mut a, &sockets, src(200, 1), Method::Ping);
        prop_assert_eq!(a.stats.snapshot().replies_dropped, before);
    }
}

/// On IPv6 the shares are per /48: many /64s of one allocation draw on
/// one sample budget, while another /48 keeps its own.
#[test]
fn many_v6_blocks_of_one_allocation_share_one_sample_budget() {
    let (mut a, sockets, mut rx) = actor();
    let t0 = Instant::now();
    let mut total = 0;
    for net in 0..20u16 {
        let from = SocketAddr::from(([0x2001, 0xdb8, 5, net, 0, 0, 0, 1], 6881));
        let tid = pretend_sent(&mut a, from, Purpose::Sample);
        let raw = hashes(SAMPLES_PER_REPLY, u32::from(net));
        let r = Response {
            id: NodeId([7; 20]),
            samples: Samples::new(&raw),
            interval: Some(60),
            ..Default::default()
        };
        respond(&mut a, &sockets, from, tid, &r);
        total += drain(&mut rx);
    }
    let share = SAMPLES_PER_NETWORK_BURST + SAMPLES_PER_NETWORK_RATE * t0.elapsed().as_secs_f64();
    assert!(total as f64 <= share + 1.0, "{total} > {share}");
    // Another /48 still has its own.
    let from = SocketAddr::from(([0x2001, 0xdb8, 6, 0, 0, 0, 0, 1], 6881));
    let tid = pretend_sent(&mut a, from, Purpose::Sample);
    let raw = hashes(10, 999);
    let r = Response {
        id: NodeId([7; 20]),
        samples: Samples::new(&raw),
        interval: Some(60),
        ..Default::default()
    };
    respond(&mut a, &sockets, from, tid, &r);
    assert_eq!(drain(&mut rx), 10);
}

#[test]
fn passive_harvest_goes_on_when_replies_are_limited() {
    let (mut a, sockets, mut rx) = actor();
    // Spend the global reply budget across many blocks.
    a.reply_rate = TokenBucket::with_burst(0.0, 1.0, Instant::now());
    a.reply_rate.tokens = 0.0;
    let mut h = [0x42u8; 20];
    for i in 0..10u8 {
        h[0] = i;
        let before = a.stats.snapshot().replies_dropped;
        query(
            &mut a,
            &sockets,
            src(i, 1),
            Method::GetPeers {
                info_hash: InfoHash(h),
                want: Want::default(),
                scrape: false,
                noseed: false,
            },
        );
        assert_eq!(a.stats.snapshot().replies_dropped, before + 1);
    }
    assert_eq!(drain(&mut rx), 10, "every hash still reached the pipeline");
}

/// A loopback socket standing in for a querier, and its reply.
fn ask(a: &mut Actor, sockets: &[SendSocket], method: Method<'_>) -> Vec<u8> {
    let p = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    p.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    query(a, sockets, p.local_addr().unwrap(), method);
    let mut buf = vec![0u8; 4096];
    let (n, _) = p.recv_from(&mut buf).expect("a reply");
    buf.truncate(n);
    buf
}

#[test]
fn a_get_peers_reply_lists_a_bounded_number_of_peers() {
    let (mut a, sockets, _rx) = actor();
    let h = InfoHash([0x55; 20]);
    let now = Instant::now();
    for i in 0..50u16 {
        a.peer_store
            .announce(h, SocketAddr::from(([10, 1, 1, 1], 1000 + i)), false, now);
    }
    assert!(a.peer_store.peers(&h, false, false, now).len() > VALUES_PER_REPLY);
    let raw = ask(
        &mut a,
        &sockets,
        Method::GetPeers {
            info_hash: h,
            want: Want::default(),
            scrape: false,
            noseed: false,
        },
    );
    let Body::Response(r) = krpc::parse(&raw).unwrap().body else {
        panic!("a response");
    };
    assert_eq!(r.values.len(), VALUES_PER_REPLY);
}

#[test]
fn an_address_counts_as_proven_once_it_answers_a_query() {
    let (mut a, sockets, _rx) = actor();
    let from = src(3, 3);
    assert!(!a.answered.contains(from.ip()));
    // A reply to a query we didn't send to it proves nothing.
    let tid = pretend_sent(&mut a, src(3, 4), Purpose::Ping);
    let r = Response {
        id: NodeId([7; 20]),
        ..Default::default()
    };
    respond(&mut a, &sockets, from, tid, &r);
    assert!(!a.answered.contains(from.ip()));
    let tid = pretend_sent(&mut a, from, Purpose::Ping);
    respond(&mut a, &sockets, from, tid, &r);
    assert!(a.answered.contains(from.ip()));
}

#[test]
fn one_block_gets_its_share_of_passive_harvest() {
    let (mut a, sockets, mut rx) = actor();
    let t0 = Instant::now();
    let get_peers = |i: u32| {
        let mut h = [0x33u8; 20];
        h[..4].copy_from_slice(&i.to_be_bytes());
        Method::GetPeers {
            info_hash: InfoHash(h),
            want: Want::default(),
            scrape: false,
            noseed: false,
        }
    };
    for i in 0..1000u32 {
        query(&mut a, &sockets, src(20, (i % 200) as u8 + 1), get_peers(i));
    }
    let got = drain(&mut rx);
    let share = PASSIVE_PER_NETWORK_BURST + PASSIVE_PER_NETWORK_RATE * t0.elapsed().as_secs_f64();
    assert!(got as f64 <= share + 1.0, "{got} > {share}");
    assert!(got > 0);
    // Another block still has its own.
    query(&mut a, &sockets, src(21, 1), get_peers(5000));
    assert_eq!(drain(&mut rx), 1);
}

#[test]
fn a_valid_announce_is_stored_when_its_reply_is_dropped() {
    let (mut a, sockets, mut rx) = actor();
    a.reply_rate = TokenBucket::with_burst(0.0, 1.0, Instant::now());
    a.reply_rate.tokens = 0.0;
    let from = src(7, 1);
    let token = a.tokens.issue(&from.ip());
    let h = InfoHash([0x77; 20]);
    let before = a.stats.snapshot().replies_dropped;
    query(
        &mut a,
        &sockets,
        from,
        Method::AnnouncePeer {
            info_hash: h,
            port: 6881,
            implied_port: false,
            token: &token,
            seed: false,
        },
    );
    assert_eq!(a.stats.snapshot().replies_dropped, before + 1);
    assert_eq!(
        a.peer_store.peers(&h, false, false, Instant::now()),
        [SocketAddr::new(from.ip(), 6881)]
    );
    assert_eq!(drain(&mut rx), 1);
}

/// BEP 32: a querier that hasn't answered us gets its own family's nodes
/// only, whatever it asks for; once it has answered, it gets both.
#[test]
fn the_other_familys_nodes_go_only_to_a_proven_querier() {
    let (mut a, sockets, _rx) = actor();
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], true),
        ipv6: true,
    });
    let now = a.now_secs();
    let v4 = NodeInfo {
        id: NodeId([0x11; 20]),
        addr: "192.0.2.1:6881".parse().unwrap(),
    };
    let v6 = NodeInfo {
        id: NodeId([0x12; 20]),
        addr: "[2001:db8::1]:6881".parse().unwrap(),
    };
    a.families[0].table.insert(v4, Contact::Response, now);
    a.families[1].table.insert(v6, Contact::Response, now);
    let find = || Method::FindNode {
        target: NodeId([0x10; 20]),
        want: Want { v4: true, v6: true },
    };
    let nodes = |raw: &[u8]| {
        let Body::Response(r) = krpc::parse(raw).unwrap().body else {
            panic!("a response");
        };
        (r.nodes.len(), r.nodes6.len())
    };
    let raw = ask(&mut a, &sockets, find());
    assert_eq!(
        nodes(&raw),
        (1, 0),
        "an unproven querier gets its own family's"
    );
    a.answered
        .insert("127.0.0.1".parse().unwrap(), Instant::now());
    let raw = ask(&mut a, &sockets, find());
    assert_eq!(nodes(&raw), (1, 1), "a proven one gets what it asked for");
}

/// The actor's queries carry keyed ids on the wire: the ids the peers
/// receive are the pending ones, and two in a row aren't neighbours, as a
/// plain counter's would be.
#[test]
fn queries_on_the_wire_carry_keyed_ids() {
    let (mut a, sockets, _rx) = actor();
    let peers: Vec<_> = (0..2)
        .map(|_| {
            let p = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            p.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            p
        })
        .collect();
    for p in &peers {
        let to = p.local_addr().unwrap();
        assert_eq!(
            a.try_query(&sockets, 0, to, None, Method::Ping, Purpose::Ping),
            Sent::Yes
        );
    }
    let mut wire = vec![];
    for p in &peers {
        let mut buf = [0u8; 512];
        let (n, _) = p.recv_from(&mut buf).expect("the query");
        let t = krpc::parse(&buf[..n]).unwrap().transaction_id.to_vec();
        wire.push(u64::from_be_bytes(t.try_into().expect("8-byte id")));
    }
    let mut pending: Vec<u64> = a.pending.keys().copied().collect();
    pending.sort_unstable();
    let mut sorted = wire.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, pending);
    assert!(wire.iter().all(|t| *t < super::super::tids::LIMIT));
    assert!(wire[0].abs_diff(wire[1]) > 1, "{wire:?}");
}
