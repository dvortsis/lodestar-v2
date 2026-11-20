//! Pins the store's reader of identify terms records, which the snapshot
//! purge uses (`lodestar_store::snapshots::claims`), to the identify
//! runtime's own: the keys, the slack, `Terms::record` and
//! `Terms::from_record`, and `Retention::drop_after`
//! (X-snapshot-claims-retention). The store can't depend on
//! `lodestar-sources`, so the two are kept in step here.

use std::time::Duration;

use lodestar_pipeline::identify;
use lodestar_sources::terms::{Retention, Terms};
use lodestar_store::snapshots::claims;
use lodestar_store::{Store, StoreConfig};
use proptest::prelude::*;

/// The retention the live sweep (`identify::sweep_recorded`) reads from a
/// stored record.
fn live_retention(record: &[u8]) -> Retention {
    Terms::from_record(record).map_or(Retention::DEFAULT, |t| t.retention)
}

#[test]
fn keys_slack_and_default_match() {
    assert_eq!(claims::TERMS_KEY, identify::TERMS_KEY);
    assert_eq!(claims::CLOCK_KEY, identify::CLOCK_KEY);
    assert_eq!(claims::FUTURE_SLACK_SECS, identify::FUTURE_SLACK_SECS);
    assert_eq!(
        Duration::from_secs(claims::DEFAULT_MAX_AGE_SECS),
        Retention::DEFAULT.max_age
    );
    assert_eq!(
        claims::drop_after(claims::max_age_secs(&[])),
        Retention::DEFAULT.drop_after()
    );
}

#[test]
fn a_clock_mark_reads_as_the_sweep_writes_it() {
    let now: i64 = 1_800_000_000;
    assert_eq!(claims::read_clock(&now.to_be_bytes()), Some(now));
    assert_eq!(claims::read_clock(&[1, 2, 3]), None);
}

proptest! {
    /// Every record the runtime writes reads as the same drop age.
    #[test]
    fn written_records_read_alike(
        bg: bool,
        re: bool,
        adult: bool,
        secs in prop_oneof![0u64..1_000_000_000, any::<u64>()],
    ) {
        let t = Terms {
            background_walk: bg,
            redistribute: re,
            retention: Retention { max_age: Duration::from_secs(secs) },
            attribution: None,
            adult_marker_required: adult,
        };
        let record = t.record();
        prop_assert_eq!(
            claims::drop_after(claims::max_age_secs(&record)),
            live_retention(&record).drop_after()
        );
    }

    /// Any bytes at all, valid or not, read alike: an unreadable record
    /// is the default in both.
    #[test]
    fn any_bytes_read_alike(
        record in prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..14),
            (any::<u8>(), any::<u64>()).prop_map(|(f, s)| {
                let mut v = vec![1, f];
                v.extend(s.to_be_bytes());
                v
            }),
        ],
    ) {
        prop_assert_eq!(
            claims::drop_after(claims::max_age_secs(&record)),
            live_retention(&record).drop_after()
        );
    }

    /// The window is the live sweep's: a claim stamped `at` is dropped by
    /// `identify::sweep_expired` at `now` exactly when it's outside
    /// `claims::window`.
    #[test]
    fn the_window_is_the_sweeps(
        days in 0u64..2_000,
        now in 1_000_000_000i64..2_000_000_000,
        offset in prop_oneof![-200_000_000i64..200_000_000, -3i64..3, 3_597i64..3_603],
    ) {
        let r = Retention::days(days);
        let drop = r.drop_after();
        // Near both edges (the drop age, the slack) as often as anywhere.
        let at = if (-3..3).contains(&offset) {
            now - drop + offset
        } else {
            now + offset
        };
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let src = s.register_source("plugin", "1", 0).unwrap();
        s.connection()
            .execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                 VALUES (1, ?1, 't', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                [&[7u8; 20][..]],
            )
            .unwrap();
        let c = lodestar_store::claims::Claims {
            details: Some(Box::new(lodestar_store::claims::Details {
                canonical_title: Some("The Film".into()),
                ..Default::default()
            })),
            ..Default::default()
        };
        prop_assert!(s.put_claims_at(1, src, &c, false, at).unwrap());
        let swept = identify::sweep_expired(&mut s, src, &r, now).unwrap();
        let record = Terms { retention: r, ..Terms::STRICT }.record();
        let w = claims::window(claims::max_age_secs(&record), now);
        prop_assert_eq!(!swept.dropped.is_empty(), at < w.before || at > w.after);
    }
}
