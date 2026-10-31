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
    Error, Filtered, Held, NewTorrent, Prepared, PreparedTorrent, SourceId, Store, StoreConfig,
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

/// A torrent's quality as `(completeness, verification, privacy, repair)`.
fn quality(store: &Store, id: i64) -> Option<(Completeness, Verification, Privacy, i64)> {
    let q = store.import_quality(id).unwrap()?;
    Some((q.completeness, q.verification, q.privacy, q.repair))
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
        quality(&store, ia),
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
        matches!(
            store.commit_import_batch(&batch(j.id, 3, src, &records)),
            Err(Error::Invalid(_))
        ),
        "a gap is refused, and isn't 'not running'"
    );
    assert!(
        store
            .commit_import_batch(&batch(j.id, 0, src, &records))
            .is_err(),
        "batches are numbered from 1"
    );
    let drifted = ImportBatch {
        position: b"elsewhere",
        ..batch(j.id, 1, src, &records)
    };
    assert!(
        store.commit_import_batch(&drifted).is_err(),
        "the last batch replayed from another position has drifted"
    );
    assert_eq!(job(&store, j.id), before);
}

#[test]
fn block_and_fail_keep_committed_work() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    store
        .commit_import_batch(&batch(j.id, 1, src, &[torrent(&a, Privacy::Public)]))
        .unwrap();
    let blocked = store
        .block_import_job(j.id, j.revision, Reason::SourceUnavailable, T)
        .unwrap();
    assert_eq!(
        (blocked.control, blocked.reason),
        (Control::Blocked, Some(Reason::SourceUnavailable))
    );
    assert!(
        store
            .block_import_job(j.id, blocked.revision, Reason::NoSpace, T)
            .is_err(),
        "only a running job is blocked"
    );
    assert!(matches!(
        store.commit_import_batch(&batch(j.id, 2, src, &[])),
        Err(Error::ImportNotRunning)
    ));
    let failed = store
        .fail_import_job(j.id, blocked.revision, Reason::StoreError, T)
        .unwrap();
    assert_eq!(
        (failed.control, failed.reason),
        (Control::Failed, Some(Reason::StoreError))
    );
    assert!(failed.is_finished() && failed.finished_at.is_some());
    assert!(
        store.cancel_import_job(j.id, failed.revision, T).is_err(),
        "failed is final"
    );
    assert_eq!((store.count().unwrap(), failed.counters.new), (1, 1));
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
    assert!(matches!(
        store.commit_import_batch(&batch(j.id, 1, src, &records)),
        Err(Error::ImportNotRunning)
    ));
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
    let debug = format!("{:?}", new_job(b"ds-1"));
    assert!(!debug.contains("batch"), "options aren't printed: {debug}");
    let debug = format!(
        "{:?}",
        ImportBatch {
            position: b"secret-position",
            ..batch(1, 1, DHT, &[])
        }
    );
    assert!(
        !debug.contains("secret"),
        "the position isn't printed: {debug}"
    );
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

    // Without issues it's plain complete, and phases may skip forward.
    let k = store.create_import_job(&new_job(b"ds-2")).unwrap();
    let k = store
        .set_import_phase(k.id, k.revision, Phase::Indexing, T)
        .unwrap();
    let k = store
        .set_import_phase(k.id, k.revision, Phase::Complete, T)
        .unwrap();
    assert_eq!(k.phase, Phase::Complete);
    assert!(
        store
            .set_import_phase(k.id, k.revision, Phase::CompleteWithIssues, T)
            .is_err()
    );
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
    assert_eq!(
        quality(&store, id).map(|q| q.1),
        Some(Verification::Verified),
        "still verified"
    );
    assert_eq!(count(&store, "SELECT count(*) FROM import_quality"), 0);
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
        assert_eq!(quality(&store, id).unwrap().2, Privacy::Private);
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
    assert_eq!(quality(&store, ia).unwrap().2, Privacy::Private);
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
        quality(&store, id),
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

/// Reported as abuse, or added by a floor list file: the hash goes into
/// the floor's list without a tombstone, and its candidate goes too.
#[test]
fn a_floored_candidate_is_deleted_and_journalled() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    store
        .commit_import_batch(&batch(
            j.id,
            1,
            src,
            &[candidate([4; 20], Some(b"Four"), None)],
        ))
        .unwrap();
    store.add_cs_hashes(&[[4; 20]], 1, T).unwrap();
    assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 0);
    assert_eq!(count(&store, "SELECT count(*) FROM snapshot_deletions"), 1);
}

/// A candidate with a v2 hash goes when its truncated v2 hash is held.
#[test]
fn a_candidate_is_held_by_its_truncated_v2_hash() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let v2 = lodestar_metainfo::InfoHashV2([6; 32]);
    let records: Vec<ImportRecord<'_>> = [[5u8; 20], [8; 20]]
        .into_iter()
        .map(|h| {
            ImportRecord::Candidate(ImportCandidate {
                hash_v2: Some(lodestar_metainfo::InfoHashV2([h[0] + 1; 32])),
                ..match candidate(h, Some(b"Hybrid"), None) {
                    ImportRecord::Candidate(c) => c,
                    _ => unreachable!(),
                }
            })
        })
        .collect();
    store
        .commit_import_batch(&batch(j.id, 1, src, &records))
        .unwrap();
    hold(&mut store, v2.truncated().0, Held::Tombstone);
    store.add_cs_hashes(&[[9; 20]], 1, T).unwrap();
    assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 0);
    let journal: Vec<Vec<u8>> = store
        .connection()
        .prepare("SELECT hash FROM snapshot_deletions ORDER BY hash")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        journal,
        [vec![5; 20], vec![8; 20]],
        "journalled by primary hash"
    );
    // Held by the truncated v2 hash, a new record is refused too.
    let again = ImportRecord::Candidate(ImportCandidate {
        hash_v2: Some(v2),
        ..match candidate([5; 20], Some(b"Hybrid"), None) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    assert_eq!(
        outcomes(
            store
                .commit_import_batch(&batch(j.id, 2, src, &[again]))
                .unwrap()
        ),
        [RecordOutcome::Filtered]
    );
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

/// The legacy importer (`Store::write` with `IMPORTED`, `info_size` 0)
/// still writes after V13: its rows read as unknown, a private import
/// claim sticks, and a verified fetch writes the row out as verified, so
/// a later private claim can't flag it.
#[test]
fn legacy_imports_read_as_unknown_until_verified() {
    let mut store = mem();
    let src = source(&mut store);
    let mut legacy = info("Alpha");
    legacy.info_size = 0;
    let id = store
        .insert(&[NewTorrent {
            info: &legacy,
            raw_info: None,
            discovered_at: T,
            metadata_at: T,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            source: src,
        }])
        .unwrap()[0]
        .id();
    assert_eq!(
        quality(&store, id),
        Some((
            Completeness::Partial,
            Verification::Unknown,
            Privacy::Unknown,
            0
        ))
    );
    assert_eq!(count(&store, "SELECT count(*) FROM import_quality"), 0);

    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    store
        .commit_import_batch(&batch(j.id, 1, src, &[torrent(&a, Privacy::Private)]))
        .unwrap();
    assert_eq!(quality(&store, id).unwrap().2, Privacy::Private);
    assert_ne!(
        store.torrent(id).unwrap().unwrap().flags & flags::PRIVATE,
        0
    );

    // A verified fetch: written out as verified, private kept.
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
        quality(&store, id),
        Some((
            Completeness::ReportedComplete,
            Verification::Verified,
            Privacy::Private,
            0
        ))
    );

    // A public legacy import upgraded the same way can't be flagged
    // private by a later import.
    let mut beta = info("Beta");
    beta.info_size = 0;
    let ib = store
        .insert(&[NewTorrent {
            info: &beta,
            raw_info: None,
            discovered_at: T,
            metadata_at: T,
            extra_flags: flags::IMPORTED | flags::FILES_PARTIAL,
            source: src,
        }])
        .unwrap()[0]
        .id();
    let fetched = prepared(&store, "Beta");
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
    store
        .commit_import_batch(&batch(j.id, 2, src, &[torrent(&fetched, Privacy::Private)]))
        .unwrap();
    assert_eq!(
        quality(&store, ib).map(|q| (q.1, q.2)),
        Some((Verification::Verified, Privacy::Public))
    );
    assert_eq!(
        store.torrent(ib).unwrap().unwrap().flags & flags::PRIVATE,
        0
    );
}

#[test]
fn a_batch_must_come_from_the_jobs_adapter() {
    let mut store = mem();
    source(&mut store);
    let other = store.register_source("magnetico", "1", T).unwrap();
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    assert!(matches!(
        store.commit_import_batch(&batch(j.id, 1, other, &[torrent(&a, Privacy::Public)])),
        Err(Error::Invalid(_))
    ));
    assert_eq!(store.count().unwrap(), 0);
    assert_eq!(job(&store, j.id).seq, 0);
}

#[test]
fn the_schema_refuses_a_secret_as_a_credential() {
    let store = mem();
    store
        .connection()
        .execute_batch(
            "INSERT INTO import_sources (id, adapter, dataset, schema_sig)
                 VALUES (1, 'bitmagnet', x'01', x'02');",
        )
        .unwrap();
    for credential in ["postgres://u:pw@db/bm", "Has Spaces", ""] {
        assert!(
            store
                .connection()
                .execute(
                    "INSERT INTO import_jobs (source, adapter_ver, contract_ver, phase, control,
                         options, credential, created_at, updated_at)
                     VALUES (1, 1, 1, 0, 0, x'', ?1, 0, 0)",
                    [credential],
                )
                .is_err(),
            "{credential}"
        );
    }
}

/// A candidate stored as a torrent later doesn't count as an issue, and
/// a hybrid candidate and a pure v2 one under the same truncated v2 hash
/// are one candidate.
#[test]
fn promoted_candidates_are_no_issue_and_v2_candidates_are_one() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let a = prepared(&store, "Alpha");
    let v2 = lodestar_metainfo::InfoHashV2([6; 32]);
    let hybrid = ImportRecord::Candidate(ImportCandidate {
        hash_v2: Some(v2),
        ..match candidate([5; 20], Some(b"Hybrid"), None) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    let pure_v2 = ImportRecord::Candidate(ImportCandidate {
        hash_v2: Some(v2),
        ..match candidate(v2.truncated().0, None, Some(9)) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    store
        .commit_import_batch(&batch(
            j.id,
            1,
            src,
            &[
                candidate(a.info_hash.0, Some(b"Alpha"), None),
                hybrid,
                pure_v2,
            ],
        ))
        .unwrap();
    let filled: (Vec<u8>, Vec<u8>, i64) = store
        .connection()
        .query_row(
            "SELECT hash, name, size FROM import_candidates WHERE hash_v2 IS NOT NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        filled,
        (vec![5; 20], b"Hybrid".to_vec(), 9),
        "one candidate, filled"
    );
    assert_eq!(job(&store, j.id).open_candidates, 2);
    // The v2 candidate is held; Alpha's is stored as a torrent.
    hold(&mut store, v2.truncated().0, Held::Tombstone);
    store
        .commit_import_batch(&batch(j.id, 2, src, &[torrent(&a, Privacy::Public)]))
        .unwrap();
    let j = job(&store, j.id);
    assert_eq!((j.open_candidates, j.counters.candidates), (0, 3));
    let j = store
        .set_import_phase(j.id, j.revision, Phase::Indexing, T)
        .unwrap();
    let j = store
        .set_import_phase(j.id, j.revision, Phase::Complete, T)
        .unwrap();
    assert_eq!(j.phase, Phase::Complete, "no candidate is left waiting");
}

fn v2_of(n: u8) -> lodestar_metainfo::InfoHashV2 {
    lodestar_metainfo::InfoHashV2([n + 1; 32])
}

fn v2_candidate(
    hash: [u8; 20],
    hash_v2: Option<lodestar_metainfo::InfoHashV2>,
) -> ImportRecord<'static> {
    ImportRecord::Candidate(ImportCandidate {
        hash: InfoHash(hash),
        hash_v2,
        name: None,
        size: Some(3),
        privacy: Privacy::Unknown,
        source_at: None,
        decision: PASS,
    })
}

/// A hybrid's v1 and v2 views stored as separate candidates, then a
/// record carrying both: they fold into the oldest, and the batch
/// commits (it used to fail on the v2 index every time, wedging the job).
#[test]
fn a_record_spanning_two_candidates_folds_them() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let v = lodestar_metainfo::InfoHashV2([7; 32]);
    let v1_only = ImportRecord::Candidate(ImportCandidate {
        privacy: Privacy::Private,
        ..match candidate([1; 20], Some(b"Hybrid"), None) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    let v2_only = ImportRecord::Candidate(ImportCandidate {
        hash_v2: Some(v),
        ..match candidate(v.truncated().0, Some(b"Other name"), Some(9)) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    let both = ImportRecord::Candidate(ImportCandidate {
        hash_v2: Some(v),
        ..match candidate([1; 20], None, None) {
            ImportRecord::Candidate(c) => c,
            _ => unreachable!(),
        }
    });
    for (seq, rec) in [(1, v1_only), (2, v2_only), (3, both)] {
        store
            .commit_import_batch(&batch(j.id, seq, src, &[rec]))
            .unwrap();
    }
    type Row = (Vec<u8>, Vec<u8>, Vec<u8>, i64, i64);
    let rows: Vec<Row> = store
        .connection()
        .prepare("SELECT hash, hash_v2, name, size, privacy FROM import_candidates")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [(
            vec![1; 20],
            vec![7; 32],
            b"Hybrid".to_vec(),
            9,
            Privacy::Private.code()
        )],
        "one row, the oldest's values first, private sticky"
    );
    let j = job(&store, j.id);
    assert_eq!((j.counters.candidates, j.open_candidates), (3, 1));
    assert!(j.counters.conserved());
}

/// A fold keeps the hybrid's v1 hash as the key, in either order, so a
/// floor or abuse hit on the v1 hash, or a tombstone on the truncated v2
/// hash, removes the candidate and journals it.
#[test]
fn a_folded_hybrid_is_keyed_by_its_v1_hash() {
    let v = lodestar_metainfo::InfoHashV2([7; 32]);
    let h1 = [1u8; 20];
    let orders: [&[ImportRecord<'static>]; 3] = [
        &[
            v2_candidate(v.truncated().0, Some(v)),
            v2_candidate(h1, Some(v)),
        ],
        &[v2_candidate(h1, None), v2_candidate(h1, Some(v))],
        &[
            v2_candidate(v.truncated().0, Some(v)),
            v2_candidate(h1, None),
            v2_candidate(h1, Some(v)),
        ],
    ];
    for (i, order) in orders.iter().enumerate() {
        for floor_v1 in [true, false] {
            let mut store = mem();
            let src = source(&mut store);
            let j = reading(&mut store, b"ds-1");
            for (seq, rec) in order.iter().enumerate() {
                store
                    .commit_import_batch(&batch(
                        j.id,
                        seq as u64 + 1,
                        src,
                        std::slice::from_ref(rec),
                    ))
                    .unwrap();
            }
            let key: (Vec<u8>, Vec<u8>) = store
                .connection()
                .query_row("SELECT hash, hash_v2 FROM import_candidates", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!(key, (h1.to_vec(), v.0.to_vec()), "order {i}");
            if floor_v1 {
                store.add_cs_hashes(&[h1], 1, T).unwrap();
            } else {
                hold(&mut store, v.truncated().0, Held::Tombstone);
            }
            assert_eq!(
                count(&store, "SELECT count(*) FROM import_candidates"),
                0,
                "order {i}, floor_v1 {floor_v1}"
            );
            assert_eq!(
                count(
                    &store,
                    "SELECT count(*) FROM snapshot_deletions WHERE hash = x'0101010101010101010101010101010101010101'"
                ),
                1,
                "journalled under the v1 key: order {i}, floor_v1 {floor_v1}"
            );
        }
    }
}

/// Records that disagree on a torrent's identity (two v2 hashes, or two
/// v1 hashes for one v2) are refused as invalid, and nothing is folded or
/// dropped.
#[test]
fn a_conflicting_identity_is_refused() {
    let mut store = mem();
    let src = source(&mut store);
    let j = reading(&mut store, b"ds-1");
    let v = lodestar_metainfo::InfoHashV2([7; 32]);
    let other = lodestar_metainfo::InfoHashV2([8; 32]);
    store
        .commit_import_batch(&batch(j.id, 1, src, &[v2_candidate([1; 20], Some(v))]))
        .unwrap();
    let before: Vec<(Vec<u8>, Vec<u8>)> = store
        .connection()
        .prepare("SELECT hash, hash_v2 FROM import_candidates")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let out = outcomes(
        store
            .commit_import_batch(&batch(
                j.id,
                2,
                src,
                &[
                    v2_candidate([1; 20], Some(other)),
                    v2_candidate([2; 20], Some(v)),
                ],
            ))
            .unwrap(),
    );
    assert_eq!(out, [RecordOutcome::Invalid, RecordOutcome::Invalid]);
    let after: Vec<(Vec<u8>, Vec<u8>)> = store
        .connection()
        .prepare("SELECT hash, hash_v2 FROM import_candidates")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after, before);
    let c = job(&store, j.id).counters;
    assert_eq!((c.invalid, c.candidates), (2, 1));
    assert!(c.conserved());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Random batches over a small set of hashes, each submitted once or
    /// more (replays), some refused while paused, with some hashes held
    /// and candidates under torrents' own hashes: counters stay
    /// conserved, a replay never moves them, held hashes are never
    /// stored, and what's stored matches what was committed.
    #[test]
    fn counters_are_conserved_under_replay(
        batches in proptest::collection::vec(
            (proptest::collection::vec((0u8..12, 0u8..8), 0..8), 1usize..3, any::<bool>()),
            1..10,
        ),
    ) {
        let mut store = mem();
        let src = source(&mut store);
        let j = reading(&mut store, b"ds-1");
        let names: Vec<Prepared> = (0..12).map(|i| prepared(&store, &format!("Item {i}"))).collect();
        // Items 10 and 11 are held, and candidate 111 is on the floor.
        hold(&mut store, names[10].info_hash.0, Held::Tombstone);
        hold(&mut store, names[11].info_hash.0, Held::Quarantine);
        store.add_cs_hashes(&[[111; 20]], 1, T).unwrap();
        let mut seq = 0;
        let mut revision = j.revision;
        for (records, sends, pause_first) in &batches {
            let recs: Vec<ImportRecord<'_>> = records
                .iter()
                .map(|&(n, kind)| match kind {
                    0 | 1 => torrent(&names[n as usize], Privacy::Unknown),
                    2 => candidate([100 + n; 20], Some(b"c"), None),
                    // A candidate under a torrent's hash: one that's
                    // stored merges, one that isn't waits until it is.
                    3 => candidate(names[n as usize].info_hash.0, None, Some(5)),
                    // A hybrid's views over shared hashes: v1 alone, v2
                    // alone, and both, in any order.
                    4 => v2_candidate([200 + n; 20], None),
                    5 => v2_candidate(v2_of(n).truncated().0, Some(v2_of(n))),
                    6 => v2_candidate([200 + n; 20], Some(v2_of(n))),
                    _ => ImportRecord::Invalid,
                })
                .collect();
            if *pause_first {
                let p = store.pause_import_job(j.id, revision, T).unwrap();
                prop_assert!(matches!(
                    store.commit_import_batch(&batch(j.id, seq + 1, src, &recs)),
                    Err(Error::ImportNotRunning)
                ));
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
        for held in [&names[10], &names[11]] {
            prop_assert_eq!(store.find(&held.info_hash).unwrap(), None);
        }
        prop_assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM import_candidates c JOIN torrents t ON t.info_hash = c.hash"
            ),
            0,
            "a stored torrent has no candidate"
        );
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

/// IMP-4: removing a job's records deletes what only it inserted, keeps
/// what another job or a network fetch vouches for, and takes back its
/// provenance and candidates, in bounded steps.
#[test]
fn removing_a_job_takes_back_only_what_it_alone_added() {
    let mut store = mem();
    let src = source(&mut store);
    // Stored before the import: the job only merges into it.
    let crawled = prepared(&store, "Crawled");
    store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &crawled,
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source: DHT,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    let names = ["Crawled", "Alone", "Shared", "Fetched", "Other"];
    let p: Vec<Prepared> = names.iter().map(|n| prepared(&store, n)).collect();
    let a = reading(&mut store, b"ds-a");
    let records = [
        torrent(&p[0], Privacy::Private),
        torrent(&p[1], Privacy::Unknown),
        torrent(&p[2], Privacy::Unknown),
        // Partial, so a verified fetch upgrades it.
        ImportRecord::Torrent(ImportTorrent {
            completeness: Completeness::Partial,
            repair: repair::MISSING_FILES,
            ..match torrent(&p[3], Privacy::Unknown) {
                ImportRecord::Torrent(t) => t,
                _ => unreachable!(),
            }
        }),
        candidate([7; 20], Some(b"Waiting"), None),
    ];
    let out = outcomes(
        store
            .commit_import_batch(&batch(a.id, 1, src, &records))
            .unwrap(),
    );
    let ids: Vec<i64> = out
        .iter()
        .filter_map(|o| match o {
            RecordOutcome::New(id) | RecordOutcome::Merged(id) => Some(*id),
            _ => None,
        })
        .collect();
    // Another job sees "Shared" and inserts "Other".
    let b = reading(&mut store, b"ds-b");
    outcomes(
        store
            .commit_import_batch(&batch(
                b.id,
                1,
                src,
                &[
                    torrent(&p[2], Privacy::Unknown),
                    torrent(&p[4], Privacy::Unknown),
                ],
            ))
            .unwrap(),
    );
    // The crawler fetches "Fetched" and verifies it.
    store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &prepared(&store, "Fetched"),
                discovered_at: T,
                metadata_at: T,
                extra_flags: 0,
                source: DHT,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    assert_eq!(store.import_job_members(a.id, 0, 10).unwrap().len(), 4);

    let err = store.remove_import_job_records(a.id, 2).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    let a = store
        .cancel_import_job(a.id, job(&store, a.id).revision, T)
        .unwrap();
    let reindexed = count(&store, "SELECT count(*) FROM reindex_log");
    let mut total = lodestar_store::import::Removed::default();
    let mut calls = 0;
    while !total.done {
        total.add(store.remove_import_job_records(a.id, 2).unwrap());
        calls += 1;
        assert!(calls < 10, "removal must finish");
    }
    assert!(calls >= 3, "bounded steps: {calls}");
    assert_eq!(
        (
            total.torrents,
            total.kept_shared,
            total.kept_verified,
            total.provenance,
            total.candidates
        ),
        (1, 1, 1, 1, 1)
    );
    assert!(store.torrent(ids[1]).unwrap().is_none(), "Alone is gone");
    for kept in [ids[0], ids[2], ids[3]] {
        assert!(store.torrent(kept).unwrap().is_some());
    }
    assert_eq!(
        count(&store, "SELECT count(*) FROM reindex_log") - reindexed,
        1,
        "the indexer drops the deleted row"
    );
    assert!(store.import_job_members(a.id, 0, 10).unwrap().is_empty());
    assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 0);
    assert_eq!(store.import_job_members(b.id, 0, 10).unwrap().len(), 2);
    // A verified row's privacy never took the import's.
    assert_eq!(quality(&store, ids[0]).map(|q| q.2), Some(Privacy::Public));
    assert_eq!(job(&store, a.id).counters, a.counters, "history stays");
    // Removing again finds nothing.
    let again = store.remove_import_job_records(a.id, 2).unwrap();
    assert_eq!(
        again,
        lodestar_store::import::Removed {
            done: true,
            ..Default::default()
        }
    );
}

/// IMP-4 review: overlapping pilots remove cleanly in either order, a
/// paused job is refused (resuming it would skip what was removed), and a
/// row the crawler or you have touched since stays.
#[test]
fn overlapping_pilots_remove_in_any_order_and_touched_rows_stay() {
    for a_first in [true, false] {
        let mut store = mem();
        // The crawler's source first, so the adapter's bit is another.
        assert_eq!(store.register_source("dht", "1", T).unwrap(), DHT);
        let src = source(&mut store);
        let p: Vec<Prepared> = ["Both", "Seen", "Yours", "Private"]
            .iter()
            .map(|n| prepared(&store, n))
            .collect();
        let a = reading(&mut store, b"ds-a");
        let out = outcomes(
            store
                .commit_import_batch(&batch(
                    a.id,
                    1,
                    src,
                    &[
                        torrent(&p[0], Privacy::Unknown),
                        torrent(&p[1], Privacy::Unknown),
                        torrent(&p[2], Privacy::Unknown),
                        ImportRecord::Candidate(ImportCandidate {
                            privacy: Privacy::Private,
                            ..match candidate([7; 20], None, Some(5)) {
                                ImportRecord::Candidate(c) => c,
                                _ => unreachable!(),
                            }
                        }),
                    ],
                ))
                .unwrap(),
        );
        let RecordOutcome::New(both) = out[0] else {
            panic!("{out:?}")
        };
        let (seen, yours) = match (out[1], out[2]) {
            (RecordOutcome::New(s), RecordOutcome::New(y)) => (s, y),
            o => panic!("{o:?}"),
        };
        // The 100k pilot over the same source merges what the 10k one
        // inserted.
        let b = reading(&mut store, b"ds-b");
        store
            .commit_import_batch(&batch(b.id, 1, src, &[torrent(&p[0], Privacy::Unknown)]))
            .unwrap();
        // The crawler reports one; you label another.
        store
            .write(&WriteBatch {
                touches: &[lodestar_store::Touch {
                    hash: p[1].info_hash,
                    sources: 1 << DHT.get(),
                    at: T,
                    public: true,
                }],
                ..WriteBatch::default()
            })
            .unwrap();
        store
            .set_user_item(
                yours,
                &lodestar_store::UserItemUpdate {
                    favourite: Some(true),
                    hidden: None,
                    labels: None,
                    note: None,
                },
                T,
            )
            .unwrap();

        let paused = store
            .pause_import_job(a.id, job(&store, a.id).revision, T)
            .unwrap();
        assert!(matches!(
            store.remove_import_job_records(paused.id, 10),
            Err(Error::Invalid(_))
        ));
        for j in [a.id, b.id] {
            store
                .cancel_import_job(j, job(&store, j).revision, T)
                .unwrap();
        }
        let order = if a_first { [a.id, b.id] } else { [b.id, a.id] };
        let mut first = store.remove_import_job_records(order[0], 0).unwrap();
        while !first.done {
            first.add(store.remove_import_job_records(order[0], 0).unwrap());
        }
        let second = store.remove_import_job_records(order[1], 10).unwrap();
        assert!(second.done);
        assert!(
            store.torrent(both).unwrap().is_none(),
            "a_first={a_first}: the shared row goes with its last job"
        );
        assert!(store.torrent(seen).unwrap().is_some(), "the crawler saw it");
        assert!(store.torrent(yours).unwrap().is_some(), "it's yours");
        let removed_a = if a_first { first } else { second };
        assert_eq!((removed_a.kept_seen, removed_a.kept_yours), (1, 1));
        assert_eq!(removed_a.kept_private_candidates, 1);
        assert_eq!(count(&store, "SELECT count(*) FROM import_candidates"), 1);
        assert_eq!(count(&store, "SELECT count(*) FROM import_membership"), 0);
    }
}
