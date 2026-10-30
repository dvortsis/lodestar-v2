use super::*;
use proptest::prelude::*;

const H: u32 = 21_604;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lodestar-restart-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run() -> RunHeader {
    RunHeader {
        generation: 7,
        dir_id: [1; 16],
        ids: [2; 32],
        run_id: [3; 16],
    }
}

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn snapshot() -> Snapshot {
    Snapshot {
        run: run(),
        at_ms: 1_700_000_000_000,
        horizon_s: H,
        gate_s: 30,
        entries: vec![
            Entry {
                addr: addr("198.51.100.1:6881"),
                remaining_s: 60,
            },
            Entry {
                addr: addr("198.51.100.1:6882"),
                remaining_s: H,
            },
            Entry {
                addr: addr("203.0.113.9:1"),
                remaining_s: 1,
            },
            Entry {
                addr: addr("[2001:db8::1]:6881"),
                remaining_s: 3600,
            },
        ],
    }
}

fn marker(state: MarkerState) -> Marker {
    Marker {
        state,
        run: run(),
        dir_path: [4; 32],
        at_ms: 1_700_000_000_500,
    }
}

fn witness() -> Witness {
    Witness {
        generation: 7,
        dir_id: [1; 16],
        at_ms: 1_699_999_000_000,
        path: "/srv/lodestar".into(),
    }
}

/// A consistent set that resumes, 10 s after the marker was written.
fn resumable() -> (Loaded, Here) {
    let s = snapshot();
    let bytes = s.encode();
    let (_, sum) = Snapshot::decode(&bytes).unwrap();
    let m = marker(MarkerState::Clean {
        snap: sum,
        state_bin: [5; 32],
    });
    let loaded = Loaded {
        marker: Found::Ok(m),
        snapshot: Found::Ok((s, sum)),
        witness: WitnessFound::Ok(witness()),
        state_bin: Some([5; 32]),
    };
    let here = Here {
        path: "/srv/lodestar".into(),
        dir_path: [4; 32],
        ids: [2; 32],
        horizon_s: H,
        wall0: Duration::from_millis(m.at_ms + 10_000),
        mono0: Instant::now(),
    };
    (loaded, here)
}

fn suppressed(loaded: &Loaded, here: &Here) -> Option<Reason> {
    match plan(loaded, here).decision {
        Decision::Suppress(r) => Some(r),
        Decision::Resume(_) => None,
    }
}

// ---- Formats ----------------------------------------------------------------

#[test]
fn formats_roundtrip() {
    let s = snapshot();
    let (back, sum) = Snapshot::decode(&s.encode()).unwrap();
    assert_eq!(back, s);
    let bytes = s.encode();
    assert_eq!(sum.as_slice(), &bytes[bytes.len() - 32..]);
    for state in [
        MarkerState::Clean {
            snap: [9; 32],
            state_bin: [8; 32],
        },
        MarkerState::Dirty,
        MarkerState::Off,
    ] {
        let m = marker(state);
        assert_eq!(Marker::decode(&m.encode()).unwrap(), m);
    }
    let w = witness();
    assert_eq!(Witness::decode(&w.encode()).unwrap(), w);
}

type Decodes = fn(&[u8]) -> bool;

#[test]
fn every_bit_flip_and_truncation_is_rejected() {
    let files: [(&str, Vec<u8>, Decodes); 4] = [
        ("snapshot", snapshot().encode(), |b| {
            Snapshot::decode(b).is_ok()
        }),
        ("dirty marker", marker(MarkerState::Dirty).encode(), |b| {
            Marker::decode(b).is_ok()
        }),
        (
            "clean marker",
            marker(MarkerState::Clean {
                snap: [9; 32],
                state_bin: [8; 32],
            })
            .encode(),
            |b| Marker::decode(b).is_ok(),
        ),
        ("witness", witness().encode(), |b| {
            Witness::decode(b).is_ok()
        }),
    ];
    for (name, bytes, ok) in files {
        assert!(ok(&bytes), "{name} baseline");
        for i in 0..bytes.len() {
            for bit in 0..8 {
                let mut b = bytes.clone();
                b[i] ^= 1 << bit;
                assert!(!ok(&b), "{name}: flip at {i}.{bit} accepted");
            }
        }
        for len in 0..bytes.len() {
            assert!(!ok(&bytes[..len]), "{name}: truncation to {len} accepted");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(!ok(&longer), "{name}: trailing byte accepted");
    }
}

#[test]
fn magic_and_version() {
    let mut b = snapshot().encode();
    assert_eq!(Marker::decode(&b), Err(Fault::Corrupt("magic")));
    b[8] = 2;
    assert_eq!(Snapshot::decode(&b), Err(Fault::Version(2)));
}

/// Seals a hand-built snapshot body.
fn snapshot_with(f: impl FnOnce(&mut bencode::DictEncoder<'_>)) -> Vec<u8> {
    let mut body = Vec::new();
    Encoder::new(&mut body).dict(f);
    seal(MAGIC_SNAPSHOT, &body)
}

fn fields(
    gate: i64,
    horizon: i64,
    r4: &[u8],
    r6: &[u8],
) -> impl FnOnce(&mut bencode::DictEncoder<'_>) {
    let (r4, r6) = (r4.to_vec(), r6.to_vec());
    move |d| {
        d.entry(b"at").int(1);
        d.entry(b"dir_id").bytes(&[1; 16]);
        d.entry(b"gate").int(gate);
        d.entry(b"gen").int(1);
        d.entry(b"horizon").int(horizon);
        d.entry(b"ids").bytes(&[2; 32]);
        d.entry(b"r4").bytes(&r4);
        d.entry(b"r6").bytes(&r6);
        d.entry(b"run_id").bytes(&[3; 16]);
    }
}

fn r4(ip: [u8; 4], port: u16, rem: u32) -> Vec<u8> {
    let mut v = ip.to_vec();
    v.extend_from_slice(&port.to_be_bytes());
    v.extend_from_slice(&rem.to_be_bytes());
    v
}

#[test]
fn entry_rules() {
    let ok = |b: Vec<u8>| Snapshot::decode(&b).map(|_| ());
    let a = r4([198, 51, 100, 1], 6881, 60);
    let b = r4([198, 51, 100, 2], 6881, 60);
    assert_eq!(
        ok(snapshot_with(fields(
            0,
            100,
            &[a.clone(), b.clone()].concat(),
            &[]
        ))),
        Ok(())
    );
    let bad = |what| Err(Fault::Corrupt(what));
    assert_eq!(
        ok(snapshot_with(fields(
            0,
            100,
            &[b.clone(), a.clone()].concat(),
            &[]
        ))),
        bad("entry order")
    );
    assert_eq!(
        ok(snapshot_with(fields(
            0,
            100,
            &[a.clone(), a.clone()].concat(),
            &[]
        ))),
        bad("entry order")
    );
    assert_eq!(
        ok(snapshot_with(fields(0, 100, &a[..9], &[]))),
        bad("entry length")
    );
    assert_eq!(
        ok(snapshot_with(fields(0, 100, &r4([1, 2, 3, 4], 0, 60), &[]))),
        bad("entry port")
    );
    assert_eq!(
        ok(snapshot_with(fields(0, 100, &r4([1, 2, 3, 4], 1, 0), &[]))),
        bad("entry remaining")
    );
    assert_eq!(
        ok(snapshot_with(fields(
            0,
            100,
            &r4([1, 2, 3, 4], 1, 101),
            &[]
        ))),
        bad("entry remaining")
    );
    let mut mapped = Ipv4Addr::new(1, 2, 3, 4).to_ipv6_mapped().octets().to_vec();
    mapped.extend_from_slice(&1u16.to_be_bytes());
    mapped.extend_from_slice(&5u32.to_be_bytes());
    assert_eq!(
        ok(snapshot_with(fields(0, 100, &[], &mapped))),
        bad("entry mapped")
    );
    assert_eq!(ok(snapshot_with(fields(101, 100, &[], &[]))), bad("gate"));
    assert_eq!(ok(snapshot_with(fields(-1, 100, &[], &[]))), bad("gate"));
    assert_eq!(ok(snapshot_with(fields(0, 0, &[], &[]))), bad("horizon"));
    assert_eq!(
        ok(snapshot_with(fields(
            0,
            i64::from(MAX_HORIZON_S) + 1,
            &[],
            &[]
        ))),
        bad("horizon")
    );
}

#[test]
fn unknown_and_missing_keys() {
    let extra = snapshot_with(|d| {
        fields(0, 100, &[], &[])(d);
        d.entry(b"zz").int(1);
    });
    assert_eq!(Snapshot::decode(&extra), Err(Fault::Corrupt("keys")));
    let mut body = Vec::new();
    Encoder::new(&mut body).dict(|d| {
        d.entry(b"at").int(1);
    });
    assert_eq!(
        Snapshot::decode(&seal(MAGIC_SNAPSHOT, &body)),
        Err(Fault::Corrupt("keys"))
    );
    // A dirty marker carrying a snapshot digest is malformed.
    let mut body = Vec::new();
    Encoder::new(&mut body).dict(|d| {
        d.entry(b"at").int(1);
        d.entry(b"dir_id").bytes(&[1; 16]);
        d.entry(b"dir_path").bytes(&[4; 32]);
        d.entry(b"gen").int(1);
        d.entry(b"ids").bytes(&[2; 32]);
        d.entry(b"run_id").bytes(&[3; 16]);
        d.entry(b"snap").bytes(&[9; 32]);
        d.entry(b"state").bytes(b"dirty");
        d.entry(b"state_bin").bytes(&[8; 32]);
    });
    assert_eq!(
        Marker::decode(&seal(MAGIC_MARKER, &body)),
        Err(Fault::Corrupt("state"))
    );
}

#[test]
fn snapshot_of_the_maximum_size_decodes_and_one_more_is_rejected() {
    let entries: Vec<Entry> = (0..MAX_ENTRIES as u32)
        .map(|i| Entry {
            addr: SocketAddr::from((i.to_be_bytes(), 6881)),
            remaining_s: 1,
        })
        .collect();
    let mut s = Snapshot {
        entries,
        ..snapshot()
    };
    let bytes = s.encode();
    assert!(bytes.len() <= MAX_SNAPSHOT_BODY);
    assert_eq!(
        Snapshot::decode(&bytes).unwrap().0.entries.len(),
        MAX_ENTRIES
    );
    s.entries.push(Entry {
        addr: addr("[2001:db8::1]:1"),
        remaining_s: 1,
    });
    assert_eq!(
        Snapshot::decode(&s.encode()),
        Err(Fault::Corrupt("too many entries"))
    );
}

proptest! {
    #[test]
    fn decoders_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = Snapshot::decode(&bytes);
        let _ = Marker::decode(&bytes);
        let _ = Witness::decode(&bytes);
    }

    #[test]
    fn sealed_garbage_bodies_never_panic(body in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = Snapshot::decode(&seal(MAGIC_SNAPSHOT, &body));
        let _ = Marker::decode(&seal(MAGIC_MARKER, &body));
        let _ = Witness::decode(&seal(MAGIC_WITNESS, &body));
    }
}

// ---- Capture and seeding ---------------------------------------------------

#[test]
fn capture_sorts_dedups_normalises_and_skips_expired() {
    let mono = Instant::now();
    let mapped = SocketAddr::new(
        IpAddr::V6(Ipv4Addr::new(198, 51, 100, 1).to_ipv6_mapped()),
        6881,
    );
    // The same endpoint twice (two tables merged): the longer one wins.
    let table = [
        (addr("[2001:db8::1]:6881"), mono + Duration::from_secs(10)),
        (addr("[2001:db8::1]:6881"), mono + Duration::from_secs(20)),
        (mapped, mono + Duration::from_millis(1500)),
        (addr("198.51.100.1:6881"), mono + Duration::from_secs(5)),
        (addr("10.0.0.1:1"), mono),
        (addr("10.0.0.2:1"), mono - Duration::from_secs(1)),
    ];
    let s = capture(run(), table, None, mono, Duration::from_millis(5), H).unwrap();
    assert_eq!(
        s.entries,
        vec![
            Entry {
                addr: addr("198.51.100.1:6881"),
                remaining_s: 5
            },
            Entry {
                addr: addr("[2001:db8::1]:6881"),
                remaining_s: 20
            },
        ]
    );
    assert_eq!((s.gate_s, s.at_ms), (0, 5));
    // A table at the horizon is fine; past it refuses the commit.
    let at_h = capture(
        run(),
        [(addr("1.2.3.4:5"), mono + Duration::from_secs(u64::from(H)))],
        None,
        mono,
        Duration::ZERO,
        H,
    );
    assert_eq!(at_h.unwrap().entries[0].remaining_s, H);
    let past = mono + Duration::from_secs(u64::from(H)) + Duration::from_nanos(1);
    assert_eq!(
        capture(
            run(),
            [(addr("1.2.3.4:5"), past)],
            None,
            mono,
            Duration::ZERO,
            H
        ),
        Err(CaptureError::BeyondHorizon)
    );
    assert_eq!(
        capture(run(), [], Some(past), mono, Duration::ZERO, H),
        Err(CaptureError::BeyondHorizon)
    );
    assert_eq!(
        capture(
            run(),
            [(addr("1.2.3.4:0"), mono + Duration::from_secs(1))],
            None,
            mono,
            Duration::ZERO,
            H
        ),
        Err(CaptureError::PortZero)
    );
}

/// S10 then S2 on clocks that advance together, with the reads in the
/// required order and sub-millisecond lags: the restored deadline (on the
/// new monotonic clock, mapped back to wall time) is never before the true
/// one, and nothing still live is dropped (I4).
fn assert_capture_then_seed_never_early(
    offset_ns: u64,
    s10_lag_ns: u64,
    gap_ns: u64,
    s2_lag_ns: u64,
) {
    let mono_a = Instant::now();
    let wall_a = Duration::from_nanos(1_700_000_000_123_456_789);
    let until = mono_a + Duration::from_nanos(offset_ns);
    let true_deadline = wall_a + Duration::from_nanos(offset_ns);
    // S10: monotonic first (mono_a), the wall a little later.
    let wall_s10 = wall_a + Duration::from_nanos(s10_lag_ns);
    let s = capture(
        run(),
        [(addr("1.2.3.4:5"), until)],
        Some(until),
        mono_a,
        wall_s10,
        H,
    )
    .unwrap();
    // S2: the wall first, the monotonic clock a little later.
    let wall0 = wall_a + Duration::from_nanos(gap_ns);
    let mono0 = mono_a + Duration::from_nanos(gap_ns + s2_lag_ns);
    let here = Here {
        path: "/x".into(),
        dir_path: [0; 32],
        ids: [0; 32],
        horizon_s: H,
        wall0,
        mono0,
    };
    let seeded = seed(&s, floor_ms(wall0), &here).unwrap();
    if true_deadline > wall0 {
        assert_eq!(seeded.entries.len(), 1, "a live entry was dropped");
        assert!(seeded.gate_until.is_some(), "a live gate was dropped");
    }
    for until in seeded.entries.iter().map(|e| e.1).chain(seeded.gate_until) {
        let wall_of_until = wall0 + until.duration_since(mono0);
        assert!(
            wall_of_until >= true_deadline,
            "early: offset {offset_ns} lag {s10_lag_ns} gap {gap_ns} lag {s2_lag_ns}"
        );
    }
}

#[test]
fn capture_then_seed_never_moves_a_deadline_earlier() {
    for offset_ns in [
        1u64,
        999_999,
        1_000_000,
        1_000_001,
        999_999_999,
        1_000_000_000,
        1_500_000_000,
    ] {
        for gap_ns in [0u64, 1, 999_999, 1_000_000, 500_000_000] {
            for lag in [0u64, 3, 999_999] {
                assert_capture_then_seed_never_early(offset_ns, lag, gap_ns, lag);
            }
        }
    }
}

proptest! {
    #[test]
    fn capture_then_seed_never_early_for_any_lags(
        offset_ns in 1u64..30_000_000_000,
        s10_lag_ns in 0u64..2_000_000,
        gap_ns in 0u64..30_000_000_000,
        s2_lag_ns in 0u64..2_000_000,
    ) {
        assert_capture_then_seed_never_early(offset_ns, s10_lag_ns, gap_ns, s2_lag_ns);
    }
}

#[test]
fn rounding_boundaries_at_zero_one_ms_and_the_horizon() {
    let (loaded, mut here) = resumable();
    let Found::Ok((s, _)) = &loaded.snapshot else {
        unreachable!()
    };
    let deadline = s.at_ms + 60_000; // the 60 s entry
    // Exactly at the deadline: dropped. One ms before: restored for 1 ms.
    here.wall0 = Duration::from_millis(deadline);
    let Ok(seeded) = seed(s, deadline, &here) else {
        panic!()
    };
    assert!(
        !seeded
            .entries
            .iter()
            .any(|e| e.0 == addr("198.51.100.1:6881"))
    );
    let seeded = seed(s, deadline - 1, &here).unwrap();
    let e = seeded
        .entries
        .iter()
        .find(|e| e.0 == addr("198.51.100.1:6881"))
        .unwrap();
    assert_eq!(e.1 - here.mono0, Duration::from_millis(1));
    // The horizon entry, read at the snapshot's own instant, keeps all of H.
    let seeded = seed(s, s.at_ms, &here).unwrap();
    let e = seeded
        .entries
        .iter()
        .find(|e| e.0 == addr("198.51.100.1:6882"))
        .unwrap();
    assert_eq!(e.1 - here.mono0, Duration::from_secs(u64::from(H)));
}

#[test]
fn extreme_values_seed_without_panicking() {
    // The largest `at` a decoder accepts, read from the epoch: no
    // overflow, and every restored deadline stays within the horizon.
    let s = Snapshot {
        at_ms: i64::MAX as u64,
        ..snapshot()
    };
    let (_, here) = resumable();
    let seeded = seed(&s, 0, &here).unwrap();
    let h = Duration::from_secs(u64::from(H));
    assert!(seeded.entries.iter().all(|e| e.1 - here.mono0 <= h));
    assert!(seeded.gate_until.is_some_and(|g| g - here.mono0 <= h));
    // A monotonic clock near its end can't take the offset: corrupt.
    let mut far = here.clone();
    let mut step = Duration::from_secs(1 << 62);
    while step > Duration::from_secs(1) {
        if let Some(t) = far.mono0.checked_add(step) {
            far.mono0 = t;
        } else {
            step /= 2;
        }
    }
    assert_eq!(
        seed(&snapshot(), snapshot().at_ms, &far),
        Err(Reason::Corrupt)
    );
}

proptest! {
    /// Arbitrary inputs never panic, and whatever seeds is never earlier
    /// than the true deadline (I3, I4).
    #[test]
    fn seeding_is_total_and_never_early(
        at_ms in any::<u64>(),
        remaining_s in 1u32..=H,
        gate_s in 0u32..=H,
        ahead_ms in 0u64..(u64::from(H) * 1000 + 10),
    ) {
        let at_ms = at_ms % (i64::MAX as u64);
        let s = Snapshot {
            at_ms,
            gate_s,
            entries: vec![Entry { addr: addr("1.2.3.4:5"), remaining_s }],
            ..snapshot()
        };
        let (_, here) = resumable();
        let now = at_ms.saturating_add(ahead_ms);
        if let Ok(seeded) = seed(&s, now, &here) {
            let deadline = u128::from(at_ms) + u128::from(remaining_s) * 1000;
            if let Some((_, until)) = seeded.entries.first() {
                let left = until.duration_since(here.mono0).as_millis();
                prop_assert!(u128::from(now) + left >= deadline);
            } else {
                prop_assert!(deadline <= u128::from(now));
            }
            if let Some(g) = seeded.gate_until {
                let left = g.duration_since(here.mono0).as_millis();
                prop_assert!(u128::from(now) + left >= u128::from(at_ms) + u128::from(gate_s) * 1000);
            }
        }
    }
}

// ---- Validation ----------------------------------------------------------------

#[test]
fn a_consistent_set_resumes_with_entries_and_gate() {
    let (loaded, here) = resumable();
    let p = plan(&loaded, &here);
    assert_eq!(p.generation, 8);
    assert_eq!(p.dir_id, DirId::Keep([1; 16]));
    let Decision::Resume(seeded) = p.decision else {
        panic!("{:?}", p.decision)
    };
    // 10 s later: the 1 s entry has expired; the gate has 20 s left.
    assert_eq!(seeded.dropped_expired, 1);
    assert_eq!(seeded.entries.len(), 3);
    assert_eq!(
        seeded.gate_until.unwrap() - here.mono0,
        Duration::from_millis(30_000 - 10_500)
    );
}

#[test]
fn each_rule_suppresses_with_its_reason() {
    let (base, here) = resumable();
    let check = |reason: Reason, f: &dyn Fn(&mut Loaded, &mut Here)| {
        let (mut l, mut h) = (base.clone(), here.clone());
        f(&mut l, &mut h);
        assert_eq!(suppressed(&l, &h), Some(reason), "{}", reason.as_str());
    };
    let marker_mut = |l: &mut Loaded, f: &dyn Fn(&mut Marker)| {
        if let Found::Ok(m) = &mut l.marker {
            f(m)
        }
    };
    check(Reason::FirstStart, &|l, _| l.marker = Found::Missing);
    check(Reason::Corrupt, &|l, _| {
        l.marker = Found::Bad(Fault::Corrupt("x"))
    });
    check(Reason::Version, &|l, _| {
        l.marker = Found::Bad(Fault::Version(2))
    });
    check(Reason::Dirty, &|l, _| {
        marker_mut(l, &|m| m.state = MarkerState::Dirty)
    });
    check(Reason::OffRun, &|l, _| {
        marker_mut(l, &|m| m.state = MarkerState::Off)
    });
    check(Reason::Corrupt, &|l, _| l.snapshot = Found::Missing);
    check(Reason::Version, &|l, _| {
        l.snapshot = Found::Bad(Fault::Version(9))
    });
    check(Reason::DataDir, &|_, h| h.dir_path = [0; 32]);
    check(Reason::DataDir, &|_, h| h.path = "/elsewhere".into());
    check(Reason::NodeIdentity, &|_, h| h.ids = [0; 32]);
    check(Reason::ForeignRun, &|l, _| l.state_bin = Some([0; 32]));
    check(Reason::ForeignRun, &|l, _| l.state_bin = None);
    check(Reason::SnapshotMismatch, &|l, _| {
        if let Found::Ok((_, sum)) = &mut l.snapshot {
            sum[0] ^= 1
        }
    });
    check(Reason::SnapshotMismatch, &|l, _| {
        if let Found::Ok((s, _)) = &mut l.snapshot {
            s.run.run_id = [0; 16]
        }
    });
    check(Reason::NoWitness, &|l, _| l.witness = WitnessFound::Missing);
    check(Reason::NoWitness, &|l, _| {
        l.witness = WitnessFound::Unusable
    });
    check(Reason::NoWitness, &|l, _| {
        l.witness = WitnessFound::Bad(Fault::Corrupt("torn"))
    });
    check(Reason::NoWitness, &|l, _| {
        if let WitnessFound::Ok(w) = &mut l.witness {
            w.dir_id = [9; 16]
        }
    });
    check(Reason::Generation, &|l, _| {
        if let WitnessFound::Ok(w) = &mut l.witness {
            w.generation = 8
        }
    });
    check(Reason::Generation, &|l, _| {
        if let WitnessFound::Ok(w) = &mut l.witness {
            w.generation = 6
        }
    });
    check(Reason::HorizonChanged, &|_, h| h.horizon_s = H + 1);
    check(Reason::ClockBackward, &|l, h| {
        if let Found::Ok(m) = &l.marker {
            h.wall0 = Duration::from_millis(m.at_ms - 1)
        }
    });
    check(Reason::ClockBackward, &|l, _| {
        if let Found::Ok((s, _)) = &mut l.snapshot {
            s.at_ms += 10_000
        }
    });
    check(Reason::Stale, &|l, h| {
        if let Found::Ok(m) = &l.marker {
            h.wall0 = Duration::from_millis(m.at_ms + u64::from(H) * 1000 + 1)
        }
    });
}

#[test]
fn backward_one_day_and_forward_within_and_past_the_horizon() {
    let (l, mut h) = resumable();
    let Found::Ok(m) = &l.marker else {
        unreachable!()
    };
    h.wall0 = Duration::from_millis(m.at_ms - 86_400_000);
    assert_eq!(suppressed(&l, &h), Some(Reason::ClockBackward));
    h.wall0 = Duration::from_millis(m.at_ms + u64::from(H) * 1000);
    assert_eq!(
        suppressed(&l, &h),
        None,
        "exactly at the horizon still resumes"
    );
    h.wall0 += Duration::from_millis(1);
    assert_eq!(suppressed(&l, &h), Some(Reason::Stale));
}

#[test]
fn the_steady_wrong_clock_residual_is_what_the_design_says() {
    // §8.1: a clock steady but 1 h slow for a whole run, corrected forward
    // before the next start, shortens restored restrictions by that hour.
    // Pinned so a change here is a deliberate one.
    let mono = Instant::now();
    let true_wall = Duration::from_secs(1_700_000_000);
    let slow_wall = true_wall - Duration::from_secs(3600);
    let until = mono + Duration::from_secs(5 * 3600);
    let s = capture(
        run(),
        [(addr("1.2.3.4:5"), until)],
        None,
        mono,
        slow_wall,
        H,
    )
    .unwrap();
    let here = Here {
        path: "/x".into(),
        dir_path: [0; 32],
        ids: [0; 32],
        horizon_s: H,
        wall0: true_wall,
        mono0: mono,
    };
    let seeded = seed(&s, floor_ms(true_wall), &here).unwrap();
    assert_eq!(seeded.entries[0].1 - mono, Duration::from_secs(4 * 3600));
}

#[test]
fn two_reads_in_one_coarse_tick_are_not_a_backward_step() {
    // N4: S10 and S11 read the same 15.6 ms Windows tick.
    let mono = Instant::now();
    let tick = Duration::from_nanos(1_700_000_000_015_600_000);
    let s = capture(run(), [], None, mono, tick, H).unwrap();
    let m = Marker {
        state: MarkerState::Clean {
            snap: [0; 32],
            state_bin: [0; 32],
        },
        run: run(),
        dir_path: [0; 32],
        at_ms: ceil_ms(tick),
    };
    assert!(s.at_ms <= m.at_ms);
    assert!(m.at_ms <= floor_ms(tick + Duration::from_millis(1)));
}

#[test]
fn dir_id_and_generation_rules() {
    let (mut l, h) = resumable();
    // generation = max(marker, witness) + 1, whatever the decision.
    if let WitnessFound::Ok(w) = &mut l.witness {
        w.generation = 40;
    }
    assert_eq!(plan(&l, &h).generation, 41);
    // A witness for this id naming another path: a copy, so a new id.
    if let WitnessFound::Ok(w) = &mut l.witness {
        w.path = "/srv/lodestar-copy".into();
    }
    let p = plan(&l, &h);
    assert_eq!(
        (p.dir_id, p.decision),
        (DirId::New, Decision::Suppress(Reason::DataDir))
    );
    // No usable marker: a new id; generation still counts past the witness.
    let (mut l, h) = resumable();
    l.marker = Found::Bad(Fault::Corrupt("x"));
    let p = plan(&l, &h);
    assert_eq!((p.generation, p.dir_id), (8, DirId::New));
    l.marker = Found::Missing;
    l.witness = WitnessFound::Missing;
    assert_eq!(plan(&l, &h).generation, 1);
    // A moved directory keeps its id while its witness agrees.
    let (l, mut h) = resumable();
    h.dir_path = [7; 32];
    let p = plan(&l, &h);
    assert_eq!(
        (p.dir_id, p.decision),
        (DirId::Keep([1; 16]), Decision::Suppress(Reason::DataDir))
    );
}

#[test]
fn reason_labels_are_unique() {
    let labels: std::collections::HashSet<_> = Reason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(labels.len(), Reason::ALL.len());
}

// ---- Files ----------------------------------------------------------------------

#[test]
fn write_atomic_crash_points_leave_old_or_new() {
    let dir = Scratch::new("atomic");
    let path = dir.path().join(MARKER_FILE);
    let old = marker(MarkerState::Dirty).encode();
    let new = marker(MarkerState::Off).encode();
    for point in [
        Point::AfterWrite,
        Point::AfterSync,
        Point::AfterRename,
        Point::DirSync,
        Point::AfterDirSync,
    ] {
        write_atomic(&path, &old).unwrap();
        fail::arm(Some(point));
        assert!(
            write_atomic(&path, &new).is_err(),
            "{point:?} must propagate"
        );
        fail::arm(None);
        let seen = fs::read(&path).unwrap();
        let expect_new = matches!(
            point,
            Point::AfterRename | Point::DirSync | Point::AfterDirSync
        );
        assert_eq!(seen == new, expect_new, "{point:?}");
        assert!(seen == old || seen == new);
        remove_stale_tmp(dir.path()).unwrap();
        assert!(!tmp_path(&path).exists());
    }
}

/// A power cut loses whatever wasn't synced (§6 item 1). Before the
/// rename is durable that leaves the old file plus a temp file, whole or
/// torn; a later start reads the old record and removes the temp file.
#[test]
fn write_atomic_power_cut_before_the_rename_is_durable() {
    let dir = Scratch::new("powercut");
    let path = dir.path().join(MARKER_FILE);
    let old = marker(MarkerState::Dirty).encode();
    let new = marker(MarkerState::Off).encode();
    for torn_len in [0, 1, new.len() / 2, new.len()] {
        write_atomic(&path, &old).unwrap();
        fs::write(tmp_path(&path), &new[..torn_len]).unwrap();
        assert_eq!(
            read_marker(dir.path()),
            Found::Ok(marker(MarkerState::Dirty))
        );
        remove_stale_tmp(dir.path()).unwrap();
        assert!(!tmp_path(&path).exists());
        assert_eq!(fs::read(&path).unwrap(), old);
    }
}

#[test]
fn overwrite_in_place_crash_points_and_torn_writes() {
    let dir = Scratch::new("inplace");
    let path = dir.path().join(MARKER_FILE);
    let long = marker(MarkerState::Clean {
        snap: [1; 32],
        state_bin: [2; 32],
    })
    .encode();
    let short = marker(MarkerState::Dirty).encode();
    assert!(short.len() < long.len());
    for point in [Point::AfterWrite, Point::AfterSetLen, Point::AfterSync] {
        fs::write(&path, &long).unwrap();
        fail::arm(Some(point));
        assert!(overwrite_in_place(&path, &short).is_err());
        fail::arm(None);
        let seen = fs::read(&path).unwrap();
        // Before set_len the file is the new prefix over the old tail:
        // torn, and it must not decode.
        if point == Point::AfterWrite {
            assert_eq!(seen.len(), long.len());
            assert!(Marker::decode(&seen).is_err());
        } else {
            assert_eq!(Marker::decode(&seen).unwrap().state, MarkerState::Dirty);
        }
    }
    // Every torn mix of the new prefix over the old file is rejected.
    for cut in 1..short.len() {
        let mut torn = long.clone();
        torn[..cut].copy_from_slice(&short[..cut]);
        // An unchanged prefix leaves the old record itself.
        assert!(torn == long || Marker::decode(&torn).is_err(), "cut {cut}");
    }
    // A missing file is created, and its directory entry synced.
    let fresh = dir.path().join("fresh");
    fail::arm(Some(Point::DirSync));
    assert!(
        overwrite_in_place(&fresh, &short).is_err(),
        "dir fsync errors propagate"
    );
    fail::arm(None);
    fs::remove_file(&fresh).unwrap();
    overwrite_in_place(&fresh, &short).unwrap();
    assert_eq!(fs::read(&fresh).unwrap(), short);
}

#[test]
fn invalidation_never_unlinks() {
    let dir = Scratch::new("nounlink");
    let path = dir.path().join(MARKER_FILE);
    fs::write(&path, marker(MarkerState::Dirty).encode()).unwrap();
    #[cfg(unix)]
    let ino = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path).unwrap().ino()
    };
    overwrite_in_place(&path, &marker(MarkerState::Off).encode()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            ino,
            "same file, overwritten"
        );
    }
    assert_eq!(read_marker(dir.path()), Found::Ok(marker(MarkerState::Off)));
}

/// A real abort mid-write (kill -9 as far as the files can tell). The
/// child is this test binary, running only `abort_child`.
#[test]
fn abort_mid_write_leaves_a_safe_file() {
    let exe = std::env::current_exe().unwrap();
    let clean = marker(MarkerState::Clean {
        snap: [1; 32],
        state_bin: [2; 32],
    });
    let dirty = marker(MarkerState::Dirty);
    // What each abort point must leave: the atomic path only ever the old
    // or the new record; the in-place path may also leave a torn file.
    for (mode, point, expect) in [
        ("atomic", Point::AfterSync, Some(clean)),
        ("atomic", Point::AfterRename, Some(dirty)),
        ("atomic", Point::AfterDirSync, Some(dirty)),
        ("inplace", Point::AfterWrite, None),
        ("inplace", Point::AfterSetLen, Some(dirty)),
        ("inplace", Point::AfterSync, Some(dirty)),
    ] {
        let dir = Scratch::new("abort");
        let path = dir.path().join(MARKER_FILE);
        fs::write(&path, clean.encode()).unwrap();
        let status = std::process::Command::new(&exe)
            .args([
                "--exact",
                "restart::tests::abort_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LODESTAR_TEST_ABORT_AT", format!("{point:?}"))
            .env("LODESTAR_TEST_ABORT_MODE", mode)
            .env("LODESTAR_TEST_ABORT_FILE", &path)
            .status()
            .unwrap();
        assert!(!status.success(), "{mode} {point:?}: the child must abort");
        match (read_marker(dir.path()), expect) {
            (Found::Ok(m), Some(e)) => assert_eq!(m, e, "{mode} {point:?}"),
            // Torn by an overwrite whose length wasn't set yet.
            (Found::Bad(_), None) => {}
            (found, _) => panic!("{mode} {point:?}: left {found:?}"),
        }
    }
}

#[test]
fn abort_child() {
    let (Ok(mode), Ok(file)) = (
        std::env::var("LODESTAR_TEST_ABORT_MODE"),
        std::env::var("LODESTAR_TEST_ABORT_FILE"),
    ) else {
        return; // Only meaningful as the child above.
    };
    let dirty = marker(MarkerState::Dirty).encode();
    let path = Path::new(&file);
    let _ = match mode.as_str() {
        "atomic" => write_atomic(path, &dirty),
        _ => overwrite_in_place(path, &dirty),
    };
    // The abort point was never reached: make the parent fail loudly.
    std::process::exit(0);
}

#[test]
fn reads_are_bounded_and_unreadable_is_bad() {
    let dir = Scratch::new("reads");
    assert_eq!(read_marker(dir.path()), Found::Missing);
    assert_eq!(read_snapshot(dir.path()), Found::Missing);
    fs::write(
        dir.path().join(MARKER_FILE),
        vec![0u8; MAX_SMALL_BODY + 1024],
    )
    .unwrap();
    assert!(matches!(read_marker(dir.path()), Found::Bad(_)));
    // A directory where a file should be can't be read.
    fs::create_dir(dir.path().join(SNAPSHOT_FILE)).unwrap();
    assert!(matches!(read_snapshot(dir.path()), Found::Bad(_)));
}

#[test]
fn remove_stale_tmp_touches_only_ours() {
    let dir = Scratch::new("tmp");
    for name in [
        "dht-restart.mark.tmp",
        "dht-restart.bin.tmp",
        "dht-state.tmp",
        "other.tmp",
    ] {
        fs::write(dir.path().join(name), b"x").unwrap();
    }
    remove_stale_tmp(dir.path()).unwrap();
    let mut left: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["dht-state.tmp", "other.tmp"]);
}

// ---- Witness ----------------------------------------------------------------------

#[test]
fn witness_inside_the_data_dir_is_unusable() {
    let root = Scratch::new("wit");
    let data = root.path().join("data");
    let outside = root.path().join("witness");
    let inside = data.join("witness");
    fs::create_dir_all(&inside).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let data_c = canonical(&data).unwrap();
    assert!(witness_usable(&outside, &data_c));
    assert!(!witness_usable(&inside, &data_c));
    assert!(!witness_usable(&data, &data_c));
    assert_eq!(
        read_witness(&inside, &data_c, &[1; 16]),
        WitnessFound::Unusable
    );
    assert_eq!(
        read_witness(&outside, &data_c, &[1; 16]),
        WitnessFound::Missing
    );
    assert_eq!(
        read_witness(&root.path().join("nope"), &data_c, &[1; 16]),
        WitnessFound::Missing
    );
    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&inside, &link).unwrap();
        assert!(
            !witness_usable(&link, &data_c),
            "a symlink into the data dir"
        );
    }
}

#[test]
fn witness_roundtrips_through_its_file() {
    let root = Scratch::new("witfile");
    let data = root.path().join("data");
    let wdir = root.path().join("w");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&wdir).unwrap();
    let data_c = canonical(&data).unwrap();
    let w = Witness {
        path: data_c.clone(),
        ..witness()
    };
    overwrite_in_place(&witness_path(&wdir, &w.dir_id), &w.encode()).unwrap();
    assert_eq!(
        read_witness(&wdir, &data_c, &w.dir_id),
        WitnessFound::Ok(w.clone())
    );
    let bumped = Witness {
        generation: 8,
        ..w.clone()
    };
    overwrite_in_place(&witness_path(&wdir, &w.dir_id), &bumped.encode()).unwrap();
    assert_eq!(
        read_witness(&wdir, &data_c, &w.dir_id),
        WitnessFound::Ok(bumped)
    );
}

#[test]
fn orphan_witnesses_are_pruned_and_others_kept() {
    let root = Scratch::new("prune");
    let wdir = root.path().join("w");
    fs::create_dir_all(&wdir).unwrap();
    let mine = root.path().join("mine");
    let live = root.path().join("live");
    let torn = root.path().join("torn");
    let reset = root.path().join("reset");
    for d in [&mine, &live, &torn, &reset] {
        fs::create_dir_all(d).unwrap();
    }
    let c = |d: &Path| canonical(d).unwrap();
    let with_id = |id: Id16| Marker {
        run: RunHeader {
            dir_id: id,
            ..run()
        },
        ..marker(MarkerState::Dirty)
    };
    fs::write(live.join(MARKER_FILE), with_id([0x22; 16]).encode()).unwrap();
    fs::write(torn.join(MARKER_FILE), b"half a marker").unwrap();
    fs::write(reset.join(MARKER_FILE), with_id([0x55; 16]).encode()).unwrap();
    let now = 10 * ORPHAN_AGE_MS;
    let old = now - ORPHAN_AGE_MS;
    let put = |id: Id16, path: &str, at_ms: u64| {
        let w = Witness {
            dir_id: id,
            path: path.into(),
            at_ms,
            ..witness()
        };
        fs::write(witness_path(&wdir, &id), w.encode()).unwrap();
    };
    put([0x11; 16], &c(&mine), old); // this directory's current id: kept
    put([0x10; 16], &c(&mine), now); // an earlier id of this directory: removed, however new
    put([0x22; 16], &c(&live), old); // another live directory: kept
    put([0x33; 16], "/gone", old); // no marker there any more: removed
    put([0x34; 16], "/gone-too", now - 1); // the same, but too recent to judge: kept
    put([0x44; 16], &c(&torn), old); // a torn marker there: kept
    put([0x45; 16], &c(&reset), old); // a marker with another id there: removed
    fs::write(wdir.join("torn.gen"), b"not a witness").unwrap();
    assert_eq!(
        prune_witnesses(&wdir, &[0x11; 16], &c(&mine), now).unwrap(),
        3
    );
    for (id, kept) in [
        (0x11, true),
        (0x10, false),
        (0x22, true),
        (0x33, false),
        (0x34, true),
        (0x44, true),
        (0x45, false),
    ] {
        assert_eq!(witness_path(&wdir, &[id; 16]).exists(), kept, "{id:#x}");
    }
    assert!(wdir.join("torn.gen").exists());
}

// ---- Lock -------------------------------------------------------------------------

#[test]
fn the_lock_is_exclusive_and_released_on_drop() {
    let dir = Scratch::new("lock");
    let first = match try_lock(dir.path()).unwrap() {
        LockAttempt::Held(l) => l,
        other => {
            // Say so rather than pass silently: this host's temp
            // filesystem has no locks, so nothing here was checked.
            eprintln!("SKIPPED: no file locks in the temp dir ({other:?})");
            return;
        }
    };
    assert!(matches!(try_lock(dir.path()).unwrap(), LockAttempt::Busy));
    drop(first);
    assert!(matches!(
        try_lock(dir.path()).unwrap(),
        LockAttempt::Held(_)
    ));
    assert!(
        dir.path().join(LOCK_FILE).exists(),
        "the lock file is never removed"
    );
}

// ---- Review round 1 ---------------------------------------------------------------

#[test]
fn a_scoped_or_flow_labelled_key_refuses_the_commit() {
    // The sampler keys on the whole SocketAddr; stripping the scope would
    // restore the restriction under a key the sampler never looks up.
    let mono = Instant::now();
    let later = mono + Duration::from_secs(60);
    for (flow, scope) in [(0, 3), (7, 0)] {
        let a = SocketAddr::V6(std::net::SocketAddrV6::new(
            "fe80::1".parse().unwrap(),
            6881,
            flow,
            scope,
        ));
        assert_eq!(
            capture(run(), [(a, later)], None, mono, Duration::ZERO, H),
            Err(CaptureError::Scoped)
        );
    }
    // An expired scoped entry needs no record, so it doesn't block.
    let a = SocketAddr::V6(std::net::SocketAddrV6::new(
        "fe80::1".parse().unwrap(),
        6881,
        0,
        3,
    ));
    assert!(capture(run(), [(a, mono)], None, mono, Duration::ZERO, H).is_ok());
}

#[test]
fn capture_refuses_a_horizon_the_decoder_would_reject() {
    let mono = Instant::now();
    for h in [0, MAX_HORIZON_S + 1] {
        assert_eq!(
            capture(run(), [], None, mono, Duration::ZERO, h),
            Err(CaptureError::BadHorizon)
        );
    }
    assert!(capture(run(), [], None, mono, Duration::ZERO, MAX_HORIZON_S).is_ok());
}

/// Records the order the protocol reads its clocks in.
#[derive(Debug, Default)]
struct OrderClock(std::sync::Mutex<Vec<&'static str>>);

impl Clock for OrderClock {
    fn wall(&self) -> Duration {
        self.0.lock().unwrap().push("wall");
        Duration::from_secs(1_700_000_000)
    }
    fn mono(&self) -> Instant {
        self.0.lock().unwrap().push("mono");
        Instant::now()
    }
}

#[test]
fn the_clock_readers_take_their_reads_in_the_required_order() {
    let c = OrderClock::default();
    let here = Here::read(&c, "/srv/lodestar".into(), [2; 32], H);
    assert_eq!(
        *c.0.lock().unwrap(),
        ["wall", "mono"],
        "S0: wall, then mono"
    );
    assert_eq!(here.dir_path, path_digest("/srv/lodestar"));
    c.0.lock().unwrap().clear();
    capture_now(&c, run(), [], None, H).unwrap();
    let _ = stamp_after_capture(&c);
    assert_eq!(
        *c.0.lock().unwrap(),
        ["mono", "wall", "wall"],
        "S10 mono, wall; S11 wall"
    );
}

#[cfg(unix)]
#[test]
fn a_witness_file_that_is_a_symlink_is_unusable() {
    let root = Scratch::new("witlink");
    let data = root.path().join("data");
    let wdir = root.path().join("w");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&wdir).unwrap();
    let data_c = canonical(&data).unwrap();
    let target = data.join("planted.gen");
    fs::write(
        &target,
        Witness {
            path: data_c.clone(),
            ..witness()
        }
        .encode(),
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, witness_path(&wdir, &[1; 16])).unwrap();
    assert_eq!(
        read_witness(&wdir, &data_c, &[1; 16]),
        WitnessFound::Unusable
    );
}

#[test]
fn the_first_failing_rule_names_the_reason() {
    // Reason order as implemented and recorded in the decisions file
    // (CR-0d-1): marker faults, then its state, then the snapshot, then
    // the rest in rule order.
    let (base, here) = resumable();
    let mut l = base.clone();
    // A dirty marker with no snapshot (a crash during the first on-run).
    if let Found::Ok(m) = &mut l.marker {
        m.state = MarkerState::Dirty;
    }
    l.snapshot = Found::Missing;
    assert_eq!(suppressed(&l, &here), Some(Reason::Dirty));
    // A clean marker with a missing snapshot and no witness: corrupt wins.
    let mut l = base.clone();
    l.snapshot = Found::Missing;
    l.witness = WitnessFound::Missing;
    assert_eq!(suppressed(&l, &here), Some(Reason::Corrupt));
    // Node identity and a stale clock both wrong: identity comes first.
    let (l, mut h) = (base.clone(), here.clone());
    h.ids = [0; 32];
    h.wall0 += Duration::from_secs(u64::from(H) + 1);
    assert_eq!(suppressed(&l, &h), Some(Reason::NodeIdentity));
    // A snapshot whose dir_id differs from the marker's is rule 7.
    let mut l = base.clone();
    if let Found::Ok((s, _)) = &mut l.snapshot {
        s.run.dir_id = [9; 16];
    }
    assert_eq!(suppressed(&l, &here), Some(Reason::SnapshotMismatch));
}

/// Snapshots built from arbitrary, plausible parts.
fn any_snapshot() -> impl Strategy<Value = Snapshot> {
    let entry = (
        any::<bool>(),
        any::<[u8; 16]>(),
        1u16..,
        1u32..=MAX_HORIZON_S,
    );
    (
        any::<[u8; 16]>(),
        0u64..(1 << 62),
        0u64..(i64::MAX as u64),
        1u32..=MAX_HORIZON_S,
        any::<u32>(),
        proptest::collection::vec(entry, 0..40),
    )
        .prop_map(|(id, generation, at_ms, horizon_s, gate, raw)| {
            let mut entries: Vec<Entry> = raw
                .into_iter()
                .map(|(v6, ip, port, rem)| {
                    let ip = if v6 {
                        let ip = Ipv6Addr::from(ip);
                        match ip.to_ipv4_mapped() {
                            Some(v4) => IpAddr::V4(v4),
                            None => IpAddr::V6(ip),
                        }
                    } else {
                        IpAddr::V4(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]))
                    };
                    Entry {
                        addr: SocketAddr::new(ip, port),
                        remaining_s: rem.min(horizon_s),
                    }
                })
                .collect();
            entries.sort_by_key(|e| entry_key(&e.addr));
            entries.dedup_by(|a, b| a.addr == b.addr);
            Snapshot {
                run: RunHeader {
                    generation,
                    dir_id: id,
                    ids: [2; 32],
                    run_id: [3; 16],
                },
                at_ms,
                horizon_s,
                gate_s: gate % (horizon_s + 1),
                entries,
            }
        })
}

/// Dictionaries with the snapshot's or marker's keys but arbitrary values.
fn near_valid_body(keys: &'static [&'static [u8]]) -> impl Strategy<Value = Vec<u8>> {
    let value = prop_oneof![
        any::<i64>().prop_map(|v| (Some(v), Vec::new())),
        proptest::collection::vec(any::<u8>(), 0..48).prop_map(|b| (None, b)),
        // The right sizes for the fixed fields, and whole entries.
        prop_oneof![Just(16usize), Just(32), Just(10), Just(22), Just(44)]
            .prop_flat_map(|n| proptest::collection::vec(any::<u8>(), n))
            .prop_map(|b| (None, b)),
    ];
    proptest::collection::vec(value, keys.len()).prop_map(move |values| {
        let mut body = Vec::new();
        Encoder::new(&mut body).dict(|d| {
            for (k, (int, bytes)) in keys.iter().zip(values) {
                match int {
                    Some(v) => d.entry(k).int(v),
                    None => d.entry(k).bytes(&bytes),
                }
            }
        });
        body
    })
}

proptest! {
    /// Structured inputs reach the body decoder (a random input never
    /// passes the checksum), round-trip exactly, and seed never early.
    #[test]
    fn plausible_snapshots_roundtrip_and_seed_never_early(
        s in any_snapshot(),
        ahead_ms in 0u64..(u64::from(MAX_HORIZON_S) * 1000 + 10),
    ) {
        let bytes = s.encode();
        let (back, sum) = Snapshot::decode(&bytes).unwrap();
        prop_assert_eq!(&back, &s);
        prop_assert_eq!(back.encode(), bytes);
        let m = Marker {
            state: MarkerState::Clean { snap: sum, state_bin: [5; 32] },
            run: s.run,
            dir_path: path_digest("/x"),
            at_ms: s.at_ms,
        };
        let loaded = Loaded {
            marker: Found::Ok(m),
            snapshot: Found::Ok((s.clone(), sum)),
            witness: WitnessFound::Ok(Witness {
                generation: s.run.generation,
                dir_id: s.run.dir_id,
                at_ms: 0,
                path: "/x".into(),
            }),
            state_bin: Some([5; 32]),
        };
        let now = s.at_ms.saturating_add(ahead_ms);
        let here = Here {
            path: "/x".into(),
            dir_path: path_digest("/x"),
            ids: [2; 32],
            horizon_s: s.horizon_s,
            wall0: Duration::from_millis(now),
            mono0: Instant::now(),
        };
        match plan(&loaded, &here).decision {
            Decision::Resume(seeded) => {
                prop_assert!(now - s.at_ms <= u64::from(s.horizon_s) * 1000);
                for (addr, until) in &seeded.entries {
                    let e = s.entries.iter().find(|e| e.addr == *addr).unwrap();
                    let left = until.duration_since(here.mono0).as_millis() as u64;
                    prop_assert!(now + left >= s.at_ms + u64::from(e.remaining_s) * 1000);
                }
                let live = s.entries.iter().filter(|e| s.at_ms + u64::from(e.remaining_s) * 1000 > now).count();
                prop_assert_eq!(seeded.entries.len(), live);
                if s.at_ms + u64::from(s.gate_s) * 1000 > now {
                    let g = seeded.gate_until.unwrap();
                    let left = g.duration_since(here.mono0).as_millis() as u64;
                    prop_assert!(now + left >= s.at_ms + u64::from(s.gate_s) * 1000);
                }
            }
            Decision::Suppress(r) => prop_assert_eq!(r, Reason::Stale),
        }
    }

    #[test]
    fn near_valid_bodies_never_panic_and_canonical_on_success(
        snap in near_valid_body(SNAPSHOT_KEYS),
        mark in near_valid_body(CLEAN_KEYS),
        dirty in near_valid_body(MARKER_KEYS),
        wit in near_valid_body(WITNESS_KEYS),
    ) {
        let sealed = seal(MAGIC_SNAPSHOT, &snap);
        if let Ok((s, _)) = Snapshot::decode(&sealed) {
            prop_assert_eq!(s.encode(), sealed);
        }
        for body in [mark, dirty] {
            let sealed = seal(MAGIC_MARKER, &body);
            if let Ok(m) = Marker::decode(&sealed) {
                prop_assert_eq!(m.encode(), sealed);
            }
        }
        let sealed = seal(MAGIC_WITNESS, &wit);
        if let Ok(w) = Witness::decode(&sealed) {
            prop_assert_eq!(w.encode(), sealed);
        }
    }

    /// Arbitrary bytes never decode to a set that resumes (§6.4).
    #[test]
    fn arbitrary_bytes_never_resume(
        m in proptest::collection::vec(any::<u8>(), 0..256),
        s in proptest::collection::vec(any::<u8>(), 0..256),
        w in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let loaded = Loaded {
            marker: match Marker::decode(&m) { Ok(v) => Found::Ok(v), Err(f) => Found::Bad(f) },
            snapshot: match Snapshot::decode(&s) { Ok(v) => Found::Ok(v), Err(f) => Found::Bad(f) },
            witness: match Witness::decode(&w) { Ok(v) => WitnessFound::Ok(v), Err(f) => WitnessFound::Bad(f) },
            state_bin: Some([5; 32]),
        };
        let (_, here) = resumable();
        prop_assert!(matches!(plan(&loaded, &here).decision, Decision::Suppress(_)));
    }
}

#[test]
fn a_sub_millisecond_wall0_through_plan_never_seeds_early() {
    // S2 floors `wall0` to the millisecond. Through `plan` (not `seed`
    // directly), with `wall0` half a millisecond before each deadline:
    // every restored deadline, mapped back to wall time, is at or after
    // the one recorded. Rounding `wall0` up instead would put it 0.5 ms
    // early.
    let (base, _) = resumable();
    let Found::Ok((s, _)) = &base.snapshot else {
        unreachable!()
    };
    let deadlines: Vec<u64> = s
        .entries
        .iter()
        .map(|e| s.at_ms + u64::from(e.remaining_s) * 1000)
        .chain([s.at_ms + u64::from(s.gate_s) * 1000])
        .collect();
    let Found::Ok(m) = &base.marker else {
        unreachable!()
    };
    for &deadline in deadlines.iter().filter(|&&d| d > m.at_ms) {
        let wall0 =
            Duration::from_millis(deadline) - Duration::from_micros(500) + Duration::from_nanos(1);
        let here = Here::read(&FixedClock(wall0), "/srv/lodestar".into(), [2; 32], H);
        let here = Here {
            dir_path: [4; 32],
            ..here
        };
        let Decision::Resume(seeded) = plan(&base, &here).decision else {
            panic!("must resume at {deadline}");
        };
        let mut checked = 0;
        for (addr, until) in &seeded.entries {
            let e = s.entries.iter().find(|e| e.addr == *addr).unwrap();
            let recorded = Duration::from_millis(s.at_ms + u64::from(e.remaining_s) * 1000);
            assert!(
                here.wall0 + until.duration_since(here.mono0) >= recorded,
                "{addr}"
            );
            checked += 1;
        }
        if let Some(g) = seeded.gate_until {
            let recorded = Duration::from_millis(s.at_ms + u64::from(s.gate_s) * 1000);
            assert!(
                here.wall0 + g.duration_since(here.mono0) >= recorded,
                "gate"
            );
            checked += 1;
        }
        assert!(checked > 0, "nothing was live at {deadline}");
    }
}

/// A clock frozen at one wall reading.
#[derive(Debug)]
struct FixedClock(Duration);

impl Clock for FixedClock {
    fn wall(&self) -> Duration {
        self.0
    }
    fn mono(&self) -> Instant {
        Instant::now()
    }
}
