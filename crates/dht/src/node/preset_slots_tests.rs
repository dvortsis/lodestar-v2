//! CR-1c (`dht.preset_slots`) against the real actor, on tokio's paused
//! clock: with the switch on, the owned peer requests running or queued
//! follow the start preset's count (more than 32 at Balanced and
//! Aggressive) and never pass it; every permit comes back exactly once on
//! every end; no tick sends past the query rate or the outstanding cap;
//! the 25 s deadline and the 64-query cap still hold. Off, every preset
//! keeps 32 slots, as before.

use super::tests::deliver_reply;
use super::*;
use crate::node::{LedgerEnd, MAX_PEER_SLOTS, PEER_REQUEST_CAPACITY, PeerResult};
use rand::SeedableRng;
use tokio::sync::Semaphore;

const PRESETS: [Intensity; 3] = [Intensity::Low, Intensity::Balanced, Intensity::Aggressive];

fn actor(intensity: Intensity, on: bool, query_timeout: Duration) -> (Actor, Vec<SendSocket>) {
    actor_with(intensity, on, query_timeout, false)
}

/// [`actor`], with BEP 51 sampling on or off.
fn actor_with(
    intensity: Intensity,
    on: bool,
    query_timeout: Duration,
    sample: bool,
) -> (Actor, Vec<SendSocket>) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let addr = socket.local_addr().unwrap();
    let cfg = Config {
        bind: vec![addr],
        bootstrap: vec![],
        allow_private: true,
        sample_start_delay: Some(Duration::ZERO),
        // Only the requests under test send queries, unless asked.
        harvest: crate::node::HarvestConfig {
            sample,
            passive: false,
        },
        intensity,
        preset_slots: on,
        query_timeout,
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
        StdRng::seed_from_u64(11),
    );
    (a, vec![SendSocket::Udp(socket)])
}

/// Node `n`: id `n` big-endian in the first four bytes (so a smaller `n`
/// is closer to the all-zero hash), on its own loopback address, at a
/// port below 1024 that nothing listens on.
fn node(n: u32) -> NodeInfo {
    let mut id = [0u8; 20];
    id[..4].copy_from_slice(&n.to_be_bytes());
    id[19] = 1;
    let b = n.to_be_bytes();
    NodeInfo {
        id: NodeId(id),
        addr: SocketAddr::from(([127, b[1], b[2], b[3].max(1)], 999)),
    }
}

fn table(a: &mut Actor, nodes: impl IntoIterator<Item = NodeInfo>) {
    for n in nodes {
        a.families[0].table.insert(n, Contact::Response, 0);
    }
}

fn hash(i: usize) -> InfoHash {
    let mut h = [0u8; 20];
    h[..8].copy_from_slice(&(i as u64).to_be_bytes());
    InfoHash(h)
}

/// Admits request `i` through the command path; `Some` once admitted.
fn admit(a: &mut Actor, i: usize, permit: Option<PeerPermit>) -> Option<PeerResult> {
    let (tx, mut rx) = oneshot::channel();
    a.on_command(Command::RequestPeers(hash(i), tx, permit));
    rx.try_recv().expect("an answer at once")
}

fn want(intensity: Intensity, on: bool) -> usize {
    if on {
        intensity.peer_slots()
    } else {
        PEER_REQUEST_CAPACITY
    }
}

fn returns(a: &Actor) -> u64 {
    a.stats.snapshot().peer_permit_returns.iter().sum()
}

fn ended(a: &Actor, why: LedgerEnd) -> u64 {
    a.stats.snapshot().ledger.ended[why as usize]
}

#[test]
fn the_preset_counts_are_as_documented() {
    assert_eq!(Intensity::Low.peer_slots(), 32);
    assert_eq!(Intensity::Balanced.peer_slots(), 80);
    assert_eq!(Intensity::Aggressive.peer_slots(), MAX_PEER_SLOTS);
    for i in PRESETS {
        // Never fewer slots than with the switch off, and every slot's
        // ALPHA queries on both address families fit in three quarters
        // of the preset's outstanding cap.
        assert!(i.peer_slots() >= PEER_REQUEST_CAPACITY);
        assert!(i.peer_slots() <= MAX_PEER_SLOTS);
        assert!(2 * i.peer_slots() * crate::node::lookup::ALPHA <= i.max_outstanding() * 3 / 4);
        let off = Config {
            intensity: i,
            ..Default::default()
        };
        assert_eq!(off.peer_slots(), PEER_REQUEST_CAPACITY);
        let on = Config {
            preset_slots: true,
            ..off
        };
        assert_eq!(on.peer_slots(), i.peer_slots());
    }
}

/// On, the slots are the start preset's count: that many requests are
/// admitted and run at once (more than 32 above Low), and the next is
/// refused. A later intensity change moves the rate, not the slots. Off,
/// every preset admits 32, as before.
#[tokio::test(start_paused = true)]
async fn the_slots_follow_the_start_preset_and_no_further() {
    for intensity in PRESETS {
        for on in [false, true] {
            let n = want(intensity, on);
            let (mut a, s) = actor(intensity, on, Duration::from_secs(60));
            table(&mut a, (1..=64).map(node));
            assert_eq!(a.stats.snapshot().ledger.slots_capacity, n as u64);
            let held: Vec<_> = (0..n)
                .map(|i| admit(&mut a, i, None).expect("admitted"))
                .collect();
            assert!(admit(&mut a, n, None).is_none(), "{intensity:?} {on}");
            a.maintain(&s);
            assert_eq!(a.active_get_peers, n, "{intensity:?} {on}: all run");
            assert!(a.get_peers_queue.is_empty());
            if on && intensity != Intensity::Low {
                assert!(a.active_get_peers > PEER_REQUEST_CAPACITY);
            }
            a.maintain(&s);
            assert_eq!(a.stats.snapshot().ledger.slots_occupied, n as u64);
            for other in PRESETS {
                a.on_command(Command::SetIntensity(other));
                assert!(admit(&mut a, n + 1, None).is_none(), "fixed for the run");
                assert_eq!(a.stats.snapshot().ledger.slots_capacity, n as u64);
            }
            drop(held);
        }
    }
}

/// With more than 32 requests running, each permit comes back exactly
/// once, on every end: values, the closest set (no values), cancel, the
/// deadline, and shutdown (running and queued alike).
#[tokio::test(start_paused = true)]
async fn every_permit_returns_once_on_every_end() {
    let slots = Intensity::Balanced.peer_slots();
    let (mut a, s) = actor(Intensity::Balanced, true, Duration::from_secs(60));
    // One node: each request's one query goes to it, and its answer ends
    // the request.
    let only = node(7);
    table(&mut a, [only]);
    let sem = Arc::new(Semaphore::new(slots));
    let mut granted = 0u64;
    let mut permit = || {
        granted += 1;
        Some(sem.clone().try_acquire_owned().unwrap())
    };
    let mut results: Vec<PeerResult> = (0..slots)
        .map(|i| admit(&mut a, i, permit()).expect("admitted"))
        .collect();
    a.maintain(&s);
    assert_eq!(a.active_get_peers, slots);
    assert_eq!(sem.available_permits(), 0);
    let tids: Vec<u64> = a.pending.keys().copied().collect();
    assert_eq!(tids.len(), slots, "one query per request");

    // 20 end on values, 20 on their closest set without any.
    for (k, tid) in tids[..40].iter().enumerate() {
        let peers = if k < 20 {
            vec![SocketAddr::from(([10, 0, 0, 1], 6881))]
        } else {
            vec![]
        };
        deliver_reply(&mut a, &s, *tid, 0, only.addr, only.id, peers);
    }
    assert_eq!(ended(&a, LedgerEnd::Values), 20);
    assert_eq!(ended(&a, LedgerEnd::NoValues), 20);
    assert_eq!(sem.available_permits(), 40);

    // 20 callers of requests still running go.
    results.retain_mut(|r| r.try_recv() == Err(oneshot::error::TryRecvError::Empty));
    assert_eq!(results.len(), slots - 40);
    drop(results.drain(..20));
    a.maintain(&s);
    assert_eq!(ended(&a, LedgerEnd::Cancelled), 20);
    assert_eq!(sem.available_permits(), 60);

    // The rest run out the 25 s deadline on the paused clock.
    tokio::time::advance(LOOKUP_DEADLINE).await;
    a.maintain(&s);
    assert_eq!(ended(&a, LedgerEnd::Deadline), (slots - 60) as u64);
    assert_eq!(sem.available_permits(), slots);
    assert_eq!(a.active_get_peers, 0);

    // Shutdown: a full set, half running and half still queued.
    let _held: Vec<_> = (0..slots)
        .map(|i| admit(&mut a, 1000 + i, permit()).expect("admitted"))
        .collect();
    assert_eq!(sem.available_permits(), 0);
    let queued: Vec<_> = a.get_peers_queue.drain(slots / 2..).collect();
    a.maintain(&s);
    assert_eq!(a.active_get_peers, slots / 2);
    a.get_peers_queue.extend(queued);
    a.release_permits();
    a.end_ledger_at_shutdown();
    assert_eq!(ended(&a, LedgerEnd::Shutdown), (slots / 2) as u64);

    assert_eq!(sem.available_permits(), slots, "every permit is back");
    assert_eq!(returns(&a), granted, "each exactly once");
}

/// At every preset with the switch on, and the slots full of requests
/// that never hear back, no tick sends past the query rate (a full
/// bucket, then the rate), and no request sends more than its 64 queries
/// or outlives its deadline. One family's slots × `ALPHA` stay under the
/// outstanding cap here by design; the live change to Low below is where
/// that cap binds.
#[tokio::test(start_paused = true)]
async fn no_tick_passes_the_rate_or_a_requests_caps() {
    for intensity in PRESETS {
        let slots = intensity.peer_slots();
        // Short timeouts keep every request asking.
        let (mut a, s) = actor(intensity, true, Duration::from_millis(200));
        table(&mut a, (1..=4096).map(node));
        let _held: Vec<_> = (0..slots)
            .map(|i| admit(&mut a, i, None).expect("admitted"))
            .collect();
        let rate = f64::from(intensity.queries_per_sec());
        let start = Instant::now();
        let mut peak = 0;
        for _ in 0..300 {
            a.maintain(&s);
            peak = peak.max(a.active_get_peers);
            assert!(a.active_get_peers <= slots);
            assert!(a.pending.len() <= intensity.max_outstanding());
            let sent = a.stats.snapshot().queries_out as f64;
            let t = start.elapsed().as_secs_f64();
            assert!(
                sent <= rate + rate * t + 1.0,
                "{intensity:?}: {sent} by {t} s"
            );
            for r in a.peer_requests.values() {
                assert!(r.sent <= MAX_PEER_QUERIES);
                assert!(r.started.elapsed() <= LOOKUP_DEADLINE + Duration::from_millis(100));
            }
            tokio::time::advance(Duration::from_millis(100)).await;
        }
        assert_eq!(peak, slots, "{intensity:?}: the slots fill");
        assert_eq!(a.active_get_peers, 0, "{intensity:?}: all ended by 30 s");
    }
}

/// With 79 other requests holding slots, a request whose answers keep
/// naming closer nodes stops at its 64-query budget.
#[tokio::test(start_paused = true)]
async fn the_query_cap_holds_beside_a_full_set() {
    let slots = Intensity::Balanced.peer_slots();
    let (mut a, s) = actor(Intensity::Balanced, true, Duration::from_secs(60));
    let mut next = 0x8000_0000u32;
    table(
        &mut a,
        (0..8).map(|_| {
            next -= 0x0100_0000;
            node(next)
        }),
    );
    let first = admit(&mut a, 0, None).expect("admitted");
    a.maintain(&s);
    let owner = *a.peer_requests.keys().next().expect("running");
    let _others: Vec<_> = (1..slots)
        .map(|i| admit(&mut a, i, None).expect("admitted"))
        .collect();
    a.maintain(&s);
    assert_eq!(a.active_get_peers, slots);
    let mine = |a: &Actor| -> Vec<(u64, SocketAddr, NodeId)> {
        a.pending
            .iter()
            .filter(|(_, p)| match p.purpose {
                Purpose::Lookup(lid) => a.lookups.get(&lid).is_some_and(|l| l.owner == Some(owner)),
                _ => false,
            })
            .map(|(t, p)| (*t, p.to, p.id.unwrap()))
            .collect()
    };
    let mut rounds = 0;
    while a.peer_requests.contains_key(&owner) {
        rounds += 1;
        assert!(rounds < 400, "the request must end");
        let out = mine(&a);
        if out.is_empty() {
            // Throttled: the rate refills, and the tick drives it again.
            tokio::time::advance(Duration::from_millis(100)).await;
            a.maintain(&s);
            continue;
        }
        for (tid, addr, id) in out {
            let closer: Vec<NodeInfo> = (0..3)
                .map(|_| {
                    next -= 1;
                    node(next)
                })
                .collect();
            let raw = crate::compact::encode_nodes(&closer, false);
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
            a.on_response(&s, 0, &msg, Some(&response), addr);
            if let Some(r) = a.peer_requests.get(&owner) {
                assert!(r.sent <= MAX_PEER_QUERIES);
            }
        }
    }
    assert_eq!(ended(&a, LedgerEnd::QueryCap), 1);
    drop(first);
}

/// A live change from Aggressive to Low keeps the 256 slots but not the
/// network: from the change on, no query goes out while Low's outstanding
/// cap is reached, so the queries already in flight only drain, and once
/// they've had their timeout the count stays within Low's cap. The rate
/// falls to Low's at once. An 8 s timeout lets Low's 50 a second alone
/// hold 400 in flight, so it's the outstanding cap that binds.
#[tokio::test(start_paused = true)]
async fn a_live_change_to_low_holds_lows_caps_with_256_slots() {
    let timeout = Duration::from_secs(8);
    let (mut a, s) = actor(Intensity::Aggressive, true, timeout);
    table(&mut a, (1..=4096).map(node));
    let slots = Intensity::Aggressive.peer_slots();
    let _held: Vec<_> = (0..slots)
        .map(|i| admit(&mut a, i, None).expect("admitted"))
        .collect();
    a.maintain(&s);
    assert_eq!(a.active_get_peers, slots);
    let cap = Intensity::Low.max_outstanding();
    assert!(a.pending.len() > cap, "Aggressive filled past Low's cap");

    a.on_command(Command::SetIntensity(Intensity::Low));
    let changed = Instant::now();
    let sent_at_change = a.stats.snapshot().queries_out;
    let mut last = a.pending.len();
    for _ in 0..200 {
        tokio::time::advance(Duration::from_millis(100)).await;
        a.maintain(&s);
        let now = a.pending.len();
        if last >= cap {
            assert!(
                now <= last,
                "no query out while at Low's cap: {last} -> {now}"
            );
        }
        if changed.elapsed() > timeout + Duration::from_millis(100) {
            assert!(now <= cap, "within Low's cap once drained: {now}");
        }
        assert!(a.active_get_peers <= slots);
        let rate = f64::from(Intensity::Low.queries_per_sec());
        let sent = (a.stats.snapshot().queries_out - sent_at_change) as f64;
        assert!(sent <= rate + rate * changed.elapsed().as_secs_f64() + 1.0);
        last = now;
    }
}

/// The state the slots can hold at Aggressive, from each structure's size
/// and its cap: per request, its record, its values (64) and their
/// sources, its per-source vouching (a source per query, 64 queries, 4
/// values each), and per address family a lookup with its 64 candidates
/// (a candidate is a node, its 20-byte distance, a state and an optional
/// box: at most a node plus 40 bytes), its own values, and 3 queries in
/// flight with their address entries; plus its queue entry. Hash-map
/// entries are counted twice over for their table's spare room.
#[test]
fn the_slot_state_at_aggressive_is_bounded() {
    use std::mem::size_of;
    let addr = size_of::<SocketAddr>();
    let values = MAX_VALUES * addr + MAX_VALUES * (size_of::<crate::node::limits::Network>() + 24);
    let sent_by = MAX_PEER_QUERIES
        * (size_of::<crate::node::limits::Network>()
            + 24
            + (ENOUGH_VALUES / VALUE_RESPONDERS) * addr);
    let candidate = size_of::<NodeInfo>() + 40;
    let in_flight = crate::node::lookup::ALPHA
        * 2
        * (size_of::<u64>() + size_of::<Pending>() + size_of::<SocketAddr>() + size_of::<u32>());
    let leg = 2 * (size_of::<u32>() + size_of::<Lookup>()) + 64 * candidate + values + in_flight;
    let request = 2 * (size_of::<u32>() + size_of::<PeerRequest>())
        + values
        + sent_by
        + size_of::<QueuedLookup>();
    let per_slot = request + 2 * leg;
    let total = MAX_PEER_SLOTS * per_slot;
    println!("per slot {per_slot} B; at Aggressive's {MAX_PEER_SLOTS} slots {total} B");
    // Recorded in docs/decisions/2026-11-21-CR-1c.md; a change here
    // updates it.
    assert!(per_slot < 48 * 1024, "{per_slot}");
    assert!(total < 12 * 1024 * 1024, "{total}");
}

/// After a live change from Aggressive to Low, with the pool keeping every
/// slot asked for, the running lookups settle at Low's 32 (the 256 slots
/// stay admitted and queued), and BEP 51 sampling still sends: the
/// lookups don't hold all of Low's rate and outstanding cap.
#[tokio::test(start_paused = true)]
async fn after_a_change_to_low_lookups_settle_and_sampling_goes_on() {
    let (mut a, s) = actor_with(Intensity::Aggressive, true, Duration::from_secs(8), true);
    table(&mut a, (1..=4096).map(node));
    let slots = Intensity::Aggressive.peer_slots();
    let mut next = 0;
    let mut held = Vec::new();
    // The pool's part: ask again for every slot that comes free.
    let mut refill = |a: &mut Actor, held: &mut Vec<PeerResult>| {
        while let Some(r) = admit(a, next, None) {
            held.push(r);
            next += 1;
        }
    };
    refill(&mut a, &mut held);
    a.maintain(&s);
    assert_eq!(a.active_get_peers, slots);

    a.on_command(Command::SetIntensity(Intensity::Low));
    let low = Intensity::Low.peer_slots();
    let step = Duration::from_millis(100);
    // Condition wait: the running lookups settle (each ends by its 25 s
    // deadline), with a 60 s limit on the paused clock.
    let deadline = Instant::now() + Duration::from_secs(60);
    while a.active_get_peers > low {
        assert!(
            Instant::now() < deadline,
            "running {} > {low}",
            a.active_get_peers
        );
        tokio::time::advance(step).await;
        a.maintain(&s);
        refill(&mut a, &mut held);
    }
    assert_eq!(
        a.get_peers_queue.len() + a.active_get_peers,
        slots,
        "the slots stay asked for"
    );
    // Settled, the running count stays within Low's, and sampling sends
    // to fresh nodes (the table's have all timed out and cool down).
    for n in 100_000..100_200 {
        a.sampler.offer(node(n), Instant::now());
    }
    let sampled = a.stats.snapshot().samples_sent;
    let deadline = Instant::now() + Duration::from_secs(30);
    while a.stats.snapshot().samples_sent < sampled + 10 {
        assert!(Instant::now() < deadline, "sampling starved");
        tokio::time::advance(step).await;
        a.maintain(&s);
        refill(&mut a, &mut held);
        assert!(a.active_get_peers <= low);
        assert!(a.pending.len() <= Intensity::Low.max_outstanding());
    }
}
