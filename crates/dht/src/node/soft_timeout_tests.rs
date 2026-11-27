//! CR-SOFT-TIMEOUT (`dht.soft_timeout`) against the real actor, on tokio's
//! paused clock: an owned peer lookup's query left unanswered past the
//! soft timeout keeps its lane, so a leg never has more than `ALPHA` out,
//! but gives its place among the K closest up: the next candidate is asked
//! once a lane frees, and the leg can end once K others have answered. It
//! stays out, against the outstanding cap, until it's answered (once) or
//! times out; no query's timer is extended. Cases run with
//! `dht.lookup_end` off and on.

use super::*;
use crate::node::lookup::ALPHA;
use crate::node::{LedgerEnd, PermitReturn};
use lodestar_metainfo::InfoHash;
use proptest::prelude::*;
use rand::SeedableRng;
use tokio::sync::Semaphore;

fn actor(soft_timeout: bool, lookup_end: bool) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        // Sampling held for a day (a fixture's delay; production no longer
        // waits, CR-0w), so `maintain` sends no samples to the scripted
        // nodes.
        sample_start_delay: Some(Duration::from_secs(86_400)),
        soft_timeout,
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
        StdRng::seed_from_u64(7),
    );
    (a, vec![SendSocket::Udp(socket)])
}

/// The four switch combinations: (soft_timeout, lookup_end).
const COMBOS: [(bool, bool); 4] = [(false, false), (false, true), (true, false), (true, true)];

const HASH: InfoHash = InfoHash([0; 20]);

/// A node with id `[b; 20]` (distance grows with `b`) at
/// `127.0.<net>.1:<port>`, a port below 1024 that nothing listens on.
fn at(b: u8, net: u8, port: u16) -> NodeInfo {
    assert!(port < 1024);
    NodeInfo {
        id: NodeId([b; 20]),
        addr: SocketAddr::from(([127, 0, net, 1], port)),
    }
}

/// `n` nodes from `first` up, each in its own /24.
fn spread(first: u8, n: u8) -> Vec<NodeInfo> {
    (0..n)
        .map(|i| {
            at(
                first + i,
                first.wrapping_add(i),
                100 + u16::from(first) + u16::from(i),
            )
        })
        .collect()
}

fn table(a: &mut Actor, nodes: &[NodeInfo]) {
    for n in nodes {
        a.families[0].table.insert(*n, Contact::Response, 0);
    }
}

/// Starts an owned request holding a permit from `sem`.
fn request(
    a: &mut Actor,
    s: &[SendSocket],
    sem: &Arc<Semaphore>,
) -> oneshot::Receiver<Vec<SocketAddr>> {
    let (tx, rx) = oneshot::channel();
    let permit = sem.clone().try_acquire_owned().unwrap();
    a.start_peer_request(s, HASH, Some(tx.into()), Some(permit));
    rx
}

fn owned(a: &Actor, p: &Pending) -> bool {
    matches!(p.purpose, Purpose::Lookup(lid)
        if a.lookups.get(&lid).is_some_and(|l| l.owner.is_some()))
}

/// The owned lookups' queries out, by tid.
fn out(a: &Actor) -> Vec<(u64, SocketAddr, NodeId)> {
    let mut v: Vec<_> = a
        .pending
        .iter()
        .filter(|(_, p)| owned(a, p))
        .map(|(t, p)| (*t, p.to, p.id.expect("candidate query")))
        .collect();
    v.sort_unstable();
    v
}

fn detached(a: &Actor) -> usize {
    a.pending
        .values()
        .filter(|p| matches!(p.purpose, Purpose::Detached))
        .count()
}

fn tid_to(a: &Actor, n: &NodeInfo) -> u64 {
    a.pending
        .iter()
        .find(|(_, p)| {
            p.to == n.addr && matches!(p.purpose, Purpose::Lookup(_) | Purpose::Detached)
        })
        .map(|(t, _)| *t)
        .unwrap_or_else(|| panic!("no query to {:?}", n.addr))
}

/// `from` answers `tid` as `id`, listing `nodes`.
fn reply_as(
    a: &mut Actor,
    s: &[SendSocket],
    tid: u64,
    from: SocketAddr,
    id: NodeId,
    nodes: &[NodeInfo],
) {
    let raw = crate::compact::encode_nodes(nodes, false);
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
    // On the family the query went out on (a second family in two-family
    // tests), or the first for a tid nobody asked.
    let family = a.pending.get(&tid).map_or(0, |p| p.family);
    a.on_response(s, family, &msg, Some(&response), from);
}

/// `from` answers `tid` as `id` with `values`.
fn reply_values(
    a: &mut Actor,
    s: &[SendSocket],
    tid: u64,
    from: SocketAddr,
    id: NodeId,
    values: Vec<SocketAddr>,
) {
    let bytes = tid.to_be_bytes();
    let response = Response {
        id,
        values,
        ..Default::default()
    };
    let msg = Message {
        transaction_id: &bytes,
        version: None,
        ip: None,
        read_only: false,
        body: Body::Response(response.clone()),
    };
    a.on_response(s, 0, &msg, Some(&response), from);
}

/// `from` answers `tid` with a KRPC error.
fn reply_error(a: &mut Actor, s: &[SendSocket], tid: u64, from: SocketAddr) {
    let bytes = tid.to_be_bytes();
    let msg = Message {
        transaction_id: &bytes,
        version: None,
        ip: None,
        read_only: false,
        body: Body::Error(ErrorBody {
            code: 202,
            message: b"server error",
        }),
    };
    a.on_response(s, 0, &msg, None, from);
}

/// Answers, at once and with no nodes, every owned lookup query out to a
/// node not in `silent`, until none is left.
fn answer_all_but(a: &mut Actor, s: &[SendSocket], silent: &[NodeInfo]) {
    while let Some((tid, to, id)) = out(a)
        .into_iter()
        .find(|(_, to, _)| silent.iter().all(|n| n.addr != *to))
    {
        reply_as(a, s, tid, to, id, &[]);
        check(a);
    }
}

fn answer(a: &mut Actor, s: &[SendSocket], n: &NodeInfo, nodes: &[NodeInfo]) {
    let tid = tid_to(a, n);
    reply_as(a, s, tid, n.addr, n.id, nodes);
}

/// One maintenance tick, `secs` seconds on.
async fn tick(a: &mut Actor, s: &[SendSocket], secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
    a.maintain(s);
}

fn returns(a: &Actor) -> u64 {
    a.stats.snapshot().peer_permit_returns.iter().sum()
}

/// Peer lookups' queries sent so far (the node's own maintenance aside).
fn lookup_queries(a: &Actor) -> u64 {
    a.stats.snapshot().ledger.queries[QueryPurpose::PeerLookup as usize]
}

fn ended(a: &Actor, why: LedgerEnd) -> u64 {
    a.stats.snapshot().ledger.ended[why as usize]
}

/// The bookkeeping every step must keep: a request's in-flight count is
/// all its legs' queries out, slow ones included (they keep their lanes);
/// a leg has at most `ALPHA` out; every soft tid is pending and names a
/// running lookup or is detached; the outstanding cap and the 64-query cap
/// hold.
fn check(a: &Actor) {
    for (owner, r) in &a.peer_requests {
        let legs: Vec<u32> = a
            .lookups
            .iter()
            .filter(|(_, l)| l.owner == Some(*owner))
            .map(|(lid, _)| *lid)
            .collect();
        let queries = a
            .pending
            .values()
            .filter(|p| {
                matches!(p.purpose, Purpose::Lookup(lid) if legs.contains(&lid)) && !p.hinted
            })
            .count();
        assert_eq!(r.inflight, queries, "request in flight = queries out");
        assert!(r.sent <= MAX_PEER_QUERIES, "64-query cap");
    }
    for (lid, l) in &a.lookups {
        let leg = a
            .pending
            .values()
            .filter(|p| matches!(p.purpose, Purpose::Lookup(id) if id == *lid) && !p.hinted)
            .count();
        assert!(leg <= ALPHA, "a leg has {leg} out");
        assert!(l.slow() <= leg, "slow {} of {leg} out", l.slow());
    }
    assert!(a.soft_tids.iter().all(|t| a.pending.contains_key(t)));
    for t in &a.soft_tids {
        match a.pending[t].purpose {
            Purpose::Lookup(lid) => assert!(a.lookups.contains_key(&lid)),
            Purpose::Detached => {}
            p => panic!("a soft {p:?}"),
        }
    }
    assert!(a.pending.len() <= a.cfg.intensity.max_outstanding());
    let st = a.stats.snapshot();
    assert!(st.soft_timeout_late <= st.soft_timeouts);
}

/// Starts a request whose three far starting nodes answer at once, the
/// first listing `close`, as early hops of a walk do.
fn far_request(
    a: &mut Actor,
    s: &[SendSocket],
    sem: &Arc<Semaphore>,
    close: &[NodeInfo],
) -> oneshot::Receiver<Vec<SocketAddr>> {
    let far: Vec<_> = (0..3)
        .map(|i| at(0xF0 + i, 0xF0 + i, 900 + u16::from(i)))
        .collect();
    table(a, &far);
    let rx = request(a, s, sem);
    answer(a, s, &far[0], close);
    answer(a, s, &far[1], &[]);
    answer(a, s, &far[2], &[]);
    rx
}

/// The closest node stays silent; the far ones asked first and the 7
/// other close ones answer. On, once it goes soft (1–2 s) the leg ends on
/// K answers without it, and the silent query stays out, detached, until
/// it answers (once; a repeat is dropped) or times out. Off, the leg waits
/// for the query timeout.
#[tokio::test(start_paused = true)]
async fn a_silent_closest_node_stops_holding_the_leg() {
    for (soft, lookup_end) in COMBOS {
        let (mut a, s) = actor(soft, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        let close = spread(1, 8);
        let mut rx = far_request(&mut a, &s, &sem, &close);
        answer_all_but(&mut a, &s, &close[..1]);
        check(&a);
        assert!(rx.try_recv().is_err(), "the closest is unanswered");
        let sent = lookup_queries(&a);
        tick(&mut a, &s, 1).await;
        check(&a);
        assert_eq!(lookup_queries(&a), sent, "nothing more sent");
        if soft {
            assert_eq!(rx.try_recv(), Ok(vec![]), "{lookup_end}");
            assert_eq!(ended(&a, LedgerEnd::NoValues), 1);
            assert_eq!((returns(&a), sem.available_permits()), (1, 1));
            assert!(a.peer_requests.is_empty() && a.active_get_peers == 0);
            assert_eq!(detached(&a), 1, "still out, against the cap");
            assert_eq!(a.stats.snapshot().soft_timeouts, 1);
            let tid = tid_to(&a, &close[0]);
            let answered = a.stats.snapshot().answered;
            reply_as(&mut a, &s, tid, close[0].addr, close[0].id, &[]);
            let st = a.stats.snapshot();
            assert_eq!((st.soft_timeout_late, st.answered), (1, answered + 1));
            assert!(detached(&a) == 0 && a.soft_tids.is_empty());
            reply_as(&mut a, &s, tid, close[0].addr, close[0].id, &[]);
            let st = a.stats.snapshot();
            assert_eq!(
                (st.soft_timeout_late, st.answered),
                (1, answered + 1),
                "repeat"
            );
            assert_eq!(returns(&a), 1, "the permit came back once");
        } else {
            for _ in 0..2 {
                tick(&mut a, &s, 1).await;
                assert!(rx.try_recv().is_err(), "off: still waiting");
            }
            tick(&mut a, &s, 1).await;
            assert_eq!(rx.try_recv(), Ok(vec![]), "the query timeout ends it");
            assert_eq!(a.stats.snapshot().soft_timeouts, 0);
            assert!(a.soft_tids.is_empty() && detached(&a) == 0);
        }
    }
}

/// Soft queries keep their lanes: with tokens and room to spare, going
/// soft sends nothing, on and off alike, and a leg still has 3 out.
#[tokio::test(start_paused = true)]
async fn soft_queries_keep_their_lanes_and_send_nothing_more() {
    for (soft, lookup_end) in COMBOS {
        let (mut a, s) = actor(soft, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let _rx = request(&mut a, &s, &sem);
        a.query_rate.burst = 1_000.0;
        a.query_rate.tokens = 1_000.0;
        let sent = lookup_queries(&a);
        tick(&mut a, &s, 1).await;
        check(&a);
        assert_eq!(out(&a).len(), ALPHA);
        assert_eq!(lookup_queries(&a), sent, "nothing more sent");
        assert_eq!(a.soft_tids.len(), if soft { ALPHA } else { 0 });
        let lid = *a.lookups.keys().next().unwrap();
        assert_eq!(a.lookups[&lid].slow(), if soft { ALPHA } else { 0 });
    }
}

/// A late answer while the leg still runs settles the slow candidate once,
/// frees its lane, and its nodes join the lookup, which asks the closest.
#[tokio::test(start_paused = true)]
async fn a_late_answer_still_feeds_a_running_leg() {
    for lookup_end in [false, true] {
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        let nodes = spread(0x40, 4);
        table(&mut a, &nodes);
        let _rx = request(&mut a, &s, &sem);
        tick(&mut a, &s, 1).await;
        let lid = *a.lookups.keys().next().unwrap();
        assert_eq!(a.lookups[&lid].slow(), 3);
        let closer = spread(1, 2);
        answer(&mut a, &s, &nodes[0], &closer);
        check(&a);
        assert_eq!(a.lookups[&lid].slow(), 2, "settled once");
        assert_eq!(a.stats.snapshot().soft_timeout_late, 1);
        let asked: Vec<SocketAddr> = out(&a).iter().map(|(_, to, _)| *to).collect();
        assert!(
            asked.contains(&closer[0].addr),
            "its lane goes to the closest"
        );
        assert_eq!(asked.len(), ALPHA);
    }
}

/// A slow query that reaches the query timeout fails then, once, and its
/// lane goes to the next candidate, which goes soft in its turn.
#[tokio::test(start_paused = true)]
async fn a_slow_query_fails_once_at_the_query_timeout() {
    for lookup_end in [false, true] {
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let _rx = request(&mut a, &s, &sem);
        let timeouts = a.stats.snapshot().timeouts;
        for _ in 0..4 {
            tick(&mut a, &s, 1).await;
            check(&a);
        }
        let lid = *a.lookups.keys().next().unwrap();
        assert_eq!(a.stats.snapshot().timeouts - timeouts, 3);
        assert_eq!(a.lookups[&lid].slow(), 0, "the next three just went out");
        assert_eq!(out(&a).len(), ALPHA);
        tick(&mut a, &s, 1).await;
        check(&a);
        assert_eq!(a.lookups[&lid].slow(), 3);
        assert_eq!(a.stats.snapshot().soft_timeouts, 6);
    }
}

/// Every end gives the permit back exactly once with the switch on, and
/// detaches the soft queries rather than dropping them; they leave the
/// outstanding count only by answering or timing out.
#[tokio::test(start_paused = true)]
async fn each_permit_returns_once_on_every_end() {
    for lookup_end in [false, true] {
        // Every node silent: the deadline, or running out of candidates.
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 40));
        let mut rx = request(&mut a, &s, &sem);
        let mut secs = 0;
        while rx.try_recv().is_err() {
            tick(&mut a, &s, 1).await;
            check(&a);
            secs += 1;
            assert!(secs <= 26, "the deadline holds");
        }
        assert_eq!(
            ended(&a, LedgerEnd::NoValues) + ended(&a, LedgerEnd::Deadline),
            1
        );
        assert_eq!((returns(&a), sem.available_permits()), (1, 1));
        assert!(detached(&a) <= ALPHA);
        tick(&mut a, &s, 4).await;
        assert_eq!(detached(&a), 0, "detached queries time out");
        assert!(a.soft_tids.is_empty());
        assert_eq!(returns(&a), 1);

        // Deadline, with soft queries out.
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let mut rx = request(&mut a, &s, &sem);
        tick(&mut a, &s, 1).await;
        assert_eq!(a.soft_tids.len(), ALPHA);
        for r in a.peer_requests.values_mut() {
            r.started -= LOOKUP_DEADLINE;
        }
        tick(&mut a, &s, 1).await;
        assert!(rx.try_recv().is_ok());
        assert_eq!(ended(&a, LedgerEnd::Deadline), 1);
        assert_eq!((returns(&a), sem.available_permits()), (1, 1));
        assert_eq!(detached(&a), ALPHA, "kept against the cap");
        tick(&mut a, &s, 4).await;
        assert_eq!(detached(&a), 0);
        assert!(a.soft_tids.is_empty());

        // Cancel: the caller goes after queries went soft.
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let rx = request(&mut a, &s, &sem);
        tick(&mut a, &s, 1).await;
        drop(rx);
        tick(&mut a, &s, 1).await;
        assert_eq!(ended(&a, LedgerEnd::Cancelled), 1);
        assert_eq!(
            a.stats.snapshot().peer_permit_returns[PermitReturn::CancelledActive as usize],
            1
        );
        assert_eq!(returns(&a), 1);
        assert!(a.peer_requests.is_empty());
        assert_eq!(detached(&a), ALPHA);

        // Shutdown: soft queries out, then the actor stops.
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let _rx = request(&mut a, &s, &sem);
        tick(&mut a, &s, 1).await;
        a.release_permits();
        assert_eq!(
            a.stats.snapshot().peer_permit_returns[PermitReturn::Shutdown as usize],
            1
        );
        assert_eq!(returns(&a), 1);
        assert_eq!(sem.available_permits(), 1);
    }
}

/// A hostile network whose every answer lists closer ids that never
/// answer: the request spends at most 64 queries and ends by its deadline,
/// whatever the switches.
#[tokio::test(start_paused = true)]
async fn the_query_cap_and_deadline_hold_against_endless_closer_ids() {
    for (soft, lookup_end) in COMBOS {
        let (mut a, s) = actor(soft, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(0xC0, 8));
        let mut rx = request(&mut a, &s, &sem);
        let mut fresh = 0u8;
        let mut secs = 0;
        while rx.try_recv().is_err() {
            if let Some((tid, to, id)) = out(&a)
                .into_iter()
                .find(|(t, _, _)| !a.soft_tids.contains(t))
                && fresh < 0xB0
            {
                let next: Vec<_> = (0..3)
                    .map(|i| {
                        at(
                            0xB0 - fresh - i,
                            0xB0 - fresh - i,
                            300 + u16::from(fresh + i),
                        )
                    })
                    .collect();
                fresh += 3;
                reply_as(&mut a, &s, tid, to, id, &next);
            }
            check(&a);
            tick(&mut a, &s, 1).await;
            check(&a);
            secs += 1;
            assert!(secs <= 26, "the deadline holds");
        }
        assert_eq!(returns(&a), 1);
        assert_eq!(ended(&a, LedgerEnd::Deadline), 1);
    }
}

/// The query cap with queries going soft along the way: most nodes
/// answer, one per 100 ms, each listing two closer ones, and every eighth
/// stays silent and goes soft. The request spends exactly its 64 queries
/// and ends on the cap once every query is settled, slow ones included,
/// as with the switch off.
#[tokio::test(start_paused = true)]
async fn the_query_cap_ends_a_request_with_soft_queries_detached() {
    let (mut a, s) = actor(true, false);
    let sem = Arc::new(Semaphore::new(1));
    // Id `0xA0 - k` (closer as k grows), own /24 and port.
    let fake = |k: u16| at(0xA0 - k as u8, k as u8, 300 + k);
    table(&mut a, &(0..4).map(fake).collect::<Vec<_>>());
    let mut next = 4u16;
    let mut rx = request(&mut a, &s, &sem);
    let mut step = 0;
    while rx.try_recv().is_err() {
        // One answer per 100 ms step, so ticks pass while it runs.
        if let Some((tid, to, id)) = out(&a)
            .into_iter()
            .find(|(_, to, _)| (to.port() - 300) % 8 != 0)
            && next < 0x9E
        {
            reply_as(&mut a, &s, tid, to, id, &[fake(next), fake(next + 1)]);
            next += 2;
            check(&a);
        }
        tokio::time::advance(Duration::from_millis(100)).await;
        step += 1;
        if step % 10 == 0 {
            a.maintain(&s);
            check(&a);
        }
        assert!(step <= 260, "the deadline holds");
    }
    assert_eq!(ended(&a, LedgerEnd::QueryCap), 1);
    assert_eq!(
        a.stats.snapshot().ledger.queries[QueryPurpose::PeerLookup as usize],
        64
    );
    assert!(a.stats.snapshot().soft_timeouts > 0);
    assert_eq!(detached(&a), 0, "the cap's end waits for them");
    assert_eq!(returns(&a), 1);
}

/// A detached query's late answer touches no lookup, even when its old
/// lookup id has been reused by a new request asking the same node; a
/// repeat of it settles nothing.
#[tokio::test(start_paused = true)]
async fn a_detached_answer_never_reaches_a_reused_lookup() {
    let (mut a, s) = actor(true, false);
    let sem = Arc::new(Semaphore::new(2));
    let close = spread(1, 8);
    let mut rx = far_request(&mut a, &s, &sem, &close);
    let old = *a.lookups.keys().next().unwrap();
    answer_all_but(&mut a, &s, &close[..1]);
    tick(&mut a, &s, 1).await;
    assert!(rx.try_recv().is_ok());
    let late = tid_to(&a, &close[0]);
    // The next request gets the old id and asks close[0] again.
    a.next_lookup = old;
    table(&mut a, &close[..1]);
    let _rx2 = request(&mut a, &s, &sem);
    assert!(a.lookups.contains_key(&old), "id reused");
    let owner = a.lookups[&old].owner.unwrap();
    let before = (a.peer_requests[&owner].inflight, out(&a));
    reply_as(
        &mut a,
        &s,
        late,
        close[0].addr,
        close[0].id,
        &spread(0x80, 3),
    );
    check(&a);
    assert_eq!((a.peer_requests[&owner].inflight, out(&a)), before);
    assert_eq!(a.stats.snapshot().soft_timeout_late, 1);
    let answered = a.stats.snapshot().answered;
    reply_as(
        &mut a,
        &s,
        late,
        close[0].addr,
        close[0].id,
        &spread(0x80, 3),
    );
    check(&a);
    assert_eq!(a.stats.snapshot().answered, answered, "repeat dropped");
    assert_eq!(a.stats.snapshot().soft_timeout_late, 1);
    assert_eq!((a.peer_requests[&owner].inflight, out(&a)), before);
}

/// A leg whose only candidates have gone slow doesn't end on them: with
/// fewer than K answers it waits for them to settle, so their late answers
/// still feed it (here, with a closer node it then asks).
#[tokio::test(start_paused = true)]
async fn a_leg_of_slow_candidates_waits_for_their_answers() {
    for lookup_end in [false, true] {
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        let nodes = spread(0x40, 3);
        table(&mut a, &nodes);
        let mut rx = request(&mut a, &s, &sem);
        tick(&mut a, &s, 1).await;
        check(&a);
        assert_eq!(a.soft_tids.len(), 3);
        assert!(rx.try_recv().is_err(), "waits for the slow ones");
        let closer = spread(1, 2);
        answer(&mut a, &s, &nodes[0], &closer);
        check(&a);
        let asked: Vec<SocketAddr> = out(&a).iter().map(|(_, to, _)| *to).collect();
        assert!(asked.contains(&closer[0].addr));
        assert!(rx.try_recv().is_err());
    }
}

/// CR-LOOKUP-END's values end with a soft query out: the request ends on
/// enough values, its soft query is detached, the others are dropped, and
/// the permit comes back once.
#[tokio::test(start_paused = true)]
async fn enough_values_end_detaches_soft_queries() {
    let (mut a, s) = actor(true, true);
    let sem = Arc::new(Semaphore::new(1));
    let nodes = spread(1, 12);
    table(&mut a, &nodes);
    let mut rx = request(&mut a, &s, &sem);
    let values = |net: u8| -> Vec<SocketAddr> {
        (0..4)
            .map(|i| SocketAddr::from(([10, net, 0, 1], 6000 + i)))
            .collect()
    };
    // nodes[0] stays silent; nodes[1] answers with a first network's four.
    let tid = tid_to(&a, &nodes[1]);
    reply_values(&mut a, &s, tid, nodes[1].addr, nodes[1].id, values(1));
    tick(&mut a, &s, 1).await;
    assert!(a.soft_tids.contains(&tid_to(&a, &nodes[0])));
    // Another, soft too by now, answers late with a second network's
    // four: enough.
    let (tid, to, id) = out(&a)
        .into_iter()
        .find(|(_, to, _)| *to != nodes[0].addr)
        .unwrap();
    reply_values(&mut a, &s, tid, to, id, values(2));
    check(&a);
    assert_eq!(rx.try_recv().unwrap().len(), 8);
    assert_eq!(ended(&a, LedgerEnd::EnoughValues), 1);
    assert_eq!((returns(&a), sem.available_permits()), (1, 1));
    assert!(detached(&a) >= 1, "the soft ones stay out");
    assert_eq!(detached(&a), a.soft_tids.len());
    assert!(out(&a).is_empty(), "the others are dropped");
}

/// A late error reply settles a slow query once, as a failure: the leg's
/// slow count falls, the node isn't asked again, and the reply counts as
/// late.
#[tokio::test(start_paused = true)]
async fn a_late_error_reply_fails_a_slow_query_once() {
    let (mut a, s) = actor(true, false);
    let sem = Arc::new(Semaphore::new(1));
    let nodes = spread(1, 12);
    table(&mut a, &nodes);
    let _rx = request(&mut a, &s, &sem);
    tick(&mut a, &s, 1).await;
    let lid = *a.lookups.keys().next().unwrap();
    let tid = tid_to(&a, &nodes[0]);
    assert!(a.soft_tids.contains(&tid));
    reply_error(&mut a, &s, tid, nodes[0].addr);
    check(&a);
    assert_eq!(a.lookups[&lid].slow(), 2);
    assert_eq!(a.stats.snapshot().soft_timeout_late, 1);
    reply_error(&mut a, &s, tid, nodes[0].addr);
    assert_eq!(a.lookups[&lid].slow(), 2, "a repeat settles nothing");
    let asked = out(&a)
        .iter()
        .filter(|(_, to, _)| *to == nodes[0].addr)
        .count();
    assert_eq!(asked, 0, "not asked again");
}

/// Only owned peer requests' legs go soft: a refresh `find_node` and a
/// BEP 33 scrape wait for the query timeout as before.
#[tokio::test(start_paused = true)]
async fn refresh_and_scrape_lookups_never_go_soft() {
    let (mut a, s) = actor(true, true);
    let nodes = spread(1, 12);
    table(&mut a, &nodes);
    let lid = a.insert_lookup(0, NodeId([0x33; 20]), &nodes, None, false, None);
    a.drive_lookup(&s, lid);
    let (tx, _scrape) = oneshot::channel();
    a.start_scrape(&s, InfoHash([0x44; 20]), tx);
    let lookups = a
        .pending
        .values()
        .filter(|p| matches!(p.purpose, Purpose::Lookup(_)))
        .count();
    assert!(lookups >= 6, "both sent: {lookups}");
    tick(&mut a, &s, 1).await;
    tick(&mut a, &s, 1).await;
    assert_eq!(a.stats.snapshot().soft_timeouts, 0);
    assert!(a.soft_tids.is_empty());
}

/// Off, nothing is marked or kept: the soft set stays empty (a default
/// set, which allocates nothing).
#[tokio::test(start_paused = true)]
async fn off_allocates_nothing() {
    for lookup_end in [false, true] {
        let (mut a, s) = actor(false, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let _rx = request(&mut a, &s, &sem);
        for _ in 0..30 {
            tick(&mut a, &s, 1).await;
        }
        assert!(a.soft_tids.is_empty());
        // Never allocated. (Under the simulator's `testing` feature the set
        // is a B-tree, which has no capacity to read.)
        #[cfg(not(feature = "testing"))]
        assert_eq!(a.soft_tids.capacity(), 0);
        assert_eq!(detached(&a), 0);
        assert_eq!(a.stats.snapshot().soft_timeouts, 0);
    }
}

/// How a scripted node behaves in [`Script`].
#[derive(Debug, Clone, Copy)]
enum Behaviour {
    /// Answers after this many 100 ms steps (0–9: within the soft timeout).
    Fast(u8),
    /// Answers after 15–39 steps: past the soft timeout, before the query
    /// timeout.
    Late(u8),
    Silent,
}

#[derive(Debug, Clone)]
struct Script {
    /// Node `i` is `at(0x10 + i, i, 100 + i)`.
    behaviour: Vec<Behaviour>,
    /// Node `i`'s answer lists these nodes (indices).
    knows: Vec<Vec<usize>>,
    /// Nodes in the routing table at the start.
    start: Vec<usize>,
}

const NODES: usize = 80;

fn node(i: usize) -> NodeInfo {
    at(0x10 + i as u8, i as u8, 100 + i as u16)
}

fn script() -> impl Strategy<Value = Script> {
    let behaviour = prop_oneof![
        3 => (0u8..10).prop_map(Behaviour::Fast),
        2 => (15u8..40).prop_map(Behaviour::Late),
        1 => Just(Behaviour::Silent),
    ];
    (
        prop::collection::vec(behaviour, NODES),
        prop::collection::vec(prop::collection::vec(0..NODES, 0..5), NODES),
        prop::collection::vec(0..NODES, 3..8),
    )
        .prop_map(|(behaviour, knows, start)| Script {
            behaviour,
            knows,
            start,
        })
}

/// How a scripted run ended: the step, why, the leg's answers among its
/// K closest live candidates and its slow queries out then, and every
/// query the request sent, as (step, address), in order.
struct Played {
    step: u32,
    why: LedgerEnd,
    answered: usize,
    slow: usize,
    sends: Vec<(u32, SocketAddr)>,
}

/// Runs `script` with the switches given, in 100 ms steps with a
/// maintenance tick every second, until the request answers, checking the
/// bookkeeping at every step. The request runs two family legs (the second
/// on a second IPv4 socket, starting from the same nodes), so the lanes
/// they share outlive the first leg to end.
async fn play(script: &Script, soft: bool, lookup_end: bool) -> Played {
    let (mut a, mut s) = actor(soft, lookup_end);
    let second = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    second.set_nonblocking(true).unwrap();
    s.push(SendSocket::Udp(second));
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], false),
        ipv6: false,
    });
    let sem = Arc::new(Semaphore::new(1));
    let start: Vec<_> = script.start.iter().map(|i| node(*i)).collect();
    table(&mut a, &start);
    for n in &start {
        a.families[1].table.insert(*n, Contact::Response, 0);
    }
    let mut rx = request(&mut a, &s, &sem);
    let mut sends = Vec::new();
    // tid -> the step it's due, for every query seen.
    let mut due: Map<u64, Option<u32>> = Map::default();
    let mut record = |a: &Actor, step: u32, sends: &mut Vec<(u32, SocketAddr)>| {
        let mut new: Vec<(u64, SocketAddr)> = out(a)
            .into_iter()
            .filter(|(t, _, _)| !due.contains_key(t))
            .map(|(t, to, _)| (t, to))
            .collect();
        new.sort_unstable_by_key(|(_, to)| *to);
        for (tid, to) in new {
            let i = usize::from(to.port() - 100);
            let wait = match script.behaviour[i] {
                Behaviour::Fast(w) | Behaviour::Late(w) => Some(step + u32::from(w)),
                Behaviour::Silent => None,
            };
            due.insert(tid, wait);
            sends.push((step, to));
        }
        due.clone()
    };
    let mut known = record(&a, 0, &mut sends);
    for step in 1..=400u32 {
        tokio::time::advance(Duration::from_millis(100)).await;
        let mut now: Vec<u64> = known
            .iter()
            .filter(|(t, at)| at.is_some_and(|at| at <= step) && a.pending.contains_key(t))
            .map(|(t, _)| *t)
            .collect();
        now.sort_unstable();
        for tid in now {
            // An earlier reply this step may have ended the request.
            let Some(p) = a.pending.get(&tid) else {
                continue;
            };
            let i = usize::from(p.to.port() - 100);
            let knows: Vec<_> = script.knows[i].iter().map(|k| node(*k)).collect();
            let (to, id) = (p.to, p.id.unwrap());
            reply_as(&mut a, &s, tid, to, id, &knows);
            check(&a);
            known = record(&a, step, &mut sends);
        }
        if step % 10 == 0 {
            a.maintain(&s);
            check(&a);
            // No timer is extended: after a tick, nothing is still out
            // past the query timeout, soft or detached included.
            let now = Instant::now();
            assert!(
                a.pending
                    .values()
                    .all(|p| now.duration_since(p.sent) < a.cfg.query_timeout),
                "a query outlived its timeout"
            );
            known = record(&a, step, &mut sends);
        }
        if rx.try_recv().is_ok() {
            let ended = a.stats.snapshot().ledger.ended;
            let why = LedgerEnd::ALL
                .into_iter()
                .find(|w| ended[*w as usize] == 1)
                .unwrap();
            assert_eq!(ended.iter().sum::<u64>(), 1);
            assert_eq!(returns(&a), 1);
            let (answered, slow) = *a.legs_ended.last().expect("a leg ended");
            assert!(a.legs_ended.len() >= 2, "both legs ended");
            return Played {
                step,
                why,
                answered,
                slow,
                sends,
            };
        }
    }
    panic!("the request outran its deadline");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// The bounds, on a scripted network of fast, late and silent nodes,
    /// with `dht.lookup_end` off and on, for a two-family request: a leg
    /// never has more than `ALPHA` queries out, a request never sends more
    /// than its 64, and its in-flight count is exactly its legs' queries
    /// out, also after one leg ends with soft queries out, all at every
    /// step ([`check`]); no query's timer is extended (checked after every
    /// tick in [`play`]); and a leg that ends with slow queries out has its
    /// K answers. The switch-off run holds the same bounds.
    #[test]
    fn soft_expiry_keeps_every_bound(script in script(), lookup_end in any::<bool>()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let on = rt.block_on(play(&script, true, lookup_end));
        // A leg that ended by its own end test (the request's last leg
        // converging) had K answers if slow queries were out; a deadline,
        // the cap, enough values or a cancel cuts legs whatever they hold.
        let own_end = matches!(on.why, LedgerEnd::Values | LedgerEnd::NoValues);
        prop_assert!(!own_end || on.slow == 0 || on.answered >= K, "ended with {} slow and {} answers ({:?})", on.slow, on.answered, on.why);
        prop_assert!(on.sends.len() <= MAX_PEER_QUERIES, "{} sent", on.sends.len());
        let off = rt.block_on(play(&script, false, lookup_end));
        prop_assert!(off.sends.len() <= MAX_PEER_QUERIES);
        // The run reached its end by a recorded reason.
        prop_assert!(on.step > 0 && LedgerEnd::ALL.contains(&on.why));
    }
}

/// Hostile replies to soft queries: spoofed (another address), to an
/// unknown tid, repeated, or to a query already detached, never settle
/// anything twice, end a request, or grow the actor's state.
#[derive(Debug, Clone)]
enum Hostile {
    /// The matched reply to the query out at this index, listing nodes.
    Reply(usize, u8),
    /// The same from another address.
    Spoofed(usize),
    /// Again, after it was answered.
    Repeat,
    /// A tick, `n` seconds on.
    Tick(u8),
}

fn hostile() -> impl Strategy<Value = Hostile> {
    prop_oneof![
        (0usize..8, 0u8..8).prop_map(|(i, n)| Hostile::Reply(i, n)),
        (0usize..8).prop_map(Hostile::Spoofed),
        Just(Hostile::Repeat),
        (1u8..3).prop_map(Hostile::Tick),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn hostile_replies_to_soft_queries_settle_once_and_stay_bounded(
        steps in prop::collection::vec(hostile(), 1..60),
        lookup_end in any::<bool>(),
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut a, s) = actor(true, lookup_end);
            let sem = Arc::new(Semaphore::new(1));
            // One network, so its answers alone can't converge a leg early
            // under `dht.lookup_end`.
            let nodes: Vec<_> = (0..12).map(|i| at(0x80 + i, 0, 100 + u16::from(i))).collect();
            table(&mut a, &nodes);
            let mut rx = request(&mut a, &s, &sem);
            let mut last: Option<(u64, SocketAddr, NodeId)> = None;
            let mut fresh = 0u16;
            for step in steps {
                let mut all: Vec<_> = a
                    .pending
                    .iter()
                    .filter(|(_, p)| matches!(p.purpose, Purpose::Lookup(_) | Purpose::Detached))
                    .map(|(t, p)| (*t, p.to, p.id.unwrap()))
                    .collect();
                all.sort_unstable();
                let answered = a.stats.snapshot().answered;
                match step {
                    Hostile::Reply(i, n) if !all.is_empty() => {
                        let (tid, to, id) = all[i % all.len()];
                        // Closer ids in the same network, never seen before.
                        let listed: Vec<_> = (0..u16::from(n))
                            .map(|k| at(0x7F - (fresh + k) as u8 % 0x60, 0, 400 + fresh + k))
                            .collect();
                        fresh += u16::from(n);
                        reply_as(&mut a, &s, tid, to, id, &listed);
                        last = Some((tid, to, id));
                    }
                    Hostile::Spoofed(i) if !all.is_empty() => {
                        let (tid, _, id) = all[i % all.len()];
                        reply_as(&mut a, &s, tid, SocketAddr::from(([127, 0, 9, 1], 99)), id, &[]);
                        prop_assert_eq!(a.stats.snapshot().answered, answered, "spoof settles nothing");
                    }
                    Hostile::Repeat => {
                        if let Some((tid, to, id)) = last {
                            reply_as(&mut a, &s, tid, to, id, &[]);
                            prop_assert_eq!(a.stats.snapshot().answered, answered, "repeat settles nothing");
                        }
                    }
                    Hostile::Tick(n) => tick(&mut a, &s, u64::from(n)).await,
                    _ => {}
                }
                check(&a);
                prop_assert!(a.soft_tids.len() <= a.pending.len());
                let l = a.stats.snapshot().ledger;
                prop_assert!(l.ended.iter().sum::<u64>() <= 1);
                prop_assert!(returns(&a) <= 1);
                if rx.try_recv().is_ok() {
                    prop_assert_eq!(returns(&a), 1);
                }
            }
            Ok(())
        })?;
    }
}

/// A two-family request: when one leg ends with a soft query out, that
/// query leaves the request's shared in-flight count as it's detached, so
/// the other leg gets all three lanes. The second family is a second IPv4
/// socket, as in the ledger's two-family test.
#[tokio::test(start_paused = true)]
async fn a_detached_query_frees_its_request_lane_for_the_other_leg() {
    let (mut a, mut s) = actor(true, false);
    let second = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    second.set_nonblocking(true).unwrap();
    s.push(SendSocket::Udp(second));
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], false),
        ipv6: false,
    });
    // Family 1: five silent nodes, all far from the hash.
    for i in 0..5u8 {
        a.families[1].table.insert(
            at(0xD0 + i, 0xD0 + i, 800 + u16::from(i)),
            Contact::Response,
            0,
        );
    }
    let sem = Arc::new(Semaphore::new(1));
    let close = spread(1, 8);
    let _rx = far_request(&mut a, &s, &sem, &close);
    assert_eq!(a.lookups.len(), 2, "two legs");
    // Leg 0: every close node answers but the closest.
    answer_all_but(&mut a, &s, &close[..1]);
    tick(&mut a, &s, 1).await;
    check(&a);
    assert_eq!(detached(&a), 1, "leg 0 ended with its soft query out");
    assert_eq!(a.lookups.len(), 1, "leg 1 still runs");
    tick(&mut a, &s, 1).await;
    check(&a);
    let leg1 = a
        .pending
        .values()
        .filter(|p| matches!(p.purpose, Purpose::Lookup(_)) && p.family == 1)
        .count();
    assert_eq!(leg1, ALPHA, "the other leg has all three lanes");
}
