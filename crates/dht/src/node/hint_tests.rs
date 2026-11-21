//! CR-1a-H's prepared regression cases (docs/plan/2026-10-27-streaming-and-hints.md,
//! "Prepared regression cases"), on tokio's paused clock against the real
//! actor. Every responder is on 127.0.0.1, so they share one canonical IP.

use super::*;
use crate::krpc::Samples;
use crate::node::LedgerEnd;
use proptest::prelude::*;
use rand::SeedableRng;

fn actor(hints: bool) -> (Actor, Vec<SendSocket>, mpsc::Receiver<Harvest>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        responder_hints: hints,
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

fn hash(i: u8) -> InfoHash {
    InfoHash([i; 20])
}

fn at(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// A `sample_infohashes` reply from `from` listing `hashes`.
fn sample_reply(a: &mut Actor, sockets: &[SendSocket], from: SocketAddr, hashes: &[InfoHash]) {
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
    let raw: Vec<u8> = hashes.iter().flat_map(|h| h.0).collect();
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

fn request(
    a: &mut Actor,
    sockets: &[SendSocket],
    h: InfoHash,
) -> oneshot::Receiver<Vec<SocketAddr>> {
    let (tx, rx) = oneshot::channel();
    a.start_peer_request(sockets, h, Some(tx.into()), None);
    rx
}

fn hinted(a: &Actor) -> Vec<(u64, SocketAddr)> {
    let mut v: Vec<_> = a
        .pending
        .iter()
        .filter(|(_, p)| p.hinted)
        .map(|(t, p)| (*t, p.to))
        .collect();
    v.sort_unstable();
    v
}

fn fallbacks(a: &Actor, why: HintFallback) -> u64 {
    a.stats.hint_fallbacks[why as usize].load(Ordering::Relaxed)
}

fn get(c: &std::sync::atomic::AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
}

/// Answers a hinted query with `values`.
fn answer_hint(a: &mut Actor, sockets: &[SendSocket], tid: u64, values: Vec<SocketAddr>) {
    let to = a.pending[&tid].to;
    let bytes = tid.to_be_bytes();
    let response = Response {
        id: NodeId([7; 20]),
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
    a.on_response(sockets, 0, &msg, Some(&response), to);
}

#[tokio::test(start_paused = true)]
async fn switch_off_allocates_no_hint_state_and_sends_as_today() {
    let (mut a, sockets, mut rx) = actor(false);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    assert_eq!(rx.try_recv().unwrap().info_hash, hash(1));
    let _reply = request(&mut a, &sockets, hash(1));
    assert!(a.hints.is_none());
    assert!(hinted(&a).is_empty());
    // Ordinary routing only: the responder is asked as a routing-table
    // candidate, under its id, as before this slice.
    assert!(a.pending.values().all(|p| p.id.is_some()));
    assert!(a.lookups.values().all(|l| l.extra_in_flight == 0));
    let s = a.stats.snapshot();
    assert_eq!(
        s.hints_captured + s.hint_queries + s.hint_useful + s.hint_guard_skips,
        0
    );
    assert!(
        s.hint_fallbacks
            .iter()
            .chain(&s.hint_capture_rejected)
            .all(|n| *n == 0)
    );
    a.maintain(&sockets);
    assert_eq!(get(&a.stats.hints_held), 0);
}

#[tokio::test(start_paused = true)]
async fn hint_expiry_is_age_below_the_ttl_and_falls_back_at_once() {
    let (mut a, sockets, _rx) = actor(true);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    sample_reply(&mut a, &sockets, at(1002), &[hash(2)]);
    sample_reply(&mut a, &sockets, at(1003), &[hash(3)]);
    assert_eq!(get(&a.stats.hints_captured), 3);
    // Only a just-fresh hint is sent.
    tokio::time::advance(hints::HINT_TTL - Duration::from_millis(1)).await;
    let _one = request(&mut a, &sockets, hash(1));
    assert_eq!(get(&a.stats.hint_queries), 1);
    // Exactly at the TTL it's expired: no hinted send, ordinary routing at
    // once (the guard's own restriction never comes into it).
    tokio::time::advance(Duration::from_millis(1)).await;
    let mut two = request(&mut a, &sockets, hash(2));
    assert_eq!(fallbacks(&a, HintFallback::Expired), 1);
    assert_eq!(hinted(&a).len(), 1);
    // The responder is restricted, so ordinary routing has nobody left and
    // the lookup answers now, not at a deadline.
    assert_eq!(two.try_recv(), Ok(vec![]));
    tokio::time::advance(Duration::from_secs(1)).await;
    let mut three = request(&mut a, &sockets, hash(3));
    assert_eq!(fallbacks(&a, HintFallback::Expired), 2);
    assert_eq!(three.try_recv(), Ok(vec![]));
    assert_eq!(get(&a.stats.hint_queries), 1);
}

#[tokio::test(start_paused = true)]
async fn one_hinted_query_in_flight_per_ip() {
    let (mut a, sockets, _rx) = actor(true);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1), hash(2)]);
    let _one = request(&mut a, &sockets, hash(1));
    let _two = request(&mut a, &sockets, hash(2));
    assert_eq!(get(&a.stats.hint_queries), 1);
    assert_eq!(fallbacks(&a, HintFallback::Throttled), 1);
    assert_eq!(hinted(&a).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn ip_rotation_shares_one_restriction_with_no_fallback_bypass() {
    let (mut a, sockets, _rx) = actor(true);
    let mapped = SocketAddr::new(
        IpAddr::V6(std::net::Ipv4Addr::LOCALHOST.to_ipv6_mapped()),
        1003,
    );
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    sample_reply(&mut a, &sockets, at(1002), &[hash(2)]);
    sample_reply(&mut a, &sockets, mapped, &[hash(3)]);
    assert_eq!(get(&a.stats.hints_captured), 3);
    let _one = request(&mut a, &sockets, hash(1));
    let (tid, to) = hinted(&a)[0];
    assert_eq!(to, at(1001));
    // Another port while it's in flight.
    let _two = request(&mut a, &sockets, hash(2));
    assert_eq!(fallbacks(&a, HintFallback::Throttled), 1);
    answer_hint(&mut a, &sockets, tid, vec![]);
    assert!(hinted(&a).is_empty());
    // Answered, but inside the not-before interval: the mapped form is the
    // same IP.
    tokio::time::advance(Duration::from_secs(1)).await;
    let _three = request(&mut a, &sockets, hash(3));
    assert_eq!(fallbacks(&a, HintFallback::Throttled), 2);
    // Ordinary routing to that IP on yet another port waits out the
    // restriction too, so the fallback can't bypass it.
    let skips = get(&a.stats.hint_guard_skips);
    let now = a.now_secs();
    a.families[0]
        .table
        .insert(candidate_at(at(1009)), Contact::Response, now);
    let _four = request(&mut a, &sockets, hash(4));
    assert!(get(&a.stats.hint_guard_skips) > skips);
    assert!(!a.pending_addrs.contains_key(&at(1009)));
    // After the interval it's free again, for hints and routing alike.
    tokio::time::advance(hints::HINT_NOT_BEFORE).await;
    a.maintain(&sockets);
    assert_eq!(get(&a.stats.hint_guard_entries), 0);
    let _six = request(&mut a, &sockets, hash(6));
    let asked_for = |a: &Actor, h: InfoHash| {
        a.pending.values().any(|p| {
            p.to == at(1009)
                && matches!(p.purpose, Purpose::Lookup(lid)
                    if a.lookups.get(&lid).is_some_and(|l| l.info_hash == Some(h)))
        })
    };
    assert!(asked_for(&a, hash(6)));
    sample_reply(&mut a, &sockets, at(1005), &[hash(5)]);
    let _five = request(&mut a, &sockets, hash(5));
    assert_eq!(get(&a.stats.hint_queries), 2);
    // The new hinted send restricts the IP again, before routing runs.
    assert!(!asked_for(&a, hash(5)));
}

fn candidate_at(addr: SocketAddr) -> NodeInfo {
    NodeInfo {
        id: NodeId([42; 20]),
        addr,
    }
}

#[tokio::test(start_paused = true)]
async fn guard_saturation_evicts_nothing_and_ordinary_work_continues() {
    let (mut a, sockets, _rx) = actor(true);
    let now = Instant::now();
    let guard = a.hints.as_mut().unwrap();
    for i in 0..hints::GUARD_CAPACITY as u32 {
        let ip = IpAddr::V4(std::net::Ipv4Addr::from(0x1f00_0000 + i));
        guard.sent(ip, now);
        guard.settled(ip);
    }
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    let _one = request(&mut a, &sockets, hash(1));
    assert_eq!(fallbacks(&a, HintFallback::GuardFull), 1);
    assert!(hinted(&a).is_empty());
    let guard = a.hints.as_ref().unwrap();
    assert_eq!(guard.guard_len(), hints::GUARD_CAPACITY);
    // The unrestricted responder is still asked by ordinary routing.
    assert!(a.pending_addrs.contains_key(&at(1001)));
    assert_eq!(get(&a.stats.hint_guard_skips), 0);
    // Expiry frees it all.
    tokio::time::advance(hints::HINT_NOT_BEFORE).await;
    a.maintain(&sockets);
    assert_eq!(get(&a.stats.hint_guard_entries), 0);
}

/// The spec's "Private key" case, at the actor. The guarantee that a
/// private key's lookup never reaches the public DHT is the pool's
/// (`crates/sources/src/pool.rs`, `!e.private` in `request_peers`, tested
/// by `private_keys_bypass_admission_and_survive_a_pause`); and the very
/// harvest that captures a hint is a public sighting, which makes the key
/// public there. What the actor owes is shown here: an unrequested hint is
/// never sent and expires, and the harvest carries nothing hint-derived.
#[tokio::test(start_paused = true)]
async fn private_key_work_gets_no_public_dispatch_or_provenance() {
    let (mut a, sockets, mut rx) = actor(true);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    // The hash reaches the pool exactly as without hints.
    assert_eq!(
        rx.try_recv().unwrap(),
        Harvest {
            info_hash: hash(1),
            source: Source::Sample,
            peers: vec![],
            hint: false,
        }
    );
    // A private key's lookup is never asked of the public DHT (the pool
    // keeps it), so the hint is never sent and simply expires.
    for _ in 0..3 {
        a.maintain(&sockets);
        tokio::time::advance(hints::HINT_TTL / 2).await;
    }
    a.maintain(&sockets);
    assert_eq!(get(&a.stats.hint_queries), 0);
    assert_eq!(get(&a.stats.hints_held), 0);
    assert_eq!(get(&a.stats.hints_expired), 1);
    assert!(hinted(&a).is_empty());
    // A rediscovery isn't new, so it doesn't bring the hint back.
    sample_reply(&mut a, &sockets, at(1002), &[hash(1)]);
    assert_eq!(get(&a.stats.hints_captured), 1);
    assert_eq!(a.hints.as_ref().unwrap().held(), 0);
    // Nor does a private responder get kept.
    a.cfg.allow_private = false;
    sample_reply(&mut a, &sockets, at(1003), &[hash(2)]);
    assert_eq!(
        get(&a.stats.hint_capture_rejected[HintReject::Private as usize]),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn a_silent_responder_falls_back_inside_the_deadline() {
    let (mut a, sockets, _rx) = actor(true);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    let admitted = Instant::now();
    let mut one = request(&mut a, &sockets, hash(1));
    assert_eq!(hinted(&a).len(), 1);
    let started = a.peer_requests.values().next().unwrap().started;
    assert_eq!(started, admitted, "the hint doesn't move the deadline");
    // No answer: the query times out and the lookup ends with ordinary
    // routing's result, well inside the 25 s deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    while one.try_recv().is_err() {
        assert!(Instant::now() < deadline, "no answer by {deadline:?}");
        tokio::time::advance(a.cfg.tick).await;
        a.maintain(&sockets);
    }
    assert!(admitted.elapsed() < LOOKUP_DEADLINE);
    assert_eq!(get(&a.stats.timeouts), 1);
    assert_eq!(get(&a.stats.lookup_deadlines), 0);
    assert!(a.peer_requests.is_empty() && a.lookups.is_empty());
    assert!(hinted(&a).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_hinted_reply_is_routing_help_never_a_peer() {
    let (mut a, sockets, _rx) = actor(true);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    let mut one = request(&mut a, &sockets, hash(1));
    let (tid, _) = hinted(&a)[0];
    let lid = *a.lookups.keys().next().unwrap();
    assert_eq!(a.lookups[&lid].extra_in_flight, 1);
    assert_eq!(a.peer_requests.values().next().unwrap().inflight, 1);
    answer_hint(&mut a, &sockets, tid, vec![at(7000)]);
    assert_eq!(get(&a.stats.hint_useful), 1);
    // Only its values come back; the responder itself is no peer.
    assert_eq!(one.try_recv(), Ok(vec![at(7000)]));
    assert!(a.peer_requests.is_empty());
    // An empty answer isn't useful.
    tokio::time::advance(hints::HINT_NOT_BEFORE).await;
    sample_reply(&mut a, &sockets, at(1002), &[hash(2)]);
    let _two = request(&mut a, &sockets, hash(2));
    let (tid, _) = hinted(&a)[0];
    answer_hint(&mut a, &sockets, tid, vec![]);
    assert_eq!(get(&a.stats.hint_useful), 1);
}

#[tokio::test(start_paused = true)]
async fn a_hinted_reply_streams_its_peers_to_a_session() {
    use crate::node::stream::{LookupEnd, LookupSession};
    let (mut a, sockets, _rx) = actor(true);
    sample_reply(&mut a, &sockets, at(1001), &[hash(1)]);
    let (tx, mut admitted) = oneshot::channel::<Option<LookupSession>>();
    a.on_command(Command::RequestSession(hash(1), Box::new(tx), None));
    let (h, reply, permit) = a.get_peers_queue.pop_front().unwrap();
    a.start_peer_request(&sockets, h, reply, permit);
    let mut s = admitted.try_recv().unwrap().unwrap();
    let (tid, _) = hinted(&a)[0];
    answer_hint(&mut a, &sockets, tid, vec![at(7000)]);
    // The responder's values arrive as a batch; the responder is no peer.
    assert_eq!(s.batches.try_recv().unwrap(), vec![at(7000)]);
    assert_eq!(s.terminal.try_recv(), Ok(LookupEnd::Complete));
    assert!(a.peer_requests.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_hinted_send_falls_back_and_counts_send_failed() {
    let (mut a, mut sockets, _rx) = actor(true);
    // An IPv6 family whose socket is an IPv4 stand-in: a send to an IPv6
    // hint passes every check, then fails at the socket on every host.
    let stand_in = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    stand_in.set_nonblocking(true).unwrap();
    sockets.push(SendSocket::Udp(stand_in));
    a.families.push(Family {
        table: RoutingTable::new(a.ids[0], true),
        ipv6: true,
    });
    let v6: SocketAddr = "[2a00::1]:6881".parse().unwrap();
    let now = Instant::now();
    assert_eq!(
        a.hints.as_mut().unwrap().capture(hash(1), v6, now),
        Ok(true)
    );
    let mut reply = request(&mut a, &sockets, hash(1));
    assert_eq!(fallbacks(&a, HintFallback::SendFailed), 1);
    assert_eq!(get(&a.stats.send_errors), 1);
    // No hinted query went out: none counted, none pending, and the IP
    // isn't restricted, since only a sent query sets the guard.
    assert_eq!(get(&a.stats.hint_queries), 0);
    assert!(hinted(&a).is_empty());
    assert!(!a.hints.as_ref().unwrap().restricts(v6.ip(), now));
    // Ordinary routing drove both legs at once; with nobody to ask, the
    // request answers now rather than at its deadline.
    assert_eq!(reply.try_recv(), Ok(vec![]));
    let others: u64 = HintFallback::ALL
        .iter()
        .filter(|w| **w != HintFallback::SendFailed)
        .map(|w| fallbacks(&a, *w))
        .sum();
    assert_eq!(others, 0);
}

// CR-HINT-FIRST (`dht.hint_first`, docs/decisions/2026-11-21-CR-HINT-FIRST.md).

fn actor_with(
    responder_hints: bool,
    hint_first: bool,
    lookup_end: bool,
) -> (Actor, Vec<SendSocket>, mpsc::Receiver<Harvest>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        responder_hints,
        hint_first,
        lookup_end,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel(256);
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

/// A responder at its own loopback IP, `127.0.0.<host>`.
fn host(n: u8, port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, n], port))
}

fn harvests(rx: &mut mpsc::Receiver<Harvest>) -> Vec<(InfoHash, bool)> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .map(|h| (h.info_hash, h.hint))
        .collect()
}

fn skipped(a: &Actor, why: HintFirstSkip) -> u64 {
    get(&a.stats.hint_first_skipped[why as usize])
}

/// With `dht.hint_first` off, harvests carry no mark whatever
/// `responder_hints` says, and with both off no hint state exists.
#[tokio::test(start_paused = true)]
async fn hint_first_off_marks_nothing() {
    for responder_hints in [false, true] {
        let (mut a, sockets, mut rx) = actor_with(responder_hints, false, false);
        sample_reply(&mut a, &sockets, host(1, 1001), &[hash(1), hash(2)]);
        assert_eq!(harvests(&mut rx), [(hash(1), false), (hash(2), false)]);
        assert_eq!(a.hints.is_some(), responder_hints);
        assert_eq!(get(&a.stats.hint_first_marked), 0);
        assert!(a.stats.hint_first_skipped.iter().all(|n| get(n) == 0));
    }
}

/// One hash per responder: a reply marks only its first new hash; the
/// same IP (any port, or its mapped form) marks nothing more while that
/// hint is outstanding; another IP marks its own. With `responder_hints`
/// off, only the marked hash's hint is kept.
#[tokio::test(start_paused = true)]
async fn one_marked_hash_per_responder() {
    let (mut a, sockets, mut rx) = actor_with(false, true, false);
    sample_reply(
        &mut a,
        &sockets,
        host(1, 1001),
        &[hash(1), hash(2), hash(3)],
    );
    assert_eq!(
        harvests(&mut rx),
        [(hash(1), true), (hash(2), false), (hash(3), false)]
    );
    assert_eq!(a.hints.as_ref().unwrap().held(), 1, "only the T1 hint");
    let mapped = SocketAddr::new(
        IpAddr::V6(std::net::Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped()),
        1003,
    );
    sample_reply(&mut a, &sockets, host(1, 1002), &[hash(4)]);
    sample_reply(&mut a, &sockets, mapped, &[hash(5)]);
    sample_reply(&mut a, &sockets, host(2, 1001), &[hash(6)]);
    assert_eq!(
        harvests(&mut rx),
        [(hash(4), false), (hash(5), false), (hash(6), true)]
    );
    assert_eq!(skipped(&a, HintFirstSkip::Responder), 2);
    assert_eq!(get(&a.stats.hint_first_marked), 2);
    // A seen hash is never a candidate, and counts nothing.
    sample_reply(&mut a, &sockets, host(3, 1001), &[hash(1)]);
    assert!(harvests(&mut rx).is_empty());
    assert_eq!(skipped(&a, HintFirstSkip::Responder), 2);
    // The marked hash's request takes its hint: the mark ends, and the
    // guard then holds the IP, so its next reply still marks nothing.
    let _r = request(&mut a, &sockets, hash(1));
    assert_eq!(get(&a.stats.hint_queries), 1);
    sample_reply(&mut a, &sockets, host(1, 1004), &[hash(7)]);
    assert_eq!(harvests(&mut rx), [(hash(7), false)]);
    // After the TTL and the guard's interval, it marks again.
    tokio::time::advance(hints::HINT_TTL.max(hints::HINT_NOT_BEFORE)).await;
    a.maintain(&sockets);
    let tid = hinted(&a).first().map(|(t, _)| *t);
    if let Some(tid) = tid {
        answer_hint(&mut a, &sockets, tid, vec![]);
    }
    sample_reply(&mut a, &sockets, host(1, 1005), &[hash(8)]);
    assert_eq!(harvests(&mut rx), [(hash(8), true)]);
}

/// With both switches on, CR-1a-H keeps every new hash's hint as before,
/// and the reply's first is also marked.
#[tokio::test(start_paused = true)]
async fn with_responder_hints_every_hint_is_kept_and_one_marked() {
    let (mut a, sockets, mut rx) = actor_with(true, true, false);
    sample_reply(
        &mut a,
        &sockets,
        host(1, 1001),
        &[hash(1), hash(2), hash(3)],
    );
    assert_eq!(
        harvests(&mut rx),
        [(hash(1), true), (hash(2), false), (hash(3), false)]
    );
    assert_eq!(a.hints.as_ref().unwrap().held(), 3);
    assert_eq!(get(&a.stats.hints_captured), 3);
    assert_eq!(get(&a.stats.hint_first_marked), 1);
}

/// A responder failing the public-endpoint checks marks nothing; with the
/// crawler paused nothing is harvested or decided.
#[tokio::test(start_paused = true)]
async fn rejected_or_paused_responders_mark_nothing() {
    let (mut a, sockets, mut rx) = actor_with(false, true, false);
    a.cfg.allow_private = false;
    sample_reply(&mut a, &sockets, host(1, 1001), &[hash(1)]);
    assert_eq!(harvests(&mut rx), [(hash(1), false)]);
    assert_eq!(skipped(&a, HintFirstSkip::Rejected), 1);
    assert_eq!(a.hints.as_ref().unwrap().held(), 0);
    a.cfg.allow_private = true;
    a.paused = true;
    sample_reply(&mut a, &sockets, host(2, 1001), &[hash(2)]);
    assert!(harvests(&mut rx).is_empty());
    assert_eq!(skipped(&a, HintFirstSkip::Rejected), 1);
}

fn table_nodes(a: &mut Actor, n: u8) {
    for i in 1..=n {
        a.families[0].table.insert(
            NodeInfo {
                id: NodeId([0x10 * i; 20]),
                addr: SocketAddr::from(([127, 0, i, 9], 900 + u16::from(i))),
            },
            Contact::Response,
            0,
        );
    }
}

fn values(net: u8, n: u16) -> Vec<SocketAddr> {
    (0..n)
        .map(|i| SocketAddr::from(([10, net, (i >> 8) as u8, i as u8], 6881)))
        .collect()
}

/// The runner's call: a hinted reply's values count toward CR-LOOKUP-END
/// exactly like any reply's, so one responder never ends a request,
/// however many values it sends; the request carries on routing.
#[tokio::test(start_paused = true)]
async fn a_hinted_reply_alone_never_ends_a_request() {
    let (mut a, sockets, _rx) = actor_with(false, true, true);
    table_nodes(&mut a, 3);
    sample_reply(&mut a, &sockets, host(1, 1001), &[hash(1)]);
    let mut rx = request(&mut a, &sockets, hash(1));
    let [(tid, to)] = hinted(&a)[..] else {
        panic!("one hinted query: {:?}", hinted(&a));
    };
    assert_eq!(to, host(1, 1001));
    answer_hint(&mut a, &sockets, tid, values(1, 64));
    assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty));
    let l = a.stats.snapshot().ledger;
    assert_eq!(l.ended[LedgerEnd::EnoughValues as usize], 0);
    assert_eq!(get(&a.stats.hint_useful), 1);
    assert!(!a.pending.is_empty(), "ordinary routing carries on");
}

#[derive(Debug, Clone)]
enum Hostile {
    /// The hinted responder answers with these values (empty, huge,
    /// duplicate, private or port-0 junk).
    Answer(Vec<(u32, u16)>),
    /// The same reply from another address in its /24: spoofed.
    Spoofed(u8, Vec<(u32, u16)>),
    /// A sample reply from another address in its /24, listing a new hash
    /// (address rotation).
    Rotate(u8, u8),
}

fn hostile() -> impl Strategy<Value = Hostile> {
    let values = prop::collection::vec((any::<u32>(), 0u16..4), 0..120);
    prop_oneof![
        values.clone().prop_map(Hostile::Answer),
        (2u8..250, values).prop_map(|(h, v)| Hostile::Spoofed(h, v)),
        (2u8..250, 10u8..250).prop_map(|(h, k)| Hostile::Rotate(h, k)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Hostile hinted replies, all from one /24, never end a request on
    /// values, never inflate its values past 64 or let a junk endpoint in,
    /// and leave the hint state bounded: at most one mark per IP.
    #[test]
    fn hostile_hinted_replies_end_nothing_early_and_stay_bounded(
        script in prop::collection::vec(hostile(), 1..30),
    ) {
        {
            let (mut a, sockets, mut harvested) = actor_with(false, true, true);
            // Junk values include private and reserved endpoints.
            a.cfg.allow_private = false;
            table_nodes(&mut a, 3);
            // Public addresses in one /24 for the responder and its rotations.
            let r = |h: u8| SocketAddr::from(([31, 0, 0, h], 6881));
            sample_reply(&mut a, &sockets, r(1), &[hash(1)]);
            prop_assert_eq!(get(&a.stats.hint_first_marked), 1);
            let mut per_ip = std::collections::HashMap::from([(1u8, 1u32)]);
            let _rx = request(&mut a, &sockets, hash(1));
            let to_addrs = |v: &[(u32, u16)]| -> Vec<SocketAddr> {
                v.iter().map(|(ip, p)| SocketAddr::from((std::net::Ipv4Addr::from(*ip), *p))).collect()
            };
            for step in script {
                let tid = hinted(&a).first().map(|(t, _)| *t);
                match (step, tid) {
                    (Hostile::Answer(v), Some(tid)) => answer_hint(&mut a, &sockets, tid, to_addrs(&v)),
                    (Hostile::Spoofed(h, v), Some(tid)) => {
                        let bytes = tid.to_be_bytes();
                        let response = Response { id: NodeId([7; 20]), values: to_addrs(&v), ..Default::default() };
                        let msg = Message { transaction_id: &bytes, version: None, ip: None, read_only: false, body: Body::Response(response.clone()) };
                        a.on_response(&sockets, 0, &msg, Some(&response), r(h));
                    }
                    (Hostile::Rotate(h, k), _) => {
                        let before = get(&a.stats.hint_first_marked);
                        sample_reply(&mut a, &sockets, r(h), &[hash(k)]);
                        if get(&a.stats.hint_first_marked) > before {
                            *per_ip.entry(h).or_insert(0) += 1;
                        }
                        // One mark per IP: no time passes here, so none
                        // expires and none may be marked twice.
                        prop_assert!(per_ip.get(&h).copied().unwrap_or(0) <= 1);
                    }
                    _ => {}
                }
                let l = a.stats.snapshot().ledger;
                prop_assert_eq!(l.ended[LedgerEnd::EnoughValues as usize], 0);
                for req in a.peer_requests.values() {
                    prop_assert!(req.peers.len() <= 64);
                    for p in &req.peers {
                        prop_assert!(p.port() != 0 && endpoint_allowed(&a.cfg, *p));
                    }
                }
                let h = a.hints.as_ref().unwrap();
                prop_assert!(h.held() <= 250 && h.t1_len() <= 250);
                prop_assert!(hinted(&a).len() <= 1);
            }
            // Every mark reached the pool as one harvest, one per IP.
            let marked: Vec<_> = std::iter::from_fn(|| harvested.try_recv().ok()).filter(|h| h.hint).collect();
            prop_assert!(marked.len() as u64 == get(&a.stats.hint_first_marked));
            prop_assert_eq!(marked.len() as u32, per_ip.values().sum::<u32>());
        }
    }
}
