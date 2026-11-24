//! X-split-spill-horizon: what the spill filter does at its fallback, how
//! spilled cooldowns age out, and how full it gets over a day of sampling
//! at the rate the 2026-11-23 split lane saw (runner records r17), on
//! tokio's paused clock where time matters.

use std::net::SocketAddr;
use std::time::Duration;

use proptest::prelude::*;
use tokio::time::{Instant, advance};

use super::spill::{self, Spill};
use super::*;
use crate::compact::NodeId;

const MINUTE: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);
const NS: Duration = Duration::from_nanos(1);

/// One address per /24, so the shares never bind.
fn wide(i: u32) -> SocketAddr {
    SocketAddr::from((((0x0B00_0000 + (i << 8)) | 1).to_be_bytes(), 6881))
}

fn node(addr: SocketAddr) -> NodeInfo {
    NodeInfo {
        id: NodeId([1; 20]),
        addr,
    }
}

/// A full sweep pass at `now`.
fn sweep_pass(f: &mut Spill, now: Instant) {
    for _ in 0..spill::SLOTS / spill::SWEEP {
        f.sweep(now);
    }
}

/// A cooldown stops counting towards the fallback in the tick its minute
/// ends, before the sweep reaches its slot, and its slot is reused at once.
#[test]
fn ended_cooldowns_stop_counting_before_they_are_swept() {
    let base = Instant::now();
    let mut f = Spill::new(base);
    let until = base + HOUR;
    for i in 0..10_000 {
        assert!(f.insert(&wide(i), until, base), "{i}");
    }
    assert_eq!(f.live_count(), 10_000);
    f.sweep(base + HOUR - NS);
    assert_eq!(f.live_count(), 10_000, "all still live");
    assert_eq!(f.scan_live(base + HOUR - NS), 10_000);
    // One sweep visits 8,192 of the 8M slots, so most stay in use, but
    // none counts as live.
    let later = until + MINUTE;
    f.sweep(later);
    assert_eq!(f.live_count(), 0);
    assert!(f.used() > 9_000, "not swept yet: {}", f.used());
    assert!(f.has_room());
    sweep_pass(&mut f, later);
    assert_eq!((f.used(), f.live_count()), (0, 0));
    // One /24's two blocks (128 slots) full of live cooldowns refuse the
    // next; once they've ended, unswept, an insert takes an ended slot.
    let net = |i: u16| SocketAddr::from(([12, 0, 0, 1 + (i % 200) as u8], 1000 + i));
    let mut n = 0;
    while f.insert(&net(n), later + HOUR, later) {
        n += 1;
    }
    assert_eq!(usize::from(n), 2 * 64, "both blocks full");
    assert!(!f.insert(&net(n), later + HOUR, later + HOUR - NS));
    let ended = later + HOUR + MINUTE;
    assert!(f.insert(&net(n), ended + HOUR, ended), "an ended slot");
    assert_eq!(f.used(), 128, "reused, not added");
    assert!(f.live_count() >= 1, "never fewer than live");
    f.sweep(ended);
    assert_eq!(f.live_count(), 1, "exact at the sweep");
    assert!(f.contains(&net(n), ended + HOUR - NS));
}

/// The fallback (owner's brief item 1). With the table full and 15/16 of
/// the filter's slots holding live cooldowns, the sampler is saturated: the
/// actor sends no sample (`sample_tick` defers, counting
/// `dht_sample_deferred_total`), and sampler-full time counts. No cooldown
/// is shortened and no node is sampled early: a refusal never frees
/// anything, and every spilled and every listed node stays refused until
/// its own end. Admission comes back when a table place frees or the
/// spilled cooldowns end, whichever is first, and the filter's ended
/// cooldowns count as free from the tick they end, before they're swept.
#[test]
fn at_the_fallback_the_sampler_waits_and_nothing_ends_early() {
    let now = Instant::now();
    let mut s = Sampler::split_with_capacity(4);
    // The table: three cooldowns of 6 h and one of 30 min.
    for i in 0..4u32 {
        let wait = if i == 3 { 30 * MINUTE } else { MAX_INTERVAL };
        assert!(s.reserve(wide(i), u64::from(i) + 1, now));
        assert!(s.complete(wide(i), u64::from(i) + 1, wait, now));
    }
    // The filter: live cooldowns ending in 1 h, up to the fallback.
    let mut f = Spill::new(now);
    let mut i = 100u32;
    while f.live_count() < spill::FALLBACK {
        f.insert(&wide(i), now + HOUR, now);
        i += 1;
    }
    let spilled = i;
    assert!(!f.has_room());
    s.spill = Some(f);
    assert!(s.saturated(), "the fallback: the table alone");
    s.full_time(now, true);
    let t = |d: Duration| now + d;
    assert_eq!(s.full_time(t(MINUTE), true), MINUTE, "counted as full");
    // Nothing is freed: spilled and listed nodes are refused to their end.
    for a in [100, 1_000_000, spilled - 1] {
        assert!(!s.offer(node(wide(a)), t(HOUR - NS)), "spilled {a}");
        assert!(!s.reserve(wide(a), 10, t(HOUR - NS)), "spilled {a}");
    }
    for a in 0..3 {
        assert!(!s.reserve(wide(a), 10, t(MAX_INTERVAL - NS)), "listed {a}");
    }
    assert!(!s.reserve(wide(3), 10, t(30 * MINUTE - NS)), "listed 3");
    // A table place frees first (30 min): admission into it, as with the
    // switch off, while the filter is still at its line.
    s.prune(t(30 * MINUTE));
    assert!(!s.saturated());
    // An address the filter doesn't match (a false positive is a skip).
    let free = |from: u32, s: &Sampler| {
        (from..)
            .map(wide)
            .find(|a| {
                !s.spill
                    .as_ref()
                    .is_some_and(|f| f.contains(a, t(30 * MINUTE)))
            })
            .expect("one")
    };
    let newcomer = free(spilled + 10, &s);
    assert!(s.offer(node(newcomer), t(30 * MINUTE)));
    assert_eq!(s.next(t(30 * MINUTE)), Some(node(newcomer)));
    assert!(s.reserve(newcomer, 11, t(30 * MINUTE)));
    assert!(s.complete(newcomer, 11, MAX_INTERVAL, t(30 * MINUTE)));
    assert!(s.saturated(), "full again");
    // The spilled hour ends: free from that tick, before any slot is swept.
    s.prune(t(HOUR + MINUTE));
    assert!(!s.saturated(), "ended cooldowns free the filter");
    assert_eq!(s.spill_used(), 0, "the gauge reads live cooldowns");
    let next = free(spilled + 1_000, &s);
    assert!(s.offer(node(next), t(HOUR + MINUTE)));
    assert_eq!(s.next(t(HOUR + MINUTE)), Some(node(next)));
    assert!(s.reserve(next, 12, t(HOUR + MINUTE)));
    assert_eq!(s.spill_used(), 1, "a listed cooldown moved out");
    // Listed nodes are still refused to their own end.
    for a in 0..3 {
        assert!(!s.reserve(wide(a), 13, t(MAX_INTERVAL - NS)), "listed {a}");
    }
}

/// One run's trace: samples sent, sampler-full time and the filter's live
/// cooldowns at the end of each hour.
struct Day {
    sent: u64,
    full: Duration,
    live: Vec<usize>,
    used_max: usize,
    known_max: usize,
    bytes: usize,
}

/// Drives the real sampler in the actor's order (prune, account, refill,
/// the tick's budget) for `hours` of paused time at `rate` samples a
/// second. Every sample times out after 4 s and cools 6 h (the longest
/// cooldown at WK-10e scale 1, so the fullest the filter gets at a rate),
/// except every `answered`th, which answers with a 6 h interval at
/// `scale`. Discovery always has new nodes.
async fn day(s: Sampler, rate: usize, hours: u64, answered: u64, scale: u8) -> Day {
    let mut s = s;
    let (mut next, mut token, mut sent) = (0u32, 0u64, 0u64);
    let mut flying: VecDeque<(SocketAddr, u64, Instant)> = VecDeque::new();
    let mut out = Day {
        sent: 0,
        full: Duration::ZERO,
        live: vec![],
        used_max: 0,
        known_max: 0,
        bytes: 0,
    };
    for sec in 1..=hours * 3600 {
        let now = Instant::now();
        while flying
            .front()
            .is_some_and(|(_, _, at)| now.duration_since(*at) >= Duration::from_secs(4))
        {
            let (a, t, _) = flying.pop_front().expect("checked");
            if t % answered == 0 {
                assert!(s.complete_scaled(a, t, MAX_INTERVAL, scale, now));
            } else {
                assert!(s.fail(a, t, MAX_INTERVAL, now));
            }
        }
        s.prune(now);
        s.full_time(now, true);
        while s.frontier_len() < 2 * rate {
            s.offer(node(wide(next)), now);
            next += 1;
        }
        if !s.saturated() {
            for _ in 0..rate {
                let Some(n) = s.next(now) else { break };
                token += 1;
                if s.reserve(n.addr, token, now) {
                    flying.push_back((n.addr, token, now));
                    sent += 1;
                }
            }
        }
        out.used_max = out.used_max.max(s.spill.as_ref().map_or(0, Spill::used));
        out.known_max = out.known_max.max(s.known());
        if sec % 3600 == 0 {
            out.live.push(s.spill_used());
        }
        advance(Duration::from_secs(1)).await;
    }
    out.sent = sent;
    out.full = s.full_time(Instant::now(), true);
    out.bytes = s.spill_counts().1;
    out
}

/// The split-off sampler over the same day: unchanged by this slice. It
/// fills its table and then waits on its cooldowns, as before.
#[tokio::test(start_paused = true)]
async fn the_split_off_day_is_unchanged() {
    let off = day(Sampler::with_capacity(2_000), 100, 24, u64::MAX, 1).await;
    // The table fills in 20 s; each place frees 6 h after it filled, so
    // the run sends one table's worth every 6 h.
    assert_eq!(off.sent, 4 * 2_000, "one table a cooldown");
    assert!(
        off.full >= Duration::from_secs(24 * 3600 - 4 * 60),
        "full nearly all day: {:?}",
        off.full
    );
    assert_eq!((off.used_max, off.bytes), (0, 0), "no filter");
    assert!(off.live.iter().all(|n| *n == 0));
}

/// Owner's brief items 2, 3 and 5: a day (24 h of paused time) at r17's
/// rate, about 360k cooldowns an hour moving to the filter, never reaches
/// the fallback. The filter grows for one cooldown length (6 h) and then
/// holds level, since every cooldown that moves in ends within 6 h and
/// stops counting when it does; it never comes near 15/16 of its slots.
#[tokio::test(start_paused = true)]
async fn a_day_at_r17s_rate_never_reaches_the_fallback() {
    let rate = 100;
    let on = day(Sampler::split_with_capacity(2_000), rate, 24, u64::MAX, 1).await;
    assert_eq!(on.full, Duration::ZERO, "never full");
    assert!(
        on.sent >= (rate as u64) * 24 * 3600 - 2 * rate as u64,
        "the full rate all day: {}",
        on.sent
    );
    // Live: the last 6 h of samples, short of the table's 2,000.
    let six_hours = rate * 6 * 3600;
    let peak = *on.live.iter().max().expect("24 hours");
    // At most the last 6 h of samples, and the minute each end is rounded
    // up to.
    assert!(peak <= six_hours + rate * 60, "{peak} live at most");
    assert!(
        peak + 2_000 + rate * 60 >= six_hours,
        "{peak} live at least"
    );
    assert!(peak < spill::FALLBACK / 3, "{peak} far from the fallback");
    // Level from the seventh hour on.
    for (h, n) in on.live.iter().enumerate().skip(6) {
        assert!(
            n.abs_diff(peak) <= rate * 120,
            "hour {}: {n} of {peak}",
            h + 1
        );
    }
    // In use, ended slots included: at most a sweep pass (17 min) more.
    assert!(
        on.used_max <= peak + rate * 18 * 60,
        "{} slots in use",
        on.used_max
    );
    assert_eq!(on.bytes, spill::BYTES);
}

/// The memory figures for the decisions file, at the production table
/// (200,000 places): r17's rate, twice it, and WK-10e's scale 8 at both
/// (a fifth answered, cooling 48 h). Release only, about half an hour:
/// `cargo test -p lodestar-dht --release --lib horizon_tests::steady_state_memory -- --ignored --nocapture`
#[tokio::test(start_paused = true)]
#[ignore = "about 50 million samples; run in release"]
async fn steady_state_memory() {
    for (rate, hours, answered, scale) in [
        (100, 24, u64::MAX, 1),
        (200, 24, u64::MAX, 1),
        (100, 60, 5, 8),
        (200, 60, 5, 8),
    ] {
        let d = day(Sampler::new(true), rate, hours, answered, scale).await;
        let peak = d.live.iter().max().copied().unwrap_or(0);
        println!(
            "rate {rate}/s scale {scale} over {hours} h: sent {} full {} s; live by hour {:?}; peak live {peak}, peak in use {}, table {}, filter bytes {}",
            d.sent,
            d.full.as_secs(),
            d.live,
            d.used_max,
            d.known_max,
            d.bytes
        );
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// Insert address `i` cooling for `secs`.
    Insert(u16, u32),
    Advance(u32),
    Sweep(u16),
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..600u16, 1..(48 * 3600u32)).prop_map(|(i, s)| Op::Insert(i, s)),
        3 => (0..(6 * 3600u32)).prop_map(Op::Advance),
        2 => (1..64u16).prop_map(Op::Sweep),
    ]
}

/// A few hundred addresses, many to a /24, so both of a /24's blocks fill
/// and inserts get refused.
fn crowded(i: u16) -> SocketAddr {
    SocketAddr::from(([11, (i / 300) as u8, (i % 300 / 2) as u8, 1], 1000 + i))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]
    /// Ageing never frees a live cooldown and counts exactly: under any
    /// mix of inserts, steps of time and sweeps, every stored cooldown is
    /// found until its own end, the live count after a sweep is exactly
    /// the stored cooldowns not yet ended (never fewer at any time), and a
    /// full pass leaves no ended slot. Time passes in steps of up to 6 h,
    /// with a pass at least every 20 h as the 1 s tick gives (17 minutes).
    #[test]
    fn ageing_never_frees_a_live_cooldown_and_counts_exactly(
        ops in proptest::collection::vec(arb_op(), 1..200),
    ) {
        let base = Instant::now();
        let mut now = base;
        let mut since_pass = Duration::ZERO;
        let mut f = Spill::new(base);
        // Every stored cooldown, by its true end and its stored minute's.
        let mut stored: Vec<(SocketAddr, Instant, Instant)> = vec![];
        let minute = |t: Instant| -> Instant {
            let m = (t - base).as_nanos().div_ceil(MINUTE.as_nanos());
            base + MINUTE * u32::try_from(m).expect("in range")
        };
        for op in ops {
            match op {
                Op::Insert(i, secs) => {
                    let until = now + Duration::from_secs(u64::from(secs));
                    if f.insert(&crowded(i), until, now) {
                        stored.push((crowded(i), until, minute(until)));
                    }
                }
                Op::Advance(secs) => {
                    let step = Duration::from_secs(u64::from(secs));
                    if since_pass + step > 20 * HOUR {
                        sweep_pass(&mut f, now);
                        since_pass = Duration::ZERO;
                    }
                    now += step;
                    since_pass += step;
                }
                Op::Sweep(n) => {
                    for _ in 0..n {
                        f.sweep(now);
                    }
                    let truly = stored.iter().filter(|(_, _, m)| *m > now).count();
                    prop_assert_eq!(f.live_count(), truly);
                }
            }
            let truly = stored.iter().filter(|(_, _, m)| *m > now).count();
            prop_assert!(f.live_count() >= truly, "{} < {truly}", f.live_count());
            prop_assert!(f.used() >= truly);
            for (a, until, _) in &stored {
                if *until > now {
                    prop_assert!(f.contains(a, now), "{a} freed before its end");
                }
            }
        }
        sweep_pass(&mut f, now);
        let truly = stored.iter().filter(|(_, _, m)| *m > now).count();
        prop_assert_eq!((f.used(), f.live_count()), (truly, truly));
        prop_assert_eq!(f.scan_live(now), truly);
    }
}
