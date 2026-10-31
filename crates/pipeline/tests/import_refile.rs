//! Waiting import candidates are judged again when the rules change
//! (`docs/decisions/2026-10-31-B-import-refile.md`).

use lodestar_filter::{
    Action, Core, CsList, Engine, Handle, Lists, Origin, Sealer, Switches, TermSet,
};
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::enrich::{Backfill, refile_candidates};
use lodestar_store::import::{
    Decision, ImportBatch, ImportCandidate, ImportJob, ImportRecord, NewImportJob, Phase, Privacy,
};
use lodestar_store::{Store, StoreConfig};

const T: i64 = 1_700_000_000;

fn stand_in() -> Handle {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    )))
}

fn drop_rule(filter: &Handle, query: &str) {
    filter.update_engine(|e| {
        let notes = e.load_rules(&lodestar_store::RuleSet {
            rules: vec![lodestar_store::RuleRow {
                id: 1,
                query: query.into(),
                action: Action::Drop.code(),
                enabled: true,
                shadow_until: None,
                unless_query: None,
            }],
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
    });
}

/// A job that imported these names as candidates, one per hash `[i; 20]`
/// (from 1), and finished.
fn imported(store: &mut Store, names: &[String]) -> ImportJob {
    let src = store.register_source("magnetico", "1", T).unwrap();
    let job = store
        .create_import_job(&NewImportJob {
            adapter: "magnetico",
            adapter_ver: 1,
            dataset: b"ds",
            schema_sig: b"sig",
            options: b"",
            credential: None,
            at: T,
        })
        .unwrap();
    let mut job = store
        .set_import_phase(job.id, job.revision, Phase::Reading, T)
        .unwrap();
    for (seq, chunk) in names.chunks(500).enumerate() {
        let records: Vec<ImportRecord<'_>> = chunk
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let n = (seq * 500 + i + 1) as u32;
                let mut h = [0u8; 20];
                h[..4].copy_from_slice(&n.to_be_bytes());
                ImportRecord::Candidate(ImportCandidate {
                    hash: InfoHash(h),
                    hash_v2: None,
                    name: Some(name.as_bytes()),
                    size: Some(1 << 20),
                    privacy: Privacy::Unknown,
                    source_at: None,
                    decision: Decision {
                        hits: &[],
                        marks: 0,
                    },
                })
            })
            .collect();
        store
            .commit_import_batch(&ImportBatch {
                job: job.id,
                seq: seq as u64 + 1,
                position: b"",
                source: src,
                source_bytes: 0,
                at: T,
                class_gen: None,
                records: &records,
            })
            .unwrap();
    }
    for phase in [Phase::Committing, Phase::Indexing, Phase::Complete] {
        job = store
            .set_import_phase(job.id, job.revision, phase, T)
            .unwrap();
    }
    job
}

fn hash(n: u32) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[..4].copy_from_slice(&n.to_be_bytes());
    h
}

/// Runs the sweep to its end; returns how many batches it took.
fn sweep(store: &mut Store, filter: &Handle, sealer: Option<&Sealer>, limit: usize) -> usize {
    let ctx = Backfill {
        filter: None,
        sealer,
        quarantine_secs: 86_400,
    };
    let version = filter.current().version;
    let (mut after, mut batches) = (0, 0);
    while let Some(b) = refile_candidates(store, filter, version, after, limit, &ctx).unwrap() {
        assert!(!b.rejected);
        after = b.last;
        batches += 1;
    }
    batches
}

#[test]
fn a_drop_rule_added_after_import_removes_matching_candidates() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let names = ["Badword Holiday Film".to_owned(), "Plain Film".to_owned()];
    let job = imported(&mut store, &names);
    assert_eq!(job.phase, Phase::CompleteWithIssues);
    assert_eq!(job.open_candidates, 2);

    let filter = stand_in();
    // Nothing matches yet: nothing changes.
    sweep(&mut store, &filter, None, 10);
    assert_eq!(store.import_job(job.id).unwrap().unwrap(), job);

    drop_rule(&filter, "badword");
    sweep(&mut store, &filter, None, 10);
    let after = store.import_job(job.id).unwrap().unwrap();
    assert_eq!(after.open_candidates, 1, "the plain one stays");
    assert_eq!(after.counters.candidates, 1);
    assert_eq!(after.counters.filtered, 1);
    assert!(after.counters.conserved());
    assert_eq!(after.phase, Phase::CompleteWithIssues);
    let tombs = store.tombstones(10).unwrap();
    assert_eq!(tombs.len(), 1);
    assert_eq!(tombs[0].hash, hash(1));
    let left = store.waiting_candidates(0, 10).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].hash.0, hash(2));

    // Judged again, nothing more changes.
    sweep(&mut store, &filter, None, 10);
    assert_eq!(store.import_job(job.id).unwrap().unwrap(), after);
}

#[test]
fn a_core_term_or_cs1_hash_added_after_import_removes_too() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let names = [
        "Rolea Lexa clip".to_owned(),
        "Plain Film".to_owned(),
        "Another Film".to_owned(),
    ];
    let job = imported(&mut store, &names);
    // The core's terms were on at import here only in the engine: the
    // candidate went in unjudged, as with a rule that came later.
    let filter = stand_in();
    filter.update(|c| c.cs.add_hash(hash(2), Origin::Reported));
    let sealer = Sealer::new(&[9; 32]);
    sweep(&mut store, &filter, Some(&sealer), 10);

    let after = store.import_job(job.id).unwrap().unwrap();
    assert_eq!(after.open_candidates, 1);
    assert_eq!(after.counters.candidates, 1);
    assert_eq!(
        after.counters.filtered + after.counters.quarantined,
        2,
        "{after:?}"
    );
    assert!(after.counters.conserved());
    // The core's quarantine keeps the name sealed, never in the clear.
    let q = store.quarantined(10).unwrap();
    let held = q.iter().find(|q| q.hash == hash(1)).expect("quarantined");
    let sealed = held.sealed.as_deref().expect("a sealed name");
    assert!(!sealed.windows(5).any(|w| w.eq_ignore_ascii_case(b"rolea")));
    assert_eq!(sealer.open(sealed).unwrap(), "Rolea Lexa clip");
}

#[test]
fn the_last_issue_removed_completes_the_job() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let job = imported(&mut store, &["Badword one".to_owned()]);
    assert_eq!(job.phase, Phase::CompleteWithIssues);
    let filter = stand_in();
    drop_rule(&filter, "badword");
    sweep(&mut store, &filter, None, 10);
    let after = store.import_job(job.id).unwrap().unwrap();
    assert_eq!(after.phase, Phase::Complete);
    assert_eq!(after.open_candidates, 0);
    assert!(after.counters.conserved());
    assert!(after.revision > job.revision);
}

#[test]
fn many_candidates_go_in_short_batches() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let names: Vec<String> = (0..1_200)
        .map(|i| {
            if i % 2 == 0 {
                format!("Badword part {i}")
            } else {
                format!("Plain part {i}")
            }
        })
        .collect();
    let job = imported(&mut store, &names);
    let filter = stand_in();
    drop_rule(&filter, "badword");
    // Each batch is its own transaction of at most 100 candidates.
    let batches = sweep(&mut store, &filter, None, 100);
    assert_eq!(batches, 12);
    let after = store.import_job(job.id).unwrap().unwrap();
    assert_eq!(after.open_candidates, 600);
    assert_eq!(after.counters.filtered, 600);
    assert_eq!(after.counters.candidates, 600);
    assert!(after.counters.conserved());
}

#[test]
fn a_rule_change_during_a_batch_writes_nothing() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let job = imported(&mut store, &["Badword one".to_owned()]);
    let filter = stand_in();
    drop_rule(&filter, "badword");
    let ctx = Backfill::default();
    let stale = filter.current().version - 1;
    let b = refile_candidates(&mut store, &filter, stale, 0, 10, &ctx)
        .unwrap()
        .unwrap();
    assert!(b.rejected);
    assert_eq!(store.import_job(job.id).unwrap().unwrap(), job);
}

#[test]
fn a_waiting_candidate_prints_no_name() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    imported(&mut store, &["Secretname one".to_owned()]);
    let c = store.waiting_candidates(0, 1).unwrap();
    let printed = format!("{c:?}");
    assert!(!printed.to_lowercase().contains("secretname"), "{printed}");
}
