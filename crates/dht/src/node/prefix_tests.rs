//! CR-1b's target prefixes against the real actor: the switch off draws
//! every target as before, on walks each family's cursors only when a
//! query goes out, and new contacts are attributed to the target's kind.

use super::*;
use crate::compact::{CompactNodes, encode_nodes};
use rand::SeedableRng;

const DEADLINE: Duration = Duration::from_secs(10);

fn actor(target_prefixes: bool) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        target_prefixes,
        ..Default::default()
    };
    let actor = Actor::new(
        cfg,
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![],
        None,
        Instant::now(),
        StdRng::seed_from_u64(3),
    );
    (actor, vec![SendSocket::Udp(socket)])
}

/// A listening node, offered to the sampler.
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

/// The target of the next `sample_infohashes` `p` receives, waiting up to
/// [`DEADLINE`] for it.
fn sample_target(p: &std::net::UdpSocket) -> [u8; 20] {
    let mut buf = [0u8; 2048];
    loop {
        let (len, _) = p.recv_from(&mut buf).expect("a sample within the deadline");
        if let Ok(m) = crate::krpc::parse(&buf[..len])
            && let Body::Query(q) = m.body
            && let Method::SampleInfohashes { target, .. } = q.method
        {
            return target.0;
        }
    }
}

/// A sent target's position in its cursor's pass.
fn position(target: [u8; 20]) -> u8 {
    target[0].reverse_bits()
}

fn cursor(a: &Actor, kind: TargetKind) -> u8 {
    position(a.prefixes.as_ref().unwrap().target(0, kind, [0; 19]).0)
}

fn samples_pending(a: &Actor) -> usize {
    a.pending
        .values()
        .filter(|p| matches!(p.purpose, Purpose::Sample))
        .count()
}

#[test]
fn switch_off_draws_each_target_uniformly_as_before() {
    let (mut a, sockets) = actor(false);
    assert!(a.prefixes.is_none(), "off allocates no cursor");
    let (p1, p2) = (peer(&mut a), peer(&mut a));
    // Before CR-1b a tick with a low frontier drew the exploration target,
    // then each sample's in frontier order, as one uniform 20-byte draw
    // apiece.
    a.rng = StdRng::seed_from_u64(9);
    let mut rng = StdRng::seed_from_u64(9);
    let _explore: [u8; 20] = rng.random();
    let (want1, want2): ([u8; 20], [u8; 20]) = (rng.random(), rng.random());
    a.sample_tick(&sockets, Instant::now());
    assert_eq!(sample_target(&p1), want1);
    assert_eq!(sample_target(&p2), want2);
    let st = a.stats.snapshot();
    assert_eq!(st.target_prefix_queries, [0, 0]);
    assert_eq!(st.target_prefix_cycles, 0);
}

#[test]
fn switch_on_walks_the_prefixes_and_holds_on_throttle_and_pause() {
    let (mut a, sockets) = actor(true);
    let peers: Vec<_> = (0..3).map(|_| peer(&mut a)).collect();
    a.sample_tick(&sockets, Instant::now());
    let mut at: Vec<u8> = peers.iter().map(|p| position(sample_target(p))).collect();
    at.sort_unstable();
    assert_eq!(at, [0, 1, 2]);
    assert_eq!(cursor(&a, TargetKind::Sample), 3);
    assert_eq!(
        cursor(&a, TargetKind::Explore),
        0,
        "the exploration lookup had no candidate, so it sent nothing"
    );
    assert_eq!(a.stats.snapshot().target_prefix_queries, [0, 3]);

    // Throttled: the sample isn't sent and its prefix isn't skipped.
    let p = peer(&mut a);
    let (rate, burst, tokens) = (a.query_rate.rate, a.query_rate.burst, a.query_rate.tokens);
    a.query_rate.rate = 0.0;
    a.query_rate.tokens = 0.0;
    let before = samples_pending(&a);
    a.sample_tick(&sockets, Instant::now());
    assert_eq!(samples_pending(&a), before);
    assert_eq!(cursor(&a, TargetKind::Sample), 3);
    (a.query_rate.rate, a.query_rate.burst, a.query_rate.tokens) = (rate, burst, tokens);

    // Paused: no tick samples or explores, so no cursor moves.
    let explore = cursor(&a, TargetKind::Explore);
    a.paused = true;
    a.maintain(&sockets);
    assert_eq!(cursor(&a, TargetKind::Sample), 3);
    assert_eq!(cursor(&a, TargetKind::Explore), explore);
    a.paused = false;
    a.sample_tick(&sockets, Instant::now());
    assert_eq!(position(sample_target(&p)), 3, "resumes where it stopped");
}

#[test]
fn a_cursor_wraps_after_prefix_255_and_counts_a_cycle() {
    let (mut a, sockets) = actor(true);
    a.prefixes.as_mut().unwrap().set(0, TargetKind::Sample, 255);
    let p1 = peer(&mut a);
    let p2 = peer(&mut a);
    a.sample_tick(&sockets, Instant::now());
    let mut prefixes = [sample_target(&p1)[0], sample_target(&p2)[0]];
    prefixes.sort_unstable();
    assert_eq!(prefixes, [0, 255], "the pass's last prefix, then the first");
    assert_eq!(cursor(&a, TargetKind::Sample), 1);
    assert_eq!(a.stats.snapshot().target_prefix_cycles, 1);
}

#[test]
fn new_contacts_are_counted_by_the_target_that_brought_them() {
    // Counted with the switch off too, so both arms of an A/B have it.
    let (mut a, sockets) = actor(false);
    let p = peer(&mut a);
    a.sample_tick(&sockets, Instant::now());
    sample_target(&p);
    let (&tid, pend) = a
        .pending
        .iter()
        .find(|(_, q)| matches!(q.purpose, Purpose::Sample))
        .unwrap();
    let (family, to, id) = (pend.family, pend.to, pend.id.unwrap());
    let contacts = |ports: &[u16]| {
        let nodes: Vec<NodeInfo> = ports
            .iter()
            .map(|&port| NodeInfo {
                id: NodeId([port as u8; 20]),
                addr: SocketAddr::from(([127, 0, 0, 1], port)),
            })
            .collect();
        encode_nodes(&nodes, false)
    };
    let reply = |a: &mut Actor, tid: u64, raw: &[u8]| {
        let bytes = tid.to_be_bytes();
        let response = Response {
            id,
            nodes: CompactNodes::v4(raw).unwrap(),
            ..Default::default()
        };
        let msg = Message {
            transaction_id: &bytes,
            version: None,
            ip: None,
            read_only: false,
            body: Body::Response(response.clone()),
        };
        a.on_response(&sockets, family, &msg, Some(&response), to);
    };
    reply(&mut a, tid, &contacts(&[40001, 40002]));
    assert_eq!(a.stats.snapshot().target_novel_nodes, [0, 2]);

    // A ping's contacts aren't a target's; one already queued isn't new.
    assert_eq!(
        a.try_query(&sockets, family, to, Some(id), Method::Ping, Purpose::Ping),
        Sent::Yes
    );
    let (&ping, _) = a
        .pending
        .iter()
        .find(|(_, q)| matches!(q.purpose, Purpose::Ping))
        .unwrap();
    reply(&mut a, ping, &contacts(&[40001, 40003]));
    assert_eq!(a.stats.snapshot().target_novel_nodes, [0, 2]);
    assert!(
        a.sampler.frontier_len() >= 3,
        "the ping's new contact is still queued"
    );
}

#[test]
fn the_exploration_lookup_is_marked_and_its_contacts_count_as_explore() {
    let (mut a, sockets) = actor(false);
    let x: SocketAddr = "127.0.0.1:40100".parse().unwrap();
    let id = NodeId([9; 20]);
    a.families[0]
        .table
        .insert(NodeInfo { id, addr: x }, Contact::Response, 0);
    a.sample_tick(&sockets, Instant::now());
    let (&tid, lid) = a
        .pending
        .iter()
        .find_map(|(t, q)| match q.purpose {
            Purpose::Lookup(lid) => Some((t, lid)),
            _ => None,
        })
        .expect("the exploration lookup asked the table's node");
    assert!(a.lookups[&lid].explore);
    let raw = encode_nodes(
        &[NodeInfo {
            id: NodeId([1; 20]),
            addr: "127.0.0.1:40101".parse().unwrap(),
        }],
        false,
    );
    let bytes = tid.to_be_bytes();
    let response = Response {
        id,
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
    a.on_response(&sockets, 0, &msg, Some(&response), x);
    assert_eq!(a.stats.snapshot().target_novel_nodes, [1, 0]);
}

#[test]
fn the_explore_cursor_moves_only_when_its_lookup_sends() {
    let (mut a, sockets) = actor(true);
    a.families[0].table.insert(
        NodeInfo {
            id: NodeId([9; 20]),
            addr: "127.0.0.1:40200".parse().unwrap(),
        },
        Contact::Response,
        0,
    );
    a.sample_tick(&sockets, Instant::now());
    assert_eq!(cursor(&a, TargetKind::Explore), 1);
    assert_eq!(a.stats.snapshot().target_prefix_queries[0], 1);
    let target = a.lookups.values().find(|l| l.explore).unwrap().target;
    assert_eq!(target.0[0], 0, "position 0's prefix");
}
