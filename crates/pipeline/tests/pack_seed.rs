//! RP-0 at start: the pipeline's start (`prepare`) seeds the pack versions
//! once, before the filter is first read, with SC-1's switch and
//! exceptions copied to SC-2 and the seeding reported on a store that had
//! rules; a second start seeds nothing.

use lodestar_filter::PackRule;
use lodestar_filter::load::STAND_INS_KEY;
use lodestar_filter::packs::upgrade::{APPLIED_KEYS, Applied, SEEDED_KEY};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_store::{Store, StoreConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_start_seeds_pack_versions_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    // As before RP-0: scams on, SC-1 off, an exception to SC-1.
    store.set_pack_switch("scams", true).unwrap();
    store.set_pack_switch("SC-1", false).unwrap();
    store.add_exception("SC-1", 2, b"robux").unwrap();
    let config = PipelineConfig::default();
    let filter = config.filter.clone();
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    // The engine was loaded after seeding: SC-2 has SC-1's switch.
    let e = filter.current();
    assert!(!e.pack_versions.unseeded);
    assert!(!e.packs.is_on(PackRule::Sc2));
    let store = pipeline.shutdown().await.unwrap();
    let rows: Vec<Option<String>> = APPLIED_KEYS
        .iter()
        .map(|k| store.setting_text(k).unwrap())
        .collect();
    assert_eq!(rows[0], rows[1]);
    let rec = Applied::decode(rows[0].as_deref().unwrap()).unwrap();
    assert_eq!(rec.get(PackRule::Sc2), Some(1));
    assert!(store.setting_text(SEEDED_KEY).unwrap().is_some());
    let set = store.rule_set().unwrap();
    assert!(set.pack_switches.contains(&("SC-2".into(), false)));
    assert_eq!(
        set.exceptions.iter().filter(|e| e.rule == "SC-2").count(),
        1
    );
    let report = store.setting_text(STAND_INS_KEY).unwrap().unwrap();
    assert!(report.starts_with("packs.seeded@"), "{report}");
    drop(store);
    // A second start seeds nothing more.
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(PipelineConfig::default(), store, vec![])
        .await
        .unwrap();
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(store.setting_text(STAND_INS_KEY).unwrap(), Some(report));
    assert_eq!(store.rule_set().unwrap().exceptions.len(), 2);
}
