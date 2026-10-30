//! Unit and property tests for the parts with no I/O: the row-to-record
//! mapping, positions, the legacy cursor and the connection settings.

use std::time::Duration;

use proptest::prelude::*;

use super::conn::{Target, session_sql, target};
use super::*;

const NOW: i64 = 1_700_000_000;

fn torrent(status: Status) -> RawTorrent {
    RawTorrent {
        key: vec![7; 20],
        name: Bytes::Value(b"Some.Show.S01".to_vec()),
        size: Some(3000),
        private: Some(false),
        status,
        files_count: None,
        piece_length: Some(1 << 18),
        pieces_len: Some(20),
        created_at: Some(1_600_000_000),
    }
}

fn f(index: i32, path: &[u8], size: i64) -> RawFile {
    RawFile {
        index: Some(index),
        path: Bytes::Value(path.to_vec()),
        size: Some(size),
    }
}

fn norm(row: &RawTorrent, files: &[RawFile]) -> (Record, Findings) {
    let mut findings = Findings::default();
    let r = Normaliser { now: NOW }.torrent(row, files, 0, &mut findings);
    (r, findings)
}

fn torrent_of(r: Record) -> TorrentRecord {
    match r {
        Record::Torrent(t) => t,
        other => panic!("not a torrent: {other:?}"),
    }
}

#[test]
fn a_multi_file_torrent_is_complete_in_index_order() {
    let mut row = torrent(Status::Multi);
    row.files_count = Some(2);
    let (r, findings) = norm(&row, &[f(0, b"E01.mkv", 1000), f(1, b"Subs/E01.srt", 2000)]);
    let t = torrent_of(r);
    assert_eq!(t.completeness, Completeness::ReportedComplete);
    assert_eq!((t.repair, t.files_dropped, t.single_file), (0, 0, false));
    assert_eq!(t.files[1].path, vec![b"Subs".to_vec(), b"E01.srt".to_vec()]);
    assert_eq!((t.piece_len, t.piece_count), (1 << 18, 1));
    assert_eq!(findings, Findings::default());
    let info = t.info();
    assert_eq!((info.total_size, info.piece_count), (3000, 1));
}

#[test]
fn single_is_claimed_only_without_file_rows() {
    let (r, _) = norm(&torrent(Status::Single), &[]);
    let t = torrent_of(r);
    assert!(t.single_file && t.completeness == Completeness::ReportedComplete);
    assert_eq!(t.files.len(), 1);
    assert_eq!(t.files[0].path, vec![b"Some.Show.S01".to_vec()]);
    assert!(t.info().single_file);

    let (r, findings) = norm(&torrent(Status::Single), &[f(0, b"x", 3000)]);
    let t = torrent_of(r);
    assert!(!t.single_file);
    assert_eq!(t.completeness, Completeness::Partial);
    assert_eq!(findings.get(Finding::StatusMismatch), 1);
}

#[test]
fn incomplete_statuses_and_lists_are_partial() {
    for (status, finding) in [
        (Status::OverThreshold, Finding::OverThreshold),
        (Status::NoInfo, Finding::NoInfo),
    ] {
        let (r, findings) = norm(&torrent(status), &[]);
        let t = torrent_of(r);
        assert_eq!(t.completeness, Completeness::Partial);
        assert_eq!(t.repair, repair::MISSING_FILES);
        assert_eq!(findings.get(finding), 1);
        let (r, _) = norm(&torrent(status), &[f(0, b"a", 3000)]);
        assert_eq!(torrent_of(r).repair, repair::INCONSISTENT_FILES);
    }
    // Multi with no rows, a gap in the indexes, too few rows for
    // files_count, and sizes that don't add up.
    let (r, findings) = norm(&torrent(Status::Multi), &[]);
    assert_eq!(torrent_of(r).repair, repair::MISSING_FILES);
    assert_eq!(findings.get(Finding::NoFiles), 1);
    let (r, findings) = norm(
        &torrent(Status::Multi),
        &[f(0, b"a", 1000), f(2, b"b", 2000)],
    );
    assert_eq!(torrent_of(r).repair, repair::INCONSISTENT_FILES);
    assert_eq!(findings.get(Finding::FilesIncomplete), 1);
    let mut row = torrent(Status::Multi);
    row.files_count = Some(3);
    let (r, findings) = norm(&row, &[f(0, b"a", 1000), f(1, b"b", 2000)]);
    assert_eq!(torrent_of(r).completeness, Completeness::Partial);
    assert_eq!(findings.get(Finding::FilesIncomplete), 1);
    let (r, findings) = norm(&torrent(Status::Multi), &[f(0, b"a", 1)]);
    assert_eq!(torrent_of(r).completeness, Completeness::Partial);
    assert_eq!(findings.get(Finding::SizeMismatch), 1);
}

#[test]
fn malformed_file_rows_are_dropped_never_the_parent() {
    let bad = [
        f(0, b"", 1),
        f(0, b"/a", 1),
        f(0, b"a//b", 1),
        f(0, b"a/", 1),
        f(0, b"a\0b", 1),
        f(0, b"a", -1),
        RawFile {
            index: None,
            path: Bytes::Value(b"a".to_vec()),
            size: Some(1),
        },
        RawFile {
            index: Some(0),
            path: Bytes::TooLong(5000),
            size: Some(1),
        },
        RawFile {
            index: Some(0),
            path: Bytes::Null,
            size: Some(1),
        },
        RawFile {
            index: Some(0),
            path: Bytes::Value(b"a".to_vec()),
            size: None,
        },
        f(0, &b"a/".repeat(65)[..129], 1),
    ];
    for b in &bad {
        let (r, findings) = norm(&torrent(Status::Multi), &[b.clone(), f(1, b"ok", 3000)]);
        let t = torrent_of(r);
        assert_eq!(t.files.len(), 1, "{b:?}");
        assert_eq!(t.files_dropped, 1);
        assert_eq!(t.repair, repair::INCONSISTENT_FILES);
        assert_eq!(findings.get(Finding::FileMalformed), 1);
    }
    // `.` and `..` stay bytes.
    let (r, _) = norm(&torrent(Status::Multi), &[f(0, b"../x", 3000)]);
    assert_eq!(
        torrent_of(r).files[0].path,
        vec![b"..".to_vec(), b"x".to_vec()]
    );
}

#[test]
fn raw_bytes_are_kept_and_flagged() {
    let mut row = torrent(Status::Multi);
    row.name = Bytes::Value(vec![b'n', 0xff, 0xfe]);
    let (r, findings) = norm(&row, &[f(0, &[b'd', 0xc0, b'/', b'f'], 3000)]);
    let t = torrent_of(r);
    assert_eq!(t.name, vec![b'n', 0xff, 0xfe]);
    assert_eq!(t.files[0].path, vec![vec![b'd', 0xc0], b"f".to_vec()]);
    assert_eq!(findings.get(Finding::NameLossy), 1);
    assert_eq!(findings.get(Finding::PathLossy), 1);
    assert_eq!(t.info().name.raw, vec![b'n', 0xff, 0xfe]);
}

#[test]
fn rows_missing_a_name_or_size_are_kept_as_candidates() {
    for (name, size, finding) in [
        (Bytes::Value(Vec::new()), Some(5), Finding::MissingName),
        (Bytes::Null, Some(5), Finding::MissingName),
        (Bytes::TooLong(5000), Some(5), Finding::NameOverLimit),
        (
            Bytes::Value(vec![b'a'; 4097]),
            Some(5),
            Finding::NameOverLimit,
        ),
        (Bytes::Value(b"a".to_vec()), Some(0), Finding::MissingSize),
        (Bytes::Value(b"a".to_vec()), Some(-1), Finding::MissingSize),
        (Bytes::Value(b"a".to_vec()), None, Finding::MissingSize),
    ] {
        let mut row = torrent(Status::Multi);
        row.private = Some(true);
        row.name = name;
        row.size = size;
        let (r, findings) = norm(&row, &[f(0, b"a", 1)]);
        let Record::Candidate(c) = r else {
            panic!("{r:?}")
        };
        assert!(c.private);
        assert_eq!(findings.get(finding), 1);
        assert_eq!(
            c.import(Decision {
                hits: &[],
                marks: 0
            })
            .privacy,
            Privacy::Private
        );
    }
}

#[test]
fn bad_hashes_dates_and_pieces() {
    for key in [vec![], vec![1; 19], vec![1; 32], vec![0; 20]] {
        let mut row = torrent(Status::Single);
        row.key = key;
        let (r, findings) = norm(&row, &[]);
        assert_eq!(r, Record::Invalid(Finding::BadHash));
        assert_eq!(findings.get(Finding::BadHash), 1);
    }
    for date in [
        None,
        Some(0),
        Some(MIN_DATE - 1),
        Some(NOW + FUTURE_SLACK + 1),
    ] {
        let mut row = torrent(Status::Single);
        row.created_at = date;
        let (r, findings) = norm(&row, &[]);
        assert_eq!(torrent_of(r).source_at, None);
        assert_eq!(findings.get(Finding::DateOutOfRange), 1);
    }
    for (len, bytes) in [
        (Some(0), Some(20)),
        (Some(1 << 18), Some(21)),
        (Some(1 << 18), Some(40)),
        (Some(-5), Some(20)),
        (Some(1 << 18), None),
    ] {
        let mut row = torrent(Status::Single);
        row.piece_length = len;
        row.pieces_len = bytes;
        let (r, findings) = norm(&row, &[]);
        let t = torrent_of(r);
        assert_eq!((t.piece_len, t.piece_count), (0, 0));
        assert_eq!(findings.get(Finding::PiecesMalformed), 1);
    }
}

#[test]
fn privacy_is_private_only_when_true() {
    for (p, want) in [
        (Some(true), Privacy::Private),
        (Some(false), Privacy::Unknown),
        (None, Privacy::Unknown),
    ] {
        let mut row = torrent(Status::Single);
        row.private = p;
        let t = torrent_of(norm(&row, &[]).0);
        assert_eq!(privacy(t.private), want);
        assert_eq!(t.info().private, want == Privacy::Private);
    }
}

#[test]
fn positions_round_trip_and_refuse_others() {
    assert_eq!(parse_position(&[]).unwrap(), None);
    let p = position(&[3; 20]);
    assert_eq!(parse_position(&p).unwrap(), Some(&[3u8; 20][..]));
    assert_eq!(parse_position(&position(&[])).unwrap(), Some(&[][..]));
    assert_eq!(parse_position(&[2, 1]), Err(Refusal::BadPosition));
    assert_eq!(
        parse_position(&position(&[1; MAX_KEY + 1])),
        Err(Refusal::BadPosition)
    );
    assert!(
        position(&[1; MAX_KEY]).len() <= 512,
        "IMP-1's position limit"
    );
}

#[test]
fn legacy_cursors_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cursor");
    assert_eq!(read_cursor(&path).unwrap(), Vec::<u8>::new());
    std::fs::write(&path, "07".repeat(20)).unwrap();
    assert_eq!(read_cursor(&path).unwrap(), position(&[7; 20]));
    write_cursor(&path, &position(&[9; 32])).unwrap();
    assert_eq!(read_cursor(&path).unwrap(), position(&[9; 32]));
    for bad in ["0", "zz", "0201"] {
        std::fs::write(&path, bad).unwrap();
        assert!(read_cursor(&path).is_err(), "{bad}");
    }
}

#[test]
fn debug_prints_no_content() {
    let mut row = torrent(Status::Multi);
    row.name = Bytes::Value(b"secret-name".to_vec());
    row.key = vec![0xab; 20];
    let file = f(0, b"secret/path", 4242);
    let (r, findings) = norm(&row, std::slice::from_ref(&file));
    let b = Batch {
        records: vec![r.clone()],
        position: position(&row.key),
        source_bytes: 1,
        findings,
    };
    let mut s = Settings::new("db.example", "bm", "reader");
    s.password = Some(Password::new(b"hunter2-pw".to_vec()));
    let text = format!("{row:?} {file:?} {r:?} {b:?} {s:?}");
    for leak in ["secret", "4242", "171", "hunter2", "3000", "1600000000"] {
        assert!(!text.contains(leak), "{leak} in {text}");
    }
}

#[test]
fn passwords_come_from_a_bounded_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pw");
    std::fs::write(&path, b"pw\r\n").unwrap();
    assert_eq!(&Password::from_file(&path).unwrap().0[..], b"pw");
    std::fs::write(&path, b"\n").unwrap();
    assert!(Password::from_file(&path).is_err());
    std::fs::write(&path, vec![b'a'; 4097]).unwrap();
    assert!(Password::from_file(&path).is_err());
    assert!(Password::from_file(&dir.path().join("none")).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(password_file_warning(&path).is_some());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(password_file_warning(&path).is_none());
    }
}

#[test]
fn plaintext_is_refused_off_loopback_before_any_io() {
    let plain = |host: &str| {
        let mut s = Settings::new(host, "bitmagnet", "reader");
        s.tls = Tls::PlaintextLoopback;
        target(&s)
    };
    for host in [
        "192.0.2.1",
        "db.example.invalid",
        "[2001:db8::1]",
        "10.0.0.1",
    ] {
        assert_eq!(plain(host), Err(Refusal::TlsRequired), "{host}");
    }
    assert_eq!(
        plain("127.0.0.1"),
        Ok(Target::Loopback("127.0.0.1".parse().unwrap()))
    );
    assert_eq!(plain("[::1]"), Ok(Target::Loopback("::1".parse().unwrap())));
    assert_eq!(
        plain("/var/run/postgresql"),
        Ok(Target::Socket("/var/run/postgresql".into()))
    );
    // TLS is fine anywhere but a socket, and the default.
    assert_eq!(
        target(&Settings::new("db.example.invalid", "bitmagnet", "reader")),
        Ok(Target::Remote)
    );
    assert_eq!(
        target(&Settings::new("/tmp", "bitmagnet", "reader")),
        Err(Refusal::BadSettings)
    );
}

#[test]
fn settings_are_structured() {
    let ok = Settings::new("db.example", "bitmagnet", "reader");
    for bad in [
        Settings::new("db.example host=evil", "bitmagnet", "reader"),
        Settings::new("a,b", "bitmagnet", "reader"),
        Settings::new("", "bitmagnet", "reader"),
        Settings::new("db.example", "", "reader"),
        Settings::new("db.example", "bitmagnet", &"u".repeat(64)),
        Settings::new("db.example", "bit\0magnet", "reader"),
        Settings {
            port: 0,
            ..ok.clone()
        },
        Settings {
            statement_timeout: Duration::ZERO,
            ..ok.clone()
        },
    ] {
        assert_eq!(target(&bad), Err(Refusal::BadSettings), "{bad:?}");
    }
}

#[test]
fn every_session_is_read_only_and_bounded() {
    let sql = session_sql(Duration::from_secs(30));
    for part in [
        "default_transaction_read_only = on",
        "statement_timeout = 30000",
        "lock_timeout = 10000",
        "idle_in_transaction_session_timeout = 90000",
        "search_path = pg_catalog",
        "row_security = off",
    ] {
        assert!(sql.contains(part), "{part}");
    }
    // Every read is schema-qualified: nothing resolves through a search
    // path the source controls.
    for caps in [
        Caps {
            files_status: true,
            files_count: true,
            pieces: Pieces::Table,
        },
        Caps {
            files_status: false,
            files_count: false,
            pieces: Pieces::Columns,
        },
    ] {
        for sql in [torrents_sql(&caps, true), files_sql(false)] {
            for table in ["torrents ", "torrent_files ", "torrent_pieces "] {
                for kw in ["FROM", "JOIN"] {
                    assert!(
                        !sql.contains(&format!("{kw} {table}")),
                        "unqualified: {sql}"
                    );
                }
            }
        }
    }
}

#[test]
fn refusals_map_to_job_reasons() {
    assert_eq!(
        Refusal::SourceChanged.job_reason(),
        Some(Reason::SourceChanged)
    );
    assert_eq!(
        Refusal::Unavailable("auth_failed").job_reason(),
        Some(Reason::NeedsCredentials)
    );
    assert_eq!(
        Refusal::NotQuiescent.job_reason(),
        Some(Reason::SourceUnavailable)
    );
    for r in [
        Refusal::TlsRequired,
        Refusal::BadSettings,
        Refusal::UnsupportedSchema(vec![]),
        Refusal::BadKey,
        Refusal::Cancelled,
    ] {
        assert_eq!(r.job_reason(), None, "{r:?}");
    }
}

fn arb_bytes() -> impl Strategy<Value = Bytes> {
    prop_oneof![
        Just(Bytes::Null),
        any::<u64>().prop_map(Bytes::TooLong),
        prop::collection::vec(any::<u8>(), 0..40).prop_map(Bytes::Value),
        prop::collection::vec(prop_oneof![Just(b'/'), Just(b'a'), Just(0xff)], 0..12)
            .prop_map(Bytes::Value),
    ]
}

fn arb_status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Single),
        Just(Status::Multi),
        Just(Status::OverThreshold),
        Just(Status::NoInfo),
    ]
}

fn arb_torrent() -> impl Strategy<Value = RawTorrent> {
    (
        prop_oneof![
            prop::collection::vec(any::<u8>(), 0..33),
            prop::collection::vec(any::<u8>(), 20..21)
        ],
        arb_bytes(),
        prop::option::of(prop_oneof![any::<i64>(), -2i64..10_000]),
        prop::option::of(any::<bool>()),
        arb_status(),
        prop::option::of(-1i32..5),
        (
            prop::option::of(prop_oneof![any::<i64>(), 1i64..5]),
            prop::option::of(prop_oneof![any::<i32>(), 0i32..100]),
        ),
        prop::option::of(any::<i64>()),
    )
        .prop_map(
            |(
                key,
                name,
                size,
                private,
                status,
                files_count,
                (piece_length, pieces_len),
                created_at,
            )| {
                RawTorrent {
                    key,
                    name,
                    size,
                    private,
                    status,
                    files_count,
                    piece_length,
                    pieces_len,
                    created_at,
                }
            },
        )
}

fn arb_file() -> impl Strategy<Value = RawFile> {
    (
        prop::option::of(-1i32..6),
        arb_bytes(),
        prop::option::of(prop_oneof![any::<i64>(), -1i64..3_000]),
    )
        .prop_map(|(index, path, size)| RawFile { index, path, size })
}

proptest! {
    /// Arbitrary bytes, odd sizes and missing values: the mapping never
    /// panics, and what it claims holds.
    #[test]
    fn the_mapping_keeps_its_claims(
        row in arb_torrent(),
        files in prop::collection::vec(arb_file(), 0..8),
        overflow in 0u32..3,
    ) {
        let mut findings = Findings::default();
        let r = Normaliser { now: NOW }.torrent(&row, &files, overflow, &mut findings);
        let good_hash = row.key.len() == 20 && row.key.iter().any(|b| *b != 0);
        match r {
            Record::Invalid(f) => {
                prop_assert!(!good_hash);
                prop_assert_eq!(f, Finding::BadHash);
            }
            Record::Candidate(c) => {
                prop_assert!(good_hash);
                prop_assert!(c.name.is_none() || c.size.is_none());
                prop_assert!(c.name.as_ref().is_none_or(|n| !n.is_empty() && n.len() <= 4096));
                prop_assert!(c.size.is_none_or(|s| s > 0));
                prop_assert_eq!(c.private, row.private == Some(true));
            }
            Record::Torrent(t) => {
                prop_assert!(good_hash);
                prop_assert!(!t.name.is_empty() && t.name.len() <= 4096 && t.size > 0);
                prop_assert_eq!(t.private, row.private == Some(true));
                prop_assert!(t.source_at.is_none_or(|d| d >= MIN_DATE));
                if t.single_file {
                    prop_assert!(files.is_empty() && overflow == 0);
                    prop_assert_eq!(t.files.len(), 1);
                } else {
                    prop_assert_eq!(t.files.len() as u64 + u64::from(t.files_dropped),
                        files.len() as u64 + u64::from(overflow));
                }
                let complete = t.completeness == Completeness::ReportedComplete;
                prop_assert_eq!(complete, t.repair == 0);
                if complete {
                    prop_assert_eq!(t.files_dropped, 0);
                    prop_assert_eq!(t.files.iter().map(|f| f.size).sum::<u64>(), t.size);
                    prop_assert!(t.single_file || row.status == Status::Multi);
                }
                if t.piece_count > 0 {
                    prop_assert_eq!(t.size.div_ceil(t.piece_len), t.piece_count);
                }
                for f in &t.files {
                    prop_assert!(!f.path.is_empty() && f.path.iter().all(|s| !s.is_empty()));
                }
            }
        }
    }
}
