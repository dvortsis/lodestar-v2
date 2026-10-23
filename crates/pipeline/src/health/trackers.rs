//! Per-tracker pacing and health (architecture §15: "all within
//! per-tracker rate limits").
//!
//! Each tracker gets a token bucket: one request per `interval`, with no
//! burst beyond one. Failures move it through backoff states so a tracker
//! that's down or banning us is asked less and less, never hammered:
//!
//! ```text
//!  Ok ──failure──▶ Backoff (1 min, doubling) ──5 in a row──▶ Down (probe hourly)
//!  any ──refusal naming a limit or ban──▶ Banned (24 h, doubling to a week)
//!  any ──success──▶ Ok
//! ```
//!
//! A tracker's own `min_request_interval` slows its bucket down for good,
//! never speeds it up.

use std::time::Duration;

use lodestar_tracker_scrape::Error;

/// Health as shown in Insights (§10.9.5: "health, rate limit, ban state").
/// Codes are persisted in `trackers.state`: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Ok = 0,
    Backoff = 1,
    Down = 2,
    Banned = 3,
}

/// Failures in a row before a tracker counts as down.
const DOWN_AFTER: u32 = 5;
const BACKOFF_MIN: Duration = Duration::from_secs(60);
const DOWN_PROBE: Duration = Duration::from_secs(3600);
const BAN_MIN: Duration = Duration::from_secs(24 * 3600);
const BAN_MAX: Duration = Duration::from_secs(7 * 24 * 3600);

/// How a request failed, as far as pacing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Timeouts, network errors, bad replies, HTTP errors.
    Transient,
    /// The tracker refused in words that mean "slow down" or "go away".
    Banned,
}

impl Failure {
    pub fn of(e: &Error) -> Failure {
        match e {
            Error::Refused(m) if is_ban(m) => Failure::Banned,
            // 429 Too Many Requests and 403 Forbidden are how HTTP
            // trackers usually say it.
            Error::Status(429 | 403) => Failure::Banned,
            _ => Failure::Transient,
        }
    }
}

/// Words trackers use when rate-limiting or banning, matched as whole
/// words so "inaccurate" or "generated" don't read as "rate".
const BAN_WORDS: [&str; 12] = [
    "rate",
    "ratelimit",
    "ratelimited",
    "limit",
    "limited",
    "banned",
    "ban",
    "blocked",
    "blacklisted",
    "abuse",
    "denied",
    "throttled",
];
/// Two-word phrases with the same meaning.
const BAN_PHRASES: [[&str; 2]; 3] = [["too", "many"], ["too", "fast"], ["slow", "down"]];

fn is_ban(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().any(|w| BAN_WORDS.contains(w))
        || words
            .windows(2)
            .any(|pair| BAN_PHRASES.iter().any(|p| pair == p))
}

/// One tracker's pacing state. Times are milliseconds on a monotonic
/// clock the caller supplies, so tests can drive it.
#[derive(Debug, Clone)]
pub struct Pacer {
    interval: Duration,
    /// Earliest time the next request may go.
    next_at: u64,
    health: Health,
    failures: u32,
    ban: Duration,
}

impl Pacer {
    pub fn new(interval: Duration, now: u64) -> Pacer {
        Pacer {
            interval,
            next_at: now,
            health: Health::Ok,
            failures: 0,
            ban: BAN_MIN,
        }
    }

    /// Time between requests (a tracker's `min_request_interval` may have
    /// raised it).
    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn health(&self) -> Health {
        self.health
    }

    /// Failures in a row.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// When the next request may go.
    pub fn ready_at(&self) -> u64 {
        self.next_at
    }

    pub fn is_ready(&self, now: u64) -> bool {
        now >= self.next_at
    }

    /// Takes the token for a request starting now.
    pub fn start(&mut self, now: u64) {
        debug_assert!(self.is_ready(now));
        self.next_at = now + ms(self.interval);
    }

    pub fn succeeded(&mut self, now: u64, retry_after: Option<Duration>) {
        if let Some(wait) = retry_after {
            // Slower for good, never faster.
            self.interval = self.interval.max(wait);
            self.next_at = self.next_at.max(now + ms(wait));
        }
        self.health = Health::Ok;
        self.failures = 0;
        self.ban = BAN_MIN;
    }

    pub fn failed(&mut self, now: u64, failure: Failure) {
        self.failures = self.failures.saturating_add(1);
        let wait = match failure {
            Failure::Banned => {
                let wait = self.ban;
                self.ban = (self.ban * 2).min(BAN_MAX);
                self.health = Health::Banned;
                wait
            }
            Failure::Transient if self.failures >= DOWN_AFTER => {
                self.health = Health::Down;
                DOWN_PROBE
            }
            Failure::Transient => {
                self.health = Health::Backoff;
                BACKOFF_MIN * 2u32.pow(self.failures.saturating_sub(1).min(5))
            }
        };
        self.next_at = self.next_at.max(now + ms(wait));
    }
}

fn ms(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1000;

    #[test]
    fn one_request_per_interval() {
        let mut p = Pacer::new(Duration::from_secs(2), 0);
        assert!(p.is_ready(0));
        p.start(0);
        assert!(!p.is_ready(S));
        assert!(p.is_ready(2 * S));
        p.succeeded(S, None);
        assert_eq!(p.ready_at(), 2 * S);
    }

    #[test]
    fn transient_failures_back_off_then_go_down() {
        let mut p = Pacer::new(Duration::from_secs(2), 0);
        let mut waits = Vec::new();
        for _ in 0..6 {
            let now = p.ready_at();
            p.start(now);
            p.failed(now, Failure::Transient);
            waits.push((p.health(), (p.ready_at() - now) / S));
        }
        assert_eq!(
            waits,
            [
                (Health::Backoff, 60),
                (Health::Backoff, 120),
                (Health::Backoff, 240),
                (Health::Backoff, 480),
                (Health::Down, 3600),
                (Health::Down, 3600),
            ]
        );
        p.succeeded(p.ready_at(), None);
        assert_eq!((p.health(), p.failures()), (Health::Ok, 0));
    }

    #[test]
    fn bans_last_a_day_and_double_to_a_week() {
        let mut p = Pacer::new(Duration::from_secs(2), 0);
        let mut days = Vec::new();
        for _ in 0..5 {
            let now = p.ready_at();
            p.failed(now, Failure::Banned);
            days.push((p.ready_at() - now) / (3600 * S));
        }
        assert_eq!(days, [24, 48, 96, 168, 168]);
        assert_eq!(p.health(), Health::Banned);
    }

    #[test]
    fn retry_after_slows_the_tracker_for_good() {
        let mut p = Pacer::new(Duration::from_secs(2), 0);
        p.start(0);
        p.succeeded(0, Some(Duration::from_secs(900)));
        assert_eq!(p.ready_at(), 900 * S);
        p.start(900 * S);
        assert_eq!(p.ready_at(), 1800 * S);
        // A shorter hint never speeds it up.
        p.succeeded(900 * S, Some(Duration::from_secs(1)));
        assert_eq!(p.ready_at(), 1800 * S);
    }

    #[test]
    fn classifies_refusals() {
        let refused = |m: &str| Failure::of(&Error::Refused(m.into()));
        assert_eq!(refused("Rate limited, try later"), Failure::Banned);
        assert_eq!(refused("You are BANNED"), Failure::Banned);
        assert_eq!(refused("too many requests"), Failure::Banned);
        assert_eq!(refused("connection id mismatch"), Failure::Transient);
        assert_eq!(refused("unregistered torrent"), Failure::Transient);
        assert_eq!(refused("Rate-limit exceeded"), Failure::Banned);
        assert_eq!(refused("slow down!"), Failure::Banned);
        assert_eq!(refused("access denied"), Failure::Banned);
        // Whole words only.
        assert_eq!(refused("inaccurate info_hash"), Failure::Transient);
        assert_eq!(refused("generated by a bad client"), Failure::Transient);
        assert_eq!(refused("unlimited torrents"), Failure::Transient);
        assert_eq!(Failure::of(&Error::Status(429)), Failure::Banned);
        assert_eq!(Failure::of(&Error::Status(502)), Failure::Transient);
        assert_eq!(Failure::of(&Error::Timeout), Failure::Transient);
    }
}
