//! Token buckets keyed by client IP or key id, bounded in memory.
//!
//! Each key gets `burst` tokens, regaining `refill` a second. A map never
//! holds more than `max` keys: when it's full, keys whose bucket has fully
//! recovered are forgotten, and if it's still full a newcomer shares one
//! overflow bucket with every other newcomer. Many addresses can then cost
//! each other attempts, but never memory.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct Buckets<K> {
    inner: Mutex<Inner<K>>,
    burst: f64,
    /// Tokens regained per second (tests set 0, so slow hashing on a busy
    /// runner can't refill a bucket).
    pub(crate) refill: f64,
    max: usize,
}

#[derive(Debug)]
struct Inner<K> {
    map: HashMap<K, Bucket>,
    overflow: Bucket,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl<K: Eq + Hash + Copy> Buckets<K> {
    pub(crate) fn new(burst: f64, refill: f64, max: usize) -> Buckets<K> {
        Buckets {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                overflow: Bucket {
                    tokens: burst,
                    at: Instant::now(),
                },
            }),
            burst,
            refill,
            max,
        }
    }

    /// Takes a token, or says how long until one is back.
    pub(crate) fn take(&self, key: K) -> Result<(), Duration> {
        self.with(key, true)
    }

    /// Whether a token is there, without taking it.
    pub(crate) fn check(&self, key: K) -> Result<(), Duration> {
        self.with(key, false)
    }

    /// How many keys are tracked (tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().expect("limiter lock").map.len()
    }

    fn with(&self, key: K, take: bool) -> Result<(), Duration> {
        let mut inner = self.inner.lock().expect("limiter lock");
        let now = Instant::now();
        let Inner { map, overflow } = &mut *inner;
        if !map.contains_key(&key) && map.len() >= self.max {
            // Forget keys that have fully recovered.
            map.retain(|_, b| self.refilled(b, now) < self.burst);
        }
        let bucket = if map.contains_key(&key) || map.len() < self.max {
            map.entry(key).or_insert(Bucket {
                tokens: self.burst,
                at: now,
            })
        } else {
            overflow
        };
        bucket.tokens = self.refilled(bucket, now);
        bucket.at = now;
        if bucket.tokens < 1.0 {
            return Err(self.wait(bucket.tokens));
        }
        if take {
            bucket.tokens -= 1.0;
        }
        Ok(())
    }

    fn refilled(&self, b: &Bucket, now: Instant) -> f64 {
        (b.tokens + now.duration_since(b.at).as_secs_f64() * self.refill).min(self.burst)
    }

    /// Until the bucket holds one token again, rounded up to a second.
    fn wait(&self, tokens: f64) -> Duration {
        if self.refill <= 0.0 {
            return Duration::from_secs(3600);
        }
        Duration::from_secs(((1.0 - tokens) / self.refill).ceil().clamp(1.0, 3600.0) as u64)
    }
}

/// A `Retry-After` value: whole seconds.
pub(crate) fn retry_after(wait: Duration) -> String {
    wait.as_secs().max(1).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_empties_and_says_how_long() {
        let b = Buckets::new(3.0, 0.5, 10);
        for _ in 0..3 {
            assert!(b.take(1u32).is_ok());
        }
        assert_eq!(b.take(1), Err(Duration::from_secs(2)));
        assert_eq!(b.check(1), Err(Duration::from_secs(2)));
        assert!(b.take(2).is_ok(), "keys are separate");
    }

    #[test]
    fn check_takes_nothing() {
        let b = Buckets::new(1.0, 0.0, 10);
        for _ in 0..5 {
            assert!(b.check(1u32).is_ok());
        }
        assert!(b.take(1).is_ok());
        assert_eq!(b.check(1), Err(Duration::from_secs(3600)));
    }

    #[test]
    fn the_map_is_bounded() {
        let b = Buckets::new(2.0, 0.0, 4);
        for k in 0..4u32 {
            b.take(k).unwrap();
        }
        assert_eq!(b.len(), 4);
        // Full of buckets that haven't recovered: newcomers share one.
        assert!(b.take(100).is_ok());
        assert!(b.take(101).is_ok());
        assert!(b.take(102).is_err(), "the overflow bucket is shared");
        assert_eq!(b.len(), 4);
        // Known keys keep their own.
        assert!(b.take(0).is_ok());
        assert!(b.take(0).is_err());
    }

    #[test]
    fn recovered_keys_are_forgotten() {
        // Instant refill: every bucket has recovered by the next call.
        let b = Buckets::new(2.0, 1e9, 4);
        for k in 0..4u32 {
            b.take(k).unwrap();
        }
        std::thread::sleep(Duration::from_millis(2));
        for k in 10..14u32 {
            b.take(k).unwrap();
        }
        assert_eq!(b.len(), 4);
    }

    #[test]
    fn retry_after_is_whole_seconds() {
        assert_eq!(retry_after(Duration::from_millis(10)), "1");
        assert_eq!(retry_after(Duration::from_secs(12)), "12");
    }
}
