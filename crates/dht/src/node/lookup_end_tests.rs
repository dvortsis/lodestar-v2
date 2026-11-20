//! CR-LOOKUP-END (`dht.lookup_end`) against the real actor: a peer request
//! ends on enough values from enough responders, or once each leg's
//! closest set has answered, and an admitted request starts at once. With
//! the switch off, the same scripts end and start as before. Hostile
//! replies (empty, huge, duplicate, spoofed, one network) never end a
//! request early, inflate its values or grow its state.

use super::tests::deliver_reply;
use super::*;
use crate::node::{LedgerEnd, LedgerSnapshot};
use proptest::prelude::*;
use rand::SeedableRng;

fn actor(lookup_end: bool) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        lookup_end,
        ..Default::default()
    };
    let a = Actor::new(
        cfg,
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![],
        None,
        Instant::now(),
        StdRng::seed_from_u64(5),
    );
    (a, vec![SendSocket::Udp(socket)])
}

/// The hash every test looks up: all zeros, so a node's id is its distance.
const HASH: InfoHash = InfoHash([0; 20]);

/// A node with id `[b; 20]` (distance grows with `b`) at loopback address
/// `127.0.<net>.1`, on a port below 1024 that nothing listens on.
fn at(b: u8, net: u8, port: u16) -> NodeInfo {
    assert!(port < 1024);
    NodeInfo {
        id: NodeId([b; 20]),
        addr: SocketAddr::from(([127, 0, net, 1], port)),
    }
}

fn table(a: &mut Actor, nodes: &[NodeInfo]) {
    for n in nodes {
        a.families[0].table.insert(*n, Contact::Response, 0);
    }
}

fn request(a: &mut Actor, sockets: &[SendSocket]) -> oneshot::Receiver<Vec<SocketAddr>> {
    let (tx, rx) = oneshot::channel();
    a.start_peer_request(sockets, HASH, Some(tx.into()), None);
    rx
}

/// The query in flight to `n`.
fn tid_to(a: &Actor, n: &NodeInfo) -> u64 {
    a.pending
        .iter()
        .find(|(_, p)| p.to == n.addr)
        .map(|(t, _)| *t)
        .unwrap_or_else(|| panic!("no query to {:?}", n.addr))
}

/// `n` answers its query with `values` and `nodes`.
fn answer(
    a: &mut Actor,
    s: &[SendSocket],
    n: &NodeInfo,
    values: Vec<SocketAddr>,
    nodes: &[NodeInfo],
) {
    let tid = tid_to(a, n);
    let raw = crate::compact::encode_nodes(nodes, false);
    let bytes = tid.to_be_bytes();
    let response = Response {
        id: n.id,
        values,
        nodes: CompactNodes::v4(&raw).unwrap(),
        ..Default::default()
    };
    let msg = Message {
        transaction_id: &bytes,
        version: None,
        ip: None,
        read_only: false,
        body: Body::Response(response.clone()),
    };
    a.on_response(s, 0, &msg, Some(&response), n.addr);
}

fn peers(from: u16, n: u16) -> Vec<SocketAddr> {
    (from..from + n)
        .map(|p| SocketAddr::from(([10, 0, 0, 1], 2000 + p)))
        .collect()
}

fn ledger(a: &Actor) -> LedgerSnapshot {
    a.stats.snapshot().ledger
}

fn ended(a: &Actor, why: LedgerEnd) -> u64 {
    ledger(a).ended[why as usize]
}

/// Values from two networks end the request before it converges; values
/// from one network alone, however many, don't. Off, the same replies
/// leave it running until it converges, as before.
#[tokio::test(start_paused = true)]
async fn enough_values_from_two_networks_end_the_request_early() {
    for on in [true, false] {
        let (mut a, s) = actor(on);
        let (n1, n2, n3, n4) = (
            at(0x10, 0, 11),
            at(0x20, 1, 12),
            at(0x30, 0, 13),
            at(0x40, 0, 14),
        );
        table(&mut a, &[n1, n2, n3, n4]);
        let mut rx = request(&mut a, &s);
        assert_eq!(a.pending.len(), 3, "ALPHA unchanged");
        answer(&mut a, &s, &n1, peers(0, 5), &[]);
        // The farthest node is asked now; 5 values aren't enough.
        assert_eq!(a.pending.len(), 3);
        // 9 values, but all from network 127.0.0.0/24.
        answer(&mut a, &s, &n3, peers(5, 4), &[]);
        assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        // One value from a second network.
        answer(&mut a, &s, &n2, peers(9, 1), &[]);
        if on {
            let got = rx.try_recv().unwrap();
            assert_eq!(got.len(), 10);
            assert_eq!(ended(&a, LedgerEnd::EnoughValues), 1);
            // n4's query is dropped with the request; the slot is free.
            assert!(a.pending.is_empty() && a.lookups.is_empty());
            assert_eq!(a.active_get_peers, 0);
        } else {
            assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
            assert_eq!(a.pending.len(), 1, "n4 still out");
            answer(&mut a, &s, &n4, vec![], &[]);
            assert_eq!(rx.try_recv().unwrap().len(), 10);
            assert_eq!(ended(&a, LedgerEnd::Values), 1);
            assert_eq!(ended(&a, LedgerEnd::EnoughValues), 0);
        }
    }
}

/// One network's nodes can't end a request on values, however many they
/// send: it runs on to convergence, as it does with the switch off.
#[tokio::test(start_paused = true)]
async fn one_network_never_ends_a_request_on_values() {
    let (mut a, s) = actor(true);
    let nodes: Vec<_> = (1..=4)
        .map(|i| at(0x10 * i, 0, 10 + u16::from(i)))
        .collect();
    table(&mut a, &nodes);
    let mut rx = request(&mut a, &s);
    for (i, n) in nodes.iter().enumerate() {
        assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        answer(&mut a, &s, n, peers(16 * i as u16, 16), &[]);
    }
    assert_eq!(rx.try_recv().unwrap().len(), 64, "bounded at 64");
    assert_eq!(ended(&a, LedgerEnd::EnoughValues), 0);
    assert_eq!(ended(&a, LedgerEnd::Values), 1);
}

/// A leg ends once its 8 closest have answered, without waiting for the
/// queries still out to farther nodes; off, it waits for them.
#[tokio::test(start_paused = true)]
async fn the_closest_set_ends_a_leg_without_waiting_for_farther_queries() {
    for on in [true, false] {
        let (mut a, s) = actor(on);
        let far: Vec<_> = (0..3).map(|i| at(0xF0 + i, 0, 11 + u16::from(i))).collect();
        table(&mut a, &far);
        let mut rx = request(&mut a, &s);
        // The first far node knows 8 much closer ones, each in its own /24.
        let close: Vec<_> = (1..=8).map(|i| at(i, i, 20 + u16::from(i))).collect();
        answer(&mut a, &s, &far[0], vec![], &close);
        for n in &close {
            assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
            answer(&mut a, &s, n, vec![], &[]);
        }
        if on {
            assert_eq!(rx.try_recv(), Ok(vec![]));
            assert_eq!(ended(&a, LedgerEnd::NoValues), 1);
            assert!(a.pending.is_empty(), "the farther queries are dropped");
        } else {
            assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
            assert_eq!(a.pending.len(), 2, "still waiting for far[1], far[2]");
        }
    }
}

/// With the switch on, an admitted request starts after the command, not
/// on the next tick, and its queries still take the query rate's tokens.
#[tokio::test(start_paused = true)]
async fn an_admitted_request_starts_at_once_under_the_rate() {
    for on in [true, false] {
        let (mut a, s) = actor(on);
        table(&mut a, &[at(0x10, 0, 11), at(0x20, 0, 12)]);
        let (tx, _admitted) = oneshot::channel();
        a.on_command(Command::RequestPeers(HASH, tx, None));
        a.start_admitted(&s);
        if on {
            assert!(a.get_peers_queue.is_empty());
            assert_eq!(a.active_get_peers, 1);
            assert_eq!(a.pending.len(), 2);
        } else {
            assert_eq!(a.get_peers_queue.len(), 1, "waits for the tick");
            assert!(a.pending.is_empty());
        }
    }
    // An empty query rate: the request starts, its queries wait.
    let (mut a, s) = actor(true);
    table(&mut a, &[at(0x10, 0, 11)]);
    a.query_rate.tokens = 0.0;
    let throttled = a.stats.snapshot().throttled;
    let (tx, _admitted) = oneshot::channel();
    a.on_command(Command::RequestPeers(HASH, tx, None));
    a.start_admitted(&s);
    assert_eq!(a.active_get_peers, 1);
    assert!(a.pending.is_empty(), "nothing sent over the rate");
    assert!(a.stats.snapshot().throttled > throttled);
    // A full outstanding cap: nothing goes out either.
    let (mut a, s) = actor(true);
    table(&mut a, &[at(0x10, 0, 11)]);
    for i in 0..a.cfg.intensity.max_outstanding() as u64 {
        a.pending.insert(
            u64::MAX - i,
            Pending {
                to: SocketAddr::from(([127, 0, 0, 1], 1)),
                id: None,
                sent: Instant::now(),
                family: 0,
                purpose: Purpose::Ping,
                hinted: false,
            },
        );
    }
    let before = a.pending.len();
    let (tx, _admitted) = oneshot::channel();
    a.on_command(Command::RequestPeers(HASH, tx, None));
    a.start_admitted(&s);
    assert_eq!(a.active_get_peers, 1);
    assert_eq!(a.pending.len(), before, "the outstanding cap holds");
}

#[derive(Debug, Clone)]
enum Hostile {
    /// A matched reply from the node in flight at this index, with values.
    Reply(usize, Vec<(u32, u16)>),
    /// The same reply from an address in another network: spoofed.
    Spoofed(usize, Vec<(u32, u16)>),
    /// A reply to a transaction id nobody asked.
    UnknownTid(u64, Vec<(u32, u16)>),
}

fn hostile() -> impl Strategy<Value = Hostile> {
    // Empty, duplicate-heavy (a few ports) and huge value lists, and
    // addresses that include port 0 and unroutable ones.
    let values = prop::collection::vec((any::<u32>(), 0u16..4), 0..200);
    prop_oneof![
        (0usize..8, values.clone()).prop_map(|(i, v)| Hostile::Reply(i, v)),
        (0usize..8, values.clone()).prop_map(|(i, v)| Hostile::Spoofed(i, v)),
        (any::<u64>(), values).prop_map(|(t, v)| Hostile::UnknownTid(t, v)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every responder is in one network, so no sequence of replies ends a
    /// request on values, values stay at most 64 and distinct, the
    /// responder list stays at most one network, and the request ends at
    /// most once.
    #[test]
    fn hostile_replies_never_end_early_inflate_or_grow(script in prop::collection::vec(hostile(), 1..40)) {
        let (mut a, s) = actor(true);
        let nodes: Vec<_> = (1..=12).map(|i| at(i * 0x10, 0, 10 + u16::from(i))).collect();
        table(&mut a, &nodes);
        let _rx = request(&mut a, &s);
        let to_addrs = |v: &[(u32, u16)]| -> Vec<SocketAddr> {
            v.iter().map(|(ip, port)| SocketAddr::from((std::net::Ipv4Addr::from(*ip), *port))).collect()
        };
        for step in script {
            let mut out: Vec<_> = a.pending.iter().map(|(t, p)| (*t, p.to, p.id)).collect();
            out.sort_unstable();
            match step {
                Hostile::Reply(i, v) if !out.is_empty() => {
                    let (tid, to, id) = out[i % out.len()];
                    deliver_reply(&mut a, &s, tid, 0, to, id.unwrap(), to_addrs(&v));
                }
                Hostile::Spoofed(i, v) if !out.is_empty() => {
                    let (tid, _, id) = out[i % out.len()];
                    let elsewhere = SocketAddr::from(([127, 0, 9, 1], 99));
                    deliver_reply(&mut a, &s, tid, 0, elsewhere, id.unwrap(), to_addrs(&v));
                }
                Hostile::UnknownTid(tid, v) if !a.pending.contains_key(&tid) => {
                    deliver_reply(&mut a, &s, tid, 0, nodes[0].addr, nodes[0].id, to_addrs(&v));
                }
                _ => {}
            }
            for r in a.peer_requests.values() {
                prop_assert!(r.peers.len() <= 64);
                prop_assert!(r.value_nets.len() <= 1);
                for p in &r.peers {
                    prop_assert!(p.port() != 0 && endpoint_allowed(&a.cfg, *p));
                }
            }
            let l = ledger(&a);
            prop_assert_eq!(l.ended[LedgerEnd::EnoughValues as usize], 0);
            prop_assert!(l.ended.iter().sum::<u64>() <= 1);
            prop_assert!(l.values.sum <= 64);
        }
    }

    /// A spoofed reply from another network, or one to an unknown id,
    /// adds nothing: with them alone the request holds no values at all.
    #[test]
    fn spoofed_and_unknown_replies_count_for_nothing(script in prop::collection::vec(hostile(), 1..40)) {
        let (mut a, s) = actor(true);
        let nodes: Vec<_> = (1..=3).map(|i| at(i * 0x10, 0, 10 + u16::from(i))).collect();
        table(&mut a, &nodes);
        let _rx = request(&mut a, &s);
        for step in script {
            let mut out: Vec<_> = a.pending.iter().map(|(t, p)| (*t, p.id)).collect();
            out.sort_unstable();
            let values = |v: &[(u32, u16)]| -> Vec<SocketAddr> {
                v.iter().map(|(ip, port)| SocketAddr::from((std::net::Ipv4Addr::from(*ip), *port))).collect()
            };
            match step {
                Hostile::Spoofed(i, v) | Hostile::Reply(i, v) if !out.is_empty() => {
                    let (tid, id) = out[i % out.len()];
                    let elsewhere = SocketAddr::from(([127, 0, 9, 1], 99));
                    deliver_reply(&mut a, &s, tid, 0, elsewhere, id.unwrap(), values(&v));
                }
                Hostile::UnknownTid(tid, v) if !a.pending.contains_key(&tid) => {
                    deliver_reply(&mut a, &s, tid, 0, nodes[0].addr, nodes[0].id, values(&v));
                }
                _ => {}
            }
            prop_assert_eq!(a.pending.len(), 3, "nothing was answered");
            for r in a.peer_requests.values() {
                prop_assert!(r.peers.is_empty() && r.value_nets.is_empty());
            }
        }
    }
}

/// One network answering at once for 8 made-up ids next to the target
/// can't converge a leg early: the honest queries out still hold it.
#[tokio::test(start_paused = true)]
async fn one_network_cannot_converge_a_leg_early() {
    let (mut a, s) = actor(true);
    let far: Vec<_> = (0..3).map(|i| at(0xF0 + i, i, 11 + u16::from(i))).collect();
    table(&mut a, &far);
    let mut rx = request(&mut a, &s);
    // far[0] lists 8 ids just next to the target, all in its own /24.
    let fake: Vec<_> = (1..=8).map(|i| at(i, 0, 20 + u16::from(i))).collect();
    answer(&mut a, &s, &far[0], vec![], &fake);
    for n in &fake {
        answer(&mut a, &s, n, vec![], &[]);
    }
    assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    assert_eq!(a.pending.len(), 2, "far[1] and far[2] still out");
}

/// The nodes storing a hash list the same swarm: a second network
/// repeating values already held counts, as corroboration. Without a
/// second network, more of the same never ends the request.
#[tokio::test(start_paused = true)]
async fn a_second_network_corroborating_the_same_values_counts() {
    let (mut a, s) = actor(true);
    let (n1, n2, n3) = (at(0x10, 0, 11), at(0x20, 0, 12), at(0x30, 1, 13));
    table(&mut a, &[n1, n2, n3]);
    let mut rx = request(&mut a, &s);
    answer(&mut a, &s, &n1, peers(0, 9), &[]);
    answer(&mut a, &s, &n2, peers(0, 9), &[]);
    assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    answer(&mut a, &s, &n3, peers(0, 9), &[]);
    assert_eq!(rx.try_recv().unwrap().len(), 9);
    assert_eq!(ended(&a, LedgerEnd::EnoughValues), 1);
}

/// Each new end releases the request's permit exactly once, as
/// `completed`, for a final reply and for a stream (which reads
/// `complete`).
#[tokio::test(start_paused = true)]
async fn every_new_end_returns_its_permit_once() {
    use crate::node::LookupEnd;
    use crate::node::stream::LookupSession;
    let completed =
        |a: &Actor| a.stats.snapshot().peer_permit_returns[PermitReturn::Completed as usize];
    for stream in [false, true] {
        for end_on_values in [true, false] {
            let (mut a, s) = actor(true);
            let sem = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = sem.clone().try_acquire_owned().unwrap();
            let (n1, n2, n3) = (at(0x10, 0, 11), at(0x20, 1, 12), at(0x30, 2, 13));
            table(&mut a, &[n1, n2, n3]);
            let mut session = None;
            let mut rx = None;
            if stream {
                let (tx, mut admitted) = oneshot::channel::<Option<LookupSession>>();
                a.on_command(Command::RequestSession(HASH, Box::new(tx), Some(permit)));
                a.start_admitted(&s);
                session = Some(admitted.try_recv().unwrap().expect("a session"));
            } else {
                let (tx, r) = oneshot::channel();
                a.start_peer_request(&s, HASH, Some(tx.into()), Some(permit));
                rx = Some(r);
            }
            assert_eq!(sem.available_permits(), 0);
            if end_on_values {
                answer(&mut a, &s, &n1, peers(0, 8), &[]);
                answer(&mut a, &s, &n2, peers(8, 1), &[]);
            } else {
                for n in [&n1, &n2, &n3] {
                    answer(&mut a, &s, n, vec![], &[]);
                }
            }
            assert!(
                a.peer_requests.is_empty(),
                "stream {stream}, values {end_on_values}"
            );
            assert_eq!(sem.available_permits(), 1);
            assert_eq!(completed(&a), 1);
            let why = if end_on_values {
                LedgerEnd::EnoughValues
            } else {
                LedgerEnd::NoValues
            };
            assert_eq!(ended(&a, why), 1);
            if let Some(mut s) = session {
                assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Complete));
            }
            if let Some(mut r) = rx {
                assert!(r.try_recv().is_ok());
            }
            // Nothing later returns it again.
            a.scrape_tick(&s, Instant::now());
            a.release_permits();
            assert_eq!(completed(&a), 1);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Two networks answering with fewer than 8 distinct valid values
    /// (duplicates and port-0 junk included) never end on values.
    #[test]
    fn two_networks_with_too_few_values_never_end_early(
        replies in prop::collection::vec((0u16..7, 0u16..2), 0..60),
    ) {
        let (mut a, s) = actor(true);
        let nodes: Vec<_> = (1..=10).map(|i| at(i * 0x10, i % 2, 10 + u16::from(i))).collect();
        table(&mut a, &nodes);
        let _rx = request(&mut a, &s);
        for (chunk, junk) in replies.chunks(3).zip(0u16..) {
            let mut out: Vec<_> = a.pending.iter().map(|(t, p)| (*t, p.to, p.id)).collect();
            out.sort_unstable();
            let Some(&(tid, to, id)) = out.get(usize::from(junk) % out.len().max(1)) else {
                break;
            };
            // Only 7 distinct valid values exist; port 0 never counts.
            let values: Vec<SocketAddr> = chunk
                .iter()
                .map(|(v, zero)| SocketAddr::from(([10, 0, 0, 1], if *zero == 1 { 0 } else { 3000 + v })))
                .collect();
            deliver_reply(&mut a, &s, tid, 0, to, id.unwrap(), values);
            prop_assert_eq!(ended(&a, LedgerEnd::EnoughValues), 0);
            for r in a.peer_requests.values() {
                prop_assert!(r.peers.len() < ENOUGH_VALUES);
            }
        }
    }
}
