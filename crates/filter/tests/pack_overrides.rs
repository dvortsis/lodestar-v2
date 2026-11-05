//! RP-2's engine gates (plan 2026-11-04 §4.3): a pack-rule override
//! changes the rule's action or adds an `unless`, never what it matches; a
//! base version change keeps strengthening overrides and pauses weakening
//! ones; an unreadable row fails closed (the stronger action kept, the
//! `unless` dropped) and is listed; and the layers above the packs still
//! decide first.

use std::sync::Arc;

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::load::{KIND_HASHES, RowKind, StandIns};
use lodestar_filter::packs::Ctx as PackCtx;
use lodestar_filter::packs::manifest::{Code, Direction, Manifest, RuleEntry, Version};
use lodestar_filter::packs::overrides::{PackOverride, label};
use lodestar_filter::packs::upgrade::{self, FRESH_KEY, UNKNOWN};
use lodestar_filter::{
    Action, Core, CsList, Engine, Facts, Item, Lists, PackRule, RuleRef, Switches, TermSet,
};
use lodestar_store::{ExceptionRow, OverrideRow, RuleSet, Store, StoreConfig};

const MW2: RuleRef = RuleRef::Pack(PackRule::Mw2);

/// A store an earlier build set up, with packs on and the versions of
/// `manifest`'s first versions applied.
fn store(applied: &str) -> Store {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    s.write_pack_versions([Some(applied), Some(applied)], Some(("malware", true)))
        .unwrap();
    s
}

fn builtin_record() -> String {
    upgrade::current_record(&Manifest::builtin()).encode()
}

fn engine(manifest: &Arc<Manifest>, set: &RuleSet) -> (Engine, Vec<String>) {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut e = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
    e.manifest = manifest.clone();
    let notes = e.load_rules(set);
    (e, notes)
}

fn load(manifest: &Arc<Manifest>, store: &Store) -> Engine {
    let (e, notes) = engine(manifest, &store.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    e
}

/// A movie with an executable that isn't a lure name: MW-2 alone holds it.
const A: (&str, &[(&str, u64)]) = (
    "Some Movie 2019 1080p",
    &[("Some.Movie.2019.mkv", 2 << 30), ("bonus.exe", 1 << 20)],
);
/// Another one, with a name `unless "some"` doesn't match.
const B: (&str, &[(&str, u64)]) = (
    "Other Movie 2020 1080p",
    &[("Other.Movie.2020.mkv", 2 << 30), ("extra.exe", 1 << 20)],
);
/// A plain movie nothing holds.
const C: (&str, &[(&str, u64)]) = ("Third Movie 2021 1080p", &[("Third.mkv", 2 << 30)]);

fn decide(e: &Engine, t: (&str, &[(&str, u64)])) -> Option<(RuleRef, Action)> {
    let f: Vec<FileInfo> =
        t.1.iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
            .collect();
    let class = classify(t.0, &f);
    let facts = Facts {
        class: &class,
        size: f.iter().map(|x| x.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[[7; 20]],
        name: t.0,
        files: &f,
        content_type: class.content_type,
    };
    let v = e.judge(&item, Some(&facts), 0);
    v.decision.map(|d| (d.rule, d.action))
}

fn row(base: u32, action: Action, unless: Option<&str>) -> String {
    PackOverride {
        base,
        action,
        unless: unless.map(str::to_owned),
    }
    .encode()
}

fn v(version: u32, direction: Direction, action: Action, code: Code) -> Version {
    Version {
        version,
        direction,
        action,
        code: Some(code),
    }
}

fn real_mw2(c: &PackCtx<'_>) -> bool {
    c.matches(PackRule::Mw2)
}

/// MW-2 at version 2 matching the same with `action`, version 1 (hide)
/// kept.
fn mw2_v2(direction: Direction, action: Action) -> Arc<Manifest> {
    let b = Manifest::builtin();
    let mut entries: Vec<RuleEntry> = PackRule::ALL
        .into_iter()
        .filter(|r| *r != PackRule::Mw2)
        .map(|r| b.entry(r).clone())
        .collect();
    entries.push(RuleEntry::new(
        PackRule::Mw2,
        true,
        vec![
            v(1, Direction::Widens, Action::Hide, Code::Fn(real_mw2)),
            v(2, direction, action, Code::Fn(real_mw2)),
        ],
    ));
    Arc::new(Manifest::new(entries).unwrap())
}

#[test]
fn an_override_changes_the_action_never_what_the_rule_matches() {
    let m = Manifest::builtin();
    let mut s = store(&builtin_record());
    assert_eq!(decide(&load(&m, &s), A), Some((MW2, Action::Hide)));
    for action in [Action::Flag, Action::Demote, Action::Drop] {
        s.set_pack_override("MW-2", Some(&row(1, action, None)))
            .unwrap();
        let e = load(&m, &s);
        assert_eq!(decide(&e, A), Some((MW2, action)));
        assert_eq!(decide(&e, C), None, "{action:?} holds nothing new");
        let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
        assert!(!o.changed && !o.paused && !o.unreadable());
    }
    // An `unless` keeps the rule from applying where it matches, as it is.
    s.set_pack_override("MW-2", Some(&row(1, Action::Flag, Some("some"))))
        .unwrap();
    let e = load(&m, &s);
    assert_eq!(decide(&e, A), None);
    assert_eq!(decide(&e, B), Some((MW2, Action::Flag)));
    assert_eq!(decide(&e, C), None);
    // Removed: the rule as built.
    s.set_pack_override("MW-2", None).unwrap();
    assert_eq!(decide(&load(&m, &s), A), Some((MW2, Action::Hide)));
}

#[test]
fn an_override_of_a_rule_switched_off_does_nothing() {
    let m = Manifest::builtin();
    let mut s = store(&builtin_record());
    s.set_pack_switch("MW-2", false).unwrap();
    s.set_pack_override("MW-2", Some(&row(1, Action::Drop, None)))
        .unwrap();
    assert_eq!(decide(&load(&m, &s), A), None);
}

#[test]
fn the_layers_above_the_packs_still_decide_first() {
    let m = Manifest::builtin();
    let mut s = store(&builtin_record());
    s.set_pack_override("MW-2", Some(&row(1, Action::Drop, None)))
        .unwrap();
    let base = s.rule_set().unwrap();
    // An exception to the rule releases from it.
    let set = RuleSet {
        exceptions: vec![ExceptionRow {
            id: 1,
            rule: "MW-2".into(),
            kind: KIND_HASHES,
            value: vec![7; 20],
        }],
        ..base.clone()
    };
    assert_eq!(decide(&engine(&m, &set).0, A), None);
    // *Always allow* skips the packs.
    let set = RuleSet {
        allow: vec![(1, "some".into())],
        ..base.clone()
    };
    assert_eq!(decide(&engine(&m, &set).0, A), None);
    // A CSX-5 Override entry passes everything below the floor.
    let set = RuleSet {
        overrides: vec![OverrideRow {
            id: 1,
            kind: KIND_HASHES,
            value: vec![7; 20],
            strength: 0,
            scope: None,
            created_at: 0,
            expires_at: None,
            shadow_until: None,
            hits: 0,
        }],
        ..base.clone()
    };
    assert_eq!(decide(&engine(&m, &set).0, A), None);
    // An exception to MW-2 doesn't lift another pack rule's decision, and
    // the strongest pack action still wins: MW-2 lowered to flag under a
    // rule of yours that hides keeps yours.
    let set = RuleSet {
        rules: vec![lodestar_store::RuleRow {
            id: 1,
            query: "some".into(),
            action: Action::Hide.code(),
            enabled: true,
            shadow_until: None,
            unless_query: None,
        }],
        pack_overrides: vec![("MW-2".into(), Ok(row(1, Action::Flag, None)))],
        ..base
    };
    assert_eq!(
        decide(&engine(&m, &set).0, A),
        Some((RuleRef::User(1), Action::Hide))
    );
}

#[test]
fn a_base_change_keeps_strengthening_overrides_and_pauses_weakening_ones() {
    let v1 = builtin_record();
    // Version 2 drops what version 1 hid.
    let m = mw2_v2(Direction::ActionHeavier, Action::Drop);
    let cases: [(Action, Option<&str>, Option<Action>, bool); 4] = [
        // Lighter than version 1: weakens it, and the new one too.
        (Action::Demote, None, Some(Action::Drop), true),
        // Equal to version 1 (no password then), lighter than version 2.
        (Action::Hide, None, Some(Action::Drop), true),
        // Any `unless` weakens: paused, the rule applies as built.
        (Action::Drop, Some("third"), Some(Action::Drop), true),
        // Heavier than both: keeps applying.
        (Action::Drop, None, Some(Action::Drop), false),
    ];
    for (action, unless, after, paused) in cases {
        let mut s = store(&v1);
        s.set_pack_override("MW-2", Some(&row(1, action, unless)))
            .unwrap();
        let e = load(&m, &s);
        assert!(e.pack_versions.state(PackRule::Mw2).pending());
        let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
        assert!(!o.changed && !o.paused, "{action:?}: bound to v1, applied");
        let before = decide(&e, B);
        assert_eq!(before, Some((MW2, action)), "{action:?} applies on v1");
        upgrade::accept(&mut s, &m, &[PackRule::Mw2], 10).unwrap();
        let e = load(&m, &s);
        let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
        assert!(o.changed, "flagged: changed since you edited");
        assert_eq!(o.paused, paused, "{action:?} {unless:?}");
        assert_eq!(decide(&e, B).map(|d| d.1), after, "{action:?} {unless:?}");
        assert_eq!(decide(&e, C), None);
    }
    // Version 2 lighter (demote): a hide override, heavier than the new
    // base, keeps applying; a flag one pauses.
    let m = mw2_v2(Direction::ActionLighter, Action::Demote);
    for (action, after, paused) in [
        (Action::Hide, Action::Hide, false),
        (Action::Flag, Action::Demote, true),
    ] {
        let mut s = store(&v1);
        s.set_pack_override("MW-2", Some(&row(1, action, None)))
            .unwrap();
        upgrade::accept(&mut s, &m, &[PackRule::Mw2], 10).unwrap();
        let e = load(&m, &s);
        let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
        assert!(o.changed);
        assert_eq!(o.paused, paused, "{action:?}");
        assert_eq!(decide(&e, A), Some((MW2, after)), "{action:?}");
    }
}

#[test]
fn a_version_state_in_doubt_counts_as_changed_and_never_drops_unaccepted() {
    let m = mw2_v2(Direction::ActionHeavier, Action::Drop);
    let mut s = store(&builtin_record());
    // Both applied rows lost: the interim mode, nothing accepted.
    s.update_settings_text(&[], &upgrade::APPLIED_KEYS).unwrap();
    s.set_pack_override("MW-2", Some(&row(1, Action::Flag, None)))
        .unwrap();
    s.set_pack_override("MW-1", Some(&row(UNKNOWN, Action::Drop, None)))
        .unwrap();
    let e = load(&m, &s);
    let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
    assert!(o.changed && o.paused);
    // The interim mode's own result: version 2's drop held at quarantine.
    assert_eq!(decide(&e, A), Some((MW2, Action::Quarantine)));
    let o = e.pack_overrides.get(PackRule::Mw1).unwrap();
    assert!(o.changed && !o.paused);
}

#[test]
fn unreadable_rows_fail_closed_and_are_listed() {
    let m = Manifest::builtin();
    let s = store(&builtin_record());
    let base = s.rule_set().unwrap();
    let with = |value: Result<String, Vec<u8>>| RuleSet {
        pack_overrides: vec![("MW-2".into(), value)],
        ..base.clone()
    };
    // (value, decision on A, on B)
    let cases: Vec<(Result<String, Vec<u8>>, Option<Action>, Option<Action>)> = vec![
        // A lighter action with an `unless` that doesn't read: the stronger
        // (the rule's own) applies, everywhere.
        (
            Ok("1\n1\ndemote\nsize:>>".into()),
            Some(Action::Hide),
            Some(Action::Hide),
        ),
        // A heavier action is kept, its `unless` dropped.
        (
            Ok("1\n1\ndrop\nsize:>>".into()),
            Some(Action::Drop),
            Some(Action::Drop),
        ),
        // A base that doesn't read: the stronger action, no `unless`.
        (
            Ok("1\nx\nflag\nsome".into()),
            Some(Action::Hide),
            Some(Action::Hide),
        ),
        (
            Ok("1\nx\ndrop\nsome".into()),
            Some(Action::Drop),
            Some(Action::Drop),
        ),
        // An action this build doesn't know, a format it doesn't, bytes:
        // the rule as built.
        (
            Ok("1\n1\nsmite".into()),
            Some(Action::Hide),
            Some(Action::Hide),
        ),
        (
            Ok("9\n1\nflag\nsome".into()),
            Some(Action::Hide),
            Some(Action::Hide),
        ),
        (Err(vec![0xff, 0]), Some(Action::Hide), Some(Action::Hide)),
        (Ok(String::new()), Some(Action::Hide), Some(Action::Hide)),
    ];
    for (value, a, b) in cases {
        let (e, notes) = engine(&m, &with(value.clone()));
        assert_eq!(decide(&e, A).map(|d| d.1), a, "{value:?}");
        assert_eq!(decide(&e, B).map(|d| d.1), b, "{value:?}");
        assert_eq!(decide(&e, C), None, "{value:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(e.unreadable.len(), 1);
        let u = &e.unreadable[0];
        assert_eq!(u.kind, RowKind::PackOverride);
        assert!(u.fails_closed);
        assert_eq!(u.label(), label(PackRule::Mw2));
        assert_eq!(u.label(), "packs.override.MW-2");
        // Recorded with the pack conditions by the API, not as a stand-in
        // whose holds are re-judged.
        let (record, recovered) = StandIns::default().reconcile(&e.unreadable, 5);
        assert!(record.0.is_empty() && recovered.is_empty());
    }
    // Fixed: no longer listed.
    let (e, notes) = engine(&m, &with(Ok(row(1, Action::Flag, None))));
    assert!(notes.is_empty() && e.unreadable.is_empty());
    assert_eq!(decide(&e, A), Some((MW2, Action::Flag)));
}

#[test]
fn unknown_rules_are_kept_and_ignored() {
    let m = Manifest::builtin();
    let mut s = store(&builtin_record());
    s.set_pack_override("XX-9", Some("1\n1\ndrop")).unwrap();
    let e = load(&m, &s);
    assert!(e.pack_overrides.is_empty() && e.unreadable.is_empty());
    assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
    assert_eq!(s.rule_set().unwrap().pack_overrides.len(), 1);
}
