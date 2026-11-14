//! Pings to queriers have a budget of their own: whatever arrives, they take
//! at most [`VERIFY_SHARE`] of the query rate and their share of the
//! outstanding cap, and the rest stays with lookups and sampling.

use std::net::{Ipv4Addr, SocketAddr};

use super::tests::{deliver_reply, fixture};
use super::*;

/// A ping query from the `n`th querier, under a node id the table would
/// take; returns its address.
fn querier(a: &mut Actor, sockets: &[SendSocket], n: u32) -> SocketAddr {
    let [b, c, d] = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
    let from = SocketAddr::from((Ipv4Addr::new(127, b.max(1), c, d), 6881));
    let mut id = [0u8; 20];
    id[..4].copy_from_slice(&n.wrapping_mul(0x9e37_79b9).to_be_bytes());
    id[16..].copy_from_slice(&n.to_be_bytes());
    let q = Query {
        id: NodeId(id),
        method: Method::Ping,
    };
    let msg = Message {
        transaction_id: b"aa",
        version: None,
        ip: None,
        read_only: false,
        body: Body::Query(q.clone()),
    };
    a.on_query(sockets, 0, &msg, &q, from);
    from
}

fn share(a: &Actor) -> f64 {
    f64::from(a.cfg.intensity.queries_per_sec()) * VERIFY_SHARE
}

#[test]
fn queriers_spend_only_their_share_of_the_query_rate() {
    let (mut a, sockets) = fixture();
    let full = a.query_rate.tokens;
    let share = share(&a);
    assert!(
        share >= 1.0 && share < full / 2.0,
        "a share, not the budget"
    );
    // Only what the bucket holds: no refill, however long the loop takes
    // on a loaded runner.
    a.verify_rate.rate = 0.0;
    let n = a.cfg.intensity.queries_per_sec() * 20;
    for i in 0..n {
        querier(&mut a, &sockets, i + 1);
    }
    let s = a.stats.snapshot();
    let pings = s.queries_out + s.send_errors;
    assert!(pings >= 1, "a querier is still verified");
    assert!(
        pings as f64 <= share + 1.0,
        "{pings} pings for a share of {share}"
    );
    // What they didn't get is still there for the crawl. (The bucket
    // refills as the test runs, so only a floor is checked.)
    assert!(a.query_rate.tokens >= full - share - 1.0);
    assert!(a.verify_tids.len() as u64 <= pings);
    assert!(s.throttled >= u64::from(n) - pings);
    // And a lookup's query still goes out.
    let to: SocketAddr = "127.0.0.9:6881".parse().unwrap();
    assert!(a.query(
        &sockets,
        0,
        to,
        None,
        Method::Ping,
        Purpose::Lookup(u32::MAX)
    ));
}

#[test]
fn queriers_hold_only_their_share_of_the_outstanding_cap() {
    let (mut a, sockets) = fixture();
    let cap = a.cfg.intensity.max_outstanding() / VERIFY_OUTSTANDING_DIV;
    assert!(cap >= 1 && cap < a.cfg.intensity.max_outstanding() / 2);
    // No rate limit at all: only the cap is left to stop them.
    for bucket in [&mut a.verify_rate, &mut a.query_rate] {
        bucket.burst = 1e9;
        bucket.tokens = 1e9;
    }
    for i in 0..(cap as u32 * 4) {
        querier(&mut a, &sockets, i + 1);
    }
    assert_eq!(a.verify_tids.len(), cap);
    assert_eq!(a.pending.len(), cap);
    // An answered ping gives its place back.
    let tids: Vec<u64> = a.verify_tids.iter().copied().collect();
    for tid in &tids[..cap / 2] {
        let p = &a.pending[tid];
        let (to, id) = (p.to, p.id.expect("a ping to a known id"));
        deliver_reply(&mut a, &sockets, *tid, 0, to, id, Vec::new());
    }
    assert_eq!(a.verify_tids.len(), cap - cap / 2);
    // The places are free again: more queriers are pinged, and never past
    // the cap. (How many depends on what the table still has room for.)
    let before = a.stats.snapshot().queries_out;
    for i in 0..(cap as u32 * 8) {
        querier(&mut a, &sockets, 1_000_000 + i);
    }
    assert!(a.stats.snapshot().queries_out > before);
    assert!(a.verify_tids.len() > cap - cap / 2 && a.verify_tids.len() <= cap);
    for tid in a.verify_tids.clone() {
        a.take_pending(tid);
    }
    assert!(a.verify_tids.is_empty() && a.pending.is_empty() && a.pending_addrs.is_empty());
}

#[test]
fn an_unsent_ping_keeps_its_token() {
    let (mut a, sockets) = fixture();
    // The shared rate is used up, as it is while sampling runs.
    a.query_rate.tokens = 0.0;
    a.query_rate.rate = 0.0;
    a.verify_rate.rate = 0.0;
    let held = a.verify_rate.tokens;
    for i in 0..100 {
        querier(&mut a, &sockets, i + 1);
    }
    let s = a.stats.snapshot();
    assert_eq!(s.queries_out + s.send_errors, 0);
    assert!((a.verify_rate.tokens - held).abs() < 1e-9);
    assert!(a.verify_tids.is_empty() && a.pending.is_empty());
}

/// A ping the shared rate let through spends the share's token too, even
/// when the packet then can't be sent: only a refusal gives it back.
#[test]
fn a_ping_that_fails_to_send_still_spends_the_share() {
    let (mut a, sockets) = fixture();
    // The socket is IPv4; a family marked IPv6 passes the family check and
    // then every send fails in the OS.
    a.families[0].ipv6 = true;
    // No refill of either bucket, so what was taken can be read exactly.
    a.verify_rate.rate = 0.0;
    a.query_rate.rate = 0.0;
    let share = share(&a);
    let full = a.query_rate.tokens;
    let n = a.cfg.intensity.queries_per_sec() * 20;
    for i in 0..n {
        let from = SocketAddr::from((
            std::net::Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 1, i as u16 + 1),
            6881,
        ));
        let mut id = [0u8; 20];
        id[..4].copy_from_slice(&(i + 1).wrapping_mul(0x9e37_79b9).to_be_bytes());
        let q = Query {
            id: NodeId(id),
            method: Method::Ping,
        };
        let msg = Message {
            transaction_id: b"aa",
            version: None,
            ip: None,
            read_only: false,
            body: Body::Query(q.clone()),
        };
        a.on_query(&sockets, 0, &msg, &q, from);
    }
    let s = a.stats.snapshot();
    assert_eq!(s.queries_out, 0, "nothing can leave");
    // Replies fail on this socket too, so the errors count more than the
    // pings; the shared rate's own tokens say how many pings were tried.
    assert!(s.send_errors >= 1, "the sends were tried and failed");
    let tried = full - a.query_rate.tokens;
    assert!(tried >= 1.0 && tried <= share + 1e-9, "{tried} tries");
    assert!(
        a.verify_rate.tokens < 1.0,
        "the share is spent, not given back"
    );
    assert!(a.verify_tids.is_empty() && a.pending.is_empty());
}

#[test]
fn a_ping_that_times_out_gives_its_place_back() {
    let (mut a, sockets) = fixture();
    for i in 0..8 {
        querier(&mut a, &sockets, i + 1);
    }
    assert!(!a.verify_tids.is_empty());
    a.cfg.query_timeout = Duration::ZERO;
    a.maintain(&sockets);
    assert!(a.verify_tids.is_empty());
}

#[test]
fn the_share_follows_the_intensity() {
    let (mut a, _sockets) = fixture();
    a.verify_rate.tokens = 0.0;
    for intensity in [Intensity::Low, Intensity::Aggressive, Intensity::Balanced] {
        a.on_command(Command::SetIntensity(intensity));
        let qps = f64::from(intensity.queries_per_sec());
        let b = &a.verify_rate;
        assert!((b.rate - qps * VERIFY_SHARE).abs() < 1e-9);
        assert!(b.burst >= 1.0 && (b.tokens - b.burst).abs() < 1e-9, "full");
        assert!(intensity.max_outstanding() / VERIFY_OUTSTANDING_DIV >= 1);
    }
}
