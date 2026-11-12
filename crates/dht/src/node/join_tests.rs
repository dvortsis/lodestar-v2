//! X-dht-lookup-converge against the real actor, on a scripted network
//! answered by hand (no socket is read, no clock is waited on):
//!
//! - a pool request made before the table holds anyone is parked until
//!   the bootstrap self-lookup ends, then finds its peer, and the router
//!   is never asked on its behalf;
//! - a failed bootstrap, or [`PARK_DEADLINE`], ends parked requests empty,
//!   as before;
//! - a joined node looks up every prefix level above its closest
//!   neighbour once (Kademlia's join step), at most half of
//!   [`MAX_LOOKUPS`] at a time, so it learns the far keyspace even when
//!   its neighbours don't know it.

use super::tests::unbound;
use super::*;
use crate::compact::{CompactNodes, encode_nodes};
use rand::SeedableRng;

/// Answers rounds at most; every test ends well within it.
const ROUNDS: usize = 64;

fn actor(router: SocketAddr, query_timeout: Duration) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![router.to_string()],
        virtual_ids: 1,
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        harvest: super::super::HarvestConfig {
            sample: false,
            passive: true,
        },
        query_timeout,
        ..Default::default()
    };
    let actor = Actor::new(
        cfg,
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![router],
        None,
        Instant::now(),
        StdRng::seed_from_u64(11),
    );
    (actor, vec![SendSocket::Udp(socket)])
}

/// `own` with bit `bit` (0 = most significant) flipped, and `tag` in the
/// last byte so ids stay distinct: shares exactly `bit` leading bits.
fn at_level(own: NodeId, bit: usize, tag: u8) -> NodeId {
    let mut id = own.0;
    id[bit / 8] ^= 0x80 >> (bit % 8);
    id[19] ^= tag;
    NodeId(id)
}

/// The scripted network. The router knows everyone, as a long-running
/// router does. Every other node knows only its own group: young nodes
/// that never refreshed their far buckets, the e2e network's islands.
struct Net {
    router: SocketAddr,
    /// The router's id: in the far half (so the far lookups ask it), a
    /// level above the far group.
    router_id: NodeId,
    groups: Vec<Vec<NodeInfo>>,
    /// The node storing a peer for `hash`, and that peer.
    holder: Option<(SocketAddr, InfoHash, SocketAddr)>,
}

impl Net {
    fn all(&self) -> impl Iterator<Item = &NodeInfo> {
        self.groups.iter().flatten()
    }

    fn id_of(&self, addr: SocketAddr) -> NodeId {
        if addr == self.router {
            return self.router_id;
        }
        self.all()
            .find(|n| n.addr == addr)
            .map_or(NodeId([0xee; 20]), |n| n.id)
    }

    /// The 8 nodes closest to `target` that `addr` knows.
    fn nodes_for(&self, addr: SocketAddr, target: NodeId) -> Vec<NodeInfo> {
        let mut known: Vec<NodeInfo> = if addr == self.router {
            self.all().copied().collect()
        } else {
            self.groups
                .iter()
                .find(|g| g.iter().any(|n| n.addr == addr))
                .cloned()
                .unwrap_or_default()
        };
        known.retain(|n| n.addr != addr);
        known.sort_by_key(|n| n.id.distance(&target));
        known.truncate(K);
        known
    }
}

/// What one round of answers saw.
#[derive(Default)]
struct Seen {
    /// get_peers queries the router got.
    router_get_peers: usize,
    /// find_node queries the router got.
    router_find_node: usize,
}

/// Answers every query in flight, as `net` would.
fn answer(a: &mut Actor, sockets: &[SendSocket], net: &Net, seen: &mut Seen) {
    let tids: Vec<u64> = a.pending.keys().copied().collect();
    for tid in tids {
        let Some(p) = a.pending.get(&tid) else {
            continue;
        };
        let (to, family, expected) = (p.to, p.family, p.id);
        let lookup = match p.purpose {
            Purpose::Lookup(lid) => a.lookups.get(&lid).map(|l| (l.target, l.info_hash)),
            _ => None,
        };
        if to == net.router {
            match lookup {
                Some((_, Some(_))) => seen.router_get_peers += 1,
                _ => seen.router_find_node += 1,
            }
        }
        let nodes = lookup.map_or_else(Vec::new, |(target, _)| net.nodes_for(to, target));
        let encoded = encode_nodes(&nodes, false);
        let values = match (lookup, net.holder) {
            (Some((_, Some(h))), Some((at, held, peer))) if at == to && h == held => vec![peer],
            _ => Vec::new(),
        };
        let token = [7u8; 4];
        let response = Response {
            id: expected.unwrap_or_else(|| net.id_of(to)),
            nodes: CompactNodes::v4(&encoded).expect("whole entries"),
            values,
            token: lookup.and_then(|(_, h)| h).map(|_| &token[..]),
            ..Default::default()
        };
        let bytes = tid.to_be_bytes();
        let msg = Message {
            transaction_id: &bytes,
            version: None,
            ip: None,
            read_only: false,
            body: Body::Response(response.clone()),
        };
        a.on_response(sockets, family, &msg, Some(&response), to);
    }
}

/// Asks for `hash`'s peers as the pool does, and returns the reply.
fn request(a: &mut Actor, hash: InfoHash) -> oneshot::Receiver<Vec<SocketAddr>> {
    let (tx, mut rx) = oneshot::channel();
    a.on_command(Command::RequestPeers(hash, tx, None));
    rx.try_recv()
        .expect("answered at once")
        .expect("admitted, not refused")
}

/// Two groups around the actor's id: a young neighbourhood of 8 (levels
/// 4–11), so the router's bootstrap answer is all near, and the far half
/// (level 0, 8 nodes).
fn two_neighbourhoods(own: NodeId) -> (SocketAddr, Vec<Vec<NodeInfo>>) {
    let router = unbound(false, 7);
    let near = (0..8)
        .map(|i| NodeInfo {
            id: at_level(own, 4 + i, 1),
            addr: unbound(false, 100 + i as u16),
        })
        .collect();
    let far = (0..8)
        .map(|i| NodeInfo {
            id: at_level(at_level(own, 0, 2), 3 + i, 3),
            addr: unbound(false, 200 + i as u16),
        })
        .collect();
    (router, vec![near, far])
}

/// One run of the far-half request: made before the bootstrap (parked)
/// or after it. Returns the peers found and what the router saw.
fn far_request(parked: bool) -> (Vec<SocketAddr>, Seen, bool) {
    let router = unbound(false, 7);
    let (mut a, sockets) = actor(router, Duration::from_secs(4));
    let own = a.ids[0];
    let (_, groups) = two_neighbourhoods(own);
    // The hash sits in the far half, stored on the far node closest to
    // it (where an announce lands).
    let hash = InfoHash(at_level(own, 0, 9).0);
    let holder = groups[1]
        .iter()
        .min_by_key(|n| n.id.distance(&NodeId(hash.0)))
        .expect("a far group")
        .addr;
    let peer = unbound(false, 900);
    let net = Net {
        router,
        router_id: at_level(at_level(own, 0, 0x55), 1, 0),
        groups,
        holder: Some((holder, hash, peer)),
    };
    let mut seen = Seen::default();
    a.start_bootstrap(&sockets, Vec::new());
    let mut rx = None;
    let mut queued = false;
    if parked {
        rx = Some(request(&mut a, hash));
        a.maintain(&sockets);
        // Parked: queued, not started, and nothing asked for it.
        queued = a.get_peers_queue.len() == 1 && a.peer_requests.is_empty();
    }
    for _ in 0..ROUNDS {
        if a.bootstrapped && rx.is_none() {
            rx = Some(request(&mut a, hash));
        }
        let was_parked = !a.bootstrapped;
        answer(&mut a, &sockets, &net, &mut seen);
        if was_parked {
            assert_eq!(seen.router_get_peers, 0, "nothing asked while parked");
            assert!(seen.router_find_node <= 1, "only the bootstrap's query");
        }
        a.maintain(&sockets);
        if let Some(Ok(peers)) = rx.as_mut().map(|r| r.try_recv()) {
            return (peers, seen, queued);
        }
    }
    panic!("no answer within {ROUNDS} rounds");
}

#[test]
fn a_request_before_any_node_is_parked_then_finds_its_peer() {
    let (peers, parked, queued) = far_request(true);
    // Unparked, it would have started with nobody to ask and ended empty.
    assert_eq!(peers, vec![unbound(false, 900)], "found the peer");
    assert!(queued, "it waited in the queue, not started");
    // The same request made after the bootstrap: parking costs the
    // router nothing extra.
    let (_, control, _) = far_request(false);
    // (The join refresh's find_node queries differ only by when each run
    // stops counting; while parked, the router saw only the bootstrap's.)
    // The router answers in the routing table like any node, and its id
    // here is near the hash, so ordinary routing asks it once either way.
    assert_eq!(parked.router_get_peers, control.router_get_peers);
    assert_eq!(parked.router_get_peers, 1, "ordinary routing, not parking");
}

#[test]
fn a_failed_bootstrap_ends_parked_requests_empty() {
    // Nothing ever answers: every query times out at the next turn.
    let router = unbound(false, 7);
    let (mut a, sockets) = actor(router, Duration::ZERO);
    a.start_bootstrap(&sockets, Vec::new());
    let mut rx = request(&mut a, InfoHash([5; 20]));
    let mut got = None;
    for _ in 0..ROUNDS {
        a.maintain(&sockets);
        if let Ok(peers) = rx.try_recv() {
            got = Some(peers);
            break;
        }
    }
    assert!(a.bootstrapped, "a self-lookup that failed still ended");
    assert_eq!(got, Some(Vec::new()), "ended empty, as before parking");
    assert_eq!(a.stats.snapshot().lookups_completed, 1);
}

#[test]
fn parked_requests_start_at_the_deadline_without_a_bootstrap() {
    // The router never answers and its query never times out; only the
    // deadline releases the request.
    let router = unbound(false, 7);
    let (mut a, sockets) = actor(router, Duration::from_secs(3600));
    a.start_bootstrap(&sockets, Vec::new());
    let mut rx = request(&mut a, InfoHash([5; 20]));
    a.maintain(&sockets);
    assert_eq!(a.get_peers_queue.len(), 1, "parked");
    let start = a.epoch;
    assert!(a.parks_lookups(start + PARK_DEADLINE - Duration::from_millis(1)));
    assert!(!a.parks_lookups(start + PARK_DEADLINE));
    // The actor's own clock past the deadline: started, and with nobody
    // to ask it ends empty at once.
    a.epoch = Instant::now()
        .checked_sub(PARK_DEADLINE)
        .expect("the clock is past the deadline");
    a.maintain(&sockets);
    assert_eq!(rx.try_recv(), Ok(Vec::new()));
    assert!(!a.bootstrapped, "the bootstrap is still out");
}

#[test]
fn no_routers_means_no_parking() {
    let (mut a, sockets) = super::tests::fixture();
    a.families[0].table.insert(
        NodeInfo {
            id: NodeId([42; 20]),
            addr: unbound(false, 9),
        },
        Contact::Response,
        0,
    );
    let _rx = request(&mut a, InfoHash([5; 20]));
    a.maintain(&sockets);
    assert!(a.get_peers_queue.is_empty(), "started at once");
    assert_eq!(a.active_get_peers, 1);
}

#[test]
fn a_joined_node_learns_the_far_half_its_neighbours_dont_know() {
    let router = unbound(false, 7);
    let (mut a, sockets) = actor(router, Duration::from_secs(4));
    let own = a.ids[0];
    let (_, groups) = two_neighbourhoods(own);
    let far: Vec<NodeId> = groups[1].iter().map(|n| n.id).collect();
    // Only the near group answers the router's bootstrap reply for our
    // own id, and the near group knows nobody far.
    let net = Net {
        router,
        router_id: at_level(at_level(own, 0, 0x55), 1, 0),
        groups,
        holder: None,
    };
    let mut seen = Seen::default();
    a.start_bootstrap(&sockets, Vec::new());
    let mut max_maintenance = 0;
    let mut planned = None;
    for _ in 0..ROUNDS {
        answer(&mut a, &sockets, &net, &mut seen);
        if planned.is_none()
            && let Some(t) = &a.join_refresh
        {
            planned = Some(t.len());
        }
        a.maintain(&sockets);
        max_maintenance = max_maintenance.max(a.maintenance_lookups());
        if a.join_planned[0] && a.join_refresh.is_none() && a.lookups.is_empty() {
            break;
        }
    }
    // The far half is in the table now, so we answer for it with nodes
    // that lead there. Without the join refresh, only the near group
    // would be: nobody but the router knows the far half.
    let target = at_level(own, 0, 0x77);
    let closest: Vec<NodeId> = a.families[0]
        .table
        .closest(&target, K, a.now_secs())
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert!(
        closest.iter().any(|id| far.contains(id)),
        "far nodes known: {closest:?}, far group {far:?}, own {own:?}"
    );
    // The closest neighbour shares 11 bits (levels 4–11): levels 0..11.
    assert_eq!(planned, Some(11), "one target per level above it");
    assert!(a.join_refresh.is_none(), "the join refresh ran out");
    assert!(
        max_maintenance <= MAX_LOOKUPS / 2,
        "paced: {max_maintenance} maintenance lookups at once"
    );
    // Once per start: another bootstrap round plans nothing.
    a.start_bootstrap(&sockets, Vec::new());
    for _ in 0..ROUNDS {
        answer(&mut a, &sockets, &net, &mut seen);
        a.maintain(&sockets);
        if a.lookups.is_empty() {
            break;
        }
    }
    assert!(a.join_refresh.is_none(), "not planned twice");
}

#[test]
fn a_v6_family_without_a_router_doesnt_end_the_parking() {
    // Dual stack, routers only in v4: the v6 self-lookup has nobody to
    // ask and ends at once, inside start_bootstrap. v4's is still out.
    let router = unbound(false, 7);
    let (mut a, mut sockets) = actor(router, Duration::from_secs(4));
    // The v6 leg never sends (no v6 router, no v6 node), so its socket
    // slot needs no IPv6 on the host: a placeholder runs everywhere.
    let placeholder = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    placeholder.set_nonblocking(true).unwrap();
    sockets.push(SendSocket::Udp(placeholder));
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], true),
        ipv6: true,
    });
    a.join_planned.push(false);
    a.start_bootstrap(&sockets, Vec::new());
    assert!(
        !a.lookups.values().any(|l| l.bootstrap && l.family == 1),
        "the v6 self-lookup ended at once"
    );
    assert!(!a.bootstrapped, "v4's self-lookup is still out");
    let _rx = request(&mut a, InfoHash([5; 20]));
    a.maintain(&sockets);
    assert_eq!(a.get_peers_queue.len(), 1, "still parked");
    assert!(a.parks_lookups(Instant::now()));
    // v4's bootstrap ends with nodes: parking lifts, and v4 alone plans
    // its join refresh. The empty v6 family stays unplanned, free to
    // plan once its own bootstrap fills it.
    let own = a.ids[0];
    let (_, groups) = two_neighbourhoods(own);
    let net = Net {
        router,
        router_id: at_level(at_level(own, 0, 0x55), 1, 0),
        groups,
        holder: None,
    };
    let mut seen = Seen::default();
    for _ in 0..ROUNDS {
        if a.bootstrapped {
            break;
        }
        answer(&mut a, &sockets, &net, &mut seen);
    }
    assert!(a.bootstrapped, "v4's self-lookup ended with nodes");
    assert_eq!(a.join_planned, [true, false]);
}
