//! Rules, pack switches, overrides, exceptions and *Always allow* (§20.7).

use lodestar_store::{Store, StoreConfig};

#[test]
fn rule_set_round_trips() {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    assert!(s.rule_set().unwrap().rules.is_empty());
    let a = s.add_rule("cam", 2, None, Some("year:<1990")).unwrap();
    let b = s.add_rule("ts", 1, Some(99), None).unwrap();
    s.set_pack_switch("spam", false).unwrap();
    s.set_pack_switch("spam", true).unwrap();
    let o = s
        .add_override(1, b"alpha", 1, Some("type:adult"), 10, Some(20), None)
        .unwrap();
    let e = s.add_exception("CS-4a", 0, &[7; 20]).unwrap();
    let w = s.add_allow("group:x").unwrap();
    let set = s.rule_set().unwrap();
    assert_eq!(set.rules.iter().map(|r| r.id).collect::<Vec<_>>(), [a, b]);
    assert_eq!(set.rules[0].unless_query.as_deref(), Some("year:<1990"));
    assert_eq!(set.pack_switches, vec![("spam".to_owned(), true)]);
    assert_eq!(set.overrides[0].scope.as_deref(), Some("type:adult"));
    assert_eq!(set.exceptions[0].rule, "CS-4a");
    assert_eq!(set.allow, vec![(w, "group:x".to_owned())]);

    let mut r = set.rules[1].clone();
    r.enabled = false;
    assert!(s.update_rule(&r).unwrap());
    assert!(!s.rule_set().unwrap().rules[1].enabled);
    for removed in [
        s.remove_rule(a).unwrap(),
        s.remove_override(o).unwrap(),
        s.remove_exception(e).unwrap(),
        s.remove_allow(w).unwrap(),
    ] {
        assert!(removed);
    }
    assert!(!s.remove_rule(a).unwrap());
    let f = s.add_fixture("U-2", false, &[1, 2, 3]).unwrap();
    assert_eq!(s.fixtures("U-2").unwrap(), vec![(f, false, vec![1, 2, 3])]);
    assert_eq!(s.previous().unwrap(), None);
    s.save_previous(b"v1").unwrap();
    s.save_previous(b"v2").unwrap();
    assert_eq!(s.previous().unwrap().as_deref(), Some(&b"v2"[..]));
}
