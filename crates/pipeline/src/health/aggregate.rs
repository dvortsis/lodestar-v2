//! One torrent's scrape round in one record: what every tracker said plus
//! the DHT estimate, reduced to the swarm row and hot columns (§6, §8).
//!
//! - **Trackers reporting** counts only answers with s+l+c > 0: trackers
//!   such as opentracker answer zeros for hashes they've never seen (§15).
//!   **Checked** counts every tracker that answered at all, so the UI can
//!   say "reported by 4 of 20 trackers checked".
//! - **Seeders, leechers and completed** are the most any reporting
//!   tracker gave. Trackers see different slices of a swarm, and the
//!   largest is the best lower bound.
//! - **Swarm consistency** (§8): one tracker claiming thousands of seeders
//!   while every other tracker and the DHT see next to nothing is the
//!   fake-seeding signature. The swarm is flagged suspect and its counts
//!   come from the other sources; the outlier's own answer is still kept
//!   per tracker, so the detail view can show it.

use lodestar_tracker_scrape::Counts;

use super::schedule::Seen;

/// A claim below this is never called fake: small swarms vary too much
/// between trackers.
pub const SUSPECT_MIN: u32 = 1000;
/// How far above every other source a claim must be to be called fake.
pub const SUSPECT_RATIO: u32 = 50;

/// The DHT's BEP 33 estimate, if the scrape got one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhtSeen {
    pub seeds: u32,
    pub peers: u32,
    /// Nodes whose bloom filters went into the estimate.
    pub responders: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Aggregate {
    pub seeders: u32,
    pub leechers: u32,
    pub completed: u32,
    pub trackers_reporting: u16,
    pub trackers_checked: u16,
    pub dht: Option<(u32, u32)>,
    /// Reporting trackers only, by tracker id.
    pub per_tracker: Vec<(u16, Counts)>,
    pub suspect: bool,
    /// For the schedule: `None` when no source answered.
    pub found: Option<Seen>,
}

impl Aggregate {
    /// The value for the seeders hot column: trackers when any report the
    /// swarm, else the DHT's seeds.
    pub fn hot_seeders(&self) -> u32 {
        match self.dht {
            Some((seeds, _)) if self.trackers_reporting == 0 => seeds,
            _ => self.seeders,
        }
    }

    pub fn hot_leechers(&self) -> u32 {
        match self.dht {
            Some((_, peers)) if self.trackers_reporting == 0 => peers,
            _ => self.leechers,
        }
    }
}

/// Reduces one round. `answers` holds one entry per tracker that answered:
/// its id and its entry for this hash (`None` if it left the hash out).
/// Trackers that failed aren't in it.
pub fn aggregate(answers: &[(u16, Option<Counts>)], dht: Option<DhtSeen>) -> Aggregate {
    let mut per_tracker: Vec<(u16, Counts)> = answers
        .iter()
        .filter_map(|(t, c)| c.filter(Counts::is_reporting).map(|c| (*t, c)))
        .collect();
    per_tracker.sort_by_key(|(t, _)| *t);
    let dht = dht.filter(|d| d.responders > 0);
    let suspect = is_suspect(&per_tracker, dht);
    // With a suspect swarm, the outlier doesn't set the counts.
    let outlier = suspect
        .then(|| {
            per_tracker
                .iter()
                .max_by_key(|(_, c)| c.seeders)
                .map(|(t, _)| *t)
        })
        .flatten();
    let counted = per_tracker.iter().filter(|(t, _)| Some(*t) != outlier);
    let max = |f: fn(&Counts) -> u32| counted.clone().map(|(_, c)| f(c)).max().unwrap_or(0);
    let mut agg = Aggregate {
        seeders: max(|c| c.seeders),
        leechers: max(|c| c.leechers),
        completed: max(|c| c.completed),
        trackers_reporting: per_tracker.len().min(usize::from(u16::MAX)) as u16,
        trackers_checked: answers.len().min(usize::from(u16::MAX)) as u16,
        dht: dht.map(|d| (d.seeds, d.peers)),
        per_tracker,
        suspect,
        found: None,
    };
    if agg.trackers_checked > 0 || agg.dht.is_some() {
        agg.found = Some(Seen {
            seeders: agg.hot_seeders().max(agg.dht.map_or(0, |d| d.0)),
            leechers: agg.hot_leechers().max(agg.dht.map_or(0, |d| d.1)),
        });
    }
    agg
}

/// One tracker far above every other source, when there are other
/// sources to compare with.
fn is_suspect(reporting: &[(u16, Counts)], dht: Option<DhtSeen>) -> bool {
    let Some((top_tracker, top)) = reporting.iter().max_by_key(|(_, c)| c.seeders) else {
        return false;
    };
    if top.seeders < SUSPECT_MIN {
        return false;
    }
    let others = reporting
        .iter()
        .filter(|(t, _)| t != top_tracker)
        .map(|(_, c)| c.seeders.max(c.leechers));
    // A DHT estimate from a single node is too thin to overrule a tracker.
    let dht_view = dht
        .filter(|d| d.responders >= 2)
        .map(|d| d.seeds.max(d.peers));
    let evidence: Vec<u32> = others.chain(dht_view).collect();
    let Some(best_other) = evidence.iter().max() else {
        return false;
    };
    top.seeders / SUSPECT_RATIO > *best_other
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(seeders: u32, leechers: u32, completed: u32) -> Option<Counts> {
        Some(Counts {
            seeders,
            leechers,
            completed,
        })
    }

    fn dht(seeds: u32, peers: u32, responders: u8) -> Option<DhtSeen> {
        Some(DhtSeen {
            seeds,
            peers,
            responders,
        })
    }

    #[test]
    fn reporting_checked_and_maxima() {
        let a = aggregate(
            &[
                (1, c(10, 2, 50)),
                (2, c(0, 0, 0)),
                (3, None),
                (4, c(4, 9, 80)),
            ],
            None,
        );
        assert_eq!((a.seeders, a.leechers, a.completed), (10, 9, 80));
        assert_eq!((a.trackers_reporting, a.trackers_checked), (2, 4));
        assert_eq!(
            a.per_tracker.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            [1, 4]
        );
        assert!(!a.suspect);
        assert_eq!(
            a.found,
            Some(Seen {
                seeders: 10,
                leechers: 9
            })
        );
    }

    #[test]
    fn nothing_answered_is_not_zero() {
        let a = aggregate(&[], None);
        assert_eq!(a.found, None);
        assert_eq!(a.trackers_checked, 0);
        // A DHT estimate alone is an answer.
        let a = aggregate(&[], dht(3, 7, 4));
        assert_eq!((a.hot_seeders(), a.hot_leechers()), (3, 7));
        assert_eq!(
            a.found,
            Some(Seen {
                seeders: 3,
                leechers: 7
            })
        );
        // A DHT lookup where nobody sent a filter isn't.
        assert_eq!(aggregate(&[], dht(0, 0, 0)).found, None);
    }

    #[test]
    fn trackers_win_over_the_dht_when_they_report() {
        let a = aggregate(&[(1, c(20, 5, 1))], dht(3, 7, 4));
        assert_eq!((a.hot_seeders(), a.hot_leechers()), (20, 5));
        assert_eq!(a.dht, Some((3, 7)));
        // Every tracker says zero: the DHT is all we know.
        let a = aggregate(&[(1, c(0, 0, 0))], dht(3, 7, 4));
        assert_eq!((a.hot_seeders(), a.hot_leechers()), (3, 7));
    }

    #[test]
    fn fake_seeding_is_flagged_and_discounted() {
        let a = aggregate(&[(1, c(40_000, 3, 1)), (2, c(2, 1, 9))], dht(1, 2, 5));
        assert!(a.suspect);
        assert_eq!(a.seeders, 2, "counts come from the others");
        assert_eq!(a.trackers_reporting, 2);
        assert_eq!(a.per_tracker[0].1.seeders, 40_000, "the claim is kept");

        // A big swarm everyone agrees on is fine.
        let a = aggregate(&[(1, c(40_000, 3, 1)), (2, c(30_000, 1, 9))], None);
        assert!(!a.suspect);
        // One tracker alone: nothing to compare with.
        assert!(!aggregate(&[(1, c(40_000, 3, 1))], None).suspect);
        // A one-node DHT estimate is too thin to overrule it.
        assert!(!aggregate(&[(1, c(40_000, 3, 1))], dht(0, 0, 1)).suspect);
        // But a solid DHT estimate of nothing is evidence.
        assert!(aggregate(&[(1, c(40_000, 3, 1))], dht(0, 1, 6)).suspect);
        // Small claims are never called fake.
        assert!(!aggregate(&[(1, c(900, 0, 0)), (2, c(1, 0, 0))], None).suspect);
    }
}
