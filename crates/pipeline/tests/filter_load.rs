//! F-filter-load-fail-closed at start: the loaders the pipeline's start
//! runs (`Engine::load_rules` and `Engine::load_class_rules` over the
//! stored rows, after the CS-1 floor from the filter state) read
//! unparseable stored rows as failing closed or skipped, list them, and
//! leave the CS-1 floor as stored.
//!
//! rc.3 backport: v0.5.1 has no shared `load_filter`/`StoreFilter` (they
//! came after the cut), so this replays the start's own steps from
//! `lodestar_pipeline::Pipeline::start` with the same public calls.

use lodestar_filter::load::RowKind;
use lodestar_filter::{
    Action, Core, CoreRule, CsList, Engine, Handle, Lists, Origin, RuleRef, Switches,
};
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
    let over = store
        .add_override(1, b"alpha", 0, Some("size:>>"), 0, None, None)
        .unwrap();
    let exc = store.add_exception("Z-1", 1, b"alpha").unwrap();
    let allow = store.add_allow("size:>>").unwrap();
    let filter = Handle::new(Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )));
    // As the pipeline's start does (`Pipeline::start` in
    // crates/pipeline/src/lib.rs, the `load_filter` and `update_engine`
    // calls after `prepare`): the floor from the filter state, then the
    // stored rules and classification rules. CS-8 signatures and switches
    // aren't stored here, so they're left out.
    let state = store.filter_state().unwrap();
    filter.update(|core| {
        for (hash, origin) in &state.cs_hashes {
            core.cs.add_hash(
                *hash,
                Origin::from_code(*origin).unwrap_or(Origin::Imported),
            );
        }
    });
    let set = store.rule_set().unwrap();
    let class_rules = store.class_rules().unwrap();
    let notes = filter.update_engine(|e| {
        let mut notes = e.load_rules(&set);
        notes.extend(e.load_class_rules(&class_rules));
        notes
    });
    assert_eq!(notes.len(), 6, "{notes:?}");
    let e = filter.current();
    let mut found: Vec<(RowKind, i64)> = e.unreadable.iter().map(|u| (u.kind, u.id)).collect();
    found.sort_by_key(|(k, id)| (k.as_str(), *id));
    assert_eq!(
        found,
        [
            (RowKind::Allow, allow),
            (RowKind::ClassRule, class),
            (RowKind::Exception, exc),
            (RowKind::Override, over),
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
    let closed = reasons.iter().filter(|r| **r == "fails_closed").count();
    assert_eq!(closed, 3, "{reasons:?}");
    assert!(e.overrides.is_empty() && e.exceptions.is_empty() && e.allow.is_empty());
}
