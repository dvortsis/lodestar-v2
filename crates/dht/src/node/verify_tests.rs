//! Pings to queriers have a budget of their own: whatever arrives, they take
//! at most [`VERIFY_SHARE`] of the query rate and their share of the
//! outstanding cap, and the rest stays with lookups and sampling.

use std::net::{Ipv4Addr, SocketAddr};

use super::tests::fixture;
use super::*;

/// A ping query from `from`, under a node id the table would take.
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
    // Far more queriers than the whole query rate, all at once.
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
    // An answered or timed-out ping gives its place back.
    let tids: Vec<u64> = a.verify_tids.iter().copied().collect();
    for tid in &tids[..cap / 2] {
        a.take_pending(*tid);
    }
    assert_eq!(a.verify_tids.len(), cap - cap / 2);
    for i in 0..(cap as u32) {
        querier(&mut a, &sockets, 1_000_000 + i);
    }
    assert_eq!(a.verify_tids.len(), cap);
    for tid in a.verify_tids.clone() {
        a.take_pending(tid);
    }
    assert!(a.verify_tids.is_empty() && a.pending.is_empty() && a.pending_addrs.is_empty());
}

#[test]
fn the_share_follows_the_intensity() {
    let (mut a, _sockets) = fixture();
    for intensity in [Intensity::Low, Intensity::Balanced, Intensity::Aggressive] {
        let b = verify_bucket(intensity, Instant::now());
        let qps = f64::from(intensity.queries_per_sec());
        assert!((b.rate - qps * VERIFY_SHARE).abs() < 1e-9 && b.burst >= 1.0);
        a.cfg.intensity = intensity;
        assert!(a.cfg.intensity.max_outstanding() / VERIFY_OUTSTANDING_DIV >= 1);
    }
}
