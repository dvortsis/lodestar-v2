//! Imports (IMP-1): durable jobs and the batch commit, through the public
//! API. The crash-at-every-step tests are unit tests (`src/import.rs`),
//! where the fail points are.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::{Info, InfoHash};
use lodestar_store::import::{
    BatchCommitted, Completeness, Control, Decision, ImportBatch, ImportCandidate, ImportJob,
    ImportRecord, ImportTorrent, NewImportJob, Phase, Privacy, Reason, RecordOutcome, Revalidation,
    Verification, repair,
};
use lodestar_store::{
    Filtered, Held, NewTorrent, Prepared, PreparedTorrent, SourceId, Store, StoreConfig,
    WriteBatch, flags,
};
use proptest::prelude::*;

const DHT: SourceId = SourceId::new(0).unwrap();
const T: i64 = 1_700_000_000;
const PASS: Decision<'static> = Decision {
    hits: &[],
    marks: 0,
};

fn info_bytes(name: &str, private: bool) -> Vec<u8> {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let mut e: Vec<(Vec<u8>, OwnedValue)> = vec![
        (b"length".to_vec(), OwnedValue::Int(5)),
        (b"name".to_vec(), b(name.as_bytes())),
        (b"piece length".to_vec(), OwnedValue::Int(16_384)),
        (b"pieces".to_vec(), b(&[0; 20])),
    ];
    if private {
        e.push((b"private".to_vec(), OwnedValue::Int(1)));
    }
    to_vec(&OwnedValue::Dict(e.into_iter().collect()))
}

fn info(name: &str) -> Info {
    Info::parse(&info_bytes(name, false)).unwrap()
}

fn prepared(store: &Store, name: &str) -> Prepared {
    store.preparer().prepare(&info(name), None).unwrap()
}

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

fn source(store: &mut Store) -> SourceId {
    store.register_source("bitmagnet", "1", T).unwrap()
}

fn new_job<'a>(dataset: &'a [u8]) -> NewImportJob<'a> {
    NewImportJob {
        adapter: "bitmagnet",
        adapter_ver: 1,
        dataset,
        schema_sig: b"sig-1",
        options: b"{\"batch\":500}",
        credential: Some("bitmagnet-pg"),
        at: T,
    }
}

/// A job in `reading`.
fn reading(store: &mut Store, dataset: &[u8]) -> ImportJob {
    let job = store.create_import_job(&new_job(dataset)).unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, T)
        .unwrap();
    store
        .set_import_phase(job.id, job.revision, Phase::Reading, T)
        .unwrap()
}

fn torrent<'a>(p: &'a Prepared, privacy: Privacy) -> ImportRecord<'a> {
    ImportRecord::Torrent(ImportTorrent {
        torrent: p,
        completeness: Completeness::ReportedComplete,
        verification: Verification::Unverified,
        privacy,
        repair: 0,
        source_at: Some(T - 100),
        files_dropped: 0,
        decision: PASS,
    })
}

fn candidate(hash: [u8; 20], name: Option<&[u8]>, size: Option<u64>) -> ImportRecord<'_> {
    ImportRecord::Candidate(ImportCandidate {
        hash: InfoHash(hash),
        hash_v2: None,
        name,
        size,
        privacy: Privacy::Unknown,
        source_at: None,
        decision: PASS,
    })
}

fn batch<'a>(
    job: i64,
    seq: u64,
    source: SourceId,
    records: &'a [ImportRecord<'a>],
) -> ImportBatch<'a> {
    ImportBatch {
        job,
        seq,
        position: b"pos",
        source,
        source_bytes: 100,
        at: T,
        class_gen: None,
        records,
    }
}

fn outcomes(r: BatchCommitted) -> Vec<RecordOutcome> {
    match r {
        BatchCommitted::Committed { outcomes, .. } => outcomes,
        r => panic!("not committed: {r:?}"),
    }
}

fn job(store: &Store, id: i64) -> ImportJob {
    store.import_job(id).unwrap().unwrap()
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection().query_row(sql, [], |r| r.get(0)).unwrap()
}

fn ok_check(job: &ImportJob) -> Revalidation<'_> {
    Revalidation {
        dataset: &job.dataset,
        schema_sig: &job.schema_sig,
        credential_ok: true,
        space_ok: true,
    }
}

#[test]
fn a_batch_commits_records_counters_and_cursor_together() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let (a, b) = (prepared(&store, "Alpha"), prepared(&store, "Beta"));
    let records = [
        torrent(&a, Privacy::Public),
        torrent(&b, Privacy::Unknown),
        torrent(&a, Privacy::Public),
        candidate([9; 20], Some(b"Gamma"), None),
        ImportRecord::Invalid,
    ];
    let out = outcomes(
        store
            .commit_import_batch(&batch(j.id, 1, src, &records))
            .unwrap(),
    );
    let (RecordOutcome::New(ia), RecordOutcome::New(ib)) = (out[0], out[1]) else {
        panic!("{out:?}");
    };
    assert_eq!(
        out[2..],
        [
            RecordOutcome::Merged(ia),
            RecordOutcome::Candidate,
            RecordOutcome::Invalid
        ]
    );
    let after = job(&store, j.id);
    let c = after.counters;
    assert_eq!(
        (c.read, c.new, c.merged, c.candidates, c.invalid),
        (5, 2, 1, 1, 1)
    );
    assert!(c.conserved());
    assert_eq!(c.source_bytes, 100);
    assert_eq!((after.seq, &after.position[..]), (1, &b"pos"[..]));
    assert_eq!(after.max_new_id, ia.max(ib));
    assert_eq!(
        after.revision, j.revision,
        "batches don't bump the revision"
    );

    let row = store.torrent(ia).unwrap().unwrap();
    assert_ne!(row.flags & flags::IMPORTED, 0);
    assert_eq!(row.flags & flags::FILES_PARTIAL, 0);
    assert_eq!(row.discovered_at, T - 100, "the source's date, not now");
    assert_eq!(
        row.last_seen_at,
        T - 100,
        "an import isn't a fresh sighting"
    );
    assert_eq!(row.sources, 1 << src.get());
    assert_eq!(
        store.import_quality(ia).unwrap(),
        Some((
            Completeness::ReportedComplete,
            Verification::Unverified,
            Privacy::Public,
            0
        ))
    );
    assert_eq!(
        count(&store, "SELECT count(*) FROM import_membership"),
        2,
        "one per torrent and job"
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM import_membership WHERE created = 1"
        ),
        2
    );
}

#[test]
fn a_replayed_batch_writes_nothing_and_counts_nothing() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    let records = [torrent(&a, Privacy::Public), ImportRecord::Invalid];
    store
        .commit_import_batch(&batch(j.id, 1, src, &records))
        .unwrap();
    let before = job(&store, j.id);
    for _ in 0..3 {
        assert_eq!(
            store
                .commit_import_batch(&batch(j.id, 1, src, &records))
                .unwrap(),
            BatchCommitted::Replayed {
                counters: before.counters
            }
        );
    }
    assert_eq!(job(&store, j.id), before);
    assert_eq!(store.count().unwrap(), 1);
    assert!(
        store
            .commit_import_batch(&batch(j.id, 3, src, &records))
            .is_err(),
        "a gap is refused"
    );
    assert_eq!(job(&store, j.id), before);
}

#[test]
fn pause_and_cancel_are_truthful() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    let records = [torrent(&a, Privacy::Public)];

    let paused = store.pause_import_job(j.id, j.revision, T).unwrap();
    assert_eq!(paused.control, Control::Paused);
    assert!(
        store
            .commit_import_batch(&batch(j.id, 1, src, &records))
            .is_err()
    );
    assert_eq!(store.count().unwrap(), 0, "nothing commits once paused");
    assert!(
        store.pause_import_job(j.id, j.revision, T).is_err(),
        "a stale revision is refused"
    );

    let resumed = store
        .resume_import_job(j.id, paused.revision, &ok_check(&paused), T)
        .unwrap();
    assert_eq!((resumed.control, resumed.reason), (Control::Running, None));
    store
        .commit_import_batch(&batch(j.id, 1, src, &records))
        .unwrap();

    let cancelled = store.cancel_import_job(j.id, resumed.revision, T).unwrap();
    assert_eq!(cancelled.control, Control::Cancelled);
    assert!(cancelled.finished_at.is_some());
    assert!(
        store
            .commit_import_batch(&batch(j.id, 2, src, &records))
            .is_err()
    );
    assert!(
        store
            .resume_import_job(j.id, cancelled.revision, &ok_check(&cancelled), T)
            .is_err(),
        "cancelled is final"
    );
    assert_eq!(store.count().unwrap(), 1, "committed work stays");
    // The source is free for a new job once this one is finished.
    store.create_import_job(&new_job(b"ds-1")).unwrap();
}

#[test]
fn one_unfinished_job_per_source_and_credentials_are_references() {
    let mut store = mem();
    store.create_import_job(&new_job(b"ds-1")).unwrap();
    assert!(store.create_import_job(&new_job(b"ds-1")).is_err());
    store.create_import_job(&new_job(b"ds-2")).unwrap();
    for bad in ["postgres://u:pw@host/db", "Secret", "", &"x".repeat(65)] {
        let mut j = new_job(b"ds-3");
        j.credential = Some(bad);
        assert!(store.create_import_job(&j).is_err(), "{bad}");
    }
    let mut j = new_job(b"ds-3");
    j.options = &[0; 4097];
    assert!(store.create_import_job(&j).is_err());
    let debug = format!("{:?}", job(&store, 1));
    assert!(!debug.contains("batch"), "options aren't printed: {debug}");
}

#[test]
fn phases_move_forward_and_complete_after_indexing() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    assert!(
        store
            .set_import_phase(j.id, j.revision, Phase::Ready, T)
            .is_err()
    );
    assert!(
        store
            .set_import_phase(j.id, j.revision, Phase::Complete, T)
            .is_err()
    );
    let j = store
        .set_import_phase(j.id, j.revision, Phase::Committing, T)
        .unwrap();
    let records = [candidate([1; 20], None, Some(5))];
    store
        .commit_import_batch(&batch(j.id, 1, src, &records))
        .unwrap();
    let j = store
        .set_import_phase(j.id, j.revision, Phase::Indexing, T)
        .unwrap();
    assert!(
        store
            .commit_import_batch(&batch(j.id, 2, src, &records))
            .is_err(),
        "an indexing job takes no batches"
    );
    let j = store
        .set_import_phase(j.id, j.revision, Phase::Complete, T)
        .unwrap();
    assert_eq!(j.phase, Phase::CompleteWithIssues, "it has a candidate");
    assert!(j.is_finished());
}

#[test]
fn a_restart_blocks_running_jobs_until_they_revalidate() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let (running, paused, indexing) = {
        let mut store = Store::open(&db, StoreConfig::default()).unwrap();
        let src = source(&mut store);
        let a = prepared(&store, "Alpha");
        let r = reading(&mut store, b"ds-1");
        store
            .commit_import_batch(&batch(r.id, 1, src, &[torrent(&a, Privacy::Public)]))
            .unwrap();
        let p = reading(&mut store, b"ds-2");
        store.pause_import_job(p.id, p.revision, T).unwrap();
        let i = reading(&mut store, b"ds-3");
        store
            .set_import_phase(i.id, i.revision, Phase::Indexing, T)
            .unwrap();
        (r.id, p.id, i.id)
    };
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(store.interrupt_import_jobs(T + 1).unwrap(), 1);
    let r = job(&store, running);
    assert_eq!(
        (r.control, r.reason),
        (Control::Blocked, Some(Reason::Interrupted))
    );
    assert_eq!(job(&store, paused).control, Control::Paused, "stays paused");
    assert_eq!(job(&store, indexing).control, Control::Running);

    let changed = Revalidation {
        dataset: b"ds-1-restored",
        ..ok_check(&r)
    };
    let r = store
        .resume_import_job(running, r.revision, &changed, T)
        .unwrap();
    assert_eq!(r.reason, Some(Reason::SourceChanged));
    let no_secret = Revalidation {
        credential_ok: false,
        ..ok_check(&r)
    };
    let r = store
        .resume_import_job(running, r.revision, &no_secret, T)
        .unwrap();
    assert_eq!(
        (r.control, r.reason),
        (Control::Blocked, Some(Reason::NeedsCredentials))
    );
    let no_space = Revalidation {
        space_ok: false,
        ..ok_check(&r)
    };
    let r = store
        .resume_import_job(running, r.revision, &no_space, T)
        .unwrap();
    assert_eq!(r.reason, Some(Reason::NoSpace));
    let r = store
        .resume_import_job(running, r.revision, &ok_check(&r), T)
        .unwrap();
    assert_eq!(
        (r.control, r.seq, &r.position[..]),
        (Control::Running, 1, &b"pos"[..])
    );
    assert_eq!(r.counters.new, 1, "never restarts from zero");
}

/// Holds a hash the way ingest does: a filter hit with a tombstone or
/// quarantine row.
fn hold(store: &mut Store, hash: [u8; 20], held: Held) {
    let hashes = [hash];
    store
        .write(&WriteBatch {
            filtered: &[Filtered {
                hashes: &hashes,
                rule: "CS-2",
                action: 1,
                held: Some(held),
                sealed: None,
                evidence: &[],
                at: T,
                expires_at: T + 86_400,
                adult_hold: false,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
}

#[test]
fn held_hashes_refuse_imports_whatever_the_decision() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let (a, b, c) = (
        prepared(&store, "Alpha"),
        prepared(&store, "Beta"),
        prepared(&store, "Gamma"),
    );
    hold(&mut store, a.info_hash.0, Held::Tombstone);
    hold(&mut store, b.info_hash.0, Held::Quarantine);
    store.add_cs_hashes(&[c.info_hash.0], 0, T).unwrap();
    let records = [
        torrent(&a, Privacy::Public),
        torrent(&b, Privacy::Public),
        torrent(&c, Privacy::Public),
        candidate(a.info_hash.0, Some(b"Alpha"), Some(5)),
        candidate(b.info_hash.0, None, None),
    ];
    let out = outcomes(
        store
            .commit_import_batch(&batch(j.id, 1, src, &records))
            .unwrap(),
    );
    use RecordOutcome::{Filtered as F, Quarantined as Q};
    assert_eq!(out, [F, Q, F, F, Q]);
    assert_eq!(store.count().unwrap(), 0, "no tombstone resurrected");
    assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 0);
    let c = job(&store, j.id).counters;
    assert_eq!((c.filtered, c.quarantined), (3, 2));
    assert!(c.conserved());
}

#[test]
fn a_held_decision_writes_its_hold_and_keeps_the_record_out() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    let hashes = [a.info_hash.0];
    let hits = [Filtered {
        hashes: &hashes,
        rule: "CS-3",
        action: 1,
        held: Some(Held::Tombstone),
        sealed: None,
        evidence: &[],
        at: T,
        expires_at: T,
        adult_hold: false,
    }];
    let rec = ImportRecord::Torrent(ImportTorrent {
        decision: Decision {
            hits: &hits,
            marks: 0,
        },
        ..match torrent(&a, Privacy::Public) {
            ImportRecord::Torrent(t) => t,
            _ => unreachable!(),
        }
    });
    let out = outcomes(
        store
            .commit_import_batch(&batch(j.id, 1, src, &[rec]))
            .unwrap(),
    );
    assert_eq!(out, [RecordOutcome::Filtered]);
    assert_eq!(store.tombstones(10).unwrap().len(), 1);
    assert_eq!(store.count().unwrap(), 0);
}

#[test]
fn a_verified_row_only_gains_provenance() {
    let mut store = mem();
    let src = source(&mut store);
    let raw = info_bytes("Alpha", false);
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: T - 5,
            metadata_at: T - 5,
            extra_flags: flags::PRIVATE_SOURCE,
            source: DHT,
        }])
        .unwrap()[0]
        .id();
    let before = store.torrent(id).unwrap().unwrap();
    let j = reading(&mut store, b"ds-1");
    let mut p = prepared(&store, "Alpha");
    p.name = "Renamed by the import".into();
    p.size = 999;
    let rec = ImportRecord::Torrent(ImportTorrent {
        completeness: Completeness::Partial,
        decision: Decision {
            hits: &[],
            marks: flags::HIDDEN,
        },
        ..match torrent(&p, Privacy::Private) {
            ImportRecord::Torrent(t) => t,
            _ => unreachable!(),
        }
    });
    let out = outcomes(
        store
            .commit_import_batch(&batch(j.id, 1, src, &[rec]))
            .unwrap(),
    );
    assert_eq!(out, [RecordOutcome::Merged(id)]);
    let after = store.torrent(id).unwrap().unwrap();
    assert_eq!(
        after,
        lodestar_store::TorrentRow {
            sources: before.sources | 1 << src.get(),
            ..before
        },
        "only the source bit changes; PRIVATE_SOURCE stays"
    );
    assert_eq!(store.import_quality(id).unwrap(), None, "still verified");
    assert_eq!(count(&store, "SELECT count(*) FROM reindex_log"), 0);
}

#[test]
fn private_is_sticky() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let (a, b) = (prepared(&store, "Alpha"), prepared(&store, "Beta"));
    // A candidate declared private, then the torrent arrives as public.
    let cand = ImportRecord::Candidate(ImportCandidate {
        privacy: Privacy::Private,
        ..match candidate(b.info_hash.0, Some(b"Beta"), None) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    let out = outcomes(
        store
            .commit_import_batch(&batch(j.id, 1, src, &[torrent(&a, Privacy::Unknown), cand]))
            .unwrap(),
    );
    let RecordOutcome::New(ia) = out[0] else {
        panic!()
    };
    let out = outcomes(
        store
            .commit_import_batch(&batch(
                j.id,
                2,
                src,
                &[torrent(&a, Privacy::Private), torrent(&b, Privacy::Public)],
            ))
            .unwrap(),
    );
    let RecordOutcome::New(ib) = out[1] else {
        panic!("{out:?}")
    };
    assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 0);
    for id in [ia, ib] {
        assert_eq!(
            store.import_quality(id).unwrap().unwrap().2,
            Privacy::Private
        );
        assert_ne!(
            store.torrent(id).unwrap().unwrap().flags & flags::PRIVATE,
            0
        );
    }
    assert_eq!(
        count(&store, "SELECT count(*) FROM reindex_log"),
        1,
        "the merge that made a row private re-indexes it"
    );
    // A public duplicate never clears it.
    outcomes(
        store
            .commit_import_batch(&batch(j.id, 3, src, &[torrent(&a, Privacy::Public)]))
            .unwrap(),
    );
    assert_eq!(
        store.import_quality(ia).unwrap().unwrap().2,
        Privacy::Private
    );
}

#[test]
fn a_verified_fetch_upgrades_a_partial_import_and_keeps_it_private() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    let rec = ImportRecord::Torrent(ImportTorrent {
        completeness: Completeness::Partial,
        repair: repair::MISSING_FILES,
        ..match torrent(&a, Privacy::Private) {
            ImportRecord::Torrent(t) => t,
            _ => unreachable!(),
        }
    });
    let out = outcomes(
        store
            .commit_import_batch(&batch(j.id, 1, src, &[rec]))
            .unwrap(),
    );
    let RecordOutcome::New(id) = out[0] else {
        panic!()
    };
    assert_eq!(job(&store, j.id).counters.partial, 1);
    assert_ne!(
        store.torrent(id).unwrap().unwrap().flags & flags::FILES_PARTIAL,
        0
    );
    let fetched = prepared(&store, "Alpha");
    store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &fetched,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source: DHT,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    assert_eq!(
        store.import_quality(id).unwrap(),
        Some((
            Completeness::ReportedComplete,
            Verification::Verified,
            Privacy::Private,
            0
        ))
    );
    let row = store.torrent(id).unwrap().unwrap();
    assert_eq!(row.flags & flags::FILES_PARTIAL, 0);
    assert_ne!(row.flags & flags::PRIVATE, 0, "the restriction survives");
}

#[test]
fn candidates_never_hold_empty_names_or_zero_sizes() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let records = [
        candidate([1; 20], Some(b""), Some(0)),
        candidate([2; 20], Some(b"Named"), None),
    ];
    store
        .commit_import_batch(&batch(j.id, 1, src, &records))
        .unwrap();
    let rows: Vec<(Option<Vec<u8>>, Option<i64>, i64)> = store
        .connection()
        .prepare("SELECT name, size, missing FROM import_candidates ORDER BY hash")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (None, None, repair::ALL & !repair::INCONSISTENT_FILES),
            (
                Some(b"Named".to_vec()),
                None,
                repair::MISSING_SIZE | repair::MISSING_FILES
            ),
        ]
    );
    // A later record fills what was missing, never blanks it.
    store
        .commit_import_batch(&batch(j.id, 2, src, &[candidate([2; 20], None, Some(7))]))
        .unwrap();
    let filled: (Vec<u8>, i64, i64) = store
        .connection()
        .query_row(
            "SELECT name, size, missing FROM import_candidates WHERE hash = ?1",
            [&[2u8; 20][..]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(filled, (b"Named".to_vec(), 7, repair::MISSING_FILES));
    // The schema refuses an empty name or a zero size outright.
    for sql in [
        "UPDATE import_candidates SET name = x'' WHERE hash = x'0202020202020202020202020202020202020202'",
        "UPDATE import_candidates SET size = 0 WHERE hash = x'0202020202020202020202020202020202020202'",
    ] {
        assert!(store.connection().execute(sql, []).is_err(), "{sql}");
    }
}

#[test]
fn a_held_candidate_is_deleted_and_journalled() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    store
        .commit_import_batch(&batch(
            j.id,
            1,
            src,
            &[
                candidate([1; 20], Some(b"One"), None),
                candidate([2; 20], Some(b"Two"), None),
            ],
        ))
        .unwrap();
    hold(&mut store, [1; 20], Held::Tombstone);
    hold(&mut store, [2; 20], Held::Quarantine);
    assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 0);
    assert_eq!(count(&store, "SELECT count(*) FROM snapshot_deletions"), 2);
}

#[test]
fn a_backup_copy_holds_the_jobs_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let copy = dir.path().join("backup.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    store
        .commit_import_batch(&batch(
            j.id,
            1,
            src,
            &[
                torrent(&a, Privacy::Public),
                candidate([7; 20], Some(b"C"), None),
            ],
        ))
        .unwrap();
    // As `lodestar backup` copies the store.
    store
        .connection()
        .execute("VACUUM INTO ?1", [copy.to_string_lossy()])
        .unwrap();
    drop(store);
    let mut restored = Store::open(&copy, StoreConfig::default()).unwrap();
    let back = job(&restored, j.id);
    assert_eq!(
        (back.seq, back.counters.new, back.counters.candidates),
        (1, 1, 1)
    );
    for table in ["import_membership", "import_candidates", "import_quality"] {
        assert_eq!(
            count(&restored, &format!("SELECT count(*) FROM {table}")),
            1,
            "{table}"
        );
    }
    let b = prepared(&restored, "Beta");
    restored
        .commit_import_batch(&batch(j.id, 2, src, &[torrent(&b, Privacy::Public)]))
        .unwrap();
    assert_eq!(job(&restored, j.id).counters.new, 2);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Random batches over a small set of hashes, each submitted once or
    /// more (replays), some refused while paused: counters stay
    /// conserved, a replay never moves them, and what's stored matches
    /// what was committed.
    #[test]
    fn counters_are_conserved_under_replay(
        batches in proptest::collection::vec(
            (proptest::collection::vec((0u8..12, 0u8..4), 0..8), 1usize..3, any::<bool>()),
            1..10,
        ),
    ) {
        let mut store = mem();
        let src = source(&mut store);
        let j = reading(&mut store, b"ds-1");
        let names: Vec<Prepared> = (0..12).map(|i| prepared(&store, &format!("Item {i}"))).collect();
        let mut seq = 0;
        let mut revision = j.revision;
        for (records, sends, pause_first) in &batches {
            let recs: Vec<ImportRecord<'_>> = records
                .iter()
                .map(|&(n, kind)| match kind {
                    0 | 1 => torrent(&names[n as usize], Privacy::Unknown),
                    2 => candidate([100 + n; 20], Some(b"c"), None),
                    _ => ImportRecord::Invalid,
                })
                .collect();
            if *pause_first {
                let p = store.pause_import_job(j.id, revision, T).unwrap();
                prop_assert!(store.commit_import_batch(&batch(j.id, seq + 1, src, &recs)).is_err());
                let r = store.resume_import_job(j.id, p.revision, &ok_check(&p), T).unwrap();
                revision = r.revision;
            }
            seq += 1;
            let before = job(&store, j.id).counters;
            for send in 0..*sends {
                let r = store.commit_import_batch(&batch(j.id, seq, src, &recs)).unwrap();
                let now = job(&store, j.id).counters;
                prop_assert!(now.conserved(), "{now:?}");
                if send == 0 {
                    prop_assert_eq!(now.read, before.read + recs.len() as u64);
                } else {
                    prop_assert_eq!(r, BatchCommitted::Replayed { counters: now });
                }
            }
        }
        let c = job(&store, j.id).counters;
        prop_assert_eq!(c.new as i64, count(&store, "SELECT count(*) FROM torrents"));
        prop_assert_eq!(
            count(&store, "SELECT count(*) FROM import_membership") as u64,
            c.new,
            "one job: one membership per stored torrent"
        );
        prop_assert!(
            count(&store, "SELECT count(*) FROM import_candidates") as u64 <= c.candidates
        );
        prop_assert_eq!(job(&store, j.id).seq, seq);
    }
}
