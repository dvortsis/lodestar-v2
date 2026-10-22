//! When to scrape a torrent again (architecture §15: "new torrents once;
//! … everything else on a slow decay schedule").
//!
//! The next interval depends on what the last scrape found and on the
//! interval before it, which the store keeps as `next_scrape_at -
//! last_scrape_at`:
//! - an alive swarm whose counts moved starts over at the alive minimum;
//! - one that held steady doubles, up to the alive maximum;
//! - a swarm that just died starts at the dead minimum and doubles from
//!   there, up to the dead maximum;
//! - a scrape nobody answered (every tracker failed, no DHT estimate)
//!   retries after an hour, doubling to a day with each unanswered round
//!   in a row. The first answer after such a streak starts its class
//!   over at the minimum: the interval before the outage is unknown, and
//!   `next − last answer` would count the outage as steadiness.
//!
//! Every interval is spread by a per-torrent jitter of ±10%, so torrents
//! scraped together don't stay in lockstep forever.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub alive_min: Duration,
    pub alive_max: Duration,
    pub dead_min: Duration,
    pub dead_max: Duration,
    pub unreached_min: Duration,
    pub unreached_max: Duration,
}

impl Default for Schedule {
    fn default() -> Self {
        const HOUR: u64 = 3600;
        Schedule {
            alive_min: Duration::from_secs(6 * HOUR),
            alive_max: Duration::from_secs(72 * HOUR),
            dead_min: Duration::from_secs(48 * HOUR),
            dead_max: Duration::from_secs(30 * 24 * HOUR),
            unreached_min: Duration::from_secs(HOUR),
            unreached_max: Duration::from_secs(24 * HOUR),
        }
    }
}

/// A swarm as one scrape saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Seen {
    pub seeders: u32,
    pub leechers: u32,
}

impl Seen {
    pub fn is_alive(&self) -> bool {
        self.seeders > 0 || self.leechers > 0
    }

    /// Moved enough to look at it again soon: by a fifth, or by at least
    /// two peers in a small swarm.
    fn moved_from(&self, before: &Seen) -> bool {
        let moved = |a: u32, b: u32| {
            let d = a.abs_diff(b);
            d >= 2 && d.saturating_mul(5) >= a.max(b)
        };
        moved(self.seeders, before.seeders) || moved(self.leechers, before.leechers)
    }
}

/// What the scheduler knew before this scrape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Before {
    pub seen: Seen,
    /// The interval that led to this scrape, in seconds.
    pub interval: i64,
    /// Unanswered rounds in a row just before this one.
    pub unreached: u8,
}

/// What this scrape found: `None` when no tracker answered and the DHT
/// gave no estimate.
pub type Found = Option<Seen>;

impl Schedule {
    /// Seconds until the next scrape. `before` is `None` for a torrent's
    /// first scrape; `id` seeds the jitter.
    pub fn next_interval(&self, id: i64, before: Option<Before>, found: Found) -> i64 {
        let secs = |d: Duration| d.as_secs() as i64;
        if found.is_none() {
            let streak = before.map_or(0, |b| b.unreached).min(16);
            let base = secs(self.unreached_min).saturating_mul(1 << streak);
            return jitter(id, base.min(secs(self.unreached_max)));
        }
        // After an outage the last interval is unknown: start the class
        // over at its minimum.
        let prev = before.filter(|b| b.unreached == 0).map(|b| b.interval);
        let grow = |min: Duration, max: Duration| {
            let base = prev.map_or(secs(min), |p| p.saturating_mul(2));
            base.clamp(secs(min), secs(max))
        };
        let base = match (found, before) {
            (None, _) => unreachable!("handled above"),
            (Some(now), Some(b)) if now.is_alive() && now.moved_from(&b.seen) => {
                secs(self.alive_min)
            }
            (Some(now), Some(b)) if now.is_alive() && b.seen.is_alive() => {
                grow(self.alive_min, self.alive_max)
            }
            (Some(now), _) if now.is_alive() => secs(self.alive_min),
            (Some(_), Some(b)) if b.seen.is_alive() => secs(self.dead_min),
            (Some(_), Some(_)) => grow(self.dead_min, self.dead_max),
            (Some(_), None) => secs(self.dead_min),
        };
        jitter(id, base)
    }
}

/// `base` ± 10%, the same for the same torrent and interval.
fn jitter(id: i64, base: i64) -> i64 {
    // splitmix64 of the id: cheap, well spread, deterministic.
    let mut x = (id as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    let spread = base / 10;
    if spread == 0 {
        return base;
    }
    base - spread + (x % (2 * spread as u64 + 1)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const H: i64 = 3600;

    fn seen(seeders: u32, leechers: u32) -> Seen {
        Seen { seeders, leechers }
    }

    fn within(v: i64, base: i64) -> bool {
        (base - base / 10..=base + base / 10).contains(&v)
    }

    #[test]
    fn first_scrape() {
        let s = Schedule::default();
        assert!(within(s.next_interval(1, None, Some(seen(3, 1))), 6 * H));
        assert!(within(s.next_interval(1, None, Some(seen(0, 0))), 48 * H));
        assert!(within(s.next_interval(1, None, None), H));
    }

    #[test]
    fn steady_swarms_back_off_and_moving_ones_reset() {
        let s = Schedule::default();
        let before = |seeders, interval| {
            Some(Before {
                seen: seen(seeders, 0),
                interval,
                unreached: 0,
            })
        };
        assert!(within(
            s.next_interval(1, before(10, 6 * H), Some(seen(10, 0))),
            12 * H
        ));
        assert!(within(
            s.next_interval(1, before(10, 48 * H), Some(seen(11, 0))),
            72 * H
        ));
        // Moved by more than a fifth.
        assert!(within(
            s.next_interval(1, before(10, 48 * H), Some(seen(20, 0))),
            6 * H
        ));
        // Small swarms need two peers of movement.
        assert!(within(
            s.next_interval(1, before(1, 12 * H), Some(seen(2, 0))),
            24 * H
        ));
        assert!(within(
            s.next_interval(1, before(1, 12 * H), Some(seen(3, 0))),
            6 * H
        ));
    }

    #[test]
    fn dead_swarms_decay_to_a_month() {
        let s = Schedule::default();
        let dead = |interval| {
            Some(Before {
                seen: seen(0, 0),
                interval,
                unreached: 0,
            })
        };
        assert!(within(
            s.next_interval(
                1,
                Some(Before {
                    seen: seen(4, 0),
                    interval: 6 * H,
                    unreached: 0,
                }),
                Some(seen(0, 0))
            ),
            48 * H
        ));
        assert!(within(
            s.next_interval(1, dead(48 * H), Some(seen(0, 0))),
            96 * H
        ));
        assert!(within(
            s.next_interval(1, dead(600 * H), Some(seen(0, 0))),
            720 * H
        ));
        // Revived.
        assert!(within(
            s.next_interval(1, dead(720 * H), Some(seen(1, 0))),
            6 * H
        ));
    }

    #[test]
    fn unreached_scrapes_retry_soon() {
        let s = Schedule::default();
        let b = |interval, unreached| {
            Some(Before {
                seen: seen(5, 0),
                interval,
                unreached,
            })
        };
        // An outage starts at an hour whatever the swarm's own interval,
        // and doubles with each unanswered round in a row.
        assert!(within(s.next_interval(1, b(72 * H, 0), None), H));
        assert!(within(s.next_interval(1, b(H, 1), None), 2 * H));
        assert!(within(s.next_interval(1, b(H, 9), None), 24 * H));
        // The first answer after it starts the swarm's class over.
        assert!(within(
            s.next_interval(1, b(30 * H, 3), Some(seen(5, 0))),
            6 * H
        ));
        assert!(within(
            s.next_interval(1, b(30 * H, 3), Some(seen(0, 0))),
            48 * H
        ));
    }

    #[test]
    fn jitter_spreads_but_is_deterministic() {
        let a: Vec<i64> = (0..100).map(|id| jitter(id, 1000)).collect();
        let b: Vec<i64> = (0..100).map(|id| jitter(id, 1000)).collect();
        assert_eq!(a, b);
        assert!(a.iter().all(|v| (900..=1100).contains(v)));
        let distinct: std::collections::HashSet<_> = a.iter().collect();
        assert!(distinct.len() > 50, "{distinct:?}");
        assert_eq!(jitter(5, 9), 9);
    }

    proptest! {
        /// Every interval stays inside the schedule's overall bounds, give
        /// or take the jitter, whatever came before.
        #[test]
        fn intervals_stay_in_bounds(
            id in any::<i64>(),
            prev in proptest::option::of((any::<u32>(), any::<u32>(), 0i64..100_000_000)),
            found in proptest::option::of((any::<u32>(), any::<u32>())),
        ) {
            let s = Schedule::default();
            let before = prev.map(|(a, b, interval)| Before { seen: seen(a, b), interval, unreached: (interval % 20) as u8 });
            let v = s.next_interval(id, before, found.map(|(a, b)| seen(a, b)));
            let lo = s.unreached_min.as_secs() as i64 * 9 / 10;
            let hi = s.dead_max.as_secs() as i64 * 11 / 10;
            prop_assert!((lo..=hi).contains(&v), "{v}");
        }
    }
}
