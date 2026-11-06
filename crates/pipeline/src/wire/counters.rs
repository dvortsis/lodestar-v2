//! The `Counters` payload (architecture §19.2): rollup deltas a worker
//! counted, keyed as `rollup_hour` keys them (metric, dim, key, hour). The
//! coordinator adds them to its rollups with the worker as a dimension
//! (WK-9). The codec admits only summed metrics (a gauge can't be added
//! up) and positive deltas within [`MAX_VALUE`]; which of those metrics a
//! worker may send (never `HARVESTED`, counted at grant, §19.3) is the
//! intake's rule.
//!
//! # Layout, version 1
//! ```text
//! batch   = version:u8(=1) batch_id:uvar count:uvar counter*count
//! counter = hour:ivar metric:uvar dim:uvar key:ivar value:ivar
//! ```
//! Counters are in strictly ascending `(hour, metric, dim, key)` order, so
//! a payload has one canonical form and no key twice. The encoder sorts
//! and sums duplicates; the decoder refuses anything out of order.

use lodestar_store::{Aggregation, Counter};

use super::{Error, Reader, Result, Writer};
use crate::rollups::metric;

/// The format version this build writes and the only one it reads.
pub const VERSION: u8 = 1;

/// Counters in one batch, at most.
pub const MAX_COUNTERS: usize = 8192;

/// The largest delta one counter carries: far above any real hour's count.
/// It bounds one batch only: summing within a batch can't overflow, but
/// across batches a worker can add to the same row again and again. The
/// intake (WK-9) must cap what it accumulates per row, accept only hours
/// near its own clock after offset correction, and keys from each metric's
/// own domain (`docs/decisions/2026-11-06-WK-6.md`).
pub const MAX_VALUE: i64 = 1 << 40;

/// The latest hour a counter may name (Unix hours; about the year 245,000).
/// A shape check only: the intake's window around its own clock is the
/// real bound.
pub const MAX_HOUR: i64 = i32::MAX as i64;

/// One varint byte for each of a counter's five fields.
const MIN_COUNTER: usize = 5;

/// One `Counters` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountersBatch {
    /// Idempotency: a replayed batch has the same id.
    pub batch: u64,
    /// In canonical order after [`decode`](Self::decode).
    pub counters: Vec<Counter>,
}

fn order(c: &Counter) -> (i64, u32, u32, i64) {
    (c.hour, c.metric, c.dim, c.key)
}

impl CountersBatch {
    /// The payload: counters sorted and duplicates summed. Refuses a metric
    /// that isn't a known sum, an hour outside `0..=MAX_HOUR`, a delta (or
    /// sum) outside `1..=MAX_VALUE`, or more distinct keys than
    /// [`MAX_COUNTERS`].
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut sorted = self.counters.clone();
        sorted.sort_unstable_by_key(order);
        let mut merged: Vec<Counter> = Vec::with_capacity(sorted.len());
        for c in sorted {
            check(&c)?;
            match merged.last_mut() {
                Some(last) if order(last) == order(&c) => {
                    // Both at most MAX_VALUE: no overflow.
                    last.value += c.value;
                    check(last)?;
                }
                _ => merged.push(c),
            }
        }
        if merged.len() > MAX_COUNTERS {
            return Err(Error::TooLarge("counters"));
        }
        let mut w = Writer::default();
        w.u8(VERSION);
        w.uvar(self.batch);
        w.uvar(merged.len() as u64);
        for c in &merged {
            w.ivar(c.hour);
            w.uvar(u64::from(c.metric));
            w.uvar(u64::from(c.dim));
            w.ivar(c.key);
            w.ivar(c.value);
        }
        Ok(w.out)
    }

    /// The whole batch, or an error and nothing.
    pub fn decode(payload: &[u8]) -> Result<CountersBatch> {
        let mut r = Reader::new(payload)?;
        let version = r.u8()?;
        if version != VERSION {
            return Err(Error::Version(version));
        }
        let batch = r.uvar()?;
        let n = r.count(MAX_COUNTERS, MIN_COUNTER, "counters")?;
        let mut counters: Vec<Counter> = Vec::with_capacity(n);
        for _ in 0..n {
            let hour = r.ivar()?;
            let metric = u32::try_from(r.uvar()?).map_err(|_| Error::Invalid("metric"))?;
            let dim = u32::try_from(r.uvar()?).map_err(|_| Error::Invalid("dim"))?;
            let key = r.ivar()?;
            let value = r.ivar()?;
            let c = Counter {
                hour,
                metric,
                dim,
                key,
                value,
            };
            check(&c)?;
            if counters.last().is_some_and(|p| order(p) >= order(&c)) {
                return Err(Error::Invalid("order"));
            }
            counters.push(c);
        }
        r.finish()?;
        Ok(CountersBatch { batch, counters })
    }
}

fn check(c: &Counter) -> Result<()> {
    if metric::aggregation(c.metric) != Some(Aggregation::Sum) {
        return Err(Error::Invalid("metric"));
    }
    if !(0..=MAX_HOUR).contains(&c.hour) {
        return Err(Error::Invalid("hour"));
    }
    if !(1..=MAX_VALUE).contains(&c.value) {
        return Err(Error::Invalid("value"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(hour: i64, metric: u32, dim: u32, key: i64, value: i64) -> Counter {
        Counter {
            hour,
            metric,
            dim,
            key,
            value,
        }
    }

    /// A payload with these raw fields, so the decoder's own refusals
    /// are tested, not only the encoder's.
    fn raw(rows: &[(i64, u64, u64, i64, i64)]) -> Vec<u8> {
        let mut w = Writer::default();
        w.u8(VERSION);
        w.uvar(0);
        w.uvar(rows.len() as u64);
        for &(hour, metric, dim, key, value) in rows {
            w.ivar(hour);
            w.uvar(metric);
            w.uvar(dim);
            w.ivar(key);
            w.ivar(value);
        }
        w.out
    }

    #[test]
    fn duplicates_are_summed() {
        let b = CountersBatch {
            batch: 3,
            counters: vec![
                c(10, 4, 1, 0, 2),
                c(9, 4, 1, 0, 1),
                c(10, 4, 1, 0, 3),
                c(10, 14, 2, 0, 5),
            ],
        };
        let back = CountersBatch::decode(&b.encode().unwrap()).unwrap();
        assert_eq!(back.batch, 3);
        assert_eq!(
            back.counters,
            vec![c(9, 4, 1, 0, 1), c(10, 4, 1, 0, 5), c(10, 14, 2, 0, 5)]
        );
        CountersBatch::decode(&raw(&[(10, 4, 1, 0, 1), (10, 4, 1, 1, 1)])).unwrap();
    }

    #[test]
    fn out_of_order_or_repeated_keys_are_refused() {
        let order = Err(Error::Invalid("order"));
        assert_eq!(CountersBatch::decode(&raw(&[(10, 4, 1, 0, 1); 2])), order);
        assert_eq!(
            CountersBatch::decode(&raw(&[(10, 4, 1, 1, 1), (10, 4, 1, 0, 1)])),
            order
        );
        assert_eq!(
            CountersBatch::decode(&raw(&[(11, 4, 1, 0, 1), (10, 14, 1, 0, 1)])),
            order
        );
    }

    #[test]
    fn bad_fields_are_refused_by_the_decoder() {
        let searchable = u64::from(metric::SEARCHABLE);
        let big = u64::from(u32::MAX) + 1;
        for (row, why) in [
            ((1, 0, 0, 0, 1), "metric"),
            ((1, u64::from(metric::LAST_CODE) + 1, 0, 0, 1), "metric"),
            ((1, big, 0, 0, 1), "metric"),
            // A gauge: two snapshots can't be added up.
            ((1, searchable, 0, 0, 1), "metric"),
            ((1, 4, big, 0, 1), "dim"),
            ((-1, 4, 0, 0, 1), "hour"),
            ((MAX_HOUR + 1, 4, 0, 0, 1), "hour"),
            ((1, 4, 0, 0, 0), "value"),
            ((1, 4, 0, 0, -1), "value"),
            ((1, 4, 0, 0, MAX_VALUE + 1), "value"),
        ] {
            assert_eq!(
                CountersBatch::decode(&raw(&[row])),
                Err(Error::Invalid(why)),
                "{row:?}"
            );
        }
        let mut too_many = Writer::default();
        too_many.u8(VERSION);
        too_many.uvar(0);
        too_many.uvar(MAX_COUNTERS as u64 + 1);
        assert_eq!(
            CountersBatch::decode(&too_many.out),
            Err(Error::TooLarge("counters"))
        );
    }

    #[test]
    fn the_encoder_refuses_what_the_decoder_would() {
        for bad in [
            c(1, 0, 0, 0, 1),
            c(1, metric::LAST_CODE + 1, 0, 0, 1),
            c(1, metric::SEARCHABLE, 0, 0, 1),
            c(-1, 1, 0, 0, 1),
            c(1, 1, 0, 0, 0),
            c(1, 1, 0, 0, -3),
        ] {
            let b = CountersBatch {
                batch: 0,
                counters: vec![bad],
            };
            assert!(b.encode().is_err(), "{bad:?}");
        }
        // A sum past the cap.
        let over = CountersBatch {
            batch: 0,
            counters: vec![c(1, 1, 0, 0, MAX_VALUE), c(1, 1, 0, 0, 1)],
        };
        assert_eq!(over.encode(), Err(Error::Invalid("value")));
    }
}
