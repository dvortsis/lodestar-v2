//! X-sampler-cooldown-split: the spill filter, the split sampler, the
//! switch-off equivalence and the 2026-11-16 failure, on tokio's paused
//! clock where time matters.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use proptest::prelude::*;
use tokio::time::{Instant, advance};

use super::spill::{self, Spill};
use super::*;
use crate::compact::NodeId;

const MINUTE: Duration = Duration::from_secs(60);
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

/// The filter's snapshot section at `now`, as (block, word).
fn exported(f: &Spill, now: Instant, horizon: Duration) -> Vec<(usize, u32)> {
    f.export(now, horizon).map_or_else(Vec::new, |s| {
        crate::restart::Spilled::words(&s.raw).collect()
    })
}

#[test]
fn a_spilled_cooldown_never_ends_early_and_ends_within_a_minute() {
    let base = Instant::now();
    for secs in [1u64, 59, 60, 61, 3599, 21_600, 48 * 3600] {
        let mut f = Spill::new(base);
        let now = base + Duration::from_millis(12_345);
        let until = now + Duration::from_secs(secs);
        assert!(f.insert(&wide(1), until, now), "{secs}");
        assert!(f.contains(&wide(1), now), "{secs}");
        assert!(f.contains(&wide(1), until - NS), "{secs}");
        assert!(!f.contains(&wide(1), until + MINUTE), "{secs}");
        // The restart snapshot keeps it to at least its end, and less
        // than two minutes past it (the filter's minute, then the
        // snapshot's).
        let words = exported(&f, now, Duration::from_secs(49 * 3600));
        assert_eq!(words.len(), 1, "{secs}");
        let kept = Duration::from_secs(u64::from(words[0].1 & 0xFFF) * 60);
        assert!(
            kept >= until - now && kept < until - now + 2 * MINUTE,
            "{secs}"
        );
        assert!(f.export(until + MINUTE, MINUTE).is_none(), "{secs}");
    }
    let mut f = Spill::new(base);
    // Past the horizon: refused, so it stays in the table.
    let far = base + MINUTE * (spill::HORIZON_MIN + 1);
    assert!(!f.insert(&wide(1), far, base));
    // Already ended: nothing to keep, nothing allocated.
    assert!(f.insert(&wide(1), base, base));
    assert_eq!(f.counts(), (0, 0));
    assert!(!f.contains(&wide(1), base));
}

/// The restart snapshot's section, through its encoding and seeding, on a
/// run whose clock reads `now` (no time between the two runs).
fn seeded(sp: crate::restart::Spilled, now: Instant, h: Duration) -> crate::restart::SeededSpill {
    let horizon_s = crate::restart::horizon_s(h);
    let wall = Duration::from_secs(1_700_000_000);
    let at_ms = crate::restart::floor_ms(wall);
    let snap = crate::restart::Snapshot {
        run: crate::restart::RunHeader {
            generation: 1,
            dir_id: [1; 16],
            ids: [2; 32],
            run_id: [3; 16],
        },
        at_ms,
        horizon_s,
        gate_s: 0,
        entries: vec![],
        spill: Some(sp),
    };
    let (snap, _) = crate::restart::Snapshot::decode(&snap.encode()).expect("decodes");
    let here = crate::restart::Here {
        path: String::new(),
        dir_path: [0; 32],
        ids: [2; 32],
        horizon_s,
        wall0: wall,
        mono0: now,
    };
    crate::restart::seed(&snap, at_ms, &here)
        .expect("seeds")
        .spill
        .expect("a section")
}

/// The filter's hash is SipHash-2-4 with the reference vectors, so the
/// next binary places a restored slot where this one looks for it.
#[test]
fn siphash_is_the_reference_function() {
    let keys = [0x0706_0504_0302_0100, 0x0f0e_0d0c_0b0a_0908];
    assert_eq!(spill::siphash(keys, &[]), 0x726f_db47_dd0e_0e31);
    let msg: Vec<u8> = (0..15).collect();
    assert_eq!(spill::siphash(keys, &msg), 0xa129_ca61_49be_45e5);
}

/// A restart keeps every spilled cooldown to its end (capped at the
/// horizon, as listed ones are) and every share it holds.
#[test]
fn the_filter_survives_a_restart_whole() {
    let now = Instant::now();
    let h = MAX_INTERVAL + Duration::from_secs(4);
    let mut f = Spill::new(now);
    let crowd = |i: u32| SocketAddr::from(([10, 1, 2, i as u8], 6000 + i as u16));
    let mut ends = Vec::new();
    for i in 0..40u32 {
        for a in [wide(i), crowd(i + 1)] {
            let until = now + Duration::from_secs(60 + 517 * u64::from(i));
            assert!(f.insert(&a, until, now));
            ends.push((a, until));
        }
    }
    let far = (wide(1000), now + Duration::from_secs(10 * 3600));
    assert!(f.insert(&far.0, far.1, now));
    ends.push(far);
    let sp = seeded(f.export(now, h).expect("live slots"), now, h);
    let g = Spill::restore(&sp, now).expect("rebuilt");
    let sorted = |f: &Spill| {
        let mut w = exported(f, now, h);
        w.sort_unstable();
        w
    };
    assert_eq!(sorted(&g), sorted(&f), "the same slots, the same minutes");
    for (a, until) in &ends {
        let kept = (*until).min(now + h);
        assert!(g.contains(a, kept - NS), "{a} ended early");
        assert_eq!(g.shares(a, now), f.shares(a, now), "{a}");
        // Past its end it answers as the old filter did (a crowded block
        // can misfire, the same way in both).
        for t in [*until + MINUTE, *until + 2 * MINUTE] {
            if *until <= now + h {
                assert_eq!(g.contains(a, t), f.contains(a, t), "{a}");
            }
        }
    }
    assert!(g.shares(&crowd(1), now).1 >= 40, "the /24 keeps its share");
}

/// If the section can't join the filter (one is already in use), the
/// start holds every sample until the last restored end instead.
#[test]
fn a_filter_that_cant_be_rebuilt_holds_until_its_last_end() {
    let now = Instant::now();
    let h = MAX_INTERVAL + Duration::from_secs(4);
    let mut f = Spill::new(now);
    assert!(f.insert(&wide(1), now + Duration::from_secs(600), now));
    assert!(f.insert(&wide(2), now + Duration::from_secs(1200), now));
    let sp = seeded(f.export(now, h).expect("live"), now, h);
    let mut s = Sampler::split_with_capacity(8);
    assert_eq!(s.restore_spill(&sp, now), None);
    let hold = s.restore_spill(&sp, now).expect("held");
    assert!(hold >= now + Duration::from_secs(1200));
    assert!(hold < now + Duration::from_secs(1200) + MINUTE);
}

#[test]
fn a_spilled_address_cools_until_its_later_end() {
    let now = Instant::now();
    let long = now + Duration::from_secs(7200);
    let short = now + Duration::from_secs(600);
    let mut f = Spill::new(now);
    assert!(f.insert(&wide(1), long, now));
    assert!(f.insert(&wide(1), short, now));
    assert!(
        f.contains(&wide(1), long - NS),
        "a shorter one can't cut it"
    );
    assert_eq!(f.counts().0, 2, "each cooldown has its own slot");
    let mut f = Spill::new(now);
    assert!(f.insert(&wide(1), short, now));
    assert!(f.insert(&wide(1), long, now));
    assert!(f.contains(&wide(1), long - NS), "the longer one holds");
    assert_eq!(f.counts().0, 2);
}

/// The bound, measured: the filter holds its slots and nothing else, takes
/// half of them (Balanced's steady state) without a refusal and nine in ten
/// with few, keeps every key, and rarely answers "maybe" for a stranger.
/// Ended slots come back.
#[test]
fn the_filter_is_bounded_keeps_every_key_and_rarely_misfires() {
    let now = Instant::now();
    let until = now + MAX_INTERVAL;
    let mut f = Spill::new(now);
    let half = spill::SLOTS as u32 / 2;
    for i in 0..half {
        assert!(f.insert(&wide(i), until, now), "refused at {i}");
    }
    let most = spill::SLOTS as u32 / 10 * 9;
    let refused = (half..most)
        .filter(|i| !f.insert(&wide(*i), until, now))
        .count();
    assert!(
        refused * 100 < most as usize,
        "{refused} of {most} refused at 90 %"
    );
    let (used, bytes) = f.counts();
    assert_eq!(bytes, spill::BYTES, "the slots, allocated once");
    assert_eq!(spill::BYTES, 32 << 20);
    // Two addresses sharing a group and a fingerprint share a slot (the
    // later end): rare, and only ever a longer cooldown.
    let stored = most as usize - refused;
    assert!(
        used <= stored && used * 1000 > stored * 999,
        "{used} of {stored}"
    );
    for i in (0..half).step_by(31) {
        assert!(f.contains(&wide(i), until - NS), "{i}");
    }
    let strangers = 200_000;
    let misfires = (0..strangers)
        .filter(|i| f.contains(&wide(most + i), now))
        .count();
    assert!(
        misfires * 1000 < strangers as usize,
        "{misfires} false positives"
    );
    // Ended slots are cleared within one pass of sweeps.
    let later = until + MINUTE;
    for _ in 0..spill::SLOTS / spill::SWEEP {
        f.sweep(later);
    }
    assert_eq!(f.counts(), (0, spill::BYTES));
    assert!(f.insert(&wide(1), later + MINUTE, later), "room again");
}

/// With the switch on, a table full of cooldowns still admits a new node;
/// every cooled node stays refused until its own end.
#[test]
fn a_full_split_table_keeps_admitting_and_refuses_cooled_nodes() {
    let mut s = Sampler::split_with_capacity(4);
    let now = Instant::now();
    for i in 1..=4 {
        assert!(s.reserve(wide(i), u64::from(i), now));
        assert!(s.complete_scaled(wide(i), u64::from(i), MAX_INTERVAL, MAX_SCALE, now));
    }
    assert_eq!(s.known(), 4);
    assert!(!s.saturated(), "a cooldown can move out");
    assert!(s.offer(node(wide(9)), now));
    assert_eq!(s.next(now), Some(node(wide(9))));
    assert!(s.reserve(wide(9), 9, now));
    assert_eq!((s.known(), s.inflight()), (4, 1), "the table stays at 4");
    let end = now + MAX_INTERVAL * u32::from(MAX_SCALE);
    for i in 1..=4 {
        assert!(!s.offer(node(wide(i)), now), "{i} is cooling");
        assert!(!s.reserve(wide(i), 100 + u64::from(i), end - NS), "{i}");
    }
    // Free again at the end, give or take the filter's minute.
    s.prune(end + MINUTE);
    assert!(s.offer(node(wide(1)), end + MINUTE));
    assert!(s.next(end + MINUTE).is_some());
}

/// Switch off, the same full table refuses as it always has.
#[test]
fn a_full_table_with_the_switch_off_waits_as_before() {
    let mut s = Sampler::with_capacity(4);
    let now = Instant::now();
    for i in 1..=4 {
        assert!(s.reserve(wide(i), u64::from(i), now));
        assert!(s.complete(wide(i), u64::from(i), MAX_INTERVAL, now));
    }
    assert!(s.saturated());
    assert!(s.offer(node(wide(9)), now));
    assert_eq!(s.next(now), None);
    assert!(!s.reserve(wide(9), 9, now));
    assert_eq!(s.spill_counts(), (0, 0), "no filter allocated");
}

/// A restored record that moves out counts as honoured: the filter keeps it
/// to its end.
#[test]
fn a_restored_record_that_moves_out_is_honoured() {
    let mut s = Sampler::split_with_capacity(1);
    let now = Instant::now();
    s.restore(wide(1), now + Duration::from_secs(600));
    assert_eq!(s.restored_counts(), (1, 0, 0));
    assert!(s.reserve(wide(2), 2, now));
    assert_eq!(s.restored_counts(), (0, 0, 1));
    assert!(!s.reserve(wide(1), 3, now + Duration::from_secs(600) - NS));
}

/// When even the filter is full the table keeps the cooldown: never
/// shortened, and the sampler reports itself full, as with the switch off.
#[test]
fn a_full_filter_leaves_the_table_full() {
    let mut s = Sampler::split_with_capacity(1);
    let now = Instant::now();
    assert!(s.reserve(wide(1), 1, now));
    // Past the filter's horizon: it can't take it.
    let past = MINUTE * (spill::HORIZON_MIN + 10);
    s.put(
        wide(1),
        State::Cooling {
            until: now + past,
            token: 1,
            struck: false,
            reason: CoolingReason::Interval,
        },
    );
    s.expiry.insert((now + past, 1, wide(1)));
    assert!(s.offer(node(wide(2)), now));
    assert_eq!(s.next(now), None);
    assert!(!s.reserve(wide(2), 2, now));
    assert!(!s.reserve(wide(1), 3, now + past - NS));
    assert_eq!(s.known(), 1);
}

/// An ended slot left unswept reads as cooling again from about 20 h after
/// it ends (here 4,096 minutes on), which only skips the node; one sweep
/// pass frees it.
#[test]
fn a_wrapped_slot_only_skips_until_swept() {
    let base = Instant::now();
    let mut f = Spill::new(base);
    let a = wide(7);
    assert!(f.insert(&a, base + Duration::from_secs(600), base));
    assert!(!f.contains(&a, base + Duration::from_secs(660)));
    // 4,096 minutes on, the stored minute aliases as live.
    let wrapped = base + Duration::from_secs(4096 * 60 + 300);
    assert!(
        f.contains(&a, wrapped),
        "a false positive, never a false negative"
    );
    for _ in 0..spill::SLOTS / spill::SWEEP {
        f.sweep(base + Duration::from_secs(660));
    }
    assert!(!f.contains(&a, wrapped), "swept");
    assert_eq!(f.counts().0, 0);
}

/// Sampler-full time counts only while sampling is allowed, and only while
/// the sampler is full.
#[test]
fn full_time_counts_only_full_and_allowed_time() {
    let mut s = Sampler::with_capacity(1);
    let now = Instant::now();
    assert_eq!(s.full_time(now, true), Duration::ZERO);
    let t = |secs| now + Duration::from_secs(secs);
    assert_eq!(s.full_time(t(5), true), Duration::ZERO, "not full");
    assert!(s.reserve(wide(1), 1, t(5)));
    assert!(s.saturated());
    assert_eq!(s.full_time(t(8), true), Duration::from_secs(3));
    assert_eq!(s.full_time(t(20), false), Duration::from_secs(3), "gated");
    assert_eq!(s.full_time(t(21), true), Duration::from_secs(4));
    // The split sampler, with a cooldown it can move, isn't full.
    let mut on = Sampler::split_with_capacity(1);
    assert!(on.reserve(wide(1), 1, now));
    assert!(on.saturated(), "an in-flight sample can't move");
    assert!(on.complete(wide(1), 1, MAX_INTERVAL, now));
    assert!(!on.saturated());
    on.full_time(now, true);
    assert_eq!(on.full_time(t(9), true), Duration::ZERO);
}

/// Samples sent to the addresses `hostile` picks, and in all, over 2 h of
/// a full table of 6 h timeouts: each second offers 5 new hostile and 5
/// new ordinary nodes and sends up to 10 samples.
fn source_samples(split: bool, hostile: impl Fn(u32) -> SocketAddr) -> (u64, u64) {
    let mut s = if split {
        Sampler::split_with_capacity(50)
    } else {
        Sampler::with_capacity(50)
    };
    let start = Instant::now();
    let (mut flying, mut token, mut sent, mut theirs, mut n) = (vec![], 0u64, 0, 0, 0u32);
    for sec in 0..2 * 3600u64 {
        let now = start + Duration::from_secs(sec);
        let (done, still): (Vec<_>, Vec<_>) =
            flying
                .drain(..)
                .partition(|(_, _, at): &(SocketAddr, u64, Instant)| {
                    now.duration_since(*at).as_secs() >= 4
                });
        flying = still;
        for (a, t, _) in done {
            assert!(s.complete(a, t, MAX_INTERVAL, now));
        }
        s.prune(now);
        for _ in 0..5 {
            s.offer(node(hostile(n)), now);
            s.offer(node(wide(n)), now);
            n += 1;
        }
        for _ in 0..10 {
            let Some(x) = s.next(now) else { break };
            token += 1;
            if s.reserve(x.addr, token, now) {
                flying.push((x.addr, token, now));
                sent += 1;
                if x.addr.ip() != wide(0).ip() && !x.addr.to_string().starts_with("11.") {
                    theirs += 1;
                }
            }
        }
    }
    (theirs, sent)
}

/// The runner's I1: with the switch on and the table full, one IP with
/// many ports, one IPv4 /24, one IPv6 /64 and one /48 each get no more
/// samples per cooldown than with the switch off: moved cooldowns still
/// hold their shares.
#[test]
fn moved_cooldowns_still_hold_their_sources_shares() {
    let one_ip = |i: u32| SocketAddr::from(([10, 9, 9, 9], 1 + (i % 60_000) as u16));
    let one_24 = |i: u32| SocketAddr::from(([10, 9, 9, 1 + (i % 250) as u8], 1 + (i / 250) as u16));
    let one_64 = |i: u32| {
        SocketAddr::from((
            [
                0x2001,
                0xdb8,
                9,
                9,
                0,
                0,
                (i >> 8) as u16,
                1 + (i & 0xFF) as u16,
            ],
            6881,
        ))
    };
    let one_48 =
        |i: u32| SocketAddr::from(([0x2001, 0xdb8, 9, (i % 5000) as u16, 0, 0, 0, 1], 6881));
    for (name, cap, f) in [
        ("one IP", MAX_PER_IP, &one_ip as &dyn Fn(u32) -> SocketAddr),
        ("one /24", MAX_PER_V4_NETWORK, &one_24),
        ("one /64", MAX_PER_V6_NETWORK, &one_64),
        ("one /48", MAX_PER_V6_ALLOCATION, &one_48),
    ] {
        let (off, off_all) = source_samples(false, f);
        let (on, on_all) = source_samples(true, f);
        assert!(off <= u64::from(cap), "{name}: off {off}");
        assert!(on <= u64::from(cap), "{name}: on {on} > {cap}");
        assert!(on <= off.max(u64::from(cap)), "{name}: on {on}, off {off}");
        assert!(
            on_all > 10 * off_all,
            "{name}: the split still samples others"
        );
    }
}

/// Off against on, while the table never fills, every call answers the same.
#[derive(Debug, Clone)]
enum Eq {
    Offer(u16),
    Next,
    Complete(u8, u16),
    Abort(u8),
    Advance(u16),
}

fn arb_eq() -> impl Strategy<Value = Eq> {
    prop_oneof![
        4 => (0..300u16).prop_map(Eq::Offer),
        3 => Just(Eq::Next),
        2 => (any::<u8>(), any::<u16>()).prop_map(|(i, w)| Eq::Complete(i, w)),
        1 => any::<u8>().prop_map(Eq::Abort),
        1 => any::<u16>().prop_map(Eq::Advance),
    ]
}

/// Many addresses per /24 and per IP, so the shares do bind.
fn crowded(i: u16) -> SocketAddr {
    SocketAddr::from(([10, 0, (i % 3) as u8, (i % 97) as u8], 1 + i % 11))
}

proptest! {
    #[test]
    fn below_capacity_the_switch_changes_nothing(ops in proptest::collection::vec(arb_eq(), 1..400)) {
        let mut off = Sampler::new(false);
        let mut on = Sampler::new(true);
        let mut now = Instant::now();
        let mut token = 0;
        let mut flying: Vec<(SocketAddr, u64)> = vec![];
        for op in ops {
            match op {
                Eq::Offer(i) => {
                    prop_assert_eq!(off.offer(node(crowded(i)), now), on.offer(node(crowded(i)), now));
                }
                Eq::Next => {
                    let n = off.next(now);
                    prop_assert_eq!(n, on.next(now));
                    if let Some(n) = n {
                        token += 1;
                        let r = off.reserve(n.addr, token, now);
                        prop_assert_eq!(r, on.reserve(n.addr, token, now));
                        if r {
                            flying.push((n.addr, token));
                        }
                    }
                }
                Eq::Complete(i, w) if !flying.is_empty() => {
                    let (a, t) = flying.remove(usize::from(i) % flying.len());
                    let wait = Duration::from_secs(u64::from(w));
                    let scale = i % 10;
                    prop_assert_eq!(
                        off.complete_scaled(a, t, wait, scale, now),
                        on.complete_scaled(a, t, wait, scale, now)
                    );
                }
                Eq::Abort(i) if !flying.is_empty() => {
                    let (a, t) = flying.remove(usize::from(i) % flying.len());
                    prop_assert_eq!(off.abort(a, t), on.abort(a, t));
                }
                Eq::Advance(secs) => {
                    now += Duration::from_secs(u64::from(secs));
                    off.prune(now);
                    on.prune(now);
                }
                _ => {}
            }
            prop_assert_eq!(
                (off.known(), off.cooling(), off.frontier_len(), off.saturated()),
                (on.known(), on.cooling(), on.frontier_len(), on.saturated())
            );
            let a: HashMap<_, _> = off.restrictions(now).collect();
            let b: HashMap<_, _> = on.restrictions(now).collect();
            prop_assert_eq!(a, b);
            prop_assert_eq!(on.spill_counts(), (0, 0), "nothing spilled");
        }
    }
}

/// Hostile or ordinary, any sequence of offers, samples, answers, timeouts,
/// restarts and time never samples a node before its cooldown ends.
#[derive(Debug, Clone)]
enum Op {
    Offer(u16),
    Next,
    /// Try a node directly, as a spoofed or duplicate contact would.
    Reserve(u16),
    Complete(u8, u8, u8),
    /// A timeout or error reply (CR-SAMPLE-BACKOFF-ESCALATE's `fail`).
    Fail(u8),
    Abort(u8),
    Advance(u16),
    Prune,
    Restart,
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..400u16).prop_map(Op::Offer),
        4 => Just(Op::Next),
        2 => (0..400u16).prop_map(Op::Reserve),
        3 => (any::<u8>(), 0..5u8, 0..10u8).prop_map(|(i, w, s)| Op::Complete(i, w, s)),
        2 => any::<u8>().prop_map(Op::Fail),
        1 => any::<u8>().prop_map(Op::Abort),
        2 => (0..10_800u16).prop_map(Op::Advance),
        2 => Just(Op::Prune),
        1 => Just(Op::Restart),
    ]
}

/// An address of a few hundred, a few to a /24.
fn few(i: u16) -> SocketAddr {
    SocketAddr::from((
        [11, (i / 50) as u8, (i % 50) as u8, 1 + (i % 7) as u8],
        6881,
    ))
}

/// What the restart proof does with the sampler at shutdown and start
/// (proof.rs): seal what's in flight (the caller), list the table, keep
/// the filter, and restore both into a fresh sampler through the
/// snapshot's encoding and seeding, with no time between. Returns the
/// hold the start must keep: none, unless the filter couldn't be rebuilt.
fn restart(
    s: &Sampler,
    now: Instant,
    h: Duration,
    cap: usize,
    escalate: bool,
) -> (Sampler, Option<Instant>) {
    let horizon_s = crate::restart::horizon_s(h);
    let wall = Duration::from_secs(1_700_000_000);
    let run = crate::restart::RunHeader {
        generation: 1,
        dir_id: [1; 16],
        ids: [2; 32],
        run_id: [3; 16],
    };
    let snap = crate::restart::capture_with_spill(
        run,
        s.restrictions(now).collect::<Vec<_>>(),
        None,
        s.spill_export(now, h),
        now,
        wall,
        horizon_s,
    )
    .expect("the sampler's state always captures");
    let (snap, _) = crate::restart::Snapshot::decode(&snap.encode()).expect("decodes");
    let here = crate::restart::Here {
        path: String::new(),
        dir_path: [0; 32],
        ids: [2; 32],
        horizon_s,
        wall0: wall,
        mono0: now,
    };
    let seeded = crate::restart::seed(&snap, crate::restart::floor_ms(wall), &here).expect("seeds");
    let mut next = Sampler::split_with_capacity(cap).with_escalation(escalate);
    for (a, until) in seeded.entries {
        next.restore(a, until);
    }
    let hold = seeded.spill.and_then(|sp| next.restore_spill(&sp, now));
    (next, hold)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]
    #[test]
    fn no_node_is_sampled_in_cooldown(
        cap in 1..6usize,
        escalate in any::<bool>(),
        ops in proptest::collection::vec(arb_op(), 1..300),
    ) {
        let h = MAX_INTERVAL + Duration::from_secs(4);
        let mut s = Sampler::split_with_capacity(cap).with_escalation(escalate);
        let mut now = Instant::now();
        let mut token = 0u64;
        let mut flying: Vec<(SocketAddr, u64)> = vec![];
        // The true end of each node's cooldown, and whether it came from a
        // sample's answer (a wire token, which the snapshot may bound to
        // `MAX_INTERVAL` ahead, WK-10e) or from the proof.
        let mut truth: HashMap<SocketAddr, (Instant, bool)> = HashMap::new();
        let free = |truth: &HashMap<SocketAddr, (Instant, bool)>, a: &SocketAddr, now: Instant| {
            truth.get(a).is_none_or(|(end, _)| *end <= now)
        };
        for op in ops {
            match op {
                Op::Offer(i) => {
                    s.offer(node(few(i)), now);
                }
                Op::Next => {
                    if let Some(n) = s.next(now) {
                        token += 1;
                        if s.reserve(n.addr, token, now) {
                            prop_assert!(free(&truth, &n.addr, now), "{} sampled early", n.addr);
                            flying.push((n.addr, token));
                        }
                    }
                }
                Op::Reserve(i) => {
                    token += 1;
                    if s.reserve(few(i), token, now) {
                        prop_assert!(free(&truth, &few(i), now), "{} reserved early", few(i));
                        flying.push((few(i), token));
                    }
                }
                Op::Complete(i, w, scale) if !flying.is_empty() => {
                    let (a, t) = flying.remove(usize::from(i) % flying.len());
                    let wait = [0, 60, 900, 21_600, 99_999][usize::from(w)];
                    let wait = Duration::from_secs(wait);
                    prop_assert!(s.complete_scaled(a, t, wait, scale, now));
                    let end = now
                        + wait
                            .clamp(MIN_INTERVAL, MAX_INTERVAL)
                            .saturating_mul(u32::from(scale.clamp(1, MAX_SCALE)));
                    truth.insert(a, (end, true));
                }
                Op::Fail(i) if !flying.is_empty() => {
                    let (a, t) = flying.remove(usize::from(i) % flying.len());
                    if i % 2 == 0 {
                        prop_assert!(s.fail(a, t, MAX_INTERVAL, now));
                    } else {
                        prop_assert!(s.fail_reply(a, t, MAX_INTERVAL, now));
                    }
                    // With escalation on, at least the first failure's
                    // backoff (exactly so in escalate_tests); off, in full.
                    let end = now + if escalate { FIRST_FAILURE_BACKOFF } else { MAX_INTERVAL };
                    truth.insert(a, (end, true));
                }
                Op::Abort(i) if !flying.is_empty() => {
                    let (a, t) = flying.remove(usize::from(i) % flying.len());
                    prop_assert!(s.abort(a, t));
                }
                Op::Advance(secs) => now += Duration::from_secs(u64::from(secs)),
                Op::Prune => s.prune(now),
                Op::Restart => {
                    // S8: in flight → now + H.
                    s.seal_in_flight(now + h);
                    // The sealed sampler's counts, before it's replaced.
                    prop_assert_eq!(s.cooling_by_reason(), s.recount_cooling());
                    for (a, _) in flying.drain(..) {
                        truth.insert(a, (now + h, false));
                    }
                    // The snapshot bounds a sample's cooldown (WK-10e).
                    for (end, wire) in truth.values_mut() {
                        if *wire {
                            *end = (*end).min(now + MAX_INTERVAL);
                        }
                        *wire = false;
                    }
                    let (next, hold) = restart(&s, now, h, cap, escalate);
                    s = next;
                    // X-split-resume-gate: the filter comes back whole, so
                    // nothing holds the next run.
                    prop_assert_eq!(hold, None, "the filter was rebuilt");
                }
                _ => {}
            }
            prop_assert!(s.known() <= cap, "the table holds at most its capacity");
            prop_assert_eq!(s.cooling_by_reason(), s.recount_cooling());
            prop_assert!(s.spill_counts().1 <= spill::BYTES);
        }
    }
}

/// One tick of a sampler loop shaped like the actor's: prune, account, then
/// send this tick's budget to whoever is next, where nothing ever answers
/// (each sample times out after 4 s and cools 6 h, as on 2026-11-16).
struct Crawl {
    s: Sampler,
    next_addr: u32,
    flying: Vec<(SocketAddr, u64, Instant)>,
    token: u64,
    sent: u64,
}

impl Crawl {
    fn new(s: Sampler) -> Crawl {
        Crawl {
            s,
            next_addr: 0,
            flying: vec![],
            token: 0,
            sent: 0,
        }
    }

    fn tick(&mut self, budget: usize) {
        let now = Instant::now();
        let timeout = Duration::from_secs(4);
        let (done, still): (Vec<_>, Vec<_>) = self
            .flying
            .drain(..)
            .partition(|(_, _, sent)| now.duration_since(*sent) >= timeout);
        self.flying = still;
        for (a, t, _) in done {
            assert!(self.s.complete(a, t, MAX_INTERVAL, now));
        }
        self.s.prune(now);
        self.s.full_time(now, true);
        // Discovery always has new nodes to offer.
        while self.s.frontier_len() < 2 * budget {
            self.s.offer(node(wide(self.next_addr)), now);
            self.next_addr += 1;
        }
        if self.s.saturated() {
            return;
        }
        for _ in 0..budget {
            let Some(n) = self.s.next(now) else { break };
            self.token += 1;
            if self.s.reserve(n.addr, self.token, now) {
                self.flying.push((n.addr, self.token, now));
                self.sent += 1;
            }
        }
    }
}

/// Runs both arms for `hours` of paused time; returns (sent, full time).
async fn crawl(cap: usize, budget: usize, hours: u64) -> [(u64, Duration); 2] {
    let mut off = Crawl::new(Sampler::with_capacity(cap));
    let mut on = Crawl::new(Sampler::split_with_capacity(cap));
    for _ in 0..hours * 3600 {
        off.tick(budget);
        on.tick(budget);
        advance(Duration::from_secs(1)).await;
    }
    let now = Instant::now();
    [
        (off.sent, off.s.full_time(now, true)),
        (on.sent, on.s.full_time(now, true)),
    ]
}

/// The 2026-11-16 failure, scaled down (a 2,000-place table, 10 samples a
/// second, 3 h): switch off, the table fills with 6 h timeout cooldowns in
/// about 200 s and sampling stops for the rest of the run; switch on,
/// sampling carries on at the full rate and the sampler is never full.
#[tokio::test(start_paused = true)]
async fn a_table_of_timeouts_idles_the_sampler_off_but_not_on() {
    let [(off_sent, off_full), (on_sent, on_full)] = crawl(2_000, 10, 3).await;
    assert!(off_sent <= 2_000 + 10, "off stops at the table: {off_sent}");
    assert!(
        off_full >= Duration::from_secs(3 * 3600 - 300),
        "off is full nearly all the time: {off_full:?}"
    );
    assert!(
        on_sent >= 10 * 3 * 3600 - 10,
        "on keeps the full rate: {on_sent}"
    );
    assert_eq!(on_full, Duration::ZERO, "on is never full");
}

/// The same at production scale (the 200,000-place table, Balanced's 100
/// samples a second, 8 h), for the decisions file. Release only:
/// `cargo test -p lodestar-dht --release --lib split_tests::full_scale -- --ignored --nocapture`
#[tokio::test(start_paused = true)]
#[ignore = "about 2.9 million samples; run in release"]
async fn full_scale() {
    let [(off_sent, off_full), (on_sent, on_full)] = crawl(MAX_KNOWN, 100, 8).await;
    println!("off: sent {off_sent}, full {} s", off_full.as_secs());
    println!("on:  sent {on_sent}, full {} s", on_full.as_secs());
    assert!(on_sent > off_sent);
}
