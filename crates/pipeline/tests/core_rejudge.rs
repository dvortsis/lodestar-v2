//! The core re-judge (§20.5): stored torrents judged by an older core are
//! judged again by today's, in batches that resume after a restart; a
//! store judged already, or by a newer core, is left alone. Stand-in
//! terms as in the core's tests: `lexa` for a sexual term, `rolea` for a
//! child role.

use std::cell::RefCell;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_filter::{
    CORE_VERSION, Core, CsList, Engine, Handle, Lists, Sealer, Switches, TermSet,
};
use lodestar_metainfo::Info;
use lodestar_pipeline::core_rejudge::{CoreRejudged, rejudge_stored};
use lodestar_pipeline::enrich::Backfill;
use lodestar_store::{
    CORE_AFTER_KEY, CORE_JUDGED_KEY, CORE_PASS_KEY, NewTorrent, SourceId, Store, StoreConfig,
};

fn add(store: &mut Store, name: &str) -> (i64, [u8; 20]) {
    let raw = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(1 << 30)),
            (
                b"name".to_vec(),
                OwnedValue::Bytes(name.as_bytes().to_vec()),
            ),
            (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
            (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
        ]
        .into_iter()
        .collect(),
    ));
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_000,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap()[0]
        .id();
    (id, store.torrent(id).unwrap().unwrap().info_hash.0)
}

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

/// One batch of `limit`, and the hashes it marked for intake.
fn batch(
    store: &mut Store,
    filter: &Handle,
    sealer: &Sealer,
    limit: usize,
) -> (Option<CoreRejudged>, Vec<[u8; 20]>) {
    let ctx = Backfill {
        filter: Some(filter),
        sealer: Some(sealer),
        quarantine_secs: 30 * 86_400,
        adult: Default::default(),
    };
    let marked = RefCell::new(Vec::new());
    let mark = |_: &Store, hs: &[[u8; 20]]| marked.borrow_mut().extend_from_slice(hs);
    let r = rejudge_stored(store, filter, limit, &ctx, &mark).unwrap();
    (r, marked.into_inner())
}

/// Runs the re-judge to its end, a batch of `limit` at a time.
fn run(store: &mut Store, filter: &Handle, sealer: &Sealer, limit: usize) -> usize {
    let mut batches = 0;
    while let (Some(b), _) = batch(store, filter, sealer, limit) {
        assert!(!b.rejected);
        batches += 1;
        assert!(batches < 100, "the re-judge ends");
    }
    batches
}

fn stored(store: &Store, id: i64) -> bool {
    store.torrent(id).unwrap().is_some()
}

/// A store judged by the older core: what today's holds is held, the rest
/// stays, and the store is marked as judged by today's.
#[test]
fn a_store_judged_by_an_older_core_is_judged_again() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
    let (plain, _) = add(&mut store, "Another.Film.2001.1080p.mkv");
    let (pair, pair_hash) = add(&mut store, "rolea lexa.mkv");
    // A file list that won't read can't be judged: held, never shown.
    let (unreadable, unreadable_hash) = add(&mut store, "Some.Show.S01E01.mkv");
    store
        .connection()
        .execute(
            "UPDATE torrent_files SET codec = 250 WHERE torrent_id = ?1",
            [unreadable],
        )
        .unwrap();
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), Some(0));
    let (first, marked) = batch(&mut store, &filter, &sealer, 10);
    let first = first.unwrap();
    assert_eq!(first.judged, 3);
    assert_eq!(marked, first.held, "marked for intake before the write");
    assert!(first.held.contains(&pair_hash) && first.held.contains(&unreadable_hash));
    assert!(stored(&store, plain));
    assert!(!stored(&store, pair) && !stored(&store, unreadable));
    assert_eq!(
        store.quarantine_rule(&pair_hash).unwrap().as_deref(),
        Some("CS-4a")
    );
    assert_eq!(
        store.quarantine_rule(&unreadable_hash).unwrap().as_deref(),
        Some("CS-0a")
    );
    // The next batch finds nothing left, and records the version.
    assert_eq!(batch(&mut store, &filter, &sealer, 10).0, None);
    assert_eq!(
        store.setting_i64(CORE_JUDGED_KEY).unwrap(),
        Some(CORE_VERSION)
    );
    for key in [CORE_PASS_KEY, CORE_AFTER_KEY] {
        assert_eq!(store.setting_i64(key).unwrap(), None, "{key}");
    }
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), None);
}

/// A restart mid-run resumes after the last batch it finished: the store
/// holds the place, and every torrent is judged.
#[test]
fn a_restart_resumes_where_the_rejudge_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(add(&mut store, &format!("Plain.Film.{i}.mkv")).0);
    }
    let (pair, pair_hash) = add(&mut store, "rolea lexa.mkv");
    let (first, _) = batch(&mut store, &filter, &sealer, 2);
    assert_eq!(first.unwrap().judged, 2);
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), Some(ids[1]));
    drop(store);
    // A new start, as after a crash or an upgrade mid-run.
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), Some(ids[1]));
    assert_eq!(run(&mut store, &filter, &sealer, 2), 2);
    assert!(!stored(&store, pair));
    assert_eq!(
        store.quarantine_rule(&pair_hash).unwrap().as_deref(),
        Some("CS-4a")
    );
    assert!(ids.iter().all(|&id| stored(&store, id)));
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), None);
}

/// A pass left by another core version starts again from the first
/// torrent, so nothing is judged by a mix of the two.
#[test]
fn another_versions_pass_starts_again() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (pair, _) = add(&mut store, "rolea lexa.mkv");
    store
        .set_core_rejudge_after(CORE_VERSION - 1, pair)
        .unwrap();
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), Some(0));
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    assert!(!stored(&store, pair));
}

/// A store judged by today's core, or by a newer one (a downgrade), is
/// judged again by nothing, and its marker never goes down.
#[test]
fn a_current_or_newer_store_is_left_alone() {
    for judged in [CORE_VERSION, CORE_VERSION + 1] {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let (pair, _) = add(&mut store, "rolea lexa.mkv");
        store.set_setting_i64(CORE_JUDGED_KEY, judged).unwrap();
        assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), None);
        let (r, marked) = batch(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
        assert_eq!((r, marked), (None, Vec::new()), "v{judged}");
        assert!(stored(&store, pair), "v{judged}");
        store.finish_core_rejudge(CORE_VERSION).unwrap();
        assert_eq!(store.setting_i64(CORE_JUDGED_KEY).unwrap(), Some(judged));
    }
}

/// The stand-ins ten words apart: only the Adult-only CS-5a (window 12)
/// catches them, not CS-4a (window 8).
const SPREAD: &str = "Rolea one two three four five six seven eight nine Lexa";

/// The re-judge reads a torrent as ingest does: on detection's own
/// classification, so a Not adult correction never takes a detected-adult
/// torrent out of the Adult-only core, and as Adult too where an Is adult
/// correction says so, which holds it as an adult hold.
#[test]
fn corrections_are_judged_as_ingest_judges_them() {
    use lodestar_store::ClassCorrection;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (detected, detected_hash) = add(&mut store, &format!("Some.Title.XXX {SPREAD}.mp4"));
    let (relabelled, relabelled_hash) = add(&mut store, &format!("Plain {SPREAD}.mp4"));
    let (plain, _) = add(&mut store, &format!("Other {SPREAD}.mp4"));
    let set = |store: &mut Store, id, adult| {
        store
            .set_correction(
                id,
                ClassCorrection {
                    adult: Some(adult),
                    not_junk: false,
                },
            )
            .unwrap();
    };
    set(&mut store, detected, false);
    set(&mut store, relabelled, true);
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    let stored_rule = |store: &Store, hash: [u8; 20]| -> String {
        store
            .connection()
            .query_row(
                "SELECT rule FROM quarantine WHERE hash = ?1",
                [&hash[..]],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert!(
        !stored(&store, detected),
        "Not adult doesn't lift the Adult-only core"
    );
    assert_eq!(stored_rule(&store, detected_hash), "CS-5a");
    assert!(!stored(&store, relabelled), "Is adult is read as adult");
    assert_eq!(stored_rule(&store, relabelled_hash), "CS-5a+adult");
    assert!(stored(&store, plain), "only CS-5a reads that far apart");
}
