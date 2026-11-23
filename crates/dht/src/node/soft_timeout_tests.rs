//! CR-SOFT-TIMEOUT (`dht.soft_timeout`) against the real actor, on tokio's
//! paused clock: an owned peer lookup's query left unanswered past the
//! soft timeout frees its lane and stops holding the leg's end, while it
//! stays out, against the outstanding cap, until it's answered (once) or
//! times out. Off, the same scripts wait as before. Every case runs with
//! `dht.lookup_end` off and on.

use super::*;
use crate::node::lookup::ALPHA;
use crate::node::{Intensity, LedgerEnd, PeerResult, PermitReturn};
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
        // Sampling suppressed, so `maintain` sends no samples to the
        // scripted nodes.
        sample_start_delay: None,
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
    a.on_response(s, 0, &msg, Some(&response), from);
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

fn ended(a: &Actor, why: LedgerEnd) -> u64 {
    a.stats.snapshot().ledger.ended[why as usize]
}

/// The bookkeeping every step must keep: a request's in-flight count is
/// its legs' queries out that haven't gone soft; a leg holds at most
/// `ALPHA` soft queries and `2 · ALPHA` in all; every soft tid is pending;
/// the outstanding cap and the 64-query cap hold.
fn check(a: &Actor) {
    for (owner, r) in &a.peer_requests {
        let legs: Vec<u32> = a
            .lookups
            .iter()
            .filter(|(_, l)| l.owner == Some(*owner))
            .map(|(lid, _)| *lid)
            .collect();
        let hard = a
            .pending
            .iter()
            .filter(|(t, p)| {
                matches!(p.purpose, Purpose::Lookup(lid) if legs.contains(&lid))
                    && !a.soft_tids.contains(t)
            })
            .count();
        assert_eq!(r.inflight, hard, "request in flight = hard queries out");
        assert!(r.sent <= MAX_PEER_QUERIES, "64-query cap");
    }
    for (lid, l) in &a.lookups {
        assert!(l.slow() <= ALPHA, "at most ALPHA slow");
        let leg = a
            .pending
            .values()
            .filter(|p| matches!(p.purpose, Purpose::Lookup(id) if id == *lid))
            .count();
        assert!(leg <= 2 * ALPHA, "a leg has {leg} out");
    }
    assert!(a.soft_tids.iter().all(|t| a.pending.contains_key(t)));
    // A soft query names a running lookup, or is detached.
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

/// One silent node in the closest set: on, it stops holding the leg after
/// the soft timeout (1–2 s) and the request ends on its closest set of K
/// answers, with
/// the silent query still out (detached, against the outstanding cap).
/// Its late answer settles once; a repeat is dropped. Off, the leg waits
/// for the query timeout.
#[tokio::test(start_paused = true)]
async fn a_silent_node_stops_holding_the_leg() {
    for (soft, lookup_end) in COMBOS {
        let (mut a, s) = actor(soft, lookup_end);
        let sem = Arc::new(Semaphore::new(4));
        // The table keeps 8 of them (one bucket); nodes[1] lists the 9th.
        let nodes = spread(1, 9);
        table(&mut a, &nodes[..8]);
        let mut rx = request(&mut a, &s, &sem);
        assert_eq!(out(&a).len(), 3, "ALPHA unchanged");
        // nodes[0] stays silent; the other 8 answer at once.
        answer(&mut a, &s, &nodes[1], &nodes[8..]);
        answer_all_but(&mut a, &s, &nodes[..1]);
        check(&a);
        assert!(
            rx.try_recv().is_err(),
            "nodes[0] is the closest, unanswered"
        );
        tick(&mut a, &s, 1).await;
        check(&a);
        if soft {
            // nodes[0] slow, the 9th node joins the closest set; it answers.
            assert!(rx.try_recv().is_err());
            answer_all_but(&mut a, &s, &nodes[..1]);
            assert_eq!(rx.try_recv(), Ok(vec![]), "{lookup_end}");
            assert_eq!(ended(&a, LedgerEnd::NoValues), 1);
            assert_eq!(returns(&a), 1);
            assert_eq!(sem.available_permits(), 4);
            assert!(a.lookups.values().all(|l| l.owner.is_none()));
            assert!(a.peer_requests.is_empty());
            assert_eq!(a.active_get_peers, 0);
            // Still out, against the cap, naming no lookup.
            assert_eq!(detached(&a), 1);
            assert!(out(&a).is_empty());
            assert_eq!(a.stats.snapshot().soft_timeouts, 1);
            // The late answer: counted, settles once.
            let tid = tid_to(&a, &nodes[0]);
            reply_as(&mut a, &s, tid, nodes[0].addr, nodes[0].id, &[]);
            let st = a.stats.snapshot();
            assert_eq!((st.soft_timeout_late, st.answered), (1, 9));
            assert!(detached(&a) == 0 && a.soft_tids.is_empty());
            assert!(!a.pending_addrs.contains_key(&nodes[0].addr));
            reply_as(&mut a, &s, tid, nodes[0].addr, nodes[0].id, &[]);
            let st = a.stats.snapshot();
            assert_eq!(
                (st.soft_timeout_late, st.answered),
                (1, 9),
                "repeat dropped"
            );
            assert_eq!(returns(&a), 1, "the permit came back once");
        } else {
            assert!(rx.try_recv().is_err(), "off: still waiting at 1 s");
            tick(&mut a, &s, 1).await;
            tick(&mut a, &s, 1).await;
            assert!(rx.try_recv().is_err(), "off: still waiting at 3 s");
            tick(&mut a, &s, 1).await;
            // nodes[0] failed at the query timeout; the 9th takes its place.
            answer_all_but(&mut a, &s, &nodes[..1]);
            assert_eq!(rx.try_recv(), Ok(vec![]), "the query timeout ends it");
            assert_eq!(a.stats.snapshot().soft_timeouts, 0);
            assert!(a.soft_tids.is_empty());
            assert!(out(&a).is_empty() && detached(&a) == 0);
        }
    }
}

/// Silent nodes free their lanes for the next candidates, but a leg holds
/// at most ALPHA soft queries: the next ones keep their lanes until the
/// query timeout. A leg never has more than 2 · ALPHA queries out, and no
/// node is asked twice.
#[tokio::test(start_paused = true)]
async fn freed_lanes_ask_the_next_candidates_within_the_bound() {
    for (soft, lookup_end) in COMBOS {
        let (mut a, s) = actor(soft, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        let nodes = spread(1, 12);
        table(&mut a, &nodes);
        let _rx = request(&mut a, &s, &sem);
        assert_eq!(out(&a).len(), 3);
        tick(&mut a, &s, 1).await;
        check(&a);
        let lid = *a.lookups.keys().next().unwrap();
        if soft {
            assert_eq!(a.lookups[&lid].slow(), 3);
            assert_eq!(out(&a).len(), 6, "three more asked");
            // A second second: the three new ones can't go soft too.
            tick(&mut a, &s, 1).await;
            check(&a);
            assert_eq!(out(&a).len(), 6);
            assert_eq!(a.stats.snapshot().soft_timeouts, 3);
        } else {
            assert_eq!(out(&a).len(), 3, "off: lanes held");
        }
        let asked: Vec<SocketAddr> = out(&a).iter().map(|(_, to, _)| *to).collect();
        let mut unique = asked.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(asked.len(), unique.len(), "nobody asked twice");
    }
}

/// A late answer while the leg still runs settles the slow candidate once
/// and its nodes join the lookup, which asks them.
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
        assert_eq!(out(&a).len(), 4, "the fourth took a freed lane; two free");
        // The first slow node answers late, with closer nodes.
        let closer = spread(1, 2);
        answer(&mut a, &s, &nodes[0], &closer);
        check(&a);
        assert_eq!(a.lookups[&lid].slow(), 2, "settled once");
        assert_eq!(a.stats.snapshot().soft_timeout_late, 1);
        let asked: Vec<SocketAddr> = out(&a).iter().map(|(_, to, _)| *to).collect();
        assert!(closer.iter().all(|c| asked.contains(&c.addr)));
    }
}

/// A slow query that reaches the query timeout fails then, once: the
/// leg's slow count and the request's in-flight count stay right.
#[tokio::test(start_paused = true)]
async fn a_slow_query_fails_once_at_the_query_timeout() {
    for lookup_end in [false, true] {
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        let nodes = spread(1, 12);
        table(&mut a, &nodes);
        let _rx = request(&mut a, &s, &sem);
        let timeouts = a.stats.snapshot().timeouts;
        for _ in 0..4 {
            tick(&mut a, &s, 1).await;
            check(&a);
        }
        // The first three timed out at 4 s, and in the same tick the next
        // three went soft.
        let lid = *a.lookups.keys().next().unwrap();
        assert_eq!(a.stats.snapshot().timeouts - timeouts, 3);
        assert_eq!(a.lookups[&lid].slow(), 3);
        assert!(a.soft_tids.len() == 3);
    }
}

/// Every end gives the permit back exactly once with the switch on, and
/// detaches the soft queries rather than dropping them; they leave the
/// outstanding count only by answering or timing out.
#[tokio::test(start_paused = true)]
async fn each_permit_returns_once_on_every_end() {
    for lookup_end in [false, true] {
        // Every node silent: the candidates run out (no values) by the
        // deadline at the latest.
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

/// A query cap: a hostile network whose every answer lists closer ids
/// that never answer. The request spends at most 64 queries and ends by
/// its deadline, whatever the switches.
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
            // Whoever is out answers with 3 new closer silent ids, then
            // those stay silent.
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

/// The query cap with soft queries out: half the nodes answer at once,
/// each listing two closer ones, and half stay silent and go soft. The
/// request spends exactly its 64 queries, ends on the cap, and its soft
/// queries are detached, not dropped.
#[tokio::test(start_paused = true)]
async fn the_query_cap_ends_a_request_with_soft_queries_detached() {
    let (mut a, s) = actor(true, false);
    let sem = Arc::new(Semaphore::new(1));
    // Id `0xA0 - k` (closer as k grows), own /24 and port; even k is silent.
    let fake = |k: u16| at(0xA0 - k as u8, k as u8, 300 + k);
    table(&mut a, &(0..4).map(fake).collect::<Vec<_>>());
    let mut next = 4u16;
    let mut rx = request(&mut a, &s, &sem);
    let mut step = 0;
    while rx.try_recv().is_err() {
        for (tid, to, id) in out(&a) {
            let k = to.port() - 300;
            if k % 2 == 1 && !a.soft_tids.contains(&tid) && next < 0x9E {
                reply_as(&mut a, &s, tid, to, id, &[fake(next), fake(next + 1)]);
                next += 2;
                check(&a);
            }
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
    assert!((1..=ALPHA).contains(&detached(&a)), "{}", detached(&a));
    assert_eq!(returns(&a), 1);
}

/// A freed lane still waits for the query rate and the outstanding cap:
/// with no tokens left, or the cap full, nothing more goes out.
#[tokio::test(start_paused = true)]
async fn freed_lanes_wait_for_the_rate_and_the_outstanding_cap() {
    for lookup_end in [false, true] {
        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let _rx = request(&mut a, &s, &sem);
        a.query_rate.tokens = 0.0;
        a.query_rate.rate = 0.0;
        let sent = a.stats.snapshot().queries_out;
        tick(&mut a, &s, 1).await;
        check(&a);
        assert_eq!(
            a.stats.snapshot().queries_out,
            sent,
            "over the rate: nothing"
        );
        assert_eq!(out(&a).len(), 3);

        let (mut a, s) = actor(true, lookup_end);
        let sem = Arc::new(Semaphore::new(1));
        table(&mut a, &spread(1, 12));
        let _rx = request(&mut a, &s, &sem);
        let cap = a.cfg.intensity.max_outstanding();
        for i in 0..(cap - a.pending.len()) as u64 {
            a.pending.insert(
                u64::MAX - i,
                Pending {
                    to: SocketAddr::from(([127, 0, 200, 1], 1)),
                    id: None,
                    sent: Instant::now() + Duration::from_secs(60),
                    family: 0,
                    purpose: Purpose::Ping,
                    hinted: false,
                },
            );
        }
        tick(&mut a, &s, 1).await;
        assert_eq!(a.pending.len(), cap, "the outstanding cap holds");
        assert_eq!(out(&a).len(), 3, "the soft queries still count");
    }
}

/// A detached query's late answer touches no lookup, even when its old
/// lookup id has been reused by a new request asking the same node.
#[tokio::test(start_paused = true)]
async fn a_detached_answer_never_reaches_a_reused_lookup() {
    let (mut a, s) = actor(true, false);
    let sem = Arc::new(Semaphore::new(2));
    let nodes = spread(1, 9);
    table(&mut a, &nodes[..8]);
    let mut rx = request(&mut a, &s, &sem);
    let old = *a.lookups.keys().next().unwrap();
    answer(&mut a, &s, &nodes[1], &nodes[8..]);
    answer_all_but(&mut a, &s, &nodes[..1]);
    tick(&mut a, &s, 1).await;
    answer_all_but(&mut a, &s, &nodes[..1]);
    assert!(rx.try_recv().is_ok());
    let late = tid_to(&a, &nodes[0]);
    // The next request gets the old id and asks nodes[0] again.
    a.next_lookup = old;
    let _rx2 = request(&mut a, &s, &sem);
    assert!(a.lookups.contains_key(&old), "id reused");
    let owner = a.lookups[&old].owner.unwrap();
    let before = (a.peer_requests[&owner].inflight, out(&a));
    reply_as(
        &mut a,
        &s,
        late,
        nodes[0].addr,
        nodes[0].id,
        &spread(0x80, 3),
    );
    check(&a);
    assert_eq!((a.peer_requests[&owner].inflight, out(&a)), before);
    assert_eq!(a.stats.snapshot().soft_timeout_late, 1);
}

/// A leg whose only candidates left have gone slow doesn't end on them:
/// with fewer than K answers it waits for them to settle, so their late
/// answers still feed it (here, with closer nodes it then asks).
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
        // One answers late, listing two closer nodes, which are asked.
        let closer = spread(1, 2);
        answer(&mut a, &s, &nodes[0], &closer);
        check(&a);
        let asked: Vec<SocketAddr> = out(&a).iter().map(|(_, to, _)| *to).collect();
        assert!(closer.iter().all(|c| asked.contains(&c.addr)));
        assert!(rx.try_recv().is_err());
    }
}

/// CR-LOOKUP-END's values end with soft queries out: the request ends on
/// enough values, its soft queries are detached, its other query is
/// dropped, and the permit comes back once.
#[tokio::test(start_paused = true)]
async fn enough_values_end_detaches_soft_queries() {
    let (mut a, s) = actor(true, true);
    let sem = Arc::new(Semaphore::new(1));
    table(&mut a, &spread(1, 12));
    let mut rx = request(&mut a, &s, &sem);
    tick(&mut a, &s, 1).await;
    assert_eq!(a.soft_tids.len(), 3);
    let fresh: Vec<_> = out(&a)
        .into_iter()
        .filter(|(t, _, _)| !a.soft_tids.contains(t))
        .collect();
    assert_eq!(fresh.len(), 3);
    let values = |net: u8| -> Vec<SocketAddr> {
        (0..4)
            .map(|i| SocketAddr::from(([10, net, 0, 1], 6000 + i)))
            .collect()
    };
    for (k, (tid, to, id)) in fresh[..2].iter().enumerate() {
        reply_values(&mut a, &s, *tid, *to, *id, values(k as u8 + 1));
        check(&a);
    }
    assert_eq!(rx.try_recv().unwrap().len(), 8);
    assert_eq!(ended(&a, LedgerEnd::EnoughValues), 1);
    assert_eq!((returns(&a), sem.available_permits()), (1, 1));
    assert_eq!(detached(&a), 3, "the soft ones stay out");
    assert!(out(&a).is_empty(), "the other one is dropped");
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

/// Runs `script` with the switches given, in 100 ms steps with a
/// maintenance tick every second, until the request answers; late nodes
/// answer late, or at once with `on_time`. Returns the step it ended and
/// why, and checks the bookkeeping at every step.
async fn play(
    script: &Script,
    soft: bool,
    lookup_end: bool,
    on_time: bool,
) -> (u32, LedgerEnd, (usize, usize)) {
    let (mut a, s) = actor(soft, lookup_end);
    let sem = Arc::new(Semaphore::new(1));
    let start: Vec<_> = script.start.iter().map(|i| node(*i)).collect();
    table(&mut a, &start);
    let mut rx = request(&mut a, &s, &sem);
    // tid -> the step it's due.
    let mut due: Map<u64, u32> = Map::default();
    for step in 1..=400u32 {
        tokio::time::advance(Duration::from_millis(100)).await;
        for (tid, to, _) in out(&a).into_iter().chain(
            a.pending
                .iter()
                .filter(|(_, p)| matches!(p.purpose, Purpose::Detached))
                .map(|(t, p)| (*t, p.to, p.id.unwrap()))
                .collect::<Vec<_>>(),
        ) {
            if due.contains_key(&tid) {
                continue;
            }
            let i = usize::from(to.port() - 100);
            let wait = match script.behaviour[i] {
                Behaviour::Fast(w) => Some(u32::from(w)),
                Behaviour::Late(_) if on_time => Some(0),
                Behaviour::Late(w) => Some(u32::from(w)),
                Behaviour::Silent => None,
            };
            if let Some(w) = wait {
                due.insert(tid, step + w);
            }
        }
        let now: Vec<u64> = due
            .iter()
            .filter(|(t, at)| **at <= step && a.pending.contains_key(t))
            .map(|(t, _)| *t)
            .collect();
        let mut now = now;
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
        }
        if step % 10 == 0 {
            a.maintain(&s);
            check(&a);
        }
        if rx.try_recv().is_ok() {
            let ended = a.stats.snapshot().ledger.ended;
            let why = LedgerEnd::ALL
                .into_iter()
                .find(|w| ended[*w as usize] == 1)
                .unwrap();
            assert_eq!(ended.iter().sum::<u64>(), 1);
            assert_eq!(returns(&a), 1);
            let leg = *a.legs_ended.last().expect("a leg ended");
            return (step, why, leg);
        }
    }
    panic!("the request outran its deadline");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// With the switch on, a leg never ends earlier than the same script
    /// with every late node answering on time, except by its closest set
    /// (K answers among its closest live candidates, which slow ones don't
    /// hold) or its values; and the bookkeeping holds at every step, with
    /// `dht.lookup_end` off and on. A leg that ends with slow queries out
    /// always has its K answers.
    #[test]
    fn soft_expiry_never_ends_a_leg_early_for_free(script in script(), lookup_end in any::<bool>()) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let (late, why, (answered, slow)) = rt.block_on(play(&script, true, lookup_end, false));
        let (on_time, _, _) = rt.block_on(play(&script, true, lookup_end, true));
        prop_assert!(slow == 0 || answered >= K, "ended with {slow} slow and {answered} answers");
        let closest_set = matches!(why, LedgerEnd::Values | LedgerEnd::NoValues) && answered >= K;
        // A deadline lands on the first drive after 25 s: a late reply can
        // bring that drive forward by under one tick.
        let deadline = why == LedgerEnd::Deadline && late >= 250;
        prop_assert!(
            late >= on_time || closest_set || why == LedgerEnd::EnoughValues || deadline,
            "ended at step {late} ({why:?}, {answered} answers) against {on_time} on time"
        );
        // The same script with the switch off holds its bookkeeping too.
        rt.block_on(play(&script, false, lookup_end, false));
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

/// [`actor`] at `intensity`, with CR-1c's switch as given and BEP 51
/// sampling on.
fn sampling_actor(intensity: Intensity, preset_slots: bool) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        harvest: crate::node::HarvestConfig {
            sample: true,
            passive: false,
        },
        intensity,
        preset_slots,
        soft_timeout: true,
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
        StdRng::seed_from_u64(13),
    );
    (a, vec![SendSocket::Udp(socket)])
}

/// Node `n` (a smaller `n` is closer to the all-zero hash), alone on its
/// loopback address, on a port nothing listens on: it never answers.
fn silent(n: u32) -> NodeInfo {
    let mut id = [0u8; 20];
    id[..4].copy_from_slice(&n.to_be_bytes());
    id[19] = 1;
    let b = n.to_be_bytes();
    NodeInfo {
        id: NodeId(id),
        addr: SocketAddr::from(([127, b[1], b[2], b[3].max(1)], 999)),
    }
}

fn hash(i: usize) -> InfoHash {
    let mut h = [0u8; 20];
    h[..8].copy_from_slice(&(i as u64).to_be_bytes());
    InfoHash(h)
}

/// Every slot full of requests whose candidates never answer, the pool
/// asking again for each slot that comes free: soft queries free lanes
/// only while lookups' queries out stay within 3/4 of the outstanding
/// cap, so BEP 51 sampling still sends and a maintenance lookup still gets
/// its queries out (runner, 2026-11-23). Without the 3/4 check, lookups'
/// soft and detached queries crowd sampling out at Low.
async fn sampling_and_maintenance_keep_room(intensity: Intensity, preset_slots: bool) {
    let (mut a, s) = sampling_actor(intensity, preset_slots);
    for n in 1..=4096 {
        a.families[0].table.insert(silent(n), Contact::Response, 0);
    }
    let budget = intensity.max_outstanding() * 3 / 4;
    let mut next = 0;
    let mut held = Vec::new();
    let mut refill = |a: &mut Actor, held: &mut Vec<PeerResult>| loop {
        let (tx, mut rx) = oneshot::channel();
        a.on_command(Command::RequestPeers(hash(next), tx, None));
        match rx.try_recv().expect("an answer at once") {
            Some(r) => {
                held.push(r);
                next += 1;
            }
            None => break,
        }
    };
    let lookups_out = |a: &Actor| {
        a.pending
            .values()
            .filter(|p| matches!(p.purpose, Purpose::Lookup(_) | Purpose::Detached))
            .count()
    };
    refill(&mut a, &mut held);
    let step = Duration::from_millis(100);
    // Warm up past the first soft expiries, timeouts and request ends.
    let mut peak = 0;
    let mut peak_ours = 0;
    for _ in 0..150 {
        tokio::time::advance(step).await;
        a.maintain(&s);
        refill(&mut a, &mut held);
        // Owned requests' queries and detached ones; the node's own
        // maintenance lookups send apart from soft expiry, into the room
        // the rule keeps.
        let ours = a
            .pending
            .values()
            .filter(|p| owned(&a, p) || matches!(p.purpose, Purpose::Detached))
            .count();
        peak_ours = peak_ours.max(ours);
        peak = peak.max(lookups_out(&a));
        assert!(a.pending.len() <= intensity.max_outstanding());
    }
    assert!(a.stats.snapshot().soft_timeouts > 0, "queries went soft");
    // Soft expiry stops at the line: requests' and detached queries never
    // pass it, and with maintenance lookups too, lookups stay under the cap.
    assert!(peak_ours <= budget, "requests' and detached: {peak_ours}");
    assert!(peak < intensity.max_outstanding(), "lookups: {peak}");
    // Sampling still sends, to fresh nodes (the table's cool down).
    for n in 100_000..100_400 {
        a.sampler.offer(silent(n), Instant::now());
    }
    let sampled = a.stats.snapshot().samples_sent;
    let deadline = Instant::now() + Duration::from_secs(30);
    while a.stats.snapshot().samples_sent < sampled + 20 {
        assert!(
            Instant::now() < deadline,
            "sampling starved: lookups hold {} of {}",
            lookups_out(&a),
            a.pending.len()
        );
        tokio::time::advance(step).await;
        a.maintain(&s);
        refill(&mut a, &mut held);
    }
    // A maintenance lookup (a refresh `find_node`) gets its queries out,
    // within the query rate it shares.
    let refresh = |a: &Actor| a.stats.snapshot().ledger.queries[QueryPurpose::Refresh as usize];
    let before = refresh(&a);
    // The table's nodes have all failed by now: it asks fresh ones.
    let fresh: Vec<_> = (200_000..200_008).map(silent).collect();
    let lid = a.insert_lookup(0, NodeId([0x55; 20]), &fresh, None, false, None);
    a.drive_lookup(&s, lid);
    let deadline = Instant::now() + Duration::from_secs(10);
    while refresh(&a) < before + ALPHA as u64 {
        assert!(
            Instant::now() < deadline,
            "maintenance starved: {} sent, {} of {} out",
            refresh(&a) - before,
            a.pending.len(),
            intensity.max_outstanding()
        );
        tokio::time::advance(step).await;
        a.maintain(&s);
        refill(&mut a, &mut held);
    }
}

#[tokio::test(start_paused = true)]
async fn at_low_with_preset_slots_sampling_and_maintenance_keep_room() {
    sampling_and_maintenance_keep_room(Intensity::Low, true).await;
}

#[tokio::test(start_paused = true)]
async fn at_balanced_without_preset_slots_sampling_and_maintenance_keep_room() {
    sampling_and_maintenance_keep_room(Intensity::Balanced, false).await;
}

/// The 3/4 rule exactly: at Low with CR-1c on, every slot's three queries
/// out and lookups' detached queries bringing lookups within 10 of 3/4 of
/// the outstanding cap, one tick marks only 10 queries soft. Lookups stay
/// within 3/4 of the cap, and sampling still sends in the same tick.
/// Without the rule all 96 would go soft, lookups would take the whole cap
/// and sampling (which stops at 4/5) would send nothing.
#[tokio::test(start_paused = true)]
async fn soft_expiry_stops_at_three_quarters_of_the_cap() {
    let (mut a, s) = sampling_actor(Intensity::Low, true);
    for n in 1..=4096 {
        a.families[0].table.insert(silent(n), Contact::Response, 0);
    }
    let slots = Intensity::Low.peer_slots();
    let mut held = Vec::new();
    // Tokens for every slot's three, so all are out before the tick.
    a.query_rate.burst = 1_000.0;
    a.query_rate.tokens = 1_000.0;
    for i in 0..slots {
        let (tx, rx) = oneshot::channel();
        a.start_peer_request(&s, hash(i), Some(tx.into()), None);
        held.push(rx);
    }
    let lookups_out = |a: &Actor| {
        a.pending
            .values()
            .filter(|p| matches!(p.purpose, Purpose::Lookup(_) | Purpose::Detached))
            .count()
    };
    assert_eq!(lookups_out(&a), slots * ALPHA);
    let budget = Intensity::Low.max_outstanding() * 3 / 4;
    // Earlier legs' detached queries, sent just now, so they stay out.
    tokio::time::advance(Duration::from_secs(1)).await;
    let room = 10;
    for i in 0..(budget - room - lookups_out(&a)) as u64 {
        a.pending.insert(
            u64::MAX - i,
            Pending {
                to: SocketAddr::from(([127, 0, 250, 1], 1)),
                id: Some(NodeId([0xEE; 20])),
                sent: Instant::now(),
                family: 0,
                purpose: Purpose::Detached,
                hinted: false,
            },
        );
    }
    for n in 300_000..300_100 {
        a.sampler.offer(silent(n), Instant::now());
    }
    let sampled = a.stats.snapshot().samples_sent;
    a.maintain(&s);
    check(&a);
    assert_eq!(a.stats.snapshot().soft_timeouts, room as u64);
    assert!(lookups_out(&a) <= budget, "{} > {budget}", lookups_out(&a));
    assert!(
        a.stats.snapshot().samples_sent > sampled,
        "sampling still sends"
    );
}

/// The 3/4 rule when hard timeouts and soft expiries share a tick: half
/// the slots' queries reach the query timeout while the other half's are
/// due to go soft, with detached queries holding lookups at the line. The
/// timed-out legs refill their lanes first, so nothing goes soft and
/// lookups stay within 3/4 of the cap after every tick.
#[tokio::test(start_paused = true)]
async fn the_three_quarters_rule_counts_lanes_refilled_after_timeouts() {
    let (mut a, s) = sampling_actor(Intensity::Low, true);
    for n in 1..=4096 {
        a.families[0].table.insert(silent(n), Contact::Response, 0);
    }
    a.query_rate.burst = 10_000.0;
    a.query_rate.tokens = 10_000.0;
    // Owned requests' queries and detached ones: the node's own
    // maintenance lookups send apart from soft expiry (the rule counts
    // them too, so this is within it).
    let lookups_out = |a: &Actor| {
        a.pending
            .values()
            .filter(|p| owned(a, p) || matches!(p.purpose, Purpose::Detached))
            .count()
    };
    let budget = Intensity::Low.max_outstanding() * 3 / 4;
    let half = Intensity::Low.peer_slots() / 2;
    let mut held = Vec::new();
    let mut start = |a: &mut Actor, from: usize| {
        for i in from..from + half {
            let (tx, rx) = oneshot::channel();
            a.start_peer_request(&s, hash(i), Some(tx.into()), None);
            held.push(rx);
        }
    };
    start(&mut a, 0);
    tokio::time::advance(Duration::from_millis(500)).await;
    // Earlier legs' detached queries, out until 4.5 s.
    let fill = budget - 2 * half * ALPHA;
    for i in 0..fill as u64 {
        a.pending.insert(
            u64::MAX - i,
            Pending {
                to: SocketAddr::from(([127, 0, 250, 1], 1)),
                id: Some(NodeId([0xEE; 20])),
                sent: Instant::now(),
                family: 0,
                purpose: Purpose::Detached,
                hinted: false,
            },
        );
    }
    tokio::time::advance(Duration::from_millis(500)).await;
    start(&mut a, half);
    assert_eq!(lookups_out(&a), budget);
    // At 4 s the first half times out while the second half is due; at
    // 5 s the detached ones have gone and there's room again.
    for _ in 0..4 {
        tick(&mut a, &s, 1).await;
        check(&a);
        assert!(lookups_out(&a) <= budget, "{} > {budget}", lookups_out(&a));
    }
}
