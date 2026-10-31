//! Imports and Insights (IMP-5): an import's records stay out of the
//! discovery cohorts and the crawl's series, keep their own count, and
//! leave start, finish and removal annotations with their counts.

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_metainfo::Info;
use lodestar_store::import::{
    BatchCommitted, Completeness, Decision, ImportBatch, ImportRecord, ImportTorrent, NewImportJob,
    Phase, Privacy, Revalidation, Verification, event,
};
use lodestar_store::{
    CohortCount, Counter, Event, Prepared, PreparedTorrent, RollupBy, SearchableCodes, SourceId,
    Store, StoreConfig, WriteBatch,
};

const DHT: SourceId = SourceId::new(0).unwrap();
const H: i64 = 480_000;
const T: i64 = H * 3600;
const CODES: SearchableCodes = SearchableCodes {
    stage: 5,
    newly: 10,
    delay: 11,
};
const PASS: Decision<'static> = Decision {
    hits: &[],
    marks: 0,
};

fn prepared(store: &Store, name: &str) -> Prepared {
    let b = |s: &[u8]| OwnedValue::Bytes(s.to_vec());
    let info = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(5)),
            (b"name".to_vec(), b(name.as_bytes())),
            (b"piece length".to_vec(), OwnedValue::Int(16_384)),
            (b"pieces".to_vec(), b(&[0; 20])),
        ]
        .into_iter()
        .collect(),
    ));
    store
        .preparer()
        .prepare(&Info::parse(&info).unwrap(), None)
        .unwrap()
}

fn crawl(store: &mut Store, names: &[&str]) {
    let p: Vec<Prepared> = names.iter().map(|n| prepared(store, n)).collect();
    let rows: Vec<PreparedTorrent<'_>> = p
        .iter()
        .map(|torrent| PreparedTorrent {
            torrent,
            discovered_at: T + 60,
            metadata_at: T + 120,
            extra_flags: 0,
            source: DHT,
            verified: true,
        })
        .collect();
    let cohorts = [CohortCount {
        cohort: H,
        source: DHT,
        stage: 4,
        reason: 0,
        count: names.len() as i64,
    }];
    let counters = [Counter {
        hour: H,
        metric: 4,
        dim: 0,
        key: 0,
        value: names.len() as i64,
    }];
    store
        .write(&WriteBatch {
            prepared: &rows,
            cohorts: &cohorts,
            counters: &counters,
            ..WriteBatch::default()
        })
        .unwrap();
}

/// A job that read `names` and moved to `indexing`; its id.
fn import(store: &mut Store, dataset: &[u8], names: &[&str]) -> i64 {
    let src = store.register_source("magnetico", "1", T).unwrap();
    let job = store
        .create_import_job(&NewImportJob {
            adapter: "magnetico",
            adapter_ver: 1,
            dataset,
            schema_sig: b"sig",
            options: b"{}",
            credential: None,
            at: T,
        })
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, T)
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Reading, T + 10)
        .unwrap();
    let p: Vec<Prepared> = names.iter().map(|n| prepared(store, n)).collect();
    let records: Vec<ImportRecord<'_>> = p
        .iter()
        .map(|torrent| {
            ImportRecord::Torrent(ImportTorrent {
                torrent,
                completeness: Completeness::ReportedComplete,
                verification: Verification::Unverified,
                privacy: Privacy::Unknown,
                repair: 0,
                source_at: Some(T - 100),
                files_dropped: 0,
                decision: PASS,
            })
        })
        .collect();
    let r = store
        .commit_import_batch(&ImportBatch {
            job: job.id,
            seq: 1,
            position: b"pos",
            source: src,
            source_bytes: 1,
            at: T + 20,
            class_gen: None,
            records: &records,
        })
        .unwrap();
    assert!(matches!(r, BatchCommitted::Committed { .. }), "{r:?}");
    let job = store.import_job(job.id).unwrap().unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Committing, T + 30)
        .unwrap();
    store
        .set_import_phase(job.id, job.revision, Phase::Indexing, T + 40)
        .unwrap();
    job.id
}

fn mem() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

/// Every rollup and cohort row, for comparing before and after.
fn reporting(store: &Store) -> (Vec<Counter>, Vec<CohortCount>) {
    let mut counters = Vec::new();
    for metric in 1..=15 {
        counters.extend(store.rollup(metric, 0, i64::MAX).unwrap());
    }
    (counters, store.cohorts(0, i64::MAX).unwrap())
}

fn newly(store: &Store, key: i64) -> i64 {
    store
        .rollup_sums(CODES.newly, H - 1, H + 2, RollupBy::Key)
        .unwrap()
        .iter()
        .filter(|c| c.key == key)
        .map(|c| c.value)
        .sum()
}

fn searchable_cohort(store: &Store) -> i64 {
    store
        .cohorts(0, i64::MAX)
        .unwrap()
        .iter()
        .filter(|c| c.stage == CODES.stage)
        .map(|c| c.count)
        .sum()
}

fn last_id(store: &Store) -> i64 {
    store
        .connection()
        .query_row("SELECT max(id) FROM torrents", [], |r| r.get(0))
        .unwrap()
}

fn import_events(store: &Store) -> Vec<Event> {
    store
        .events(0, i64::MAX)
        .unwrap()
        .into_iter()
        .filter(|e| [event::STARTED, event::FINISHED, event::REMOVED].contains(&e.kind))
        .collect()
}

#[test]
fn an_import_writes_no_crawl_rollups() {
    let mut store = mem();
    crawl(&mut store, &["a", "b"]);
    let before = reporting(&store);
    import(&mut store, b"ds", &["c", "d", "e"]);
    assert_eq!(reporting(&store), before, "no rollup, no cohort");
}

#[test]
fn a_crawl_and_an_import_in_one_hour_keep_separate_counts() {
    let mut store = mem();
    crawl(&mut store, &["a", "b"]);
    import(&mut store, b"ds", &["c", "d", "e"]);
    crawl(&mut store, &["f"]);
    let r = store
        .record_searchable(last_id(&store), T + 600, CODES)
        .unwrap()
        .unwrap();
    assert_eq!((r.torrents, r.imported), (6, 3));
    assert_eq!(newly(&store, 0), 3, "crawled");
    assert_eq!(newly(&store, 1), 3, "imported");
    assert_eq!(searchable_cohort(&store), 3, "the funnel is the crawl's");
    let delays: i64 = store
        .rollup_sums(CODES.delay, H, H + 1, RollupBy::Total)
        .unwrap()
        .iter()
        .map(|c| c.value)
        .sum();
    assert_eq!(delays, 3, "time to searchable: crawled only");
    assert_eq!(store.insights_state().unwrap().imported, 3);
}

#[test]
fn an_import_annotates_its_start_finish_and_removal() {
    let mut store = mem();
    crawl(&mut store, &["known"]);
    let job = import(&mut store, b"ds", &["known", "n1", "n2", "n3"]);
    let e = import_events(&store);
    assert_eq!(e.len(), 2, "{e:?}");
    assert_eq!(
        (e[0].at, e[0].kind, e[0].detail.as_str()),
        (
            T + 10,
            event::STARTED,
            &*format!("job={job} source=magnetico")
        )
    );
    assert_eq!(
        (e[1].at, e[1].kind, e[1].detail.as_str()),
        (
            T + 40,
            event::FINISHED,
            &*format!("job={job} source=magnetico new=3 merged=1")
        ),
        "novel against already known"
    );

    // Indexed, then removed in steps: one annotation, the steps summed,
    // and the final cohorts unchanged.
    store
        .record_searchable(last_id(&store), T + 600, CODES)
        .unwrap();
    let cohorts = store.cohorts(0, i64::MAX).unwrap();
    let mut done = false;
    while !done {
        done = store.remove_import_job_records(job, 1).unwrap().done;
    }
    assert_eq!(store.cohorts(0, i64::MAX).unwrap(), cohorts);
    assert_eq!(store.insights_state().unwrap().imported, 0);
    let e = import_events(&store);
    assert_eq!(e.len(), 3, "{e:?}");
    assert_eq!(e[2].kind, event::REMOVED);
    assert_eq!(e[2].detail, format!("job={job} source=magnetico removed=3"));
    // A second removal finds nothing and writes nothing.
    store.remove_import_job_records(job, 10).unwrap();
    assert_eq!(import_events(&store).len(), 3);
}

#[test]
fn a_job_cancelled_while_reading_annotates_its_finish() {
    let mut store = mem();
    let job = store
        .create_import_job(&NewImportJob {
            adapter: "magnetico",
            adapter_ver: 1,
            dataset: b"ds",
            schema_sig: b"sig",
            options: b"{}",
            credential: None,
            at: T,
        })
        .unwrap();
    // Cancelled before reading: nothing began, nothing to annotate.
    store.cancel_import_job(job.id, job.revision, T).unwrap();
    assert!(import_events(&store).is_empty());

    let job = store
        .create_import_job(&NewImportJob {
            adapter: "magnetico",
            adapter_ver: 1,
            dataset: b"ds2",
            schema_sig: b"sig",
            options: b"{}",
            credential: None,
            at: T,
        })
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Ready, T)
        .unwrap();
    let job = store
        .set_import_phase(job.id, job.revision, Phase::Reading, T + 1)
        .unwrap();
    let job = store.pause_import_job(job.id, job.revision, T + 2).unwrap();
    let job = store
        .resume_import_job(
            job.id,
            job.revision,
            &Revalidation {
                dataset: &job.dataset,
                schema_sig: &job.schema_sig,
                credential_ok: true,
                space_ok: true,
            },
            T + 3,
        )
        .unwrap();
    store
        .cancel_import_job(job.id, job.revision, T + 4)
        .unwrap();
    let kinds: Vec<u32> = import_events(&store).iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [event::STARTED, event::FINISHED],
        "pause and resume aren't annotated"
    );
}

/// The gate counts `Inserted::New` from the writer's commit
/// (`metadata_new_verified_total{origin="network"}`): a network fetch of
/// an imported torrent is `Existing`, so it never counts as new.
#[test]
fn a_fetch_of_an_imported_torrent_is_not_new_to_the_gate() {
    let mut store = mem();
    import(&mut store, b"ds", &["x"]);
    let p = prepared(&store, "x");
    let w = store
        .write(&WriteBatch {
            prepared: &[PreparedTorrent {
                torrent: &p,
                discovered_at: T + 60,
                metadata_at: T + 120,
                extra_flags: 0,
                source: DHT,
                verified: true,
            }],
            ..WriteBatch::default()
        })
        .unwrap();
    assert!(
        matches!(w.prepared[..], [lodestar_store::Inserted::Existing(_)]),
        "{:?}",
        w.prepared
    );
}

/// Phases may skip forward: a job that jumps past `reading` still
/// annotates its start, and one change can begin and finish it.
#[test]
fn a_job_that_skips_phases_still_annotates_its_start() {
    let mut store = mem();
    let job = store
        .create_import_job(&NewImportJob {
            adapter: "magnetico",
            adapter_ver: 1,
            dataset: b"ds",
            schema_sig: b"sig",
            options: b"{}",
            credential: None,
            at: T,
        })
        .unwrap();
    store
        .set_import_phase(job.id, job.revision, Phase::Indexing, T + 5)
        .unwrap();
    let kinds: Vec<(i64, u32)> = import_events(&store)
        .iter()
        .map(|e| (e.at, e.kind))
        .collect();
    assert_eq!(kinds, [(T + 5, event::STARTED), (T + 5, event::FINISHED)]);
}
