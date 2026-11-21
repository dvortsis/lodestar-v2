//! CR-SAMPLE-BACKOFF-ESCALATE: a node's first failed sample in a row cools
//! for `FIRST_FAILURE_BACKOFF`, every later one for the full backoff, an
//! answer ends the streak, and with the switch off a failure cools in full
//! at once. Explicit instants: the sampler takes `now` as an argument.

use std::net::SocketAddr;
use std::time::Duration;

use proptest::prelude::*;
use tokio::time::Instant;

use super::*;
use crate::compact::NodeId;

const FULL: Duration = MAX_INTERVAL;
const NS: Duration = Duration::from_nanos(1);

fn wide(i: u32) -> SocketAddr {
    SocketAddr::from((((0x0B00_0000 + (i << 8)) | 1).to_be_bytes(), 6881))
}

fn node(addr: SocketAddr) -> NodeInfo {
    NodeInfo {
        id: NodeId([1; 20]),
        addr,
    }
}

fn on() -> Sampler {
    Sampler::default().with_escalation(true)
}

/// Offers, takes and reserves `addr` at `now`, as the actor does.
fn sample(s: &mut Sampler, addr: SocketAddr, token: u64, now: Instant) {
    assert!(s.offer(node(addr), now), "offered at {now:?}");
    assert_eq!(s.next(now), Some(node(addr)));
    assert!(s.reserve(addr, token, now));
}

/// When the table says `addr` may be sampled again: the first instant,
/// searched to the nanosecond, at which it is table-eligible.
fn eligible_at(s: &Sampler, addr: SocketAddr, from: Instant) -> Instant {
    let (mut lo, mut hi) = (from, from + FULL + FULL);
    assert!(s.table_eligible(&addr, hi));
    while hi - lo > NS {
        let mid = lo + (hi - lo) / 2;
        if s.table_eligible(&addr, mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

#[test]
fn the_first_backoff_is_within_the_runners_bounds() {
    // At least the sim's 300 s sample interval, at most an hour.
    assert!(FIRST_FAILURE_BACKOFF >= Duration::from_secs(300));
    assert!(FIRST_FAILURE_BACKOFF <= Duration::from_secs(3600));
    assert!(FIRST_FAILURE_BACKOFF < FULL);
}

#[test]
fn a_first_failure_cools_briefly_and_a_second_in_a_row_in_full() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    assert!(s.fail(a, 1, FULL, t0));
    assert_eq!(eligible_at(&s, a, t0), t0 + FIRST_FAILURE_BACKOFF);
    assert!(!s.offer(node(a), t0 + FIRST_FAILURE_BACKOFF - NS));
    // Its cooldown ends: the record stays, eligible, holding the streak.
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(t1);
    assert_eq!(
        (s.known(), s.cooling(), s.struck(), s.inflight()),
        (1, 0, 1, 0)
    );
    sample(&mut s, a, 2, t1);
    assert_eq!((s.struck(), s.inflight()), (0, 1));
    assert!(s.fail(a, 2, FULL, t1));
    assert_eq!(eligible_at(&s, a, t1), t1 + FULL);
}

#[test]
fn an_answer_ends_the_streak() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    s.fail(a, 1, FULL, t0);
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(t1);
    sample(&mut s, a, 2, t1);
    assert!(s.complete(a, 2, Duration::from_secs(300), t1));
    let t2 = t1 + Duration::from_secs(300);
    s.prune(t2);
    // The answer's cooldown leaves no memory.
    assert_eq!((s.known(), s.struck()), (0, 0));
    sample(&mut s, a, 3, t2);
    s.fail(a, 3, FULL, t2);
    assert_eq!(eligible_at(&s, a, t2), t2 + FIRST_FAILURE_BACKOFF);
}

#[test]
fn a_scaled_answer_also_ends_the_streak() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    s.fail(a, 1, FULL, t0);
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    sample(&mut s, a, 2, t1);
    assert!(s.complete_scaled(a, 2, Duration::from_secs(300), 3, t1));
    let t2 = t1 + Duration::from_secs(900);
    sample(&mut s, a, 3, t2);
    s.fail(a, 3, FULL, t2);
    assert_eq!(eligible_at(&s, a, t2), t2 + FIRST_FAILURE_BACKOFF);
}

#[test]
fn with_the_switch_off_a_failure_cools_in_full_at_once() {
    let mut s = Sampler::default();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    assert!(s.fail(a, 1, FULL, t0));
    assert_eq!(eligible_at(&s, a, t0), t0 + FULL);
    s.prune(t0 + FULL);
    // Nothing remembered: the record leaves, as `complete` leaves it.
    assert_eq!((s.known(), s.struck()), (0, 0));
    sample(&mut s, a, 2, t0 + FULL);
    s.fail(a, 2, FULL, t0 + FULL);
    assert_eq!(eligible_at(&s, a, t0 + FULL), t0 + FULL + FULL);
}

/// Off, `fail` is `complete`: the same cooldown for every backoff, so the
/// switch-off sampler is today's.
#[test]
fn with_the_switch_off_fail_is_complete() {
    for secs in [0, 59, 60, 300, 1800, 21_600, 21_601] {
        let backoff = Duration::from_secs(secs);
        let t0 = Instant::now();
        let a = wide(1);
        let (mut f, mut c) = (Sampler::default(), Sampler::default());
        sample(&mut f, a, 1, t0);
        sample(&mut c, a, 1, t0);
        assert!(f.fail(a, 1, backoff, t0));
        assert!(c.complete(a, 1, backoff, t0));
        assert_eq!(eligible_at(&f, a, t0), eligible_at(&c, a, t0), "{secs}");
    }
}

#[test]
fn a_failure_shorter_than_the_first_backoff_isnt_lengthened() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    s.fail(a, 1, MIN_INTERVAL, t0);
    assert_eq!(eligible_at(&s, a, t0), t0 + MIN_INTERVAL);
}

/// A failure within a streak cools in full and ends it: a node that never
/// answers is sampled at most twice per full backoff, never more often.
#[test]
fn a_streak_ends_with_its_full_backoff() {
    let mut s = on();
    let mut t = Instant::now();
    let a = wide(1);
    for round in 0..4u64 {
        sample(&mut s, a, 2 * round + 1, t);
        s.fail(a, 2 * round + 1, FULL, t);
        assert_eq!(
            eligible_at(&s, a, t),
            t + FIRST_FAILURE_BACKOFF,
            "round {round}"
        );
        t += FIRST_FAILURE_BACKOFF;
        s.prune(t);
        sample(&mut s, a, 2 * round + 2, t);
        s.fail(a, 2 * round + 2, FULL, t);
        assert_eq!(eligible_at(&s, a, t), t + FULL, "round {round}");
        t += FULL;
        s.prune(t);
        assert_eq!((s.known(), s.struck()), (0, 0), "round {round}");
    }
}

/// A streak lasts until the failure plus the full backoff: when the
/// switch-off cooldown would have ended.
#[test]
fn a_streak_lasts_as_long_as_the_cooldown_it_replaces() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    s.fail(a, 1, FULL, t0);
    s.prune(t0 + FIRST_FAILURE_BACKOFF);
    s.prune(t0 + FULL - NS);
    assert_eq!(s.struck(), 1);
    s.prune(t0 + FULL);
    assert_eq!((s.known(), s.struck()), (0, 0));
    let t1 = t0 + FULL;
    sample(&mut s, a, 2, t1);
    s.fail(a, 2, FULL, t1);
    assert_eq!(eligible_at(&s, a, t1), t1 + FIRST_FAILURE_BACKOFF);
}

/// A sample reserved after the short cooldown ended but before a prune
/// still carries the streak.
#[test]
fn the_streak_carries_into_a_sample_before_the_prune() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    s.fail(a, 1, FULL, t0);
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    sample(&mut s, a, 2, t1);
    assert_eq!(
        (s.known(), s.cooling(), s.struck(), s.inflight()),
        (1, 0, 0, 1)
    );
    s.fail(a, 2, FULL, t1);
    assert_eq!(eligible_at(&s, a, t1), t1 + FULL);
}

#[test]
fn stale_samples_dont_touch_the_streak_and_an_abort_keeps_it() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    assert!(!s.fail(a, 2, FULL, t0), "a token not in flight");
    assert!(!s.fail(wide(2), 1, FULL, t0), "an address not in flight");
    assert!(s.fail(a, 1, FULL, t0));
    assert!(!s.fail(a, 1, FULL, t0), "already completed");
    assert_eq!(eligible_at(&s, a, t0), t0 + FIRST_FAILURE_BACKOFF);
    // A sample that wasn't sent leaves the streak as it was.
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    sample(&mut s, a, 3, t1);
    assert!(s.abort(a, 3));
    assert_eq!((s.known(), s.struck(), s.inflight()), (1, 1, 0));
    sample(&mut s, a, 4, t1);
    s.fail(a, 4, FULL, t1);
    assert_eq!(eligible_at(&s, a, t1), t1 + FULL);
    // Without a streak an abort drops the record, as with the switch off.
    let t2 = t1 + FULL;
    s.prune(t2);
    sample(&mut s, a, 5, t2);
    assert!(s.abort(a, 5));
    assert_eq!(s.known(), 0);
}

/// Split off, a streak holds its table place until its `forget`, as the
/// switch-off cooldown would: a full table of streaks refuses newcomers
/// as a full table of cooldowns does, and no streak is dropped.
#[test]
fn split_off_a_full_table_keeps_its_streaks() {
    let mut s = Sampler::with_capacity(2).with_escalation(true);
    let t0 = Instant::now();
    sample(&mut s, wide(1), 1, t0);
    s.fail(wide(1), 1, FULL, t0);
    sample(&mut s, wide(2), 2, t0);
    s.fail(wide(2), 2, FULL, t0);
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(t1);
    assert_eq!((s.known(), s.struck()), (2, 2));
    assert!(s.saturated());
    assert!(s.offer(node(wide(3)), t1));
    assert_eq!(s.next(t1), None);
    assert_eq!(s.struck(), 2);
    // Both still escalate (reserved directly: a full table's `next` hands
    // out no one, and a node's own record makes its place).
    for (a, t) in [(wide(1), 3), (wide(2), 4)] {
        assert!(s.reserve(a, t, t1));
        s.fail(a, t, FULL, t1);
        assert_eq!(eligible_at(&s, a, t1), t1 + FULL);
    }
}

/// Split on, a full table moves a streak to the filter as the cooldown it
/// stands in for, to its `forget`: never sooner than the switch-off end,
/// in that order among the cooldowns, so a node that keeps failing is
/// never sampled more than twice per full backoff (the reviewer's case:
/// the short cooldowns aren't what moves first).
#[test]
fn split_on_a_streak_moves_as_its_switch_off_cooldown() {
    let mut s = Sampler::split_with_capacity(2).with_escalation(true);
    let t0 = Instant::now();
    // A plain cooldown that ends sooner than the streak's switch-off end...
    sample(&mut s, wide(1), 1, t0);
    s.complete(wide(1), 1, Duration::from_secs(3600), t0);
    // ...and a dead node's first failure, whose short cooldown ends first.
    sample(&mut s, wide(2), 2, t0);
    s.fail(wide(2), 2, FULL, t0);
    // A newcomer: the plain cooldown moves, not the streak.
    sample(&mut s, wide(3), 3, t0);
    assert!(matches!(
        s.states.get(&wide(2)),
        Some(State::Cooling { struck: true, .. })
    ));
    s.complete(wide(3), 3, FULL, t0);
    // Another: now the streak is the soonest by its switch-off end, and it
    // moves to the filter until then.
    sample(&mut s, wide(4), 4, t0);
    assert!(!s.states.contains_key(&wide(2)));
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(t1);
    assert!(!s.offer(node(wide(2)), t1), "spilled to its switch-off end");
    assert!(!s.offer(node(wide(2)), t0 + FULL - NS));
    // The filter keeps it to the end of that minute at most.
    let after = t0 + FULL + Duration::from_secs(60);
    assert!(!s.spilled(&wide(2), after));
}

/// The snapshot lists a short cooldown as it is and never a streak, and
/// sealing ends a streak.
#[test]
fn the_snapshot_lists_cooldowns_not_streaks() {
    let mut s = on();
    let t0 = Instant::now();
    sample(&mut s, wide(1), 1, t0);
    s.fail(wide(1), 1, FULL, t0);
    sample(&mut s, wide(2), 2, t0);
    s.fail(wide(2), 2, FULL, t0);
    let t1 = t0 + Duration::from_secs(1);
    let listed: Vec<_> = s.restrictions(t1).collect();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|(_, u)| *u == t0 + FIRST_FAILURE_BACKOFF));
    let t2 = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(t2);
    assert_eq!(s.restrictions(t2).count(), 0);
    // A streak's next sample, sealed at a stop: a plain restriction.
    sample(&mut s, wide(1), 3, t2);
    s.seal_in_flight(t2 + FULL);
    let listed: Vec<_> = s.restrictions(t2).collect();
    assert_eq!(listed, [(wide(1), t2 + FULL)]);
    s.prune(t2 + FULL);
    assert!(
        !s.states.contains_key(&wide(1)),
        "the seal ended the streak"
    );
}

/// A restored restriction replaces a streak (it can't happen: seeding
/// comes before sampling), and the indexes stay exact.
#[test]
fn a_restore_replaces_a_streak() {
    for prune in [false, true] {
        let mut s = on();
        let t0 = Instant::now();
        sample(&mut s, wide(1), 1, t0);
        s.fail(wide(1), 1, FULL, t0);
        let t1 = t0 + FIRST_FAILURE_BACKOFF;
        if prune {
            s.prune(t1);
        }
        s.restore(wide(1), t1 + FULL);
        assert_eq!((s.known(), s.cooling(), s.struck()), (1, 1, 0));
        assert_eq!(eligible_at(&s, wide(1), t1), t1 + FULL);
        s.prune(t1 + FULL);
        assert_eq!(s.known(), 0);
    }
}

/// The memory per entry: the streak is a flag in the table's record
/// (which grows from 24 to 32 bytes whatever the switch). A first
/// failure's record has one more index entry for its 300 s.
#[test]
fn a_streak_costs_no_more_than_the_record() {
    assert_eq!(std::mem::size_of::<State>(), 32);
}

#[derive(Debug, Clone)]
enum Op {
    Offer(u8),
    Next,
    Answer(u8),
    Fail(u8),
    Abort(u8),
    Advance(u16),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..12u8).prop_map(Op::Offer),
        3 => Just(Op::Next),
        1 => any::<u8>().prop_map(Op::Answer),
        2 => any::<u8>().prop_map(Op::Fail),
        1 => any::<u8>().prop_map(Op::Abort),
        2 => (0..4u16).prop_map(Op::Advance),
    ]
}

proptest! {
    /// Against a model of the rule, under any sequence: a failure cools
    /// for the first backoff unless it's within a streak (a first
    /// failure's, until that failure plus the full backoff), when it
    /// cools in full and ends the streak; an answer ends it too, an abort
    /// keeps it, and nothing else drops it. With the switch off, always in
    /// full. The indexes stay exact.
    #[test]
    fn every_failure_cools_as_the_streak_says(
        escalate in any::<bool>(),
        capacity in 2..24usize,
        ops in proptest::collection::vec(arb_op(), 1..400),
    ) {
        let mut s = Sampler::with_capacity(capacity).with_escalation(escalate);
        let mut now = Instant::now();
        let mut token = 0u64;
        // In flight: the address, token and when it was reserved.
        let mut flying: Vec<(SocketAddr, u64, Instant)> = vec![];
        // The model: each node's live streak, by when it ends.
        let mut streak: std::collections::HashMap<SocketAddr, Instant> = Default::default();
        for op in ops {
            match op {
                Op::Offer(i) => {
                    s.offer(node(wide(u32::from(i))), now);
                }
                Op::Next => {
                    if let Some(n) = s.next(now) {
                        token += 1;
                        if s.reserve(n.addr, token, now) {
                            flying.push((n.addr, token, now));
                        }
                    }
                }
                Op::Answer(i) if !flying.is_empty() => {
                    let (a, t, _) = flying.remove(usize::from(i) % flying.len());
                    prop_assert!(s.complete(a, t, Duration::from_secs(300), now));
                    streak.remove(&a);
                }
                Op::Fail(i) if !flying.is_empty() => {
                    let (a, t, reserved) = flying.remove(usize::from(i) % flying.len());
                    let live = streak.remove(&a).is_some_and(|end| reserved < end);
                    prop_assert!(s.fail(a, t, FULL, now));
                    let first = escalate && !live;
                    let want = if first { FIRST_FAILURE_BACKOFF } else { FULL };
                    prop_assert_eq!(eligible_at(&s, a, now), now + want);
                    if first {
                        streak.insert(a, now + FULL);
                    }
                }
                Op::Abort(i) if !flying.is_empty() => {
                    let (a, t, _) = flying.remove(usize::from(i) % flying.len());
                    prop_assert!(s.abort(a, t));
                }
                Op::Advance(k) => {
                    now += [Duration::from_secs(1), FIRST_FAILURE_BACKOFF, FULL, FULL + FULL][usize::from(k)];
                    s.prune(now);
                }
                _ => {}
            }
            // Indexes: a plain cooldown is in `expiry` alone; a streak's
            // short cooldown in `short` and `struck`; a Struck in `struck`.
            let mut plain = 0;
            let mut short = 0;
            let mut struck = 0;
            for (a, st) in &s.states {
                match *st {
                    State::Cooling { until, token: k, struck: false } => {
                        plain += 1;
                        prop_assert!(s.expiry.contains(&(until, k, *a)));
                    }
                    State::Cooling { until, token: k, struck: true } => {
                        short += 1;
                        prop_assert!(s.short.contains(&(until, k, *a)));
                        prop_assert!(s.struck.contains(&(forget_of(until), k, *a)));
                    }
                    State::Struck { forget, token: k } => {
                        struck += 1;
                        prop_assert!(s.struck.contains(&(forget, k, *a)));
                    }
                    State::InFlight { .. } => {}
                }
            }
            prop_assert_eq!(s.expiry.len(), plain);
            prop_assert_eq!(s.short.len(), short);
            prop_assert_eq!(s.struck.len(), short + struck);
            prop_assert_eq!(s.inflight(), flying.len());
            prop_assert!(s.known() <= capacity);
            if !escalate {
                prop_assert_eq!(short + struck, 0);
            }
        }
    }
}

/// Split on, a table of nothing but streaks isn't saturated: each can move
/// to the filter, so a newcomer is admitted.
#[test]
fn split_on_a_table_of_streaks_still_admits() {
    let mut s = Sampler::split_with_capacity(2).with_escalation(true);
    let t0 = Instant::now();
    for (i, t) in [(1, 1), (2, 2)] {
        sample(&mut s, wide(i), t, t0);
        s.fail(wide(i), t, FULL, t0);
    }
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(t1);
    assert_eq!((s.known(), s.cooling(), s.struck()), (2, 0, 2));
    assert!(!s.saturated());
    sample(&mut s, wide(3), 3, t1);
    assert_eq!(s.struck(), 1);
}
