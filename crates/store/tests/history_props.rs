//! The swarm history ring on arbitrary input.

use lodestar_store::History;
use lodestar_store::history::{ENCODED_LEN, GAP};
use proptest::prelude::*;

const DAY: i64 = 86_400;
const T0: i64 = 20_000 * DAY;

fn scrapes() -> impl Strategy<Value = Vec<(i64, u32, u32)>> {
    proptest::collection::vec((0i64..400 * DAY, any::<u32>(), any::<u32>()), 0..60)
        .prop_map(|v| v.into_iter().map(|(t, s, l)| (T0 + t, s, l)).collect())
}

proptest! {
    #[test]
    fn decoding_arbitrary_bytes_never_panics(b in proptest::collection::vec(any::<u8>(), 0..400)) {
        if let Ok(h) = History::decode(&b) {
            prop_assert_eq!(h.encode(), b);
        }
    }

    #[test]
    fn round_trips(scrapes in scrapes()) {
        let mut h = History::default();
        for (at, s, l) in scrapes {
            h.record(at, s, l);
        }
        let b = h.encode();
        prop_assert_eq!(b.len(), ENCODED_LEN);
        prop_assert_eq!(History::decode(&b).unwrap(), h);
    }

    /// Recording in time order: the newest day holds the last scrape, and
    /// every day with a sample had a scrape that day (gaps stay gaps).
    #[test]
    fn samples_only_where_scraped(mut scrapes in scrapes()) {
        scrapes.sort_by_key(|s| s.0);
        let mut h = History::default();
        for &(at, s, l) in &scrapes {
            h.record(at, s, l);
        }
        let days: std::collections::HashSet<i64> =
            scrapes.iter().map(|s| s.0.div_euclid(DAY)).collect();
        for (day, sample) in h.daily() {
            prop_assert_eq!(sample.is_some(), days.contains(&i64::from(day)), "day {}", day);
            if let Some((s, l)) = sample {
                prop_assert!(s < GAP && l < GAP);
            }
        }
        if let Some(&(at, s, l)) = scrapes.last() {
            let (day, sample) = h.daily().next().unwrap();
            prop_assert_eq!(i64::from(day), at.div_euclid(DAY));
            let cap = |v: u32| v.min(u32::from(GAP - 1)) as u16;
            prop_assert_eq!(sample, Some((cap(s), cap(l))));
        }
    }
}
