//! Your classification corrections survive background enrichment (§20.9).

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_classify::{AdultKind, ContentType, Record};
use lodestar_metainfo::Info;
use lodestar_pipeline::enrich::enrich_stored;
use lodestar_store::{ClassCorrection, NewTorrent, SourceId, Store, StoreConfig};

fn add(store: &mut Store, name: &str, size: i64) -> i64 {
    let raw = to_vec(&OwnedValue::Dict(
        [
            (b"length".to_vec(), OwnedValue::Int(size)),
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
    store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: Some(&raw),
            discovered_at: 1_700_000_000,
            metadata_at: 1_700_000_000,
            extra_flags: 0,
            source: SourceId::new(0).unwrap(),
        }])
        .unwrap()[0]
        .id()
}

#[test]
fn background_enrichment_applies_corrections() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let movie = add(
        &mut store,
        "The.Matrix.1999.1080p.BluRay.x264-GRP.mkv",
        8 << 30,
    );
    let plain = add(
        &mut store,
        "Another.Film.2001.1080p.BluRay.x264-GRP.mkv",
        8 << 30,
    );
    store
        .set_correction(
            movie,
            ClassCorrection {
                adult: Some(true),
                not_junk: false,
            },
        )
        .unwrap();
    enrich_stored(&mut store, 0, 10).unwrap();
    let e = store.enrichment(movie).unwrap().unwrap();
    assert_eq!(
        ContentType::from_code(e.content_type),
        Some(ContentType::Adult)
    );
    let r = Record::decode(&e.data).unwrap();
    assert_eq!(r.adult_kind, Some(AdultKind::Movie));
    let e = store.enrichment(plain).unwrap().unwrap();
    assert_eq!(
        ContentType::from_code(e.content_type),
        Some(ContentType::Movie)
    );
}

#[test]
fn classification_rules_apply_in_the_background_and_own_corrections_win() {
    use lodestar_filter::{Core, CsList, Engine, Handle, Lists, Switches};
    use lodestar_pipeline::enrich::{Backfill, enrich_stored_with};
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let a = add(&mut store, "Holiday.Clips.2023.Lexb.mp4", 900 << 20);
    let b = add(&mut store, "Holiday.Clips.2024.Lexb.mp4", 900 << 20);
    let c = add(&mut store, "Holiday.Clips.2025.Other.mp4", 900 << 20);
    store
        .set_correction(
            b,
            ClassCorrection {
                adult: Some(false),
                not_junk: false,
            },
        )
        .unwrap();
    let filter = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )));
    filter.update_engine(|e| e.load_class_rules(&[(1, "lexb".into(), true)]));
    let ctx = Backfill {
        filter: Some(&filter),
        ..Backfill::default()
    };
    enrich_stored_with(&mut store, 0, 10, &ctx).unwrap();
    let kind = |store: &Store, id| {
        ContentType::from_code(store.enrichment(id).unwrap().unwrap().content_type)
    };
    assert_eq!(kind(&store, a), Some(ContentType::Adult), "the rule");
    assert_ne!(
        kind(&store, b),
        Some(ContentType::Adult),
        "its own correction wins"
    );
    assert_ne!(kind(&store, c), Some(ContentType::Adult), "not matched");

    // A newer rule wins over an older one.
    filter.update_engine(|e| {
        e.load_class_rules(&[(1, "lexb".into(), true), (2, "2023".into(), false)])
    });
    store.mark_unenriched(&[a]).unwrap();
    let ctx = Backfill {
        filter: Some(&filter),
        ..Backfill::default()
    };
    enrich_stored_with(&mut store, 0, 10, &ctx).unwrap();
    assert_ne!(kind(&store, a), Some(ContentType::Adult));
}

/// A filter with neutral stand-in terms: `lexa` for a sexual term, `rolea`
/// for a child role.
fn stand_in() -> lodestar_filter::Handle {
    use lodestar_filter::{Core, CsList, Engine, Handle, Lists, Switches, TermSet};
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

/// The stand-ins ten words apart: only the Adult-only CS-5a (window 12)
/// catches them, not CS-4a (window 8).
const SPREAD: &str = "Rolea one two three four five six seven eight nine Lexa";

#[test]
fn stored_torrents_relabelled_adult_meet_the_adult_only_core() {
    use lodestar_filter::{Action, Sealer};
    use lodestar_pipeline::enrich::{Backfill, enrich_stored_with};
    use lodestar_store::flags;
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let by_rule = add(&mut store, &format!("Studiox {SPREAD}.mp4"), 900 << 20);
    let by_own = add(&mut store, &format!("Other {SPREAD}.mp4"), 900 << 20);
    let hidden = add(&mut store, "Studiox Holiday Film.mp4", 900 << 20);
    let plain = add(&mut store, &format!("Plain {SPREAD}.mp4"), 900 << 20);
    enrich_stored(&mut store, 0, 10).unwrap();
    let kind = |store: &Store, id| {
        ContentType::from_code(store.enrichment(id).unwrap().unwrap().content_type)
    };
    for id in [by_rule, by_own, hidden, plain] {
        assert_ne!(kind(&store, id), Some(ContentType::Adult), "detection");
    }
    let hash = |store: &Store, id| store.torrent(id).unwrap().unwrap().info_hash;
    let (h_rule, h_own, h_plain) = (
        hash(&store, by_rule),
        hash(&store, by_own),
        hash(&store, plain),
    );

    // A classification rule for "studiox" and a per-torrent Is adult; your
    // rule hides adult torrents. Then re-queued, as a rule change does.
    let filter = stand_in();
    filter.update_engine(|e| {
        e.load_class_rules(&[(1, "studiox".into(), true)]);
        let notes = e.load_rules(&lodestar_store::RuleSet {
            rules: vec![lodestar_store::RuleRow {
                id: 1,
                query: "type:adult".into(),
                action: Action::Hide.code(),
                enabled: true,
                shadow_until: None,
                unless_query: None,
            }],
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
    });
    store
        .set_correction(
            by_own,
            ClassCorrection {
                adult: Some(true),
                not_junk: false,
            },
        )
        .unwrap();
    store
        .mark_unenriched(&[by_rule, by_own, hidden, plain])
        .unwrap();
    let head = store.reindex_head().unwrap();
    let sealer = Sealer::new(&[9; 32]);
    let ctx = Backfill {
        filter: Some(&filter),
        sealer: Some(&sealer),
        quarantine_secs: 86_400,
    };
    let b = enrich_stored_with(&mut store, 0, 10, &ctx)
        .unwrap()
        .unwrap();
    assert!(!b.rejected);

    // Both relabelled torrents with the pair are held by CS-5a: gone from
    // the store (and so the index), quarantined with sealed names, and
    // refused at intake.
    for (id, h) in [(by_rule, h_rule), (by_own, h_own)] {
        assert!(store.torrent(id).unwrap().is_none());
        assert!(store.find(&h).unwrap().is_none());
        assert!(b.held.contains(&h.0));
        let q = store.quarantined(10).unwrap();
        let row = q.iter().find(|q| q.hash == h.0).expect("quarantined");
        assert_eq!(row.rule, "CS-5a");
        assert!(row.sealed.is_some());
    }
    let logged = store.reindex_after(head, 100).unwrap();
    assert!(logged.iter().any(|(_, id)| *id == by_rule));
    assert!(logged.iter().any(|(_, id)| *id == by_own));
    let hits = store.filter_hits(0).unwrap();
    assert_eq!(hits.iter().filter(|h| h.rule == "CS-5a").count(), 2);
    // Relabelled without the pair: stored as adult, and your type:adult
    // rule's mark applies.
    assert_eq!(kind(&store, hidden), Some(ContentType::Adult));
    let f = store.torrent(hidden).unwrap().unwrap().flags;
    assert_ne!(f & flags::HIDDEN, 0);
    // Not relabelled: untouched, though the pair is there (not adult).
    assert!(store.find(&h_plain).unwrap().is_some());
    assert_ne!(kind(&store, plain), Some(ContentType::Adult));
}

#[test]
fn a_stale_generation_write_is_refused_and_the_next_pass_writes() {
    use lodestar_pipeline::enrich::{Backfill, enrich_stored_with};
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let a = add(&mut store, "Holiday.Clips.2023.Lexb.mp4", 900 << 20);
    // Start from a store with enrichment written, then marked again.
    enrich_stored(&mut store, 0, 10).unwrap();
    store.mark_unenriched(&[a]).unwrap();
    let filter = stand_in();
    let ctx = Backfill {
        filter: Some(&filter),
        ..Backfill::default()
    };
    // A rule change between reading the generation and writing: simulated
    // by bumping it before a write with a stale generation.
    let stale = store.class_gen().unwrap();
    store.bump_class_gen().unwrap();
    assert_eq!(
        store.set_enrichments_at(stale, &[], &[]).unwrap(),
        None,
        "refused at the stale generation"
    );
    // The pass itself reads the new generation and writes.
    let b = enrich_stored_with(&mut store, 0, 10, &ctx)
        .unwrap()
        .unwrap();
    assert!(!b.rejected);
    assert_eq!((b.last, b.enriched), (a, 1));
}
