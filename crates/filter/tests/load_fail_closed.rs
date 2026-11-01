//! A stored row that no longer reads never loosens filtering
//! (docs/decisions/2026-11-01-F-filter-load-fail-closed.md): for each
//! kind, the engine loaded with the row unreadable is never looser than
//! with it readable, the row is listed for the health exception, the
//! locked floor is untouched, and a fixed row clears the listing.

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::load::{KIND_KEYWORD, RowKind};
use lodestar_filter::{
    Action, Core, CoreRule, CsList, Engine, Facts, Item, Lists, Origin, PackSwitches, RuleRef,
    Switches, TermSet,
};
use lodestar_store::{ExceptionRow, OverrideRow, RuleRow, RuleSet};

/// Doesn't parse in this version (stands in for newer syntax).
const BAD: &str = "size:>>";
const CS1_HASH: [u8; 20] = [0xC5; 20];

fn engine() -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut e = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
    e.core.cs.add_hash(CS1_HASH, Origin::Imported);
    e.packs = PackSwitches::default();
    e
}

const NAMES: &[&str] = &[
    "Some Movie 2019 CAM x264",
    "Other Movie 2021 1080p",
    "Studiox Scene One",
    "goodgroup Release 2020",
    "alpha beta documentary",
    "Rolea one two Lexa",
];

/// How much a decision holds back: withheld (quarantine or drop) counts
/// the same, since an unreadable drop rule quarantines instead.
fn rank(a: Option<Action>) -> u8 {
    match a {
        None => 0,
        Some(Action::Flag) => 1,
        Some(Action::Demote) => 2,
        Some(Action::Hide) => 3,
        Some(Action::Quarantine | Action::Drop) => 4,
    }
}

/// The decision and classification correction for `name`.
fn judge(e: &Engine, name: &str, hash: [u8; 20]) -> (Option<(RuleRef, Action)>, Option<bool>) {
    let f = vec![FileInfo::new(vec![format!("{name}.mkv")], 900 << 20)];
    let class = classify(name, &f);
    let facts = Facts {
        class: &class,
        size: 900 << 20,
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 1000,
        now: 1000,
    };
    let hashes = [hash];
    let item = Item {
        hashes: &hashes,
        name,
        files: &f,
        content_type: class.content_type,
    };
    let (v, c) = e.judge_classified(&item, Some(&facts), 1000);
    (v.decision.map(|d| (d.rule, d.action)), c)
}

fn load(set: &RuleSet, class: &[(i64, String, bool)]) -> Engine {
    let mut e = engine();
    e.load_rules(set);
    e.load_class_rules(class);
    e
}

fn rule(id: i64, query: &str, action: Action, unless: Option<&str>) -> RuleRow {
    RuleRow {
        id,
        query: query.into(),
        action: action.code(),
        enabled: true,
        shadow_until: None,
        unless_query: unless.map(Into::into),
    }
}

/// `broken` is never looser than `good` on any name, and marks adult at
/// least wherever `good` does.
fn assert_not_looser(good: &Engine, broken: &Engine) {
    for name in NAMES {
        let (g, gc) = judge(good, name, [1; 20]);
        let (b, bc) = judge(broken, name, [1; 20]);
        assert!(
            rank(b.map(|d| d.1)) >= rank(g.map(|d| d.1)),
            "{name}: {g:?} readable, {b:?} unreadable"
        );
        if gc == Some(true) {
            assert_eq!(bc, Some(true), "{name}: adult when readable");
        }
    }
}

/// The floor decides a CS-1 hash first, whatever else is loaded.
fn assert_floor_intact(e: &Engine) {
    for name in NAMES {
        let (d, _) = judge(e, name, CS1_HASH);
        assert_eq!(d, Some((RuleRef::Core(CoreRule::Cs1b), Action::Drop)));
    }
    let hit = e
        .blocked_hash(&[CS1_HASH])
        .expect("blocked before fetching");
    assert_eq!(hit.rule, RuleRef::Core(CoreRule::Cs1b));
    assert_eq!(e.core.cs.len(), 1);
}

fn kinds(e: &Engine) -> Vec<(RowKind, i64)> {
    e.unreadable.iter().map(|u| (u.kind, u.id)).collect()
}

#[test]
fn an_unreadable_withholding_rule_quarantines_rather_than_vanishes() {
    for action in [Action::Hide, Action::Quarantine, Action::Drop] {
        let good = load(
            &RuleSet {
                rules: vec![rule(1, "cam", action, None)],
                ..RuleSet::default()
            },
            &[],
        );
        let set = RuleSet {
            rules: vec![rule(1, &format!("cam {BAD}"), action, None)],
            ..RuleSet::default()
        };
        let broken = load(&set, &[]);
        assert_not_looser(&good, &broken);
        assert_eq!(
            judge(&broken, "alpha beta documentary", [1; 20]).0,
            Some((RuleRef::User(1), Action::Quarantine)),
            "fails closed for everything the rule could match"
        );
        assert_eq!(kinds(&broken), vec![(RowKind::Rule, 1)]);
        assert_floor_intact(&broken);
    }
}

#[test]
fn an_unreadable_rule_keeps_its_unless_and_its_releases() {
    let set = |q: &str| RuleSet {
        rules: vec![rule(1, q, Action::Drop, Some("year:<2020"))],
        exceptions: vec![ExceptionRow {
            id: 2,
            rule: "U-1".into(),
            kind: KIND_KEYWORD,
            value: b"goodgroup".to_vec(),
        }],
        ..RuleSet::default()
    };
    let good = load(&set("movie"), &[]);
    let broken = load(&set(&format!("movie {BAD}")), &[]);
    assert_not_looser(&good, &broken);
    // The real rule couldn't have matched these, nor can its stand-in.
    assert_eq!(judge(&broken, "Some Movie 2019 CAM x264", [1; 20]).0, None);
    assert_eq!(judge(&broken, "goodgroup Release 2020", [1; 20]).0, None);
    assert!(
        judge(&broken, "Other Movie 2021 1080p", [1; 20])
            .0
            .is_some()
    );
}

#[test]
fn an_unknown_action_quarantines_within_the_query() {
    let mut row = rule(1, "cam", Action::Hide, None);
    let good = load(
        &RuleSet {
            rules: vec![row.clone()],
            ..RuleSet::default()
        },
        &[],
    );
    row.action = 9;
    let broken = load(
        &RuleSet {
            rules: vec![row],
            ..RuleSet::default()
        },
        &[],
    );
    assert_not_looser(&good, &broken);
    let (cam, _) = judge(&broken, "Some Movie 2019 CAM x264", [1; 20]);
    assert_eq!(cam, Some((RuleRef::User(1), Action::Quarantine)));
    assert_eq!(judge(&broken, "Other Movie 2021 1080p", [1; 20]).0, None);
    assert_eq!(kinds(&broken), vec![(RowKind::Rule, 1)]);
}

#[test]
fn unreadable_rules_that_withhold_nothing_are_skipped_but_listed() {
    let mut off = rule(1, BAD, Action::Drop, None);
    off.enabled = false;
    let set = RuleSet {
        rules: vec![off, rule(2, BAD, Action::Flag, None)],
        ..RuleSet::default()
    };
    let e = load(&set, &[]);
    assert!(e.rules.is_empty());
    assert_eq!(kinds(&e), vec![(RowKind::Rule, 1), (RowKind::Rule, 2)]);
}

#[test]
fn an_unreadable_unless_makes_its_rule_stricter() {
    let good = load(
        &RuleSet {
            rules: vec![rule(1, "movie", Action::Hide, Some("year:<2020"))],
            ..RuleSet::default()
        },
        &[],
    );
    let broken = load(
        &RuleSet {
            rules: vec![rule(1, "movie", Action::Hide, Some(BAD))],
            ..RuleSet::default()
        },
        &[],
    );
    assert_not_looser(&good, &broken);
    let (d, _) = judge(&broken, "Some Movie 2019 CAM x264", [1; 20]);
    assert_eq!(d, Some((RuleRef::User(1), Action::Hide)), "no exception");
    assert_eq!(kinds(&broken), vec![(RowKind::Unless, 1)]);
}

#[test]
fn an_unreadable_adult_class_rule_marks_everything_adult() {
    let good = load(&RuleSet::default(), &[(1, "studiox".into(), true)]);
    let broken = load(
        &RuleSet::default(),
        &[
            (1, format!("studiox {BAD}"), true),
            (2, BAD.into(), false),
            // Newer and readable: still decides what it matches.
            (3, "goodgroup".into(), false),
        ],
    );
    assert_not_looser(&good, &broken);
    assert_eq!(
        judge(&broken, "alpha beta documentary", [1; 20]).1,
        Some(true)
    );
    assert_eq!(
        judge(&broken, "goodgroup Release 2020", [1; 20]).1,
        Some(false)
    );
    assert_eq!(
        kinds(&broken),
        vec![(RowKind::ClassRule, 1), (RowKind::ClassRule, 2)]
    );
    assert_floor_intact(&broken);
}

#[test]
fn unreadable_releases_are_skipped_and_listed() {
    let withhold = rule(1, "movie", Action::Drop, None);
    let set = |scope: &str, rule: &str, allow: &str| RuleSet {
        rules: vec![withhold.clone()],
        overrides: vec![OverrideRow {
            id: 4,
            kind: KIND_KEYWORD,
            value: b"other".to_vec(),
            strength: 0,
            scope: Some(scope.into()),
            created_at: 0,
            expires_at: None,
            shadow_until: None,
            hits: 0,
        }],
        exceptions: vec![ExceptionRow {
            id: 5,
            rule: rule.into(),
            kind: KIND_KEYWORD,
            value: b"cam".to_vec(),
        }],
        allow: vec![(6, allow.into())],
        ..RuleSet::default()
    };
    let good = load(&set("movie", "U-1", "goodgroup"), &[]);
    let broken = load(&set(BAD, "Z-1", BAD), &[]);
    assert_not_looser(&good, &broken);
    assert!(broken.overrides.is_empty() && broken.exceptions.is_empty());
    assert!(broken.allow.is_empty());
    assert_eq!(
        kinds(&broken),
        vec![
            (RowKind::Override, 4),
            (RowKind::Exception, 5),
            (RowKind::Allow, 6)
        ]
    );
    assert!(broken.unreadable.iter().all(|u| !u.kind.loosening()));
    assert_floor_intact(&broken);
}

#[test]
fn fixing_a_row_clears_it_and_reloads_keep_when_it_was_found() {
    let broken = RuleSet {
        rules: vec![rule(1, BAD, Action::Hide, None)],
        allow: vec![(6, BAD.into())],
        ..RuleSet::default()
    };
    let class = [(1, BAD.to_owned(), true)];
    let mut e = engine();
    e.load_rules(&broken);
    e.load_class_rules(&class);
    let first = e.unreadable.clone();
    assert_eq!(first.len(), 3);
    // Reloading one layer keeps the other's rows, and every first-seen time.
    e.load_rules(&broken);
    assert_eq!(e.unreadable.len(), 3);
    for u in &e.unreadable {
        assert!(first.contains(u), "{u:?}");
    }
    e.load_rules(&RuleSet {
        rules: vec![rule(1, "cam", Action::Hide, None)],
        allow: vec![(6, "goodgroup".into())],
        ..RuleSet::default()
    });
    assert_eq!(kinds(&e), vec![(RowKind::ClassRule, 1)]);
    e.load_class_rules(&[(1, "studiox".into(), true)]);
    assert!(e.unreadable.is_empty());
    assert_eq!(judge(&e, "alpha beta documentary", [1; 20]).0, None);
    assert_floor_intact(&e);
}
