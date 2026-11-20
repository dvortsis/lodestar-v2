//! CR-LEDGER: the lookup ledger against the real actor, on tokio's paused
//! clock. Each end reason counts exactly once per request on its path,
//! late and duplicate replies count nothing, queries by purpose sum to
//! the queries sent, and residence is the sum of request lifetimes.

use super::tests::{UNBOUND, deliver_reply, fixture, unbound};
use super::*;
use crate::krpc::Samples;
use crate::node::stream::LookupSession;
use crate::node::{HintOutcome, LedgerEnd, LedgerSnapshot, QueryPurpose};
use rand::SeedableRng;

fn ledger(a: &Actor) -> LedgerSnapshot {
    a.stats.snapshot().ledger
}

fn ended(a: &Actor, why: LedgerEnd) -> u64 {
    ledger(a).ended[why as usize]
}

/// A routing-table node at a port nothing listens on.
fn node(a: &mut Actor, b: u8, port: u16) -> NodeInfo {
    let n = NodeInfo {
        id: NodeId([b; 20]),
        addr: unbound(false, port),
    };
    let now = a.now_secs();
    a.families[0].table.insert(n, Contact::Response, now);
    n
}

fn request(a: &mut Actor, sockets: &[SendSocket], h: u8) -> oneshot::Receiver<Vec<SocketAddr>> {
    let (tx, rx) = oneshot::channel();
    a.start_peer_request(sockets, InfoHash([h; 20]), Some(tx.into()), None);
    rx
}

/// The only query in flight, with where it went.
fn only_pending(a: &Actor) -> (u64, SocketAddr, NodeId) {
    assert_eq!(a.pending.len(), 1);
    let (tid, p) = a.pending.iter().next().unwrap();
    (*tid, p.to, p.id.unwrap())
}

/// The partitions every run must keep, whatever happened.
fn assert_identities(a: &Actor) {
    let s = a.stats.snapshot();
    let l = s.ledger;
    let ended: u64 = l.ended.iter().sum();
    assert_eq!(l.queries.iter().sum::<u64>(), s.queries_out, "purposes");
    assert_eq!(l.first_values.count + l.without_values, ended, "values");
    assert_eq!(l.values.count, ended, "values per lookup");
}

#[tokio::test(start_paused = true)]
async fn values_and_closest_set_end_once_and_late_replies_count_nothing() {
    let (mut a, sockets) = fixture();
    node(&mut a, 42, UNBOUND);
    let peer = unbound(false, 60);
    let mut rx = request(&mut a, &sockets, 1);
    let (tid, to, id) = only_pending(&a);
    tokio::time::advance(Duration::from_millis(1_500)).await;
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![peer]);
    assert_eq!(rx.try_recv(), Ok(vec![peer]));
    let l = ledger(&a);
    assert_eq!(l.ended[LedgerEnd::Values as usize], 1);
    assert_eq!(l.residence_millis[LedgerEnd::Values as usize], 1_500);
    // 1.5 s is in the 2 s bucket, not the 1 s one.
    assert_eq!(l.first_values.cumulative[1], 0);
    assert_eq!(l.first_values.cumulative[2], 1);
    assert_eq!(l.first_values.sum, 1_500);
    assert_eq!((l.values.count, l.values.sum), (1, 1));
    // A duplicate of the same reply after the end matches nothing.
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![peer]);
    assert_eq!(ledger(&a), l);

    let mut rx = request(&mut a, &sockets, 2);
    let (tid, to, id) = only_pending(&a);
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![]);
    assert_eq!(rx.try_recv(), Ok(vec![]));
    assert_eq!(ended(&a, LedgerEnd::NoValues), 1);
    assert_eq!(ledger(&a).without_values, 1);
    assert_eq!(
        ledger(&a).values.cumulative[0],
        1,
        "le=0 holds the empty one"
    );
    assert_eq!(
        ledger(&a).queries[QueryPurpose::PeerLookup as usize],
        2,
        "one get_peers each"
    );
    assert_identities(&a);
}

#[tokio::test(start_paused = true)]
async fn deadline_ends_once_and_a_reply_after_it_counts_nothing() {
    let (mut a, sockets) = fixture();
    node(&mut a, 42, UNBOUND);
    let mut rx = request(&mut a, &sockets, 1);
    let (tid, to, id) = only_pending(&a);
    tokio::time::advance(LOOKUP_DEADLINE).await;
    a.scrape_tick(&sockets, Instant::now());
    assert_eq!(rx.try_recv(), Ok(vec![]));
    a.scrape_tick(&sockets, Instant::now());
    let l = ledger(&a);
    assert_eq!(l.ended[LedgerEnd::Deadline as usize], 1);
    assert_eq!(l.ended.iter().sum::<u64>(), 1);
    assert_eq!(l.residence_millis[LedgerEnd::Deadline as usize], 25_000);
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![unbound(false, 60)]);
    assert_eq!(ledger(&a), l, "the late reply found no query");
    assert_identities(&a);
}

#[tokio::test(start_paused = true)]
async fn the_query_budget_ends_a_lookup_that_had_not_converged() {
    let (mut a, sockets) = fixture();
    for b in 1..=5 {
        node(&mut a, b, u16::from(b) + 10);
    }
    let _rx = request(&mut a, &sockets, 1);
    assert_eq!(a.pending.len(), super::super::lookup::ALPHA);
    // The budget is spent; the queries out time out with fresh
    // candidates still unasked.
    for r in a.peer_requests.values_mut() {
        r.sent = MAX_PEER_QUERIES;
    }
    tokio::time::advance(a.cfg.query_timeout).await;
    a.maintain(&sockets);
    assert!(a.peer_requests.is_empty());
    assert_eq!(ended(&a, LedgerEnd::QueryCap), 1);
    assert_eq!(ledger(&a).ended.iter().sum::<u64>(), 1);
    assert_identities(&a);
}

#[tokio::test(start_paused = true)]
async fn cancel_and_shutdown_end_once_each() {
    let (mut a, sockets) = fixture();
    node(&mut a, 42, UNBOUND);
    let rx = request(&mut a, &sockets, 1);
    tokio::time::advance(Duration::from_secs(3)).await;
    drop(rx);
    a.scrape_tick(&sockets, Instant::now());
    a.scrape_tick(&sockets, Instant::now());
    assert_eq!(ended(&a, LedgerEnd::Cancelled), 1);
    assert_eq!(
        ledger(&a).residence_millis[LedgerEnd::Cancelled as usize],
        3_000
    );

    let _rx = request(&mut a, &sockets, 2);
    let (tid, to, id) = only_pending(&a);
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![]);
    assert_eq!(ended(&a, LedgerEnd::NoValues), 1);
    // One still out when the actor stops.
    let _rx = request(&mut a, &sockets, 3);
    tokio::time::advance(Duration::from_secs(5)).await;
    a.release_permits();
    a.end_ledger_at_shutdown();
    let l = ledger(&a);
    assert_eq!(l.ended[LedgerEnd::Shutdown as usize], 1);
    assert_eq!(l.residence_millis[LedgerEnd::Shutdown as usize], 5_000);
    assert_eq!(l.ended.iter().sum::<u64>(), 3);
    assert_identities(&a);
}

/// The identity W rests on: residence is the sum of request lifetimes.
#[tokio::test(start_paused = true)]
async fn residence_is_the_sum_of_request_lifetimes() {
    let (mut a, sockets) = fixture();
    node(&mut a, 42, UNBOUND);
    // 2 s to an answer.
    let _one = request(&mut a, &sockets, 1);
    let (tid, to, id) = only_pending(&a);
    tokio::time::advance(Duration::from_secs(2)).await;
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![]);
    // 7 s to a cancel.
    let two = request(&mut a, &sockets, 2);
    tokio::time::advance(Duration::from_secs(7)).await;
    drop(two);
    a.scrape_tick(&sockets, Instant::now());
    // 25 s to the deadline.
    let _three = request(&mut a, &sockets, 3);
    tokio::time::advance(LOOKUP_DEADLINE).await;
    a.scrape_tick(&sockets, Instant::now());
    let l = ledger(&a);
    assert_eq!(l.ended.iter().sum::<u64>(), 3);
    assert_eq!(l.residence_millis.iter().sum::<u64>(), 34_000);
    let s = a.stats.snapshot();
    assert_eq!(
        l.ended.iter().sum::<u64>(),
        s.lookups_completed + s.peer_cancelled,
        "one end per request, as the existing totals count them"
    );
    assert_identities(&a);
}

#[tokio::test(start_paused = true)]
async fn queries_by_purpose_sum_to_the_queries_sent() {
    let (mut a, sockets) = fixture();
    for b in 1..=4 {
        node(&mut a, b, u16::from(b) + 10);
    }
    let _rx = request(&mut a, &sockets, 1);
    for _ in 0..3 {
        tokio::time::advance(Duration::from_secs(5)).await;
        a.maintain(&sockets);
    }
    let s = a.stats.snapshot();
    assert!(s.queries_out > 0);
    assert!(s.ledger.queries[QueryPurpose::PeerLookup as usize] > 0);
    assert_identities(&a);
}

fn hinted_actor() -> (Actor, Vec<SendSocket>, mpsc::Receiver<Harvest>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        responder_hints: true,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel(64);
    let a = Actor::new(
        cfg,
        Arc::new(Stats::default()),
        vec![addr],
        None,
        vec![],
        Some(tx),
        Instant::now(),
        StdRng::seed_from_u64(3),
    );
    (a, vec![SendSocket::Udp(socket)], rx)
}

/// A `sample_infohashes` reply from `from` listing `hashes`: captures them
/// as hints.
fn sample_reply(a: &mut Actor, sockets: &[SendSocket], from: SocketAddr, hashes: &[u8]) {
    let tid = a.tids.next_id().unwrap();
    a.pending.insert(
        tid,
        Pending {
            to: from,
            id: None,
            sent: Instant::now(),
            family: 0,
            purpose: Purpose::Sample,
            hinted: false,
        },
    );
    *a.pending_addrs.entry(from).or_default() += 1;
    let raw: Vec<u8> = hashes.iter().flat_map(|h| [*h; 20]).collect();
    let response = Response {
        id: NodeId([7; 20]),
        samples: Samples::new(&raw),
        interval: Some(60),
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
    a.on_response(sockets, 0, &msg, Some(&response), from);
}

fn hinted_tid(a: &Actor) -> u64 {
    let tids: Vec<_> = a
        .pending
        .iter()
        .filter(|(_, p)| p.hinted)
        .map(|(t, _)| *t)
        .collect();
    assert_eq!(tids.len(), 1);
    tids[0]
}

fn answer_hint(a: &mut Actor, sockets: &[SendSocket], tid: u64, values: Vec<SocketAddr>) {
    let to = a.pending[&tid].to;
    deliver_reply(a, sockets, tid, 0, to, NodeId([7; 20]), values);
}

fn outcome(a: &Actor, o: HintOutcome) -> u64 {
    ledger(a).hint_outcomes[o as usize]
}

fn at(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[tokio::test(start_paused = true)]
async fn each_hint_outcome_counts_once() {
    let (mut a, sockets, _harvest) = hinted_actor();
    // Every responder shares 127.0.0.1, so they share one guard entry.
    sample_reply(&mut a, &sockets, at(1001), &[1, 2]);
    let _one = request(&mut a, &sockets, 1);
    let tid = hinted_tid(&a);
    let _two = request(&mut a, &sockets, 2);
    assert_eq!(outcome(&a, HintOutcome::GuardSkipped), 1);
    answer_hint(&mut a, &sockets, tid, vec![unbound(false, 60)]);
    answer_hint_again_counts_nothing(&mut a, &sockets, tid);
    assert_eq!(outcome(&a, HintOutcome::Values), 1);

    tokio::time::advance(hints::HINT_NOT_BEFORE).await;
    a.maintain(&sockets);
    sample_reply(&mut a, &sockets, at(1003), &[3]);
    let _three = request(&mut a, &sockets, 3);
    let tid = hinted_tid(&a);
    answer_hint(&mut a, &sockets, tid, vec![]);
    assert_eq!(outcome(&a, HintOutcome::NoValues), 1);

    // Times out.
    tokio::time::advance(hints::HINT_NOT_BEFORE).await;
    a.maintain(&sockets);
    sample_reply(&mut a, &sockets, at(1004), &[4]);
    let _four = request(&mut a, &sockets, 4);
    hinted_tid(&a);
    tokio::time::advance(a.cfg.query_timeout).await;
    a.maintain(&sockets);
    assert_eq!(outcome(&a, HintOutcome::Unanswered), 1);

    // Its request ends (cancelled) before the reply.
    tokio::time::advance(hints::HINT_NOT_BEFORE).await;
    a.maintain(&sockets);
    sample_reply(&mut a, &sockets, at(1005), &[5]);
    let five = request(&mut a, &sockets, 5);
    let tid = hinted_tid(&a);
    drop(five);
    a.scrape_tick(&sockets, Instant::now());
    assert_eq!(outcome(&a, HintOutcome::Unanswered), 2);
    answer_hint_again_counts_nothing(&mut a, &sockets, tid);

    let l = ledger(&a);
    let resolved = l.hint_outcomes[HintOutcome::Values as usize]
        + l.hint_outcomes[HintOutcome::NoValues as usize]
        + l.hint_outcomes[HintOutcome::Unanswered as usize];
    assert_eq!(resolved, a.stats.snapshot().hint_queries);
    assert_eq!(l.queries[QueryPurpose::Hint as usize], resolved);
    assert_identities(&a);
}

/// A reply to a hinted query that has already resolved.
fn answer_hint_again_counts_nothing(a: &mut Actor, sockets: &[SendSocket], tid: u64) {
    let before = ledger(a);
    deliver_reply(a, sockets, tid, 0, at(1001), NodeId([7; 20]), vec![]);
    assert_eq!(ledger(a), before);
}

/// The one path whose code changed: a streaming request's legs now keep
/// their peers for the ledger. Its batches and terminal are as before.
#[tokio::test(start_paused = true)]
async fn a_streaming_request_counts_distinct_values_and_streams_as_before() {
    let (mut a, sockets) = fixture();
    node(&mut a, 1, 901);
    node(&mut a, 2, 902);
    let (tx, mut admitted) = oneshot::channel::<Option<LookupSession>>();
    a.on_command(Command::RequestSession(
        InfoHash([1; 20]),
        Box::new(tx),
        None,
    ));
    let (hash, reply, permit) = a.get_peers_queue.pop_front().expect("admitted");
    a.start_peer_request(&sockets, hash, reply, permit);
    let mut s = admitted.try_recv().unwrap().expect("a session");
    let mut tids: Vec<_> = a.pending.keys().copied().collect();
    tids.sort_unstable();
    assert_eq!(tids.len(), 2);
    let p = |n| unbound(false, n);
    tokio::time::advance(Duration::from_secs(1)).await;
    for (tid, values) in [(tids[0], vec![p(61), p(62)]), (tids[1], vec![p(62), p(63)])] {
        let q = &a.pending[&tid];
        let (to, id) = (q.to, q.id.unwrap());
        deliver_reply(&mut a, &sockets, tid, 0, to, id, values);
    }
    assert_eq!(s.batches.try_recv().unwrap(), vec![p(61), p(62)]);
    assert_eq!(s.batches.try_recv().unwrap(), vec![p(63)]);
    assert!(s.batches.try_recv().is_err());
    assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Complete));
    let l = ledger(&a);
    assert_eq!(l.ended[LedgerEnd::Values as usize], 1);
    assert_eq!((l.values.count, l.values.sum), (1, 3), "distinct values");
    assert_eq!(l.first_values.sum, 1_000);
    assert_identities(&a);
}

/// A harvest-only request (no caller) shares the slots and ends once.
#[tokio::test(start_paused = true)]
async fn a_harvest_only_request_ends_once() {
    let (mut a, sockets) = fixture();
    node(&mut a, 42, UNBOUND);
    a.on_command(Command::GetPeers(InfoHash([1; 20])));
    let (hash, reply, permit) = a.get_peers_queue.pop_front().expect("queued");
    assert!(reply.is_none());
    a.start_peer_request(&sockets, hash, reply, permit);
    let (tid, to, id) = only_pending(&a);
    deliver_reply(&mut a, &sockets, tid, 0, to, id, vec![unbound(false, 60)]);
    assert!(a.peer_requests.is_empty());
    assert_eq!(ended(&a, LedgerEnd::Values), 1);
    assert_eq!(ledger(&a).ended.iter().sum::<u64>(), 1);
    assert_identities(&a);
}

/// One end per request, not per leg: a two-family request counts a peer
/// both legs found once, and a leg's deadline ends the whole request once.
/// The second family is a second IPv4 socket, so it runs on hosts without
/// IPv6; the ledger never looks at a leg's address family.
#[tokio::test(start_paused = true)]
async fn a_two_family_request_ends_once_with_distinct_values() {
    let (mut a, mut sockets) = fixture();
    let second = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    second.set_nonblocking(true).unwrap();
    sockets.push(SendSocket::Udp(second));
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], false),
        ipv6: false,
    });
    for (family, b) in [(0, 42u8), (1, 43)] {
        let n = NodeInfo {
            id: NodeId([b; 20]),
            addr: unbound(false, u16::from(b)),
        };
        a.families[family].table.insert(n, Contact::Response, 0);
    }
    let answer = |a: &mut Actor, tid: u64, values: Vec<SocketAddr>| {
        let q = &a.pending[&tid];
        let (family, to, id) = (q.family, q.to, q.id.unwrap());
        deliver_reply(a, &sockets, tid, family, to, id, values);
    };
    let peer = unbound(false, 60);
    // Both legs find the same peer.
    let mut rx = request(&mut a, &sockets, 1);
    let tids: Vec<_> = a.pending.keys().copied().collect();
    assert_eq!(tids.len(), 2);
    answer(&mut a, tids[0], vec![peer]);
    assert_eq!(ledger(&a).ended.iter().sum::<u64>(), 0, "one leg is out");
    answer(&mut a, tids[1], vec![peer]);
    assert_eq!(rx.try_recv(), Ok(vec![peer]));
    let l = ledger(&a);
    assert_eq!(l.ended[LedgerEnd::Values as usize], 1);
    assert_eq!((l.values.count, l.values.sum), (1, 1));

    // One leg answers, the other runs into the deadline.
    let mut rx = request(&mut a, &sockets, 2);
    let tids: Vec<_> = a.pending.keys().copied().collect();
    answer(&mut a, tids[0], vec![peer]);
    tokio::time::advance(LOOKUP_DEADLINE).await;
    a.scrape_tick(&sockets, Instant::now());
    assert_eq!(rx.try_recv(), Ok(vec![peer]));
    a.scrape_tick(&sockets, Instant::now());
    let l = ledger(&a);
    assert_eq!(l.ended[LedgerEnd::Deadline as usize], 1);
    assert_eq!(l.ended.iter().sum::<u64>(), 2);
    assert_eq!(l.residence_millis[LedgerEnd::Deadline as usize], 25_000);
    assert_eq!(l.values.sum, 2, "the deadline request kept its one peer");
    assert_identities(&a);
}

/// Each purpose lands in its own bucket; the order of the match matters
/// (a scrape has a hash too, and a bootstrap lookup no hash).
#[tokio::test(start_paused = true)]
async fn each_query_counts_under_its_own_purpose() {
    let (mut a, sockets) = fixture();
    let n = node(&mut a, 42, UNBOUND);
    let purpose = |a: &Actor, p: QueryPurpose| ledger(a).queries[p as usize];
    a.query(&sockets, 0, n.addr, Some(n.id), Method::Ping, Purpose::Ping);
    assert_eq!(purpose(&a, QueryPurpose::Ping), 1);
    let (tx, _scrape) = oneshot::channel();
    a.start_scrape(&sockets, InfoHash([1; 20]), tx);
    assert_eq!(purpose(&a, QueryPurpose::Scrape), 1);
    a.start_lookup(&sockets, 0, NodeId([9; 20]), &[], &[], None);
    assert_eq!(purpose(&a, QueryPurpose::Refresh), 1);
    let target = a.ids[0];
    a.start_lookup_as(&sockets, 0, target, &[], &[], None, false, None, true);
    assert_eq!(purpose(&a, QueryPurpose::Bootstrap), 1);
    let _rx = request(&mut a, &sockets, 2);
    assert_eq!(purpose(&a, QueryPurpose::PeerLookup), 1);
    // Exploration and sampling, by classification (their sends need the
    // sampler's state).
    let lid = a.insert_lookup(0, NodeId([8; 20]), &[], None, false, None);
    a.lookups.get_mut(&lid).unwrap().explore = true;
    assert_eq!(
        a.query_purpose(Purpose::Lookup(lid), false),
        QueryPurpose::Explore
    );
    assert_eq!(
        a.query_purpose(Purpose::Sample, false),
        QueryPurpose::Sample
    );
    assert_eq!(
        a.query_purpose(Purpose::Lookup(lid), true),
        QueryPurpose::Hint
    );
    assert_eq!(ledger(&a).queries.iter().sum::<u64>(), 5);
    assert_identities(&a);
}

/// `slots_occupied` is running requests only; a queued one is `queued`.
#[tokio::test(start_paused = true)]
async fn occupied_slots_never_count_queued_requests() {
    let (mut a, sockets) = fixture();
    node(&mut a, 42, UNBOUND);
    assert_eq!(ledger(&a).slots_capacity, 32, "set at start");
    let _running = request(&mut a, &sockets, 1);
    let (tx, _queued) = oneshot::channel();
    a.admit_request(InfoHash([2; 20]), Admit::Final(tx), None);
    assert_eq!(a.get_peers_queue.len(), 1);
    a.publish_slots();
    let l = ledger(&a);
    assert_eq!((l.slots_occupied, l.queued), (1, 1));
    // Idle again: the running one cancelled, the queued one's caller gone,
    // both dropped by the actor's own paths.
    drop(_running);
    drop(_queued);
    a.scrape_tick(&sockets, Instant::now());
    a.drop_cancelled_queued();
    assert!(a.get_peers_queue.is_empty());
    a.publish_slots();
    let l = ledger(&a);
    assert_eq!((l.slots_occupied, l.queued), (0, 0));
    // And both read 0 after shutdown, whatever was held.
    let _running = request(&mut a, &sockets, 3);
    a.publish_slots();
    assert_eq!(ledger(&a).slots_occupied, 1);
    a.end_ledger_at_shutdown();
    let l = ledger(&a);
    assert_eq!((l.slots_occupied, l.queued), (0, 0));
}
