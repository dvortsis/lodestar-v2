//! F-filter-load-fail-closed at start: the one loader the server and
//! `lodestar import` share reads unparseable stored rows as failing
//! closed or skipped, lists them, and leaves the CS-1 floor as stored.

use lodestar_filter::load::RowKind;
use lodestar_filter::{Action, Core, CoreRule, CsList, Engine, Handle, Lists, RuleRef, Switches};
use lodestar_pipeline::{StoreFilter, load_filter};
use lodestar_store::{Store, StoreConfig};

const CS1: [u8; 20] = [0xC5; 20];

#[test]
fn unparseable_rows_fail_closed_at_start_and_the_floor_stays() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    store.add_cs_hashes(&[CS1], 0, 1).unwrap();
    let rule = store
        .add_rule("size:>>", Action::Drop.code(), None, Some("year:<1990"))
        .unwrap();
    let unless = store
        .add_rule("cam", Action::Hide.code(), None, Some("size:>>"))
        .unwrap();
    let class = store.add_class_rule("size:>>", true).unwrap();
    let filter = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )));
    let notes = load_filter(&filter, &StoreFilter::read(&store).unwrap());
    assert_eq!(notes.len(), 3, "{notes:?}");
    let e = filter.current();
    let mut found: Vec<(RowKind, i64)> = e.unreadable.iter().map(|u| (u.kind, u.id)).collect();
    found.sort_by_key(|(k, id)| (k.as_str(), *id));
    assert_eq!(
        found,
        [
            (RowKind::ClassRule, class),
            (RowKind::Rule, rule),
            (RowKind::Unless, unless)
        ]
    );
    assert!(e.rules.iter().all(|r| r.action >= Action::Hide));
    assert!(
        e.rules
            .iter()
            .all(|r| i64::from(r.id) != unless || r.unless.is_none())
    );
    let hit = e.blocked_hash(&[CS1]).expect("the floor still blocks");
    assert_eq!(hit.rule, RuleRef::Core(CoreRule::Cs1a));
    // The pipeline logs each note by kind and id only.
    let reasons: Vec<_> = notes
        .iter()
        .map(|n| lodestar_pipeline::log_events::filter_note(n).2)
        .collect();
    assert_eq!(reasons, ["fails_closed"; 3]);
}
