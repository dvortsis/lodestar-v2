//! RP-0 at start: the pipeline's start (`prepare`) seeds the pack versions
//! once, before the filter is first read, with SC-1's switch and
//! exceptions copied to SC-2 and the seeding reported on a store that had
//! rules; a second start seeds nothing.
//!
//! RP-DEF at start: a store created on this start (a fresh data
//! directory) has every pack on, with every rule at its current version,
//! at the first filter read, also after a crash between creating the store
//! and seeding it; a store from before keeps every pack's state.

use lodestar_filter::PackRule;
use lodestar_filter::load::STAND_INS_KEY;
use lodestar_filter::packs::manifest::Manifest;
use lodestar_filter::packs::upgrade::{
    APPLIED_KEYS, Applied, DEFAULTS_KEY, FRESH_KEY, SEEDED_KEY, current_record,
};
use lodestar_pipeline::{Pipeline, PipelineConfig};
use lodestar_store::{Store, StoreConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_start_seeds_pack_versions_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    // A store from before RP-DEF: its creating migration wrote no marker.
    store.update_settings_text(&[], &[FRESH_KEY]).unwrap();
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
    let set = store.rule_set().unwrap();
    assert!(set.pack_switches.contains(&("SC-2".into(), false)));
    assert_eq!(
        set.exceptions.iter().filter(|e| e.rule == "SC-2").count(),
        1
    );
    // Reported in the marker: the store had rules.
    let report = store.setting_text(SEEDED_KEY).unwrap().unwrap();
    assert!(report.starts_with("rules@"), "{report}");
    assert_eq!(store.setting_text(STAND_INS_KEY).unwrap(), None);
    drop(store);
    // A second start seeds nothing more.
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(PipelineConfig::default(), store, vec![])
        .await
        .unwrap();
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(store.setting_text(SEEDED_KEY).unwrap(), Some(report));
    assert_eq!(store.rule_set().unwrap().exceptions.len(), 2);
}

/// What the engine has at the first filter read after `start`: every pack
/// on, every rule applying its current version, nothing pending.
fn every_pack_on(e: &lodestar_filter::Engine) {
    let m = Manifest::builtin();
    for r in PackRule::ALL {
        assert!(e.packs.is_on(r), "{}", r.id());
        let st = e.pack_versions.state(r);
        assert_eq!(st.apply, [m.entry(r).current().version], "{}", r.id());
        assert!(!st.pending(), "{}", r.id());
    }
    assert!(e.pack_versions.conditions.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_store_has_every_pack_on_at_the_first_filter_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let config = PipelineConfig::default();
    let filter = config.filter.clone();
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    every_pack_on(&filter.current());
    let store = pipeline.shutdown().await.unwrap();
    let rec = store.setting_text(APPLIED_KEYS[0]).unwrap().unwrap();
    assert_eq!(
        Applied::decode(&rec).unwrap(),
        current_record(&Manifest::builtin())
    );
    assert_eq!(
        store.setting_text(SEEDED_KEY).unwrap().as_deref(),
        Some("1")
    );
    assert_eq!(
        store.setting_text(DEFAULTS_KEY).unwrap().as_deref(),
        Some("1")
    );
    assert_eq!(store.setting_text(FRESH_KEY).unwrap(), None);
    // A pack turned off stays off on the next start.
    let mut store = store;
    store.set_pack_switch("spam", false).unwrap();
    drop(store);
    let config = PipelineConfig::default();
    let filter = config.filter.clone();
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    assert!(!filter.current().packs.is_on(PackRule::Sp1));
    assert!(filter.current().packs.is_on(PackRule::Mw1));
    pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_after_create_before_seed_is_still_seeded_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    // Created, then the process stops before the pipeline ever starts.
    drop(Store::open(&db, StoreConfig::default()).unwrap());
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    assert_eq!(store.setting_text(FRESH_KEY).unwrap().as_deref(), Some("1"));
    let config = PipelineConfig::default();
    let filter = config.filter.clone();
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    every_pack_on(&filter.current());
    pipeline.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upgraded_store_keeps_every_packs_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let mut store = Store::open(&db, StoreConfig::default()).unwrap();
    store.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    store.set_pack_switch("malware", true).unwrap();
    let config = PipelineConfig::default();
    let filter = config.filter.clone();
    let pipeline = Pipeline::start(config, store, vec![]).await.unwrap();
    let e = filter.current();
    assert!(e.packs.is_on(PackRule::Mw1));
    for r in [PackRule::Lu1, PackRule::Fk1, PackRule::Sp1, PackRule::Sc1] {
        assert!(!e.packs.is_on(r), "{}", r.id());
    }
    let store = pipeline.shutdown().await.unwrap();
    let set = store.rule_set().unwrap();
    assert!(set.pack_switches.contains(&("malware".into(), true)));
    for p in ["lures", "fakes", "spam", "scams"] {
        assert!(set.pack_switches.contains(&(p.into(), false)), "{p}");
    }
    assert_eq!(
        store.setting_text(DEFAULTS_KEY).unwrap().as_deref(),
        Some("1")
    );
}
