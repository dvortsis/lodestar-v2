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
    let files: [(&str, Vec<u8>, Decodes); 3] = [
        ("snapshot", snapshot().encode(), |b| {
            Snapshot::decode(b).is_ok()
        }),
        ("marker", marker(MarkerState::Dirty).encode(), |b| {
            Marker::decode(b).is_ok()
        }),
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
    let scoped = SocketAddr::V6(std::net::SocketAddrV6::new(
        "2001:db8::1".parse().unwrap(),
        6881,
        7,
        3,
    ));
    let table = [
        (addr("[2001:db8::1]:6881"), mono + Duration::from_secs(10)),
        (scoped, mono + Duration::from_secs(20)),
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

#[test]
fn rounding_never_moves_a_deadline_earlier() {
    // Wall and monotonic advance together; sub-millisecond offsets
    // everywhere (I4).
    let mono_a = Instant::now();
    let wall_a = Duration::from_nanos(1_700_000_000_123_456_789);
    for offset_ns in [
        1u64,
        999_999,
        1_000_000,
        1_000_001,
        999_999_999,
        1_000_000_000,
    ] {
        let until = mono_a + Duration::from_nanos(offset_ns);
        // S10 reads mono, then wall a little later.
        let wall_s10 = wall_a + Duration::from_nanos(3);
        let s = capture(
            run(),
            [(addr("1.2.3.4:5"), until)],
            Some(until),
            mono_a,
            wall_s10,
            H,
        )
        .unwrap();
        let true_deadline = wall_a + Duration::from_nanos(offset_ns);
        // S2 one hour later: wall first, mono a little after.
        let later = Duration::from_secs(5);
        let wall0 = wall_a + later;
        let mono0 = mono_a + later + Duration::from_nanos(2);
        let here = Here {
            path: "/x".into(),
            dir_path: [0; 32],
            ids: [0; 32],
            horizon_s: H,
            wall0,
            mono0,
        };
        let seeded = seed(&s, floor_ms(wall0), &here).unwrap();
        for until in seeded.entries.iter().map(|e| e.1).chain(seeded.gate_until) {
            // until on the new clock, mapped back to wall time.
            let wall_of_until = wall0 + until.duration_since(mono0);
            assert!(
                wall_of_until >= true_deadline || true_deadline <= wall0,
                "offset {offset_ns} ns moved earlier"
            );
        }
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
    ] {
        write_atomic(&path, &old).unwrap();
        fail::arm(Some(point));
        assert!(
            write_atomic(&path, &new).is_err(),
            "{point:?} must propagate"
        );
        fail::arm(None);
        let seen = fs::read(&path).unwrap();
        let expect_new = matches!(point, Point::AfterRename | Point::DirSync);
        assert_eq!(seen == new, expect_new, "{point:?}");
        assert!(seen == old || seen == new);
        remove_stale_tmp(dir.path()).unwrap();
        assert!(!tmp_path(&path).exists());
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
    for (mode, point) in [
        ("atomic", Point::AfterSync),
        ("atomic", Point::AfterRename),
        ("inplace", Point::AfterWrite),
        ("inplace", Point::AfterSetLen),
    ] {
        let dir = Scratch::new("abort");
        let path = dir.path().join(MARKER_FILE);
        let clean = marker(MarkerState::Clean {
            snap: [1; 32],
            state_bin: [2; 32],
        });
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
        // Whatever is left either is the old clean marker, the new dirty
        // one, or fails to decode: never a different clean proof.
        match read_marker(dir.path()) {
            Found::Ok(m) => assert!(
                m == clean || m.state == MarkerState::Dirty,
                "{mode} {point:?}"
            ),
            Found::Bad(_) => {}
            Found::Missing => panic!("{mode} {point:?}: marker vanished"),
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
    fs::create_dir_all(&mine).unwrap();
    fs::create_dir_all(&live).unwrap();
    let (mine_c, live_c) = (canonical(&mine).unwrap(), canonical(&live).unwrap());
    let live_marker = Marker {
        run: RunHeader {
            dir_id: [0x22; 16],
            ..run()
        },
        ..marker(MarkerState::Dirty)
    };
    fs::write(live.join(MARKER_FILE), live_marker.encode()).unwrap();
    let put = |id: Id16, path: &str| {
        let w = Witness {
            dir_id: id,
            path: path.into(),
            ..witness()
        };
        fs::write(witness_path(&wdir, &id), w.encode()).unwrap();
    };
    put([0x11; 16], &mine_c); // this directory's current id
    put([0x10; 16], &mine_c); // an earlier id of this directory
    put([0x22; 16], &live_c); // another live data directory
    put([0x33; 16], "/gone"); // a directory that no longer exists
    fs::write(wdir.join("torn.gen"), b"not a witness").unwrap();
    assert_eq!(prune_witnesses(&wdir, &[0x11; 16], &mine_c).unwrap(), 2);
    assert!(witness_path(&wdir, &[0x11; 16]).exists());
    assert!(witness_path(&wdir, &[0x22; 16]).exists());
    assert!(!witness_path(&wdir, &[0x10; 16]).exists());
    assert!(!witness_path(&wdir, &[0x33; 16]).exists());
    assert!(wdir.join("torn.gen").exists());
}

// ---- Lock -------------------------------------------------------------------------

#[test]
fn the_lock_is_exclusive_and_released_on_drop() {
    let dir = Scratch::new("lock");
    let LockAttempt::Held(first) = try_lock(dir.path()).unwrap() else {
        // A filesystem without locks: the run would report `locked`.
        return;
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
