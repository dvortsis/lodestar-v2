//! CR-LEDGER: the lookup ledger against the real actor, on tokio's paused
//! clock. Each end reason counts exactly once per request on its path,
//! late and duplicate replies count nothing, queries by purpose sum to
//! the queries sent, and residence is the sum of request lifetimes.

use super::tests::{UNBOUND, deliver_reply, fixture, unbound};
use super::*;
use crate::krpc::Samples;
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
