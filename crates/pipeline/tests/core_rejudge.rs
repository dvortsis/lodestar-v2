//! The core re-judge (§20.5): stored torrents judged by an older core are
//! judged again by today's, in two phases and in batches that resume after
//! a restart, and so are torrents an older build stored since; a store
//! judged already is left alone. Stand-in
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
    CORE_AFTER_KEY, CORE_FOUND_KEY, CORE_HELD_KEY, CORE_JUDGED_KEY, CORE_PASS_KEY, CORE_PHASE_KEY,
    CORE_SEEN_KEY, CORE_START_KEY, CORE_UNREADABLE_KEY, CorePass, NewTorrent, SourceId, Store,
    StoreConfig,
};

fn add(store: &mut Store, name: &str) -> (i64, [u8; 20]) {
    add_pack(store, name, &[])
}

/// A torrent of `files` (one path each, a single file when empty).
fn add_pack(store: &mut Store, name: &str, files: &[String]) -> (i64, [u8; 20]) {
    let bytes = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
    let mut info = vec![
        (b"name".to_vec(), bytes(name)),
        (b"piece length".to_vec(), OwnedValue::Int(1 << 20)),
        (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
    ];
    if files.is_empty() {
        info.push((b"length".to_vec(), OwnedValue::Int(1 << 30)));
    } else {
        let list = files
            .iter()
            .map(|f| {
                OwnedValue::Dict(
                    [
                        (b"length".to_vec(), OwnedValue::Int(1 << 20)),
                        (
                            b"path".to_vec(),
                            OwnedValue::List(f.split('/').map(bytes).collect()),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                )
            })
            .collect();
        info.push((b"files".to_vec(), OwnedValue::List(list)));
    }
    let raw = to_vec(&OwnedValue::Dict(info.into_iter().collect()));
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

/// `name` after many plain words: judged in the first phase.
fn long(name: &str) -> String {
    format!("{}{name}", "word ".repeat(600))
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

/// The pass under way.
fn pass(store: &mut Store) -> Option<CorePass> {
    store.core_rejudge_due(CORE_VERSION).unwrap()
}

/// Runs the re-judge to its end, a batch of `limit` at a time.
fn run(store: &mut Store, filter: &Handle, sealer: &Sealer, limit: usize) -> usize {
    let mut batches = 0;
    while let (Some(b), _) = batch(store, filter, sealer, limit) {
        assert!(!b.rejected);
        if b.finished {
            break;
        }
        batches += 1;
        assert!(batches < 10_000, "the re-judge ends");
    }
    batches
}

fn stored(store: &Store, id: i64) -> bool {
    store.torrent(id).unwrap().is_some()
}

/// A store judged by the older core: every torrent is judged again, those
/// the earlier core may have judged differently first. What today's core
/// holds is held, the rest stays, and the store is marked as judged by
/// today's.
#[test]
fn a_store_judged_by_an_older_core_is_judged_again() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
    let (plain, _) = add(&mut store, &long("Another.Film.2001.1080p.mkv"));
    let (pair, pair_hash) = add(&mut store, &long("rolea lexa.mkv"));
    // Read whole by the earlier core: judged again in the second phase.
    let (short, short_hash) = add(&mut store, "rolea lexa.mkv");
    // A file list that won't read can't be judged: held first, never shown.
    let (unreadable, unreadable_hash) = add(&mut store, "Some.Show.S01E01.mkv");
    store
        .connection()
        .execute(
            "UPDATE torrent_files SET codec = 250 WHERE torrent_id = ?1",
            [unreadable],
        )
        .unwrap();
    assert_eq!(pass(&mut store), Some(CorePass::starting(0)));
    let (first, marked) = batch(&mut store, &filter, &sealer, 10);
    let first = first.unwrap();
    assert_eq!((first.judged, first.left), (3, 1));
    assert_eq!(marked, first.held, "marked for intake before the write");
    assert!(first.held.contains(&pair_hash) && first.held.contains(&unreadable_hash));
    assert!(stored(&store, plain) && stored(&store, short));
    assert!(!stored(&store, pair) && !stored(&store, unreadable));
    assert_eq!(
        store.quarantine_rule(&pair_hash).unwrap().as_deref(),
        Some("CS-4a")
    );
    assert_eq!(
        store.quarantine_rule(&unreadable_hash).unwrap().as_deref(),
        Some("CS-0a")
    );
    assert_eq!(
        (first.before, first.after),
        (
            CorePass::starting(0),
            CorePass {
                after: unreadable,
                found: 3,
                held: 2,
                unreadable: 1,
                ..CorePass::starting(0)
            }
        ),
        "counts only, kept with the place"
    );
    // The first phase ends; the second goes over the same ids.
    let end = batch(&mut store, &filter, &sealer, 10).0.unwrap();
    assert!(end.phase_done && !end.finished);
    let second = pass(&mut store).unwrap();
    assert_eq!((second.phase, second.after), (2, 0));
    let (b, marked) = batch(&mut store, &filter, &sealer, 10);
    let b = b.unwrap();
    assert_eq!((b.judged, b.left), (1, 1));
    assert_eq!(marked, vec![short_hash]);
    assert!(stored(&store, plain) && !stored(&store, short));
    assert_eq!(b.after.found, 4);
    // Then nothing is left: it records the version and the totals go.
    let last = batch(&mut store, &filter, &sealer, 10).0.unwrap();
    assert!(last.phase_done && last.finished);
    assert_eq!(last.after, b.after);
    assert_eq!(batch(&mut store, &filter, &sealer, 10).0, None);
    assert_eq!(
        store.setting_i64(CORE_JUDGED_KEY).unwrap(),
        Some(CORE_VERSION)
    );
    for key in [
        CORE_PASS_KEY,
        CORE_PHASE_KEY,
        CORE_START_KEY,
        CORE_AFTER_KEY,
        CORE_FOUND_KEY,
        CORE_HELD_KEY,
        CORE_UNREADABLE_KEY,
    ] {
        assert_eq!(store.setting_i64(key).unwrap(), None, "{key}");
    }
    assert_eq!(pass(&mut store), None);
}

/// A short torrent whose name the earlier core may have read differently
/// is judged in the first phase, and held.
#[test]
fn a_short_torrent_read_differently_is_judged_first() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
    let (plain, _) = add(&mut store, "rolea lexa.mkv");
    let (marked, hash) = add(&mut store, "rol\u{FFF9}ea lexa.mkv");
    let (b, _) = batch(&mut store, &filter, &sealer, 10);
    let b = b.unwrap();
    assert_eq!((b.before.phase, b.judged, b.left), (1, 1, 1));
    assert_eq!(b.held, vec![hash]);
    assert!(!stored(&store, marked) && stored(&store, plain));
    assert_eq!(
        store.quarantine_rule(&hash).unwrap().as_deref(),
        Some("CS-4a")
    );
    // The other is held in the second phase.
    run(&mut store, &filter, &sealer, 10);
    assert!(!stored(&store, plain));
}

/// A restart mid-run resumes after the last batch it finished, in its
/// phase: the store holds the place, and every torrent is judged.
#[test]
fn a_restart_resumes_where_the_rejudge_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    let mut first = Vec::new();
    let mut other = Vec::new();
    for i in 0..3 {
        first.push(add(&mut store, &long(&format!("Plain.Film.{i}.mkv"))));
        other.push(add(&mut store, &format!("Plain.Show.{i}.mkv")));
    }
    let (late_pair, late_hash) = add(&mut store, "rolea lexa.mkv");
    let (pair, pair_hash) = add(&mut store, &long("rolea lexa.mkv"));
    // Two batches of the first phase, then a new start.
    for _ in 0..2 {
        batch(&mut store, &filter, &sealer, 2);
    }
    let place = pass(&mut store).unwrap();
    assert_eq!((place.phase, place.after), (1, other[1].0));
    drop(store);
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(pass(&mut store), Some(place));
    // On to one batch into the second phase, then a new start.
    loop {
        let b = batch(&mut store, &filter, &sealer, 2).0.unwrap();
        if b.before.phase == 2 {
            break;
        }
    }
    assert!(!stored(&store, pair) && stored(&store, late_pair));
    let place = pass(&mut store).unwrap();
    assert_eq!((place.phase, place.after), (2, other[0].0));
    drop(store);
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(pass(&mut store), Some(place));
    run(&mut store, &filter, &sealer, 2);
    for (hash, id) in [(pair_hash, pair), (late_hash, late_pair)] {
        assert!(!stored(&store, id));
        assert_eq!(
            store.quarantine_rule(&hash).unwrap().as_deref(),
            Some("CS-4a")
        );
    }
    assert!(
        first
            .iter()
            .chain(&other)
            .all(|&(id, _)| stored(&store, id))
    );
    assert_eq!(pass(&mut store), None);
}

/// A pass left by another core version starts again from the first
/// torrent, so nothing is judged by a mix of the two.
#[test]
fn another_versions_pass_starts_again() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (pair, _) = add(&mut store, &long("rolea lexa.mkv"));
    let stale = CorePass {
        phase: 2,
        start: 0,
        after: pair,
        found: 7,
        held: 7,
        unreadable: 7,
    };
    store.set_core_rejudge(CORE_VERSION - 1, &stale).unwrap();
    assert_eq!(pass(&mut store), Some(CorePass::starting(0)));
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    assert!(!stored(&store, pair));
}

/// Every torrent up to the high-water id was judged by today's core:
/// nothing is judged again, and the marker stays.
#[test]
fn a_current_store_is_left_alone() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (pair, _) = add(&mut store, &long("rolea lexa.mkv"));
    store.finish_core_rejudge(CORE_VERSION).unwrap();
    assert_eq!(store.setting_i64(CORE_SEEN_KEY).unwrap(), Some(pair));
    let (r, marked) = batch(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    assert_eq!((r, marked), (None, Vec::new()));
    assert!(stored(&store, pair));
}

/// A build that doesn't keep the marker (an older one) stored torrents
/// after the store was marked: everything above the high-water id is
/// judged again, and nothing below it.
#[test]
fn what_an_older_build_stored_since_is_judged_again() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (before, _) = add(&mut store, &long("rolea lexa.mkv"));
    store.finish_core_rejudge(CORE_VERSION).unwrap();
    // Stored by the older build: the marker doesn't move.
    let (since, since_hash) = add(&mut store, &long("rolea lexa again.mkv"));
    // So an import starting now mustn't mark what it stores as judged.
    assert!(!lodestar_pipeline::core_rejudge::is_current(&mut store).unwrap());
    assert_eq!(
        store.core_rejudge_due(CORE_VERSION).unwrap(),
        Some(CorePass::starting(before))
    );
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    assert!(stored(&store, before), "judged by today's core already");
    assert!(!stored(&store, since));
    assert_eq!(
        store.quarantine_rule(&since_hash).unwrap().as_deref(),
        Some("CS-4a")
    );
    // The mark is the highest id still stored (ids are never reused).
    assert_eq!(store.setting_i64(CORE_SEEN_KEY).unwrap(), Some(before));
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), None);
    // This build records what it stores itself.
    let (own, _) = add(&mut store, "Plain.Film.mkv");
    lodestar_pipeline::core_rejudge::note_judged(&mut store).unwrap();
    assert_eq!(store.setting_i64(CORE_SEEN_KEY).unwrap(), Some(own));
    assert_eq!(store.core_rejudge_due(CORE_VERSION).unwrap(), None);
}

/// A store marked by a newer core (a downgrade): its marker is lowered to
/// today's, so an upgrade later judges again what this build stores, and
/// what was stored above its high-water id is judged again now.
#[test]
fn a_downgrade_lowers_the_marker_and_judges_what_followed() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (before, _) = add(&mut store, &long("rolea lexa.mkv"));
    store.finish_core_rejudge(CORE_VERSION + 1).unwrap();
    let (since, _) = add(&mut store, &long("rolea lexa again.mkv"));
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    assert_eq!(
        store.setting_i64(CORE_JUDGED_KEY).unwrap(),
        Some(CORE_VERSION)
    );
    assert!(stored(&store, before) && !stored(&store, since));
    // An upgrade back judges every torrent again.
    assert_eq!(
        store.core_rejudge_due(CORE_VERSION + 1).unwrap(),
        Some(CorePass::starting(0))
    );
}

/// A row that won't decode can't be judged: held under CS-0a by the
/// hashes as stored, and deleted, and the rest of the batch goes on.
#[test]
fn a_row_that_wont_decode_is_held() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (bad, bad_hash) = add(&mut store, &long("Some.Show.mkv"));
    let (pair, _) = add(&mut store, &long("rolea lexa.mkv"));
    store
        .connection()
        .execute(
            "UPDATE torrents SET name_raw = x'41', name_enc = 250 WHERE id = ?1",
            [bad],
        )
        .unwrap();
    assert!(store.torrent(bad).is_err(), "the row won't decode");
    let (b, _) = batch(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    let b = b.unwrap();
    assert_eq!((b.judged, b.after.held, b.after.unreadable), (2, 2, 1));
    let gone: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM torrents WHERE id IN (?1, ?2)",
            [bad, pair],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gone, 0);
    assert_eq!(
        store.quarantine_rule(&bad_hash).unwrap().as_deref(),
        Some("CS-0a")
    );
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A stored file list far deeper and wider than the core reads: held as
/// too large to judge, without decoding it.
#[test]
fn a_file_list_past_what_the_core_reads_is_held() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (big, big_hash) = add(&mut store, "Some.Show.mkv");
    let (plain, _) = add(&mut store, "Other.Show.mkv");
    // One chain of directories, then as many files in the deepest one.
    let n = 500_000u64;
    let mut blob = vec![1, 0, 0];
    varint(&mut blob, n);
    varint(&mut blob, n);
    for i in 0..n {
        varint(&mut blob, i);
    }
    blob.extend((0..n).map(|_| 1));
    blob.extend((0..n).map(|_| b'a'));
    for _ in 0..n {
        varint(&mut blob, n);
    }
    blob.extend((0..n).map(|_| 1));
    blob.extend((0..n).map(|_| b'b'));
    blob.extend((0..n).map(|_| 1));
    store
        .connection()
        .execute(
            "UPDATE torrent_files SET codec = 0, dict_id = NULL, data = ?2 WHERE torrent_id = ?1",
            lodestar_store::rusqlite::params![big, blob],
        )
        .unwrap();
    let (b, marked) = batch(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    let b = b.unwrap();
    assert_eq!((b.judged, b.after.unreadable), (1, 1));
    assert_eq!(marked, vec![big_hash]);
    assert!(!stored(&store, big) && stored(&store, plain));
    assert_eq!(
        store.quarantine_rule(&big_hash).unwrap().as_deref(),
        Some("CS-0a")
    );
}

/// A batch that mostly won't read is the store failing, not its
/// torrents: it stops, holds nothing, and keeps its place.
#[test]
fn a_batch_that_mostly_wont_read_holds_nothing() {
    use lodestar_pipeline::core_rejudge::{BREAKER_ROWS, is_breaker};
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let ids: Vec<i64> = (0..BREAKER_ROWS * 2)
        .map(|i| add(&mut store, &long(&format!("Plain.Film.{i}.mkv"))).0)
        .collect();
    store
        .connection()
        .execute("UPDATE torrent_files SET codec = 250", [])
        .unwrap();
    let filter = stand_in();
    let sealer = Sealer::new(&[9; 32]);
    let ctx = Backfill {
        filter: Some(&filter),
        sealer: Some(&sealer),
        quarantine_secs: 86_400,
        adult: Default::default(),
    };
    let marked = RefCell::new(0);
    let e = rejudge_stored(&mut store, &filter, ids.len(), &ctx, &|_, hs| {
        *marked.borrow_mut() += hs.len()
    })
    .unwrap_err();
    assert!(is_breaker(&e));
    assert_eq!(*marked.borrow(), 0);
    assert!(ids.iter().all(|&id| stored(&store, id)));
    assert_eq!(pass(&mut store), Some(CorePass::starting(0)));
    // A few that won't read among many that do are held, one by one.
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let ids: Vec<i64> = (0..BREAKER_ROWS * 4)
        .map(|i| add(&mut store, &long(&format!("Plain.Film.{i}.mkv"))).0)
        .collect();
    for &id in ids.iter().step_by(3) {
        store
            .connection()
            .execute(
                "UPDATE torrent_files SET codec = 250 WHERE torrent_id = ?1",
                [id],
            )
            .unwrap();
    }
    let b = batch(&mut store, &filter, &sealer, ids.len()).0.unwrap();
    assert_eq!(b.after.unreadable as usize, ids.len().div_ceil(3));
}

/// A missing compression dictionary is the store's fault, not the
/// torrent's: the batch fails and the torrent stays.
#[test]
fn a_missing_dictionary_fails_the_batch() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (id, _) = add(&mut store, &long("Plain.Film.mkv"));
    let c = store.connection();
    c.pragma_update(None, "foreign_keys", false).unwrap();
    c.execute(
        "UPDATE torrent_files SET codec = 1, dict_id = 4242 WHERE torrent_id = ?1",
        [id],
    )
    .unwrap();
    let filter = stand_in();
    let sealer = Sealer::new(&[9; 32]);
    let ctx = Backfill {
        filter: Some(&filter),
        sealer: Some(&sealer),
        quarantine_secs: 86_400,
        adult: Default::default(),
    };
    let e = rejudge_stored(&mut store, &filter, 10, &ctx, &|_, _| {}).unwrap_err();
    assert!(matches!(e, lodestar_store::Error::Compression(_)), "{e}");
    assert!(stored(&store, id));
    assert_eq!(pass(&mut store), Some(CorePass::starting(0)));
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
    let (detected, detected_hash) = add(&mut store, &long(&format!("Some.Title.XXX {SPREAD}.mp4")));
    let (relabelled, relabelled_hash) = add(&mut store, &long(&format!("Plain {SPREAD}.mp4")));
    let (plain, _) = add(&mut store, &long(&format!("Other {SPREAD}.mp4")));
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

/// A torrent an always-adult source found is read as Adult too, as
/// ingest reads it: the Adult-only core holds it, as an adult hold.
#[test]
fn an_always_adult_sources_find_is_read_as_adult() {
    use lodestar_pipeline::source_adult::AdultSources;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (found, hash) = add(&mut store, &long(&format!("Plain {SPREAD}.mp4")));
    let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
    let ctx = Backfill {
        filter: Some(&filter),
        sealer: Some(&sealer),
        quarantine_secs: 86_400,
        // Source 0 found it, and every find of source 0 is adult.
        adult: AdultSources(1),
    };
    rejudge_stored(&mut store, &filter, 10, &ctx, &|_, _| {}).unwrap();
    assert!(!stored(&store, found));
    let rule: String = store
        .connection()
        .query_row(
            "SELECT rule FROM quarantine WHERE hash = ?1",
            [&hash[..]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rule, "CS-5a+adult");
}

/// A torrent with a partial file list, stored as an import left it, is
/// judged by the list it has: what today's core holds there is deleted
/// with its hold in one write, and one whose list won't read is held as
/// too large to judge, in the first phase.
#[test]
fn a_partial_file_list_is_judged_by_what_it_holds() {
    use lodestar_store::flags::FILES_PARTIAL;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    // Short: the earlier core would have read a whole list like it.
    let (held, held_hash) = add(&mut store, "rolea lexa.mkv");
    let (unread, unread_hash) = add(&mut store, "Some.Show.mkv");
    let c = store.connection();
    for id in [held, unread] {
        c.execute(
            "UPDATE torrents SET flags = flags | ?2 WHERE id = ?1",
            [id, FILES_PARTIAL],
        )
        .unwrap();
    }
    c.execute(
        "UPDATE torrent_files SET codec = 250 WHERE torrent_id = ?1",
        [unread],
    )
    .unwrap();
    let (b, marked) = batch(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    let b = b.unwrap();
    assert_eq!((b.judged, b.left), (1, 1), "the unreadable one first");
    assert_eq!(marked, vec![unread_hash]);
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    for (id, hash, rule) in [(held, held_hash, "CS-4a"), (unread, unread_hash, "CS-0a")] {
        assert!(!stored(&store, id), "{rule}: deleted");
        assert_eq!(
            store.quarantine_rule(&hash).unwrap().as_deref(),
            Some(rule),
            "and held"
        );
    }
}

/// A copy restored from before the marker (a rollback) records none: its
/// torrents are judged again from the start.
#[test]
fn a_restored_copy_without_a_marker_is_judged_again() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let (pair, _) = add(&mut store, &long("rolea lexa.mkv"));
    store.finish_core_rejudge(CORE_VERSION).unwrap();
    store
        .connection()
        .execute("DELETE FROM settings WHERE key LIKE 'filter.core.%'", [])
        .unwrap();
    assert_eq!(pass(&mut store), Some(CorePass::starting(0)));
    run(&mut store, &stand_in(), &Sealer::new(&[9; 32]), 10);
    assert!(!stored(&store, pair));
}

/// An empty store is marked as judged by today's core, which judges what
/// it stores next; a store with torrents isn't.
#[test]
fn an_empty_store_is_marked_as_judged() {
    use lodestar_pipeline::core_rejudge::seed_if_empty;
    let mut empty = Store::open_in_memory(StoreConfig::default()).unwrap();
    seed_if_empty(&mut empty).unwrap();
    assert_eq!(empty.core_rejudge_due(CORE_VERSION).unwrap(), None);
    let mut full = Store::open_in_memory(StoreConfig::default()).unwrap();
    add(&mut full, &long("rolea lexa.mkv"));
    seed_if_empty(&mut full).unwrap();
    assert_eq!(full.setting_i64(CORE_JUDGED_KEY).unwrap(), None);
}

/// How long the re-judge takes, per million stored torrents: 20,000
/// stored, single files and packs of 20 to 200 files, 2% of them judged
/// in the first phase, in memory and on disk (closed and reopened before
/// the run, so the page cache is the OS's). A synthetic stand-in for a
/// real store: prints the rates; run with `--ignored`.
#[test]
#[ignore]
fn rejudge_rate() {
    let dir = tempfile::tempdir().unwrap();
    let n = 20_000;
    let db = dir.path().join("t.db");
    for disk in [false, true] {
        let mut store = if disk {
            Store::open(&db, StoreConfig::default()).unwrap()
        } else {
            Store::open_in_memory(StoreConfig::default()).unwrap()
        };
        let mut files = 0usize;
        for i in 0..n {
            let name = format!("Some.Show.S01E{i:05}.1080p.WEB-DL.DDP5.1.x264-GRP");
            let name = if i % 50 == 0 { long(&name) } else { name };
            let pack: Vec<String> = match i % 10 {
                0..=6 => Vec::new(),
                k => (0..20 * (k - 6) * (k - 6))
                    .map(|f| format!("Season 1/Disc {}/Episode.{f:03}.Part.mkv", f % 4))
                    .collect(),
            };
            files += pack.len().max(1);
            add_pack(&mut store, &name, &pack);
        }
        if disk {
            drop(store);
            store = Store::open(&db, StoreConfig::default()).unwrap();
        }
        let (filter, sealer) = (stand_in(), Sealer::new(&[9; 32]));
        let t = std::time::Instant::now();
        run(&mut store, &filter, &sealer, 256);
        let s = t.elapsed().as_secs_f64();
        let label = if disk { "on disk" } else { "in memory" };
        println!(
            "{label}: {n} stored ({files} files): {s:.2} s, {:.0} s per million stored",
            s * 1e6 / n as f64
        );
    }
}
