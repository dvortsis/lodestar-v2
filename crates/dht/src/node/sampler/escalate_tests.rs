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
fn the_streak_holds_through_a_full_backoff() {
    let mut s = on();
    let mut t = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t);
    s.fail(a, 1, FULL, t);
    t += FIRST_FAILURE_BACKOFF;
    s.prune(t);
    for token in 2..6 {
        sample(&mut s, a, token, t);
        assert!(s.fail(a, token, FULL, t));
        assert_eq!(eligible_at(&s, a, t), t + FULL, "failure {token}");
        t += FULL;
        s.prune(t);
        assert_eq!(s.struck(), 1);
    }
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

#[test]
fn a_streak_is_forgotten_a_full_backoff_after_its_cooldown() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    s.fail(a, 1, FULL, t0);
    let end = t0 + FIRST_FAILURE_BACKOFF;
    s.prune(end);
    s.prune(end + FULL - NS);
    assert_eq!(s.struck(), 1);
    s.prune(end + FULL);
    assert_eq!((s.known(), s.struck()), (0, 0));
    let t1 = end + FULL;
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
    assert_eq!((s.known(), s.cooling(), s.inflight()), (1, 0, 1));
    s.fail(a, 2, FULL, t1);
    assert_eq!(eligible_at(&s, a, t1), t1 + FULL);
}

#[test]
fn stale_and_aborted_samples_dont_touch_the_streak() {
    let mut s = on();
    let t0 = Instant::now();
    let a = wide(1);
    sample(&mut s, a, 1, t0);
    assert!(!s.fail(a, 2, FULL, t0), "a token not in flight");
    assert!(!s.fail(wide(2), 1, FULL, t0), "an address not in flight");
    assert!(s.fail(a, 1, FULL, t0));
    assert!(!s.fail(a, 1, FULL, t0), "already completed");
    assert_eq!(eligible_at(&s, a, t0), t0 + FIRST_FAILURE_BACKOFF);
    // Aborting a sample (it was never sent) drops the record and with it
    // the streak, as with the switch off.
    let t1 = t0 + FIRST_FAILURE_BACKOFF;
    sample(&mut s, a, 3, t1);
    assert!(s.abort(a, 3));
    assert_eq!(s.known(), 0);
}

/// A remembered streak restricts no one: a full table drops it, soonest
/// forgotten first, before refusing a new node, with the split off too.
#[test]
fn a_full_table_drops_a_streak_before_refusing_anyone() {
    let mut s = Sampler::with_capacity(2).with_escalation(true);
    let t0 = Instant::now();
    sample(&mut s, wide(1), 1, t0);
    s.fail(wide(1), 1, FULL, t0);
    let t1 = t0 + Duration::from_secs(1);
    sample(&mut s, wide(2), 2, t1);
    s.fail(wide(2), 2, FULL, t1);
    let t2 = t1 + FIRST_FAILURE_BACKOFF;
    s.prune(t2);
    assert_eq!((s.known(), s.struck()), (2, 2));
    assert!(!s.saturated(), "a streak isn't a restriction");
    sample(&mut s, wide(3), 3, t2);
    assert_eq!((s.known(), s.struck()), (2, 1));
    // wide(1)'s streak, the sooner forgotten, went: its next failure is a
    // first one again. wide(2) still escalates.
    s.complete(wide(3), 3, Duration::from_secs(300), t2);
    let t3 = t2 + Duration::from_secs(300);
    s.prune(t3);
    sample(&mut s, wide(2), 4, t3);
    s.fail(wide(2), 4, FULL, t3);
    assert_eq!(eligible_at(&s, wide(2), t3), t3 + FULL);
    sample(&mut s, wide(1), 5, t3);
    s.fail(wide(1), 5, FULL, t3);
    assert_eq!(eligible_at(&s, wide(1), t3), t3 + FIRST_FAILURE_BACKOFF);
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

/// The memory per entry: the streak is a flag in the table's record, and
/// a remembered one moves from the expiry index to an index of the same
/// size, so it adds nothing per node beyond the record's own growth.
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
    /// Against a model of the rule, under any sequence: each failure's
    /// cooldown is the first backoff after an answer, a first sample or a
    /// lapsed streak, and the full backoff after a failure in a row; with
    /// the switch off always the full backoff. The indexes stay exact.
    #[test]
    fn every_failure_cools_as_the_streak_says(
        escalate in any::<bool>(),
        capacity in 2..24usize,
        ops in proptest::collection::vec(arb_op(), 1..400),
    ) {
        let mut s = Sampler::with_capacity(capacity).with_escalation(escalate);
        let start = Instant::now();
        let mut now = start;
        let mut token = 0u64;
        // In flight: the address, token and when it was reserved.
        let mut flying: Vec<(SocketAddr, u64, Instant)> = vec![];
        // The model: per node, when its last failure's cooldown ended (so
        // when the streak lapses: that plus a full backoff).
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
                    // The table may have dropped the streak for room
                    // (capacity), which only shortens: read what it held,
                    // and check it never held one past its lapse.
                    let held = matches!(s.states.get(&a), Some(State::InFlight { struck: true, .. }));
                    let lapsed = streak.get(&a).is_none_or(|end| reserved >= *end + FULL);
                    prop_assert!(!(held && lapsed), "a streak held past its lapse");
                    // With room for every address nothing is dropped, so
                    // the table holds exactly the model's streak.
                    if capacity > 12 {
                        prop_assert_eq!(held, escalate && !lapsed);
                    }
                    prop_assert!(s.fail(a, t, FULL, now));
                    let want = if escalate && !held { FIRST_FAILURE_BACKOFF } else { FULL };
                    prop_assert_eq!(eligible_at(&s, a, now), now + want);
                    if escalate {
                        streak.insert(a, now + want);
                    }
                }
                Op::Abort(i) if !flying.is_empty() => {
                    let (a, t, _) = flying.remove(usize::from(i) % flying.len());
                    prop_assert!(s.abort(a, t));
                    streak.remove(&a);
                }
                Op::Advance(k) => {
                    now += [Duration::from_secs(1), FIRST_FAILURE_BACKOFF, FULL, FULL + FULL][usize::from(k)];
                    s.prune(now);
                }
                _ => {}
            }
            // Indexes: every Cooling record is in `expiry`, every Struck
            // in `struck`, and the counts add up.
            let cooling = s.states.iter().filter(|(_, st)| matches!(st, State::Cooling { .. })).count();
            let struck = s.states.iter().filter(|(_, st)| matches!(st, State::Struck { .. })).count();
            prop_assert_eq!(s.cooling(), cooling);
            prop_assert_eq!(s.struck(), struck);
            prop_assert_eq!(s.inflight(), flying.len());
            prop_assert!(s.known() <= capacity);
            for (forget, t, a) in &s.struck {
                let exact = matches!(s.states.get(a),
                    Some(State::Struck { forget: f, token: k }) if f == forget && k == t);
                prop_assert!(exact, "a struck entry without its record");
            }
            if !escalate {
                prop_assert_eq!(struck, 0);
            }
        }
    }
}
