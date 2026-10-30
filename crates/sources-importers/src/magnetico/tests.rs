use std::path::{Path, PathBuf};

use lodestar_store::import::{
    BatchCommitted, Completeness, Decision, ImportJob, ImportRecord, NewImportJob, Phase, Privacy,
    Verification, repair,
};
use lodestar_store::{Store, StoreConfig};
use proptest::prelude::*;
use rusqlite::types::Value;

use super::fixtures::{FileRow, Layout, TorrentRow, build, insert_file};
use super::*;

const NOW: i64 = 1_700_000_000;
const PASS: Decision<'static> = Decision {
    hits: &[],
    marks: 0,
};

fn row(hash: Raw, name: Raw, size: Raw, date: Raw) -> RawTorrent {
    RawTorrent {
        id: 1,
        info_hash: hash,
        name,
        total_size: size,
        discovered_on: date,
    }
}

fn good_hash() -> Raw {
    Raw::Blob(vec![7; 20])
}

fn good_row() -> RawTorrent {
    row(
        good_hash(),
        Raw::Text(b"name".to_vec()),
        Raw::Integer(10),
        Raw::Integer(NOW - 10),
    )
}

fn f(path: &[u8], size: i64) -> RawFile {
    RawFile {
        path: Raw::Text(path.to_vec()),
        size: Raw::Integer(size),
    }
}

fn norm(r: &RawTorrent, files: &[RawFile], overflow: u32) -> (Record, Findings) {
    let mut findings = Findings::default();
    let rec = Normaliser { now: NOW }.torrent(r, files, overflow, &mut findings);
    (rec, findings)
}

fn torrent(r: Record) -> TorrentRecord {
    match r {
        Record::Torrent(t) => t,
        r => panic!("not a torrent: {r:?}"),
    }
}

// ---- The row normaliser ----

#[test]
fn a_good_row_is_reported_complete_and_unverified() {
    let (rec, findings) = norm(&good_row(), &[f(b"dir/a", 4), f(b"dir/b", 6)], 0);
    let t = torrent(rec);
    assert_eq!(t.completeness, Completeness::ReportedComplete);
    assert_eq!(t.repair, 0);
    assert_eq!(t.source_at, Some(NOW - 10));
    assert_eq!(t.files[0].path, vec![b"dir".to_vec(), b"a".to_vec()]);
    assert_eq!(findings, Findings::default());
    let info = t.info();
    assert!(!info.single_file && !info.private);
    assert_eq!(info.info_size, 0);
}

#[test]
fn bad_hashes_are_invalid() {
    for h in [
        Raw::Blob(vec![1; 19]),
        Raw::Blob(vec![1; 32]),
        Raw::Blob(vec![0; 20]),
        Raw::Text(vec![b'a'; 20]),
        Raw::Text(b"0123456789abcdef0123456789abcdef01234567".to_vec()),
        Raw::Integer(5),
        Raw::Null,
    ] {
        let mut r = good_row();
        r.info_hash = h;
        let (rec, findings) = norm(&r, &[f(b"a", 10)], 0);
        assert_eq!(rec, Record::Invalid(Finding::BadHash));
        assert_eq!(findings.get(Finding::BadHash), 1);
    }
}

#[test]
fn missing_or_bad_sizes_and_names_make_candidates() {
    for (name, size, missing) in [
        (Raw::Null, Raw::Integer(10), Finding::MissingName),
        (
            Raw::Text(Vec::new()),
            Raw::Integer(10),
            Finding::MissingName,
        ),
        (Raw::Integer(3), Raw::Integer(10), Finding::MissingName),
        (
            Raw::Text(b"n".to_vec()),
            Raw::Integer(0),
            Finding::MissingSize,
        ),
        (
            Raw::Text(b"n".to_vec()),
            Raw::Integer(-1),
            Finding::MissingSize,
        ),
        (
            Raw::Text(b"n".to_vec()),
            Raw::Integer(i64::MIN),
            Finding::MissingSize,
        ),
        (
            Raw::Text(b"n".to_vec()),
            Raw::Real(10.0),
            Finding::MissingSize,
        ),
        (
            Raw::Text(b"n".to_vec()),
            Raw::Text(b"10".to_vec()),
            Finding::MissingSize,
        ),
        (Raw::Text(b"n".to_vec()), Raw::Null, Finding::MissingSize),
    ] {
        let r = row(good_hash(), name, size, Raw::Integer(NOW));
        let (rec, findings) = norm(&r, &[f(b"a", 10)], 0);
        let Record::Candidate(c) = rec else {
            panic!("{missing:?}: not a candidate")
        };
        assert_eq!(c.hash, [7; 20]);
        assert_eq!(findings.get(missing), 1);
    }
}

#[test]
fn a_huge_size_is_kept_and_checked_against_the_files() {
    let mut r = good_row();
    r.total_size = Raw::Integer(i64::MAX);
    let t = torrent(norm(&r, &[f(b"a", i64::MAX), f(b"b", i64::MAX)], 0).0);
    // The sum overflows nothing in u64 here, but doesn't match.
    assert_eq!(t.completeness, Completeness::Partial);
    assert_eq!(t.repair, repair::INCONSISTENT_FILES);
}

#[test]
fn invalid_utf8_is_kept_as_bytes_with_a_flag() {
    let mut r = good_row();
    r.name = Raw::Blob(vec![b'a', 0xff, 0xfe]);
    let (rec, findings) = norm(&r, &[f(b"x\xc3/y", 10)], 0);
    let t = torrent(rec);
    assert_eq!(t.name, vec![b'a', 0xff, 0xfe]);
    assert_eq!(t.files[0].path[0], b"x\xc3".to_vec());
    assert_eq!(findings.get(Finding::NameLossy), 1);
    assert_eq!(findings.get(Finding::PathLossy), 1);
}

#[test]
fn an_oversized_name_is_never_truncated() {
    let mut r = good_row();
    r.name = Raw::Text(vec![b'a'; 4097]);
    let (rec, findings) = norm(&r, &[f(b"a", 10)], 0);
    let Record::Candidate(c) = rec else {
        panic!("not a candidate")
    };
    assert_eq!(c.name, None);
    assert_eq!(c.size, Some(10));
    assert_eq!(findings.get(Finding::NameOverLimit), 1);
}

#[test]
fn malformed_file_rows_mark_the_parent_partial_and_keep_it() {
    let long = vec![b'a'; 4097];
    let deep = b"a/".repeat(64);
    for bad in [
        f(b"", 1),
        f(b"/a", 1),
        f(b"a/", 1),
        f(b"a//b", 1),
        f(b"a\0b", 1),
        f(&long, 1),
        f(&[deep.as_slice(), b"z"].concat(), 1),
        f(b"a", -1),
        RawFile {
            path: Raw::Null,
            size: Raw::Integer(1),
        },
        RawFile {
            path: Raw::Text(b"a".to_vec()),
            size: Raw::Real(1.0),
        },
    ] {
        let (rec, findings) = norm(&good_row(), &[f(b"ok", 10), bad], 0);
        let t = torrent(rec);
        assert_eq!(t.files.len(), 1);
        assert_eq!(t.files_dropped, 1);
        assert_eq!(t.completeness, Completeness::Partial);
        assert_eq!(t.repair, repair::INCONSISTENT_FILES);
        assert_eq!(findings.get(Finding::FileMalformed), 1);
    }
}

#[test]
fn dot_segments_are_kept_as_bytes() {
    let t = torrent(norm(&good_row(), &[f(b"../../etc/x", 10)], 0).0);
    assert_eq!(t.files[0].path[0], b"..".to_vec());
    assert_eq!(t.completeness, Completeness::ReportedComplete);
}

#[test]
fn a_parent_with_no_files_is_partial() {
    let (rec, findings) = norm(&good_row(), &[], 0);
    let t = torrent(rec);
    assert_eq!(t.completeness, Completeness::Partial);
    assert_eq!(t.repair, repair::MISSING_FILES);
    assert_eq!(findings.get(Finding::NoFiles), 1);
    let (rec, _) = norm(&good_row(), &[f(b"", 1)], 0);
    assert_eq!(
        torrent(rec).repair,
        repair::MISSING_FILES | repair::INCONSISTENT_FILES
    );
}

#[test]
fn file_sizes_must_add_up() {
    let (rec, findings) = norm(&good_row(), &[f(b"a", 3)], 0);
    let t = torrent(rec);
    assert_eq!(t.completeness, Completeness::Partial);
    assert_eq!(t.repair, repair::INCONSISTENT_FILES);
    assert_eq!(findings.get(Finding::SizeMismatch), 1);
}

#[test]
fn rows_over_the_reader_budget_are_counted_dropped() {
    let (rec, findings) = norm(&good_row(), &[f(b"a", 10)], 5);
    let t = torrent(rec);
    assert_eq!(t.files_dropped, 5);
    assert_eq!(t.completeness, Completeness::Partial);
    assert_eq!(findings.get(Finding::FilesOverLimit), 1);
}

#[test]
fn dates_out_of_range_are_unknown() {
    for d in [
        Raw::Integer(0),
        Raw::Integer(-5),
        Raw::Integer(MIN_DATE - 1),
        Raw::Integer(NOW + FUTURE_SLACK + 1),
        Raw::Integer(NOW * 1000),
        Raw::Real(NOW as f64),
        Raw::Text(b"2020".to_vec()),
        Raw::Null,
    ] {
        let mut r = good_row();
        r.discovered_on = d;
        let (rec, findings) = norm(&r, &[f(b"a", 10)], 0);
        assert_eq!(torrent(rec).source_at, None);
        assert_eq!(findings.get(Finding::DateOutOfRange), 1);
    }
    let mut r = good_row();
    r.discovered_on = Raw::Integer(MIN_DATE);
    assert_eq!(
        torrent(norm(&r, &[f(b"a", 10)], 0).0).source_at,
        Some(MIN_DATE)
    );
}

#[test]
fn records_never_debug_print_names() {
    let mut r = good_row();
    r.name = Raw::Text(b"secret-name".to_vec());
    let rec = norm(&r, &[f(b"secret-path", 10)], 0).0;
    let s = format!("{rec:?}");
    assert!(!s.contains("secret"), "{s}");
}

#[test]
fn positions_round_trip_and_reject_others() {
    assert_eq!(parse_position(&position(42)), Ok(42));
    assert_eq!(parse_position(&[]), Ok(i64::MIN));
    assert_eq!(
        parse_position(&[2, 0, 0, 0, 0, 0, 0, 0, 1]),
        Err(Refusal::BadPosition)
    );
    assert_eq!(parse_position(&[1, 0]), Err(Refusal::BadPosition));
}

// ---- Opening and reading real fixture files ----

fn fixture(dir: &Path, name: &str, layout: Layout, rows: &[TorrentRow]) -> PathBuf {
    let path = dir.join(name);
    build(&path, layout, rows);
    path
}

fn rows(n: u32) -> Vec<TorrentRow> {
    (1..=n)
        .map(|i| TorrentRow::good(i, &[("d/a", 3), ("d/b", 4)]))
        .collect()
}

fn open_ok(path: &Path) -> Source {
    open(path, &OpenPolicy::default()).expect("open")
}

fn read_all(src: &mut Source) -> (Vec<Batch>, Vec<u8>) {
    let mut pos = Vec::new();
    let mut out = Vec::new();
    while let Some(b) = src.read_batch(&pos, NOW).expect("batch") {
        pos = b.position.clone();
        out.push(b);
    }
    (out, pos)
}

#[test]
fn supported_layouts_open_and_read() {
    let dir = tempfile::tempdir().unwrap();
    for layout in [Layout::V2, Layout::V3, Layout::V3NoSeeders, Layout::Lax] {
        let path = fixture(dir.path(), &format!("{layout:?}"), layout, &rows(3));
        let mut src = open_ok(&path);
        let (batches, _) = read_all(&mut src);
        let recs: Vec<_> = batches.into_iter().flat_map(|b| b.records).collect();
        assert_eq!(recs.len(), 3, "{layout:?}");
        for r in recs {
            assert_eq!(torrent(r).completeness, Completeness::ReportedComplete);
        }
    }
}

#[test]
fn layouts_without_a_files_index_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    for layout in [Layout::V0, Layout::V1] {
        let path = fixture(dir.path(), &format!("{layout:?}"), layout, &rows(1));
        let err = open(&path, &OpenPolicy::default()).unwrap_err();
        assert_eq!(
            err,
            Refusal::UnsupportedSchema(vec!["missing_index: files.torrent_id"])
        );
        assert_eq!(err.label(), "unsupported_schema");
        assert_eq!(err.job_reason(), None);
    }
}

#[test]
fn missing_columns_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("odd");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE torrents (id INTEGER PRIMARY KEY, info_hash, name);
         CREATE TABLE files (id TEXT PRIMARY KEY, torrent_id, size, path);",
    )
    .unwrap();
    drop(conn);
    let err = open(&path, &OpenPolicy::default()).unwrap_err();
    assert_eq!(
        err,
        Refusal::UnsupportedSchema(vec!["total_size", "discovered_on", "files.id as rowid"])
    );
    let empty = dir.path().join("empty");
    rusqlite::Connection::open(&empty).unwrap();
    assert_eq!(
        open(&empty, &OpenPolicy::default()).unwrap_err(),
        Refusal::UnsupportedSchema(vec!["torrents", "files"])
    );
}

#[test]
fn not_a_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk");
    std::fs::write(&path, vec![0x55; 8192]).unwrap();
    assert_eq!(
        open(&path, &OpenPolicy::default()).unwrap_err(),
        Refusal::Corrupt
    );
}

#[test]
fn query_plans_use_indexes_without_sorting() {
    let dir = tempfile::tempdir().unwrap();
    for layout in [Layout::V2, Layout::V3, Layout::V3NoSeeders] {
        let path = fixture(dir.path(), &format!("{layout:?}"), layout, &rows(2));
        let plans = open_ok(&path).plans().unwrap();
        assert!(
            plans
                .iter()
                .any(|p| p.starts_with("SEARCH torrents USING INTEGER PRIMARY KEY")),
            "{layout:?}: {plans:?}"
        );
        assert!(
            plans
                .iter()
                .any(|p| p.starts_with("SEARCH files USING INDEX readme_index")),
            "{layout:?}: {plans:?}"
        );
        assert!(
            !plans
                .iter()
                .any(|p| p.starts_with("SCAN") || p.contains("TEMP B-TREE")),
            "{layout:?}: {plans:?}"
        );
    }
}

#[test]
fn batches_are_keyset_paged_and_resume_mid_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "v3", Layout::V3, &rows(1_200));
    let mut src = open_ok(&path);
    let (batches, last) = read_all(&mut src);
    let sizes: Vec<_> = batches.iter().map(|b| b.records.len()).collect();
    assert_eq!(sizes, vec![500, 500, 200]);
    assert_eq!(parse_position(&last), Ok(1_200));
    // Resuming from the first batch's position, on a fresh open, yields
    // exactly what followed it.
    let mut again = open_ok(&path);
    assert_eq!(again.identity(), src.identity());
    let b = again
        .read_batch(&batches[0].position, NOW)
        .unwrap()
        .unwrap();
    assert_eq!(b.records, batches[1].records);
    assert!(again.read_batch(&last, NOW).unwrap().is_none());
}

#[test]
fn batches_stop_at_eight_mebibytes_and_never_split_a_parent() {
    let dir = tempfile::tempdir().unwrap();
    let path_bytes = "p".repeat(3_000);
    let files: Vec<(String, i64)> = (0..1_000)
        .map(|i| (format!("{path_bytes}/{i}"), 1))
        .collect();
    let files: Vec<(&str, i64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    // About 3 MB of paths each: two fit in a batch, the third doesn't.
    let rows: Vec<_> = (1..=3).map(|i| TorrentRow::good(i, &files)).collect();
    let path = fixture(dir.path(), "big", Layout::V3, &rows);
    let mut src = open_ok(&path);
    let (batches, _) = read_all(&mut src);
    let sizes: Vec<_> = batches.iter().map(|b| b.records.len()).collect();
    assert_eq!(sizes, vec![2, 1]);
    for b in &batches {
        assert!(b.source_bytes <= MAX_BATCH_BYTES as u64);
        for r in &b.records {
            let t = torrent(r.clone());
            assert_eq!(t.files.len(), 1_000);
            assert_eq!(t.completeness, Completeness::ReportedComplete);
        }
    }
}

#[test]
fn a_parent_over_the_budget_is_partial_never_complete() {
    let dir = tempfile::tempdir().unwrap();
    let long = "q".repeat(4_000);
    let files: Vec<(String, i64)> = (0..2_200).map(|i| (format!("{long}/{i}"), 1)).collect();
    let files: Vec<(&str, i64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    let path = fixture(
        dir.path(),
        "huge",
        Layout::V3,
        &[
            TorrentRow::good(1, &files),
            TorrentRow::good(2, &[("a", 1)]),
        ],
    );
    let mut src = open_ok(&path);
    let (batches, _) = read_all(&mut src);
    let recs: Vec<_> = batches.iter().flat_map(|b| b.records.clone()).collect();
    assert_eq!(recs.len(), 2);
    let t = torrent(recs[0].clone());
    assert!(t.files.len() < 2_200);
    assert_eq!(t.files.len() as u32 + t.files_dropped, 2_200);
    assert_eq!(t.completeness, Completeness::Partial);
    assert_eq!(batches[0].findings.get(Finding::FilesOverLimit), 1);
}

#[test]
fn orphans_and_duplicates_never_become_extra_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lax");
    let mut conn = super::fixtures::create(&path, Layout::Lax);
    let mut rs = rows(700);
    // Same hash at id 1 and id 600: two records, one per row.
    rs[599].info_hash = rs[0].info_hash.clone();
    super::fixtures::insert(&mut conn, &rs);
    conn.execute("DELETE FROM torrents WHERE id = 300", [])
        .unwrap();
    for tid in [Value::Null, Value::Integer(300), Value::Integer(10_000)] {
        insert_file(
            &conn,
            true,
            &FileRow {
                torrent_id: tid,
                size: Value::Integer(1),
                path: Value::Text("orphan".into()),
                readme: None,
            },
        );
    }
    conn.close().unwrap();
    let mut src = open_ok(&path);
    let (batches, _) = read_all(&mut src);
    let recs: Vec<_> = batches.into_iter().flat_map(|b| b.records).collect();
    assert_eq!(recs.len(), 699);
    let hashes: Vec<_> = recs.into_iter().map(|r| torrent(r).hash).collect();
    assert_eq!(hashes.iter().filter(|h| **h == hashes[0]).count(), 2);
    for t in &hashes {
        assert_ne!(*t, [0; 20]);
    }
}

#[test]
fn readme_rows_are_counted_not_imported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readme");
    let mut conn = super::fixtures::create(&path, Layout::V3);
    let ids = super::fixtures::insert(&mut conn, &rows(1));
    insert_file(
        &conn,
        true,
        &FileRow {
            torrent_id: Value::Integer(ids[0]),
            size: Value::Integer(0),
            path: Value::Text("README".into()),
            readme: Some(b"secret readme".to_vec()),
        },
    );
    conn.close().unwrap();
    let mut src = open_ok(&path);
    let b = src.read_batch(&[], NOW).unwrap().unwrap();
    assert_eq!(b.findings.get(Finding::ReadmeNotImported), 1);
    // The readme row is still a file of the torrent; its text isn't read.
    assert_eq!(torrent(b.records[0].clone()).files.len(), 3);
}

#[test]
fn the_source_file_is_unchanged_and_writes_fail() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "v3", Layout::V3, &rows(600));
    let before = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    {
        let mut src = open_ok(&path);
        read_all(&mut src);
        for sql in [
            "INSERT INTO torrents (info_hash, name, total_size, discovered_on) VALUES (x'01', 'a', 1, 1)",
            "DELETE FROM files",
            "CREATE TABLE x (y)",
            "PRAGMA user_version = 9",
            "ATTACH DATABASE ':memory:' AS other",
        ] {
            assert!(src.conn().execute_batch(sql).is_err(), "{sql}");
        }
        assert!(
            src.conn()
                .execute_batch("PRAGMA query_only = OFF; DELETE FROM files")
                .is_err()
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    // SQLite's WAL siblings may appear (runner, condition C); the adapter
    // never deletes them, and they hold nothing.
    let wal = sibling(&path, "-wal");
    if wal.exists() {
        assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    }
    // Our own empty -wal doesn't block a resume.
    open_ok(&path);
}

#[test]
fn a_live_source_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "v3", Layout::V3, &rows(1));
    std::fs::write(sibling(&path, "-wal"), b"x").unwrap();
    let err = open(&path, &OpenPolicy::default()).unwrap_err();
    assert_eq!(err, Refusal::NotQuiescent);
    assert_eq!(err.job_reason(), Some(Reason::SourceUnavailable));
    assert!(err.to_string().contains("stop magneticod"));
    std::fs::write(sibling(&path, "-wal"), b"").unwrap();
    open_ok(&path);
    std::fs::write(sibling(&path, "-journal"), b"").unwrap();
    assert_eq!(
        open(&path, &OpenPolicy::default()).unwrap_err(),
        Refusal::NotQuiescent
    );
}

#[test]
fn a_source_written_mid_import_stops_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "v3", Layout::V3, &rows(600));
    let mut src = open_ok(&path);
    let first = src.read_batch(&[], NOW).unwrap().unwrap();
    {
        let mut w = rusqlite::Connection::open(&path).unwrap();
        super::fixtures::insert(&mut w, &[TorrentRow::good(9_999, &[("a", 1)])]);
    }
    let err = src.read_batch(&first.position, NOW).unwrap_err();
    assert!(
        matches!(err, Refusal::SourceChanged | Refusal::NotQuiescent),
        "{err:?}"
    );
    assert!(err.job_reason().is_some());
    // While the write sits in a non-empty -wal, a reopen is refused.
    let old = src.identity();
    drop(src);
    assert_eq!(
        open(&path, &OpenPolicy::default()).unwrap_err(),
        Refusal::NotQuiescent
    );
    // Once magneticod stops cleanly (its close checkpoints the WAL), the
    // source is another dataset: IMP-1's revalidate turns that into
    // `source_changed`.
    let w = rusqlite::Connection::open(&path).unwrap();
    w.query_row("SELECT count(*) FROM torrents", [], |r| r.get::<_, i64>(0))
        .unwrap();
    w.close().unwrap();
    let reopened = open_ok(&path);
    assert_ne!(reopened.identity().dataset, old.dataset);
    assert_eq!(reopened.identity().schema_sig, old.schema_sig);
}

#[test]
fn identity_ignores_the_file_name() {
    let dir = tempfile::tempdir().unwrap();
    let a = fixture(dir.path(), "a", Layout::V3, &rows(5));
    let b = dir.path().join("b");
    std::fs::copy(&a, &b).unwrap();
    assert_eq!(open_ok(&a).identity(), open_ok(&b).identity());
    let c = fixture(dir.path(), "c", Layout::V2, &rows(5));
    assert_ne!(
        open_ok(&a).identity().schema_sig,
        open_ok(&c).identity().schema_sig
    );
}

#[test]
fn cancel_stops_reading() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "v3", Layout::V3, &rows(3));
    let mut src = open_ok(&path);
    src.canceller().store(true, Ordering::Relaxed);
    assert_eq!(src.read_batch(&[], NOW).unwrap_err(), Refusal::Cancelled);
}

#[test]
fn a_zero_deadline_interrupts_statements() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), "v3", Layout::V3, &rows(1));
    let policy = OpenPolicy {
        statement_timeout: Duration::ZERO,
        ..OpenPolicy::default()
    };
    // The schema checks are short enough to finish under the handler's
    // 10,000-step granularity or to time out; either way it never hangs.
    match open(&path, &policy) {
        Ok(src) => {
            let big = "SELECT count(*) FROM (WITH RECURSIVE c(x) AS \
                       (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT x FROM c)";
            src.deadline.arm();
            let e = src
                .conn()
                .query_row(big, [], |r| r.get::<_, i64>(0))
                .unwrap_err();
            assert_eq!(src.refusal(e), Refusal::StatementTimeout);
        }
        Err(e) => assert_eq!(e, Refusal::StatementTimeout),
    }
}

#[test]
fn paths_that_arent_plain_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        open(dir.path(), &OpenPolicy::default()).unwrap_err(),
        Refusal::NotAFile
    );
    assert_eq!(
        open(&dir.path().join("nope"), &OpenPolicy::default()).unwrap_err(),
        Refusal::Unavailable("not_found")
    );
}

#[cfg(unix)]
#[test]
fn symlinks_may_not_escape_their_folder() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let real = fixture(b.path(), "real", Layout::V3, &rows(1));
    let escape = a.path().join("link");
    std::os::unix::fs::symlink(&real, &escape).unwrap();
    assert_eq!(
        open(&escape, &OpenPolicy::default()).unwrap_err(),
        Refusal::SymlinkEscape
    );
    let local = b.path().join("local");
    std::os::unix::fs::symlink(&real, &local).unwrap();
    open_ok(&local);
}

#[cfg(unix)]
#[test]
fn a_pipe_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo");
    let ok = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        assert_eq!(
            open(&fifo, &OpenPolicy::default()).unwrap_err(),
            Refusal::NotAFile
        );
    }
    assert_eq!(
        open(Path::new("/dev/null"), &OpenPolicy::default()).unwrap_err(),
        Refusal::NotAFile
    );
}

#[test]
fn unc_paths_are_recognised() {
    assert!(!is_unc(Path::new("/srv/magnetico.sqlite3")));
    #[cfg(windows)]
    {
        assert!(is_unc(Path::new(r"\\server\share\db.sqlite3")));
        assert!(is_unc(Path::new(r"\\?\UNC\server\share\db.sqlite3")));
        assert!(!is_unc(Path::new(r"\\?\C:\db.sqlite3")));
    }
}

// ---- Through IMP-1 into a store ----

fn reading_job(store: &mut Store, id: &Identity) -> ImportJob {
    let job = store
        .create_import_job(&NewImportJob {
            adapter: ADAPTER,
            adapter_ver: ADAPTER_VERSION,
            dataset: &id.dataset,
            schema_sig: &id.schema_sig,
            options: b"",
            credential: None,
            at: NOW,
        })
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, NOW)
        .unwrap();
    store
        .set_import_phase(job.id, job.revision, Phase::Reading, NOW)
        .unwrap()
}

/// Commits one source batch as one IMP-1 batch, the way IMP-4 will (with
/// the filter engine's decisions in place of `PASS`).
fn commit(store: &mut Store, job: i64, seq: u64, b: &Batch) -> BatchCommitted {
    let source = store.register_source(ADAPTER, "1", NOW).unwrap();
    let mut preparer = store.preparer();
    let prepared: Vec<Option<Prepared>> = b
        .records
        .iter()
        .map(|r| match r {
            Record::Torrent(t) => preparer.prepare(&t.info(), None).ok(),
            _ => None,
        })
        .collect();
    let records: Vec<ImportRecord<'_>> = b
        .records
        .iter()
        .zip(&prepared)
        .map(|(r, p)| match (r, p) {
            (Record::Torrent(t), Some(p)) => ImportRecord::Torrent(t.import(p, PASS)),
            (Record::Candidate(c), _) => ImportRecord::Candidate(c.import(PASS)),
            _ => ImportRecord::Invalid,
        })
        .collect();
    store
        .commit_import_batch(&lodestar_store::import::ImportBatch {
            job,
            seq,
            position: &b.position,
            source,
            source_bytes: b.source_bytes,
            at: NOW,
            class_gen: None,
            records: &records,
        })
        .unwrap()
}

#[test]
fn an_import_round_trips_into_a_store_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let mut rs = rows(1_100);
    rs[3].name = Value::Blob(vec![b'n', 0xff]);
    rs[4].total_size = Value::Integer(-1);
    rs[5].info_hash = Value::Blob(vec![1; 19]);
    rs[6].files.clear();
    let path = fixture(dir.path(), "v3", Layout::V3NoSeeders, &rs);
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let mut src = open_ok(&path);
    let job = reading_job(&mut store, &src.identity());

    // First batch, then a "crash": a fresh open resumes from the cursor.
    let b1 = src.read_batch(&[], NOW).unwrap().unwrap();
    commit(&mut store, job.id, 1, &b1);
    drop(src);
    let mut src = open_ok(&path);
    let mut pos = b1.position.clone();
    let mut seq = 1;
    // A replay of the committed batch writes nothing.
    assert!(matches!(
        commit(&mut store, job.id, 1, &b1),
        BatchCommitted::Replayed { .. }
    ));
    while let Some(b) = src.read_batch(&pos, NOW).unwrap() {
        seq += 1;
        commit(&mut store, job.id, seq, &b);
        pos = b.position;
    }
    let c = store.import_job(job.id).unwrap().unwrap().counters;
    assert!(c.conserved());
    assert_eq!(c.read, 1_100);
    assert_eq!(c.invalid, 1);
    assert_eq!(c.candidates, 1);
    assert_eq!(c.new, 1_098);
    assert_eq!(c.partial, 1);

    // Every imported row is unverified and of unknown privacy.
    let ids: Vec<i64> = (1..=10).collect();
    for id in ids {
        if let Some(q) = store.import_quality(id).unwrap() {
            assert_eq!(q.verification, Verification::Unverified);
            assert_eq!(q.privacy, Privacy::Unknown);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    /// Random sources, malformed values included, keep IMP-1's counter
    /// conservation through the adapter.
    #[test]
    fn random_sources_keep_conservation(
        torrents in prop::collection::vec(
            (
                prop_oneof![
                    Just(None),
                    prop::collection::vec(any::<u8>(), 0..24).prop_map(Some),
                    (0u8..8).prop_map(|b| Some(vec![b; 20])),
                ],
                prop::option::of(prop::collection::vec(any::<u8>(), 0..40)),
                prop::option::of(-5i64..1_000),
                prop::collection::vec(
                    (prop::collection::vec(prop_oneof![Just(b'/'), Just(0u8), any::<u8>()], 0..12), -2i64..50),
                    0..6,
                ),
            ),
            0..60,
        )
    ) {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<TorrentRow> = torrents
            .into_iter()
            .map(|(hash, name, size, files)| TorrentRow {
                info_hash: hash.map_or(Value::Null, Value::Blob),
                name: name.map_or(Value::Null, Value::Blob),
                total_size: size.map_or(Value::Null, Value::Integer),
                discovered_on: Value::Integer(NOW),
                files: files
                    .into_iter()
                    .map(|(p, s)| (Value::Blob(p), Value::Integer(s)))
                    .collect(),
            })
            .collect();
        let n = rows.len() as u64;
        let path = fixture(dir.path(), "p", Layout::Lax, &rows);
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let mut src = open_ok(&path);
        let job = reading_job(&mut store, &src.identity());
        let (mut pos, mut seq) = (Vec::new(), 0);
        while let Some(b) = src.read_batch(&pos, NOW).unwrap() {
            seq += 1;
            commit(&mut store, job.id, seq, &b);
            pos = b.position;
        }
        let c = store.import_job(job.id).unwrap().unwrap().counters;
        prop_assert!(c.conserved());
        prop_assert_eq!(c.read, n);
    }
}
