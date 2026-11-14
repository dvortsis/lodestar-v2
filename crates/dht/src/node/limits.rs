//! Per-source limits on inbound traffic: address blocks, a bounded table of
//! per-block token buckets, and the addresses that have answered our
//! queries. One source (a host or a block of hosts) gets a share of what
//! the node does for others, never all of it.

use std::net::IpAddr;
use std::time::Duration;

use tokio::time::Instant;

use super::transport::{Map, Set};

/// An address block: an IPv4 /24 or an IPv6 /64, the unit one operator
/// usually controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Network(pub(crate) u8, pub(crate) u64);

/// `ip`'s block. A v4-mapped address is its IPv4 block.
pub(crate) fn network(ip: IpAddr) -> Network {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            Network(4, u64::from_be_bytes([0, 0, 0, 0, 0, a, b, c]))
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            Network(6, u64::from_be_bytes(o[..8].try_into().expect("8 bytes")))
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// A token bucket per key, in a table of at most about `cap` keys. The
/// table is two generations: a key used in the current one stays; one
/// left in the previous one when the current fills is forgotten, and comes
/// back with a full bucket. Forgetting only ever hands out a burst, so the
/// table's size bounds memory without letting one key take more than its
/// rate while it's remembered.
#[derive(Debug)]
pub(crate) struct SourceBuckets<K> {
    current: Map<K, Bucket>,
    previous: Map<K, Bucket>,
    rate: f64,
    burst: f64,
    cap: usize,
}

impl<K: Copy + Eq + Ord + std::hash::Hash> SourceBuckets<K> {
    pub fn new(rate: f64, burst: f64, cap: usize) -> Self {
        SourceBuckets {
            current: Map::default(),
            previous: Map::default(),
            rate,
            burst: burst.max(1.0),
            cap: cap.max(2),
        }
    }

    /// Takes a token from `key`'s bucket; false when it's empty.
    pub fn try_take(&mut self, key: K, now: Instant) -> bool {
        let b = match self.current.get_mut(&key) {
            Some(b) => b,
            None => {
                if self.current.len() >= self.cap / 2 {
                    self.previous = std::mem::take(&mut self.current);
                }
                let b = self.previous.remove(&key).unwrap_or(Bucket {
                    tokens: self.burst,
                    last: now,
                });
                self.current.entry(key).or_insert(b)
            }
        };
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * self.rate).min(self.burst);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}

/// Addresses that answered a query of ours: a reply carrying an id we
/// chose came back from them, so they aren't a forged source. Two
/// generations of at most `cap / 2` each, turned over when the current one
/// fills or every [`Answered::GENERATION`].
#[derive(Debug)]
pub(crate) struct Answered {
    current: Set<IpAddr>,
    previous: Set<IpAddr>,
    started: Instant,
    cap: usize,
}

impl Answered {
    pub const GENERATION: Duration = Duration::from_secs(30 * 60);

    pub fn new(cap: usize, now: Instant) -> Self {
        Answered {
            current: Set::default(),
            previous: Set::default(),
            started: now,
            cap: cap.max(2),
        }
    }

    fn turn_over_if_due(&mut self, now: Instant) {
        if self.current.len() >= self.cap / 2
            || now.saturating_duration_since(self.started) >= Self::GENERATION
        {
            self.previous = std::mem::take(&mut self.current);
            self.started = now;
        }
    }

    pub fn insert(&mut self, ip: IpAddr, now: Instant) {
        let ip = ip.to_canonical();
        if self.current.contains(&ip) {
            return;
        }
        self.turn_over_if_due(now);
        self.previous.remove(&ip);
        self.current.insert(ip);
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.current.contains(&ip) || self.previous.contains(&ip)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.current.len() + self.previous.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_bucket_gives_its_burst_then_its_rate() {
        let t0 = Instant::now();
        let mut b = SourceBuckets::new(2.0, 5.0, 64);
        let k = network("10.0.0.1".parse().unwrap());
        assert_eq!((0..10).filter(|_| b.try_take(k, t0)).count(), 5);
        let t1 = t0 + Duration::from_secs(3);
        assert_eq!((0..10).filter(|_| b.try_take(k, t1)).count(), 5);
        let t2 = t1 + Duration::from_millis(500);
        assert_eq!((0..10).filter(|_| b.try_take(k, t2)).count(), 1);
    }

    #[test]
    fn keys_have_separate_buckets_and_one_block_shares_one() {
        let t0 = Instant::now();
        let mut b = SourceBuckets::new(1.0, 3.0, 64);
        let a = network("10.0.0.1".parse().unwrap());
        let same = network("10.0.0.200".parse().unwrap());
        let other = network("10.0.1.1".parse().unwrap());
        assert_eq!(a, same);
        assert_eq!(
            network("2001:db8::1".parse().unwrap()),
            network("2001:db8::ffff:1".parse().unwrap())
        );
        assert_eq!(
            network("::ffff:10.0.0.9".parse().unwrap()),
            a,
            "a v4-mapped address is its IPv4 block"
        );
        assert_eq!((0..5).filter(|_| b.try_take(a, t0)).count(), 3);
        assert!(!b.try_take(same, t0));
        assert_eq!((0..5).filter(|_| b.try_take(other, t0)).count(), 3);
    }

    #[test]
    fn answered_remembers_for_a_generation_and_forgets_after_two() {
        let t0 = Instant::now();
        let mut a = Answered::new(1000, t0);
        let ip: IpAddr = "192.0.2.7".parse().unwrap();
        assert!(!a.contains(ip));
        a.insert(ip, t0);
        assert!(a.contains(ip));
        assert!(a.contains("::ffff:192.0.2.7".parse().unwrap()));
        let other: IpAddr = "192.0.2.8".parse().unwrap();
        a.insert(other, t0 + Answered::GENERATION);
        assert!(a.contains(ip), "still in the previous generation");
        a.insert(other, t0 + Answered::GENERATION * 2);
        a.insert("192.0.2.9".parse().unwrap(), t0 + Answered::GENERATION * 2);
        assert!(!a.contains(ip));
    }

    proptest! {
        /// However many keys pass through, both tables stay within their
        /// bound.
        #[test]
        fn tables_stay_bounded(keys in proptest::collection::vec(any::<u32>(), 1..3000)) {
            let t0 = Instant::now();
            let cap = 64;
            let mut b = SourceBuckets::new(0.0, 2.0, cap);
            let mut a = Answered::new(cap, t0);
            for k in keys {
                b.try_take(k, t0);
                a.insert(IpAddr::from(k.to_be_bytes()), t0);
                prop_assert!(b.len() <= cap);
                prop_assert!(a.len() <= cap);
            }
        }

        /// A key never gets more than its burst plus its rate over the
        /// time since it was first seen, while the table remembers it.
        #[test]
        fn a_remembered_key_gets_at_most_burst_plus_rate(
            steps in proptest::collection::vec(0..2000u64, 1..300)
        ) {
            let t0 = Instant::now();
            let (rate, burst) = (3.0, 7.0);
            let mut b = SourceBuckets::new(rate, burst, 64);
            let mut now = t0;
            let mut granted = 0u64;
            for ms in steps {
                now += Duration::from_millis(ms);
                if b.try_take(1u8, now) {
                    granted += 1;
                }
                let allowed = burst + rate * now.duration_since(t0).as_secs_f64();
                prop_assert!(granted as f64 <= allowed + 1e-9, "{granted} > {allowed}");
            }
        }
    }
}
