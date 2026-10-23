//! Which torrents the next scrape round takes (§16 M4 plan): four lanes,
//! shared by deficit round robin.
//!
//! - **Interest:** torrents someone is looking at (a detail view, the top
//!   of a search). Served first, like the fetch pool's user-interest keys
//!   (§18.4), but with at most half of a round while other lanes wait, so
//!   a busy search page can't stop new torrents getting their one scrape.
//! - **New:** stored but never scraped ("new torrents once", §15).
//! - **Rediscovered:** seen again on the DHT, so probably alive (§5).
//! - **Due:** the decay schedule says it's time.
//!
//! The scheduler fills the new and due lanes from the store in batches,
//! and the others from channels. A torrent is taken once per round even
//! if several lanes hold it, and never while a round already has it.

use std::collections::{HashSet, VecDeque};

/// Lane indexes, in DRR order after interest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    Interest,
    New,
    Rediscovered,
    Due,
}

impl Lane {
    pub const ALL: [Lane; 4] = [Lane::Interest, Lane::New, Lane::Rediscovered, Lane::Due];

    /// Rollup key (`rollup_hour.key` for scrape metrics): append only.
    pub fn code(self) -> i64 {
        self as i64
    }
}

/// DRR weights of the lanes after interest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weights {
    pub new: u32,
    pub rediscovered: u32,
    pub due: u32,
}

impl Default for Weights {
    fn default() -> Self {
        // New torrents need their first numbers soonest; due work is the
        // bulk and can wait a little.
        Weights {
            new: 4,
            rediscovered: 2,
            due: 3,
        }
    }
}

/// Most torrents an interest or rediscovered lane holds; past that the
/// oldest entries go (they'll come round again).
pub const LANE_CAP: usize = 50_000;

#[derive(Debug)]
pub struct Lanes {
    queues: [VecDeque<i64>; 4],
    /// Members of the interest and rediscovered queues, for dedup.
    queued: [HashSet<i64>; 4],
    deficit: [u32; 4],
    weights: Weights,
    /// Torrents in a round that hasn't finished.
    in_flight: HashSet<i64>,
}

impl Lanes {
    pub fn new(weights: Weights) -> Lanes {
        Lanes {
            queues: Default::default(),
            queued: Default::default(),
            deficit: [0; 4],
            weights,
            in_flight: HashSet::new(),
        }
    }

    fn weight(&self, lane: Lane) -> u32 {
        match lane {
            Lane::Interest => u32::MAX,
            Lane::New => self.weights.new,
            Lane::Rediscovered => self.weights.rediscovered,
            Lane::Due => self.weights.due,
        }
    }

    /// Adds a torrent to a lane, once. Bounded lanes drop their oldest.
    pub fn push(&mut self, lane: Lane, id: i64) {
        let i = lane as usize;
        if !self.queued[i].insert(id) {
            return;
        }
        self.queues[i].push_back(id);
        if self.queues[i].len() > LANE_CAP {
            if let Some(old) = self.queues[i].pop_front() {
                self.queued[i].remove(&old);
            }
        }
    }

    pub fn len(&self, lane: Lane) -> usize {
        self.queues[lane as usize].len()
    }

    pub fn is_empty(&self) -> bool {
        self.queues.iter().all(VecDeque::is_empty)
    }

    /// Takes up to `n` torrents for a round, with the lane each came from.
    /// They stay in flight until [`Lanes::finish`].
    pub fn take(&mut self, n: usize) -> Vec<(Lane, i64)> {
        let mut out = Vec::with_capacity(n);
        // Interest first, up to half the round: a flood of searches can't
        // starve "new torrents once" (§15).
        self.drain(Lane::Interest, n.div_ceil(2), &mut out);
        // Then DRR: each pass adds a lane's weight to its deficit; a lane
        // spends one per torrent. An empty lane forfeits its deficit, so
        // idle lanes can't save up a burst.
        let lanes = [Lane::New, Lane::Rediscovered, Lane::Due];
        while out.len() < n && lanes.iter().any(|l| self.len(*l) > 0) {
            for lane in lanes {
                let i = lane as usize;
                if self.queues[i].is_empty() {
                    self.deficit[i] = 0;
                    continue;
                }
                self.deficit[i] += self.weight(lane);
                while self.deficit[i] > 0 && out.len() < n {
                    let before = out.len();
                    self.drain(lane, before + 1, &mut out);
                    if out.len() == before {
                        self.deficit[i] = 0;
                        break;
                    }
                    self.deficit[i] -= 1;
                }
            }
        }
        // What the other lanes left goes to interest after all.
        self.drain(Lane::Interest, n, &mut out);
        out
    }

    /// Moves torrents from `lane` into `out` until it holds `n`, skipping
    /// ones already in flight or already taken.
    fn drain(&mut self, lane: Lane, n: usize, out: &mut Vec<(Lane, i64)>) {
        let i = lane as usize;
        while out.len() < n {
            let Some(id) = self.queues[i].pop_front() else {
                return;
            };
            self.queued[i].remove(&id);
            if self.in_flight.insert(id) {
                out.push((lane, id));
            }
        }
    }

    /// The round with these torrents is done.
    pub fn finish(&mut self, ids: impl IntoIterator<Item = i64>) {
        for id in ids {
            self.in_flight.remove(&id);
        }
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ids(taken: &[(Lane, i64)]) -> Vec<i64> {
        taken.iter().map(|(_, id)| *id).collect()
    }

    #[test]
    fn interest_goes_first() {
        let mut l = Lanes::new(Weights::default());
        for id in 0..10 {
            l.push(Lane::Due, id);
        }
        l.push(Lane::Interest, 100);
        l.push(Lane::Interest, 101);
        let t = l.take(3);
        assert_eq!(t[0], (Lane::Interest, 100));
        assert_eq!(t[1], (Lane::Interest, 101));
        assert_eq!(t[2], (Lane::Due, 0));
    }

    #[test]
    fn interest_takes_at_most_half_while_others_wait() {
        let mut l = Lanes::new(Weights::default());
        for id in 0..1000 {
            l.push(Lane::Interest, id);
            l.push(Lane::New, 10_000 + id);
        }
        let t = l.take(74);
        let count = |lane| t.iter().filter(|(l, _)| *l == lane).count();
        assert_eq!((count(Lane::Interest), count(Lane::New)), (37, 37));
        // Alone, interest fills the round.
        let mut l = Lanes::new(Weights::default());
        for id in 0..1000 {
            l.push(Lane::Interest, id);
        }
        assert_eq!(l.take(74).len(), 74);
        // And it tops up what the other lanes can't fill.
        let mut l = Lanes::new(Weights::default());
        for id in 0..1000 {
            l.push(Lane::Interest, id);
        }
        l.push(Lane::New, 5000);
        let t = l.take(10);
        assert_eq!(t.iter().filter(|(l, _)| *l == Lane::Interest).count(), 9);
    }

    #[test]
    fn lanes_share_by_weight() {
        let mut l = Lanes::new(Weights {
            new: 4,
            rediscovered: 2,
            due: 3,
        });
        for id in 0..1000 {
            l.push(Lane::New, id);
            l.push(Lane::Rediscovered, 10_000 + id);
            l.push(Lane::Due, 20_000 + id);
        }
        let t = l.take(900);
        let count = |lane| t.iter().filter(|(l, _)| *l == lane).count();
        assert_eq!(count(Lane::New), 400);
        assert_eq!(count(Lane::Rediscovered), 200);
        assert_eq!(count(Lane::Due), 300);
    }

    #[test]
    fn idle_lanes_give_way() {
        let mut l = Lanes::new(Weights::default());
        for id in 0..100 {
            l.push(Lane::Due, id);
        }
        assert_eq!(ids(&l.take(50)), (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn one_round_per_torrent_at_a_time() {
        let mut l = Lanes::new(Weights::default());
        l.push(Lane::New, 7);
        l.push(Lane::Due, 7);
        l.push(Lane::Interest, 7);
        assert_eq!(l.take(10), [(Lane::Interest, 7)]);
        l.push(Lane::Due, 7);
        assert!(l.take(10).is_empty(), "still in flight");
        l.finish([7]);
        l.push(Lane::Due, 7);
        assert_eq!(l.take(10), [(Lane::Due, 7)]);
    }

    #[test]
    fn lanes_dedup_and_stay_bounded() {
        let mut l = Lanes::new(Weights::default());
        l.push(Lane::Rediscovered, 1);
        l.push(Lane::Rediscovered, 1);
        assert_eq!(l.len(Lane::Rediscovered), 1);
        for id in 0..(LANE_CAP as i64 + 10) {
            l.push(Lane::Rediscovered, id + 2);
        }
        assert_eq!(l.len(Lane::Rediscovered), LANE_CAP);
        // The oldest went first.
        assert_eq!(l.take(1), [(Lane::Rediscovered, 12)]);
    }

    proptest! {
        /// Nothing is taken twice while in flight, rounds never exceed
        /// their size, and every queued torrent is eventually taken.
        #[test]
        fn rounds_are_bounded_and_complete(
            pushes in proptest::collection::vec((0usize..4, 0i64..200), 0..400),
            n in 1usize..80,
        ) {
            let mut l = Lanes::new(Weights::default());
            let mut want = HashSet::new();
            for (lane, id) in pushes {
                l.push(Lane::ALL[lane], id);
                want.insert(id);
            }
            let mut got = HashSet::new();
            while !l.is_empty() {
                let t = l.take(n);
                prop_assert!(t.len() <= n);
                for (_, id) in &t {
                    prop_assert!(got.insert(*id), "{id} taken twice");
                }
                // Finish nothing: duplicates across lanes must be skipped.
                if t.is_empty() {
                    break;
                }
            }
            prop_assert_eq!(got, want);
        }
    }
}
