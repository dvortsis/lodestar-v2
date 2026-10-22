//! Swarm history (architecture §7): a fixed-size ring of seeders and
//! leechers samples per torrent, 14 daily and 26 weekly, about 170 bytes.
//!
//! A naive row per torrent per day would reach billions of rows within a
//! year, so each torrent keeps one small blob instead, and only when its
//! swarm was ever non-zero or a user looked at it. Missing samples are
//! gaps, never zeros (§10.5): a day without a scrape says nothing about
//! the swarm.
//!
//! Format 1, little-endian:
//!
//! ```text
//! version u8 = 1 | newest day u32 | newest week u32
//! | 14 × (seeders u16, leechers u16)   newest day first
//! | 26 × (seeders u16, leechers u16)   newest week first
//! ```
//!
//! Days count from the Unix epoch (UTC); a week is seven of them. `0xFFFF`
//! marks a gap, and counts saturate one below it. A day or week holds its
//! latest sample.

use std::fmt;

pub const DAYS: usize = 14;
pub const WEEKS: usize = 26;
/// A missing sample.
pub const GAP: u16 = 0xFFFF;
const VERSION: u8 = 1;
/// The encoded size.
pub const ENCODED_LEN: usize = 1 + 4 + 4 + 4 * (DAYS + WEEKS);

/// One sample: (seeders, leechers).
pub type Sample = (u16, u16);
const NONE: Sample = (GAP, GAP);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("malformed swarm history")
    }
}

impl std::error::Error for DecodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    /// Day number of `daily[0]`.
    day: u32,
    /// Week number of `weekly[0]`.
    week: u32,
    daily: [Sample; DAYS],
    weekly: [Sample; WEEKS],
}

impl Default for History {
    fn default() -> Self {
        History {
            day: 0,
            week: 0,
            daily: [NONE; DAYS],
            weekly: [NONE; WEEKS],
        }
    }
}

fn count(v: u32) -> u16 {
    v.min(u32::from(GAP - 1)) as u16
}

fn sample(s: Sample) -> Option<Sample> {
    (s != NONE).then_some(s)
}

/// Shifts `ring` (newest first) so that slot 0 is `to`, which is later
/// than `from`; slots that fall off the end are dropped and new ones are
/// gaps.
fn roll(ring: &mut [Sample], from: u32, to: u32) {
    let by = (to - from) as usize;
    if by >= ring.len() {
        ring.fill(NONE);
    } else {
        ring.rotate_right(by);
        ring[..by].fill(NONE);
    }
}

/// Puts `s` in the slot for period `p` of a ring whose newest is `head`,
/// moving the ring forward when `p` is newer. Too-old periods are dropped.
fn put(ring: &mut [Sample], head: &mut u32, empty: bool, p: u32, s: Sample) {
    if empty {
        *head = p;
    } else if p > *head {
        roll(ring, *head, p);
        *head = p;
    }
    let age = (*head - p) as usize;
    if let Some(slot) = ring.get_mut(age) {
        *slot = s;
    }
}

impl History {
    /// No samples at all.
    pub fn is_empty(&self) -> bool {
        self.daily.iter().chain(&self.weekly).all(|s| *s == NONE)
    }

    /// Records a scrape at `at` (unix seconds). Days and weeks without a
    /// sample stay gaps; a sample older than the ring keeps is dropped.
    pub fn record(&mut self, at: i64, seeders: u32, leechers: u32) {
        let Ok(day) = u32::try_from(at.div_euclid(86_400)) else {
            return;
        };
        let s = (count(seeders), count(leechers));
        let empty = self.is_empty();
        put(&mut self.daily, &mut self.day, empty, day, s);
        put(&mut self.weekly, &mut self.week, empty, day / 7, s);
    }

    /// `(day number, sample)` from the newest day back, gaps as `None`.
    pub fn daily(&self) -> impl Iterator<Item = (u32, Option<Sample>)> + '_ {
        self.daily
            .iter()
            .enumerate()
            .filter_map(|(i, s)| Some((self.day.checked_sub(i as u32)?, sample(*s))))
    }

    /// `(week number, sample)` from the newest week back, gaps as `None`.
    pub fn weekly(&self) -> impl Iterator<Item = (u32, Option<Sample>)> + '_ {
        self.weekly
            .iter()
            .enumerate()
            .filter_map(|(i, s)| Some((self.week.checked_sub(i as u32)?, sample(*s))))
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ENCODED_LEN);
        out.push(VERSION);
        out.extend_from_slice(&self.day.to_le_bytes());
        out.extend_from_slice(&self.week.to_le_bytes());
        for (s, l) in self.daily.iter().chain(&self.weekly) {
            out.extend_from_slice(&s.to_le_bytes());
            out.extend_from_slice(&l.to_le_bytes());
        }
        out
    }

    /// Strict: exactly the encoder's length and version.
    pub fn decode(b: &[u8]) -> Result<History, DecodeError> {
        if b.len() != ENCODED_LEN || b[0] != VERSION {
            return Err(DecodeError);
        }
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"));
        let u16_at = |i: usize| u16::from_le_bytes(b[i..i + 2].try_into().expect("2 bytes"));
        let mut h = History {
            day: u32_at(1),
            week: u32_at(5),
            ..History::default()
        };
        let slots = h.daily.iter_mut().chain(h.weekly.iter_mut());
        for (k, slot) in slots.enumerate() {
            let at = 9 + 4 * k;
            *slot = (u16_at(at), u16_at(at + 2));
        }
        Ok(h)
    }
}

/// The trend in seeders per day over the last week: the least-squares
/// slope of the daily samples, gaps skipped. `None` with fewer than three
/// samples, which is too few to call a direction.
pub fn trend(h: &History) -> Option<f64> {
    let points: Vec<(f64, f64)> = h
        .daily()
        .take(7)
        .filter_map(|(day, s)| Some((f64::from(day), f64::from(s?.0))))
        .collect();
    if points.len() < 3 {
        return None;
    }
    let n = points.len() as f64;
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    Some(sxy / sxx)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    /// Day 20_000 (2024-10-04).
    const T0: i64 = 20_000 * DAY;

    #[test]
    fn records_days_and_weeks_newest_first() {
        let mut h = History::default();
        assert!(h.is_empty());
        h.record(T0 + 100, 10, 2);
        h.record(T0 + 200, 11, 3); // same day: latest wins
        h.record(T0 + 2 * DAY, 20, 4);
        let daily: Vec<_> = h.daily().take(3).collect();
        assert_eq!(
            daily,
            [
                (20_002, Some((20, 4))),
                (20_001, None),
                (20_000, Some((11, 3)))
            ]
        );
        let weekly: Vec<_> = h.weekly().take(2).collect();
        // Days 20_000..20_002 are in week 2857 (20_000 / 7 = 2857.1).
        assert_eq!(weekly, [(2_857, Some((20, 4))), (2_856, None)]);
        assert_eq!(h.encode().len(), ENCODED_LEN);
    }

    #[test]
    fn long_gaps_clear_the_ring_and_old_samples_drop() {
        let mut h = History::default();
        h.record(T0, 5, 5);
        h.record(T0 + 30 * DAY, 7, 1);
        assert_eq!(h.daily().filter(|d| d.1.is_some()).count(), 1);
        // Within the weekly ring, the old week survives.
        assert_eq!(h.weekly().filter(|w| w.1.is_some()).count(), 2);
        // Older than the daily ring, newer than the weekly one: weekly only.
        h.record(T0 + 10 * DAY, 9, 9);
        assert_eq!(h.daily().filter(|d| d.1.is_some()).count(), 1);
        assert!(h.weekly().any(|w| w.1 == Some((9, 9))));
        // A year back: dropped everywhere.
        let before = h.clone();
        h.record(T0 - 365 * DAY, 1, 1);
        assert_eq!(h, before);
    }

    #[test]
    fn counts_saturate_below_the_gap_marker() {
        let mut h = History::default();
        h.record(T0, u32::MAX, 70_000);
        assert_eq!(h.daily().next().unwrap().1, Some((GAP - 1, GAP - 1)));
    }

    #[test]
    fn negative_or_huge_times_are_ignored() {
        let mut h = History::default();
        h.record(-5, 1, 1);
        h.record(i64::MAX, 1, 1);
        assert!(h.is_empty());
    }

    #[test]
    fn strict_decoding() {
        let mut h = History::default();
        h.record(T0, 3, 4);
        let b = h.encode();
        assert_eq!(History::decode(&b).unwrap(), h);
        assert!(History::decode(&b[1..]).is_err());
        let mut long = b.clone();
        long.push(0);
        assert!(History::decode(&long).is_err());
        let mut v = b;
        v[0] = 2;
        assert!(History::decode(&v).is_err());
    }

    #[test]
    fn trend_needs_three_points() {
        let mut h = History::default();
        h.record(T0, 10, 0);
        h.record(T0 + DAY, 12, 0);
        assert_eq!(trend(&h), None);
        h.record(T0 + 3 * DAY, 16, 0);
        let t = trend(&h).unwrap();
        assert!((t - 2.0).abs() < 1e-9, "{t}");
        let mut falling = History::default();
        for (i, s) in [30, 20, 10].iter().enumerate() {
            falling.record(T0 + i as i64 * DAY, *s, 0);
        }
        assert!(trend(&falling).unwrap() < 0.0);
    }
}
