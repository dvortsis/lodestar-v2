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
    use lodestar_filter::{Core, CsList, Engine, Lists, Switches};
    use lodestar_pipeline::enrich::enrich_stored_with;
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
    let mut engine = Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    ));
    engine.load_class_rules(&[(1, "lexb".into(), true)]);
    enrich_stored_with(&mut store, 0, 10, Some(&engine)).unwrap();
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
    engine.load_class_rules(&[(1, "lexb".into(), true), (2, "2023".into(), false)]);
    store.mark_unenriched(&[a]).unwrap();
    enrich_stored_with(&mut store, 0, 10, Some(&engine)).unwrap();
    assert_ne!(kind(&store, a), Some(ContentType::Adult));
}
