//! CR-1a-H's prepared regression cases (docs/plan/2026-10-27-streaming-and-hints.md,
//! "Prepared regression cases"), on tokio's paused clock against the real
//! actor. Every responder is on 127.0.0.1, so they share one canonical IP.

use super::*;
use crate::krpc::Samples;
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
    a.next_tid += 1;
    let tid = a.next_tid;
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
    a.start_peer_request(sockets, h, Some(tx), None);
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
