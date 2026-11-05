//! RP-0's gates (plan 2026-11-04 §4.3): pack versions seeded once, lost
//! or unreadable state failing closed without seeding again, changed and
//! new rules pending in shadow until accepted, off → on recording current
//! versions and nothing else accepting, the interim mode never dropping
//! what only an unaccepted version holds, downgrades, and unknown entries
//! kept and ignored.

use std::sync::Arc;

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::packs::Ctx as PackCtx;
use lodestar_filter::packs::Pack;
use lodestar_filter::packs::manifest::{Code, Direction, Manifest, RuleEntry, Version};
use lodestar_filter::packs::upgrade::{
    self, APPLIED_KEYS, Condition, DEFAULTS_KEY, FRESH_KEY, PackShadow, SEEDED_KEY, Side,
    acceptance_strengthens,
};
use lodestar_filter::{
    Action, Core, CsList, Engine, Facts, Item, Lists, PackRule, RuleRef, Switches, TermSet,
};
use lodestar_store::{RuleSet, Store, StoreConfig};

/// A store as an earlier build left it: created before RP-DEF, so
/// without the fresh marker its creating migration now writes.
/// The built-in manifest with every rule at its first version: MW-2 and
/// FK-2 cut back to the version 1 this build keeps (RP-1b ships their
/// version 2). These tests are about how versions are recorded, seeded
/// and accepted, which they pin from a build with every rule at 1; the
/// real build's MW-2@2 and FK-2@2 upgrade path is pinned in
/// `packs::upgrade`'s `unseeded_and_seeded_resolve_to_today`.
fn baseline() -> Arc<Manifest> {
    let b = Manifest::builtin();
    let entries = PackRule::ALL
        .into_iter()
        .map(|r| {
            let e = b.entry(r);
            let first = e.versions()[0].clone();
            assert!(first.code.is_some(), "{} keeps its version 1", r.id());
            RuleEntry::new(r, e.baseline, vec![first])
        })
        .collect();
    Arc::new(Manifest::new(entries).unwrap())
}

fn store() -> Store {
    let mut s = fresh_store();
    s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    s
}

/// A store created on this start (RP-DEF's fresh data directory).
fn fresh_store() -> Store {
    Store::open_in_memory(StoreConfig::default()).unwrap()
}

fn engine(manifest: &Arc<Manifest>, set: &RuleSet) -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut e = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
    e.manifest = manifest.clone();
    let notes = e.load_rules(set);
    assert!(notes.is_empty(), "{notes:?}");
    e
}

fn load(manifest: &Arc<Manifest>, store: &Store) -> Engine {
    engine(manifest, &store.rule_set().unwrap())
}

/// What the engine decides about a torrent, and its pending pack
/// versions' shadow results.
fn judge(
    e: &Engine,
    name: &str,
    paths: &[(&str, u64)],
) -> (Option<(RuleRef, Action)>, Vec<PackShadow>) {
    let f: Vec<FileInfo> = paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect();
    let class = classify(name, &f);
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
        name,
        files: &f,
        content_type: class.content_type,
    };
    let (v, shadow) = e.judge_shadowed(&item, Some(&facts), 0);
    (v.decision.map(|d| (d.rule, d.action)), shadow)
}

/// A movie with an executable that isn't a lure name: MW-2 alone.
const A: (&str, &[(&str, u64)]) = (
    "Some Movie 2019 1080p",
    &[("Some.Movie.2019.mkv", 2 << 30), ("bonus.exe", 1 << 20)],
);
/// A plain movie whose name a fake widening reads.
const B: (&str, &[(&str, u64)]) = ("Other Movie WIDEN 2020 1080p", &[("Other.mkv", 2 << 30)]);
/// A plain movie nothing holds.
const C: (&str, &[(&str, u64)]) = ("Third Movie 2021 1080p", &[("Third.mkv", 2 << 30)]);

fn decide(e: &Engine, t: (&str, &[(&str, u64)])) -> Option<(RuleRef, Action)> {
    judge(e, t.0, t.1).0
}

fn shadow_of(e: &Engine, t: (&str, &[(&str, u64)])) -> Vec<PackShadow> {
    judge(e, t.0, t.1).1
}

const MW2: RuleRef = RuleRef::Pack(PackRule::Mw2);

fn v(version: u32, direction: Direction, action: Action, code: Option<Code>) -> Version {
    Version {
        version,
        direction,
        action,
        code,
    }
}

/// The built-in manifest with some entries replaced.
fn manifest(replace: Vec<RuleEntry>) -> Arc<Manifest> {
    let b = baseline();
    let mut entries: Vec<RuleEntry> = PackRule::ALL
        .into_iter()
        .filter(|r| !replace.iter().any(|e| e.rule == *r))
        .map(|r| b.entry(r).clone())
        .collect();
    entries.extend(replace);
    Arc::new(Manifest::new(entries).unwrap())
}

fn real_mw2(c: &PackCtx<'_>) -> bool {
    c.matches(PackRule::Mw2)
}

/// MW-2 at version 2 with `code` and `direction`, version 1 kept.
fn mw2_bump(direction: Direction, action: Action, code: fn(&PackCtx<'_>) -> bool) -> Arc<Manifest> {
    manifest(vec![RuleEntry::new(
        PackRule::Mw2,
        true,
        vec![
            v(1, Direction::Widens, Action::Hide, Some(Code::Fn(real_mw2))),
            v(2, direction, action, Some(Code::Fn(code))),
        ],
    )])
}

fn widened(c: &PackCtx<'_>) -> bool {
    real_mw2(c) || c.name().contains("WIDEN")
}

fn only_widen(c: &PackCtx<'_>) -> bool {
    c.name().contains("WIDEN")
}

/// Seeds `store` as an install upgraded from before RP-0 with the
/// malware pack on.
fn seeded_with_malware(store: &mut Store, m: &Manifest) {
    store.set_pack_switch("malware", true).unwrap();
    upgrade::ensure_seeded(store, m, 0).unwrap().unwrap();
}

fn rows(store: &Store) -> [Option<String>; 2] {
    APPLIED_KEYS.map(|k| store.setting_text(k).unwrap())
}

fn write_rows(store: &mut Store, a: &str, b: &str) {
    store
        .set_settings_text(&[
            (APPLIED_KEYS[0], a),
            (APPLIED_KEYS[1], b),
            (SEEDED_KEY, "1"),
        ])
        .unwrap();
}

/// A row whose value isn't text.
fn corrupt(store: &Store, key: &str) {
    store
        .connection()
        .execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, x'ff00')",
            [key],
        )
        .unwrap();
}

#[test]
fn seeding_keeps_every_rule_at_baseline_and_copies_sc1_to_sc2() {
    let m = baseline();
    let mut s = store();
    // Before RP-0: scams on, SC-1 switched off, an exception to SC-1.
    s.set_pack_switch("scams", true).unwrap();
    s.set_pack_switch("SC-1", false).unwrap();
    s.add_exception("SC-1", 2, b"robux").unwrap();
    // Not seeded yet: the engine already applies what seeding records.
    let before = load(&m, &s);
    assert!(before.pack_versions.unseeded);
    assert!(
        !before.packs.is_on(PackRule::Sc2),
        "SC-2 follows SC-1 in memory"
    );
    assert_eq!(before.exceptions.len(), 2);
    let seeded = upgrade::ensure_seeded(&mut s, &m, 0).unwrap().unwrap();
    assert!(seeded.rule_set_nonempty, "reported: the rule set had rows");
    assert_eq!(
        s.setting_text(SEEDED_KEY).unwrap().as_deref(),
        Some("rules@0")
    );
    assert_eq!(load(&m, &s).pack_versions.seeded_over_rules, Some(0));
    assert!(seeded.switch_copied);
    assert_eq!(seeded.exceptions_copied, 1);
    let set = s.rule_set().unwrap();
    assert!(set.pack_switches.contains(&("SC-2".into(), false)));
    assert!(
        set.exceptions
            .iter()
            .any(|e| e.rule == "SC-2" && e.value == b"robux")
    );
    let [a, b] = rows(&s);
    assert_eq!(a, b);
    let rec = upgrade::Applied::decode(&a.unwrap()).unwrap();
    assert_eq!(rec, upgrade::seed_record(&m));
    assert_eq!(rec.get(PackRule::Sc2), Some(1), "SC-2 seeded at version 1");
    let e = load(&m, &s);
    assert!(!e.pack_versions.unseeded);
    assert!(e.pack_versions.conditions.is_empty());
    // Only the rules added after RP-0 are pending, in shadow: seeding
    // records what every install had, and they weren't part of it.
    let pending: Vec<PackRule> = e.pack_versions.pending().map(|(r, _)| r).collect();
    assert_eq!(
        pending,
        [PackRule::Mw6, PackRule::Uc1, PackRule::Uc2, PackRule::Uc3],
        "nothing else pending or shadowed"
    );
    assert_eq!(e.packs, before.packs);
    assert_eq!(e.exceptions.len(), before.exceptions.len());
    for r in PackRule::ALL {
        let want: &[u32] = if pending.contains(&r) { &[] } else { &[1] };
        assert_eq!(e.pack_versions.state(r).apply, want, "{}", r.id());
    }
    // Seeding never runs again, and never copies twice.
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
    assert_eq!(s.rule_set().unwrap().exceptions.len(), 2);
}

/// An upgraded store (no fresh marker) seeds quietly, as RP-0 did, and
/// keeps every pack off: RP-DEF writes the off rows explicitly.
#[test]
fn an_upgraded_store_seeds_quietly_and_keeps_packs_off() {
    let m = baseline();
    let mut s = store();
    let seeded = upgrade::ensure_seeded(&mut s, &m, 0).unwrap().unwrap();
    assert!(!seeded.rule_set_nonempty && !seeded.switch_copied);
    assert_eq!(
        s.setting_text(lodestar_filter::load::STAND_INS_KEY)
            .unwrap(),
        None
    );
    let e = load(&m, &s);
    assert!(PackRule::ALL.iter().all(|r| !e.packs.is_on(*r)));
    let set = s.rule_set().unwrap();
    for p in Pack::ALL {
        assert!(
            set.pack_switches.contains(&(p.as_str().to_owned(), false)),
            "{} has an explicit off row",
            p.as_str()
        );
    }
    assert_eq!(s.setting_text(DEFAULTS_KEY).unwrap().as_deref(), Some("1"));
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
}

/// RP-DEF: an upgraded store keeps every pack's state, on or off, and the
/// rule switches the user set.
#[test]
fn an_upgraded_store_keeps_every_packs_state() {
    let m = baseline();
    let mut s = store();
    s.set_pack_switch("malware", true).unwrap();
    s.set_pack_switch("spam", false).unwrap();
    s.set_pack_switch("MW-2", false).unwrap();
    let before = load(&m, &s).packs;
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    assert_eq!(load(&m, &s).packs, before, "filtering is unchanged");
    let set = s.rule_set().unwrap();
    assert!(set.pack_switches.contains(&("malware".into(), true)));
    assert!(set.pack_switches.contains(&("MW-2".into(), false)));
    for p in ["spam", "lures", "fakes", "scams"] {
        assert!(set.pack_switches.contains(&(p.into(), false)), "{p}");
    }
    assert_eq!(s.setting_text(FRESH_KEY).unwrap(), None);
    // Also when RP-0 had seeded it before.
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    assert!(load(&m, &s).packs.is_on(PackRule::Mw1));
    assert!(!load(&m, &s).packs.is_on(PackRule::Sp1));
}

/// RP-DEF: a fresh store has every pack on, with every rule at its
/// current version, so nothing is pending, at the first read after
/// seeding; and a rule added after RP-0 applies too.
#[test]
fn a_fresh_store_has_every_pack_on_at_current_versions() {
    let m = with_new_rule();
    let mut s = fresh_store();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_some());
    let e = load(&m, &s);
    for r in PackRule::ALL {
        assert!(e.packs.is_on(r), "{}", r.id());
        let st = e.pack_versions.state(r);
        assert_eq!(
            st.apply,
            [m.entry(r).current().version],
            "{} at its current version",
            r.id()
        );
        assert!(!st.pending(), "{}", r.id());
    }
    assert!(e.pack_versions.conditions.is_empty());
    assert_eq!(decide(&e, LURE), Some((LU4, Action::Hide)), "LU-4 applies");
    let [a, b] = rows(&s);
    assert_eq!(a, b);
    assert_eq!(
        upgrade::Applied::decode(&a.unwrap()).unwrap(),
        upgrade::current_record(&m)
    );
    assert_eq!(s.setting_text(SEEDED_KEY).unwrap().as_deref(), Some("1"));
    assert_eq!(s.setting_text(DEFAULTS_KEY).unwrap().as_deref(), Some("1"));
    assert_eq!(s.setting_text(FRESH_KEY).unwrap(), None, "consumed");
    // Once only: a pack turned off later stays off.
    upgrade::set_pack_switch(&mut s, &m, "spam", false, 0).unwrap();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
    assert!(!load(&m, &s).packs.is_on(PackRule::Sp1));
}

/// A fresh marker over state an earlier build wrote (a downgrade in
/// between): rows there are choices and kept; packs without one are
/// switched on, their rules recorded at their current versions.
#[test]
fn a_fresh_marker_over_existing_rows_keeps_them() {
    let m = with_new_rule();
    let mut s = fresh_store();
    s.set_pack_switch("spam", false).unwrap();
    write_rows(
        &mut s,
        &upgrade::seed_record(&m).encode(),
        &upgrade::seed_record(&m).encode(),
    );
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap().unwrap();
    let e = load(&m, &s);
    assert!(!e.packs.is_on(PackRule::Sp1), "spam's off row is kept");
    assert!(e.packs.is_on(PackRule::Lu4));
    assert!(!e.pack_versions.state(PackRule::Lu4).pending());
    assert_eq!(decide(&e, LURE), Some((LU4, Action::Hide)));
    assert_eq!(s.setting_text(FRESH_KEY).unwrap(), None);
}

/// RP-DEF: an Undo to a rule set from before RP-DEF (no switch rows)
/// returns to that snapshot's off state, and nothing seeds again.
#[test]
fn undo_to_a_pre_defaults_rule_set_is_off() {
    let m = baseline();
    let mut s = fresh_store();
    let before = s.rule_set().unwrap();
    assert!(before.pack_switches.is_empty());
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    assert!(load(&m, &s).packs.is_on(PackRule::Mw1));
    s.replace_rule_set(&before).unwrap();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
    let e = load(&m, &s);
    assert!(PackRule::ALL.iter().all(|r| !e.packs.is_on(*r)));
    assert!(s.rule_set().unwrap().pack_switches.is_empty());
}

#[test]
fn state_without_the_marker_or_unreadable_never_seeds() {
    let m = baseline();
    // Applied rows without the marker.
    let mut s = store();
    s.set_settings_text(&[(APPLIED_KEYS[0], "MW-1@1")]).unwrap();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
    let e = load(&m, &s);
    assert!(
        e.pack_versions
            .conditions
            .contains(&Condition::MarkerMissing)
    );
    // Not seeded over: a baseline rule missing from the record is in
    // doubt, so it runs the interim mode and keeps applying.
    assert!(
        e.pack_versions
            .conditions
            .contains(&Condition::Unknown(PackRule::Mw2))
    );
    assert_eq!(e.pack_versions.state(PackRule::Mw2).apply, [1]);
    // A marker alone, or a row that won't read, counts as state too.
    for key in [SEEDED_KEY, APPLIED_KEYS[0], APPLIED_KEYS[1]] {
        let mut s = store();
        corrupt(&s, key);
        assert!(
            upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none(),
            "{key}"
        );
        assert_eq!(s.setting_text(APPLIED_KEYS[1]).ok().flatten(), None);
    }
    // An unreadable marker is reported.
    let mut s = store();
    corrupt(&s, SEEDED_KEY);
    assert!(
        load(&baseline(), &s)
            .pack_versions
            .conditions
            .contains(&Condition::MarkerUnreadable)
    );
    // The store's own seeding refuses too.
    assert!(
        s.seed_pack_versions("MW-1@1", "SC-1", "SC-2", "rules@1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn one_unreadable_row_falls_back_to_the_other() {
    let m = mw2_bump(Direction::Widens, Action::Hide, widened);
    for bad in APPLIED_KEYS {
        let mut s = store();
        seeded_with_malware(&mut s, &m);
        upgrade::accept(&mut s, &m, &[PackRule::Mw2], 0).unwrap();
        corrupt(&s, bad);
        assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
        let e = load(&m, &s);
        assert_eq!(e.pack_versions.conditions, [Condition::RowUnreadable]);
        assert_eq!(
            e.pack_versions.state(PackRule::Mw2).apply,
            [2],
            "the accepted widening"
        );
        assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
        // The next write repairs it.
        upgrade::set_pack_switch(&mut s, &m, "lures", true, 0).unwrap();
        let [a, b] = rows(&s);
        assert!(a.is_some() && a == b);
        assert!(load(&m, &s).pack_versions.conditions.is_empty());
    }
}

/// A rule added after RP-0 (LU-4 stands in for SP-3, SP-4), and a
/// widening of MW-2 at version 2.
fn with_new_rule() -> Arc<Manifest> {
    let b = baseline();
    let mut lu4 = b.entry(PackRule::Lu4).clone();
    lu4.baseline = false;
    let mw2 = RuleEntry::new(
        PackRule::Mw2,
        true,
        vec![
            v(1, Direction::Widens, Action::Hide, Some(Code::Fn(real_mw2))),
            v(2, Direction::Widens, Action::Hide, Some(Code::Fn(widened))),
        ],
    );
    manifest(vec![lu4, mw2])
}

const LURE: (&str, &[(&str, u64)]) = (
    "Movie 2019 1080p",
    &[("Movie.mkv", 2 << 30), ("unlock survey.txt", 90)],
);
const LU4: RuleRef = RuleRef::Pack(PackRule::Lu4);

#[test]
fn both_rows_lost_runs_the_interim_mode_and_keeps_what_was_accepted() {
    let m = with_new_rule();
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    s.set_pack_switch("lures", true).unwrap();
    upgrade::accept(&mut s, &m, &[PackRule::Mw2, PackRule::Lu4], 0).unwrap();
    let e = load(&m, &s);
    assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
    assert_eq!(decide(&e, LURE), Some((LU4, Action::Hide)));
    for (a, b) in [(true, true), (true, false), (false, true)] {
        let mut s2 = store();
        s2.set_pack_switch("malware", true).unwrap();
        s2.set_pack_switch("lures", true).unwrap();
        s2.set_settings_text(&[(SEEDED_KEY, "1")]).unwrap();
        if a {
            corrupt(&s2, APPLIED_KEYS[0]);
        }
        if b {
            corrupt(&s2, APPLIED_KEYS[1]);
        }
        assert!(
            upgrade::ensure_seeded(&mut s2, &m, 0).unwrap().is_none(),
            "never re-seeds"
        );
        let e = load(&m, &s2);
        assert_eq!(e.pack_versions.conditions, [Condition::Lost], "{a} {b}");
        assert_eq!(e.pack_versions.labels(), ["packs.lost"]);
        // The accepted widening and the rule added after RP-0 apply.
        assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
        assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
        assert_eq!(decide(&e, LURE), Some((LU4, Action::Hide)));
        // Each interim rule shadows its current version alone, so the
        // acceptance preview has what accepting would change.
        let pending: Vec<PackRule> = e.pack_versions.pending().map(|(r, _)| r).collect();
        assert_eq!(
            pending,
            [
                PackRule::Mw2,
                PackRule::Mw6,
                PackRule::Lu4,
                PackRule::Uc1,
                PackRule::Uc2,
                PackRule::Uc3
            ]
        );
        assert_eq!(shadow_of(&e, A)[0].side(), Some(Side::Both));
        // Accepting is weakening (no record), and writes a fresh record.
        assert!(!acceptance_strengthens(
            &e.pack_versions,
            PackRule::Mw2,
            &[]
        ));
        // An off → on write can't record a partial set meanwhile.
        upgrade::set_pack_switch(&mut s2, &m, "spam", true, 0).unwrap();
        assert_eq!(load(&m, &s2).pack_versions.conditions, [Condition::Lost]);
        // Accepting one rule: the rest stay in the interim mode.
        let part = upgrade::accept(&mut s2, &m, &[PackRule::Mw1], 0).unwrap();
        assert_eq!(part.get(PackRule::Mw1), Some(1));
        assert_eq!(part.get(PackRule::Mw2), Some(upgrade::UNKNOWN));
        let e = load(&m, &s2);
        assert!(!e.pack_versions.conditions.contains(&Condition::Lost));
        assert!(
            e.pack_versions
                .conditions
                .contains(&Condition::Unknown(PackRule::Mw2))
        );
        assert_eq!(e.pack_versions.state(PackRule::Mw2).apply, [1, 2]);
        assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
        assert_eq!(decide(&e, LURE), Some((LU4, Action::Hide)));
        // Accepting all of them: a clean record.
        let fresh = upgrade::accept(&mut s2, &m, &PackRule::ALL, 0).unwrap();
        assert_eq!(fresh.get(PackRule::Mw2), Some(2));
        assert_eq!(fresh.get(PackRule::Lu4), Some(1));
        let e = load(&m, &s2);
        assert!(e.pack_versions.conditions.is_empty());
        assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
    }
}

#[test]
fn the_marker_without_rows_is_lost_state() {
    let m = baseline();
    let mut s = store();
    s.set_settings_text(&[(SEEDED_KEY, "1")]).unwrap();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_none());
    assert_eq!(load(&m, &s).pack_versions.conditions, [Condition::Lost]);
}

#[test]
fn a_version_bump_is_pending_until_accepted_and_releases_nothing() {
    let m = mw2_bump(Direction::Widens, Action::Hide, widened);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let e = load(&m, &s);
    let st = e.pack_versions.state(PackRule::Mw2);
    assert_eq!(
        (st.applied, st.apply.clone(), st.shadow),
        (Some(1), vec![1], Some(2))
    );
    assert_eq!(st.direction, Some(Direction::Widens));
    // The applied version decides; the new one only shadows.
    assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
    assert_eq!(decide(&e, B), None, "held only after acceptance");
    assert_eq!(decide(&e, C), None);
    let both = shadow_of(&e, A);
    assert_eq!(both.len(), 1);
    assert_eq!(both[0].side(), Some(Side::Both));
    let new = shadow_of(&e, B);
    assert_eq!(new[0].side(), Some(Side::ShadowOnly));
    assert_eq!(new[0].shadow, Some(Action::Hide));
    assert!(shadow_of(&e, C).is_empty());
    // The shadow side is logged as a shadow decision, never applied.
    let (name, files) = B;
    let f: Vec<FileInfo> = files
        .iter()
        .map(|(p, s)| FileInfo::new(vec![(*p).to_owned()], *s))
        .collect();
    let class = classify(name, &f);
    let facts = Facts {
        class: &class,
        size: 0,
        piece_len: 0,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[[7; 20]],
        name,
        files: &f,
        content_type: class.content_type,
    };
    let verdict = e.judge(&item, Some(&facts), 0);
    assert!(verdict.decision.is_none());
    assert_eq!(verdict.shadow.len(), 1);
    assert_eq!(verdict.shadow[0].rule, MW2);
    assert!(acceptance_strengthens(
        &e.pack_versions,
        PackRule::Mw2,
        &[both[0], new[0]]
    ));
    // A hold the old version made, in quarantine: acceptance keeps it.
    s.connection()
        .execute(
            "INSERT INTO quarantine (hash, rule, sealed, at, expires_at) VALUES (?1, 'MW-2', NULL, 1, 9999999999)",
            [vec![9u8; 20]],
        )
        .unwrap();
    let before: i64 = s
        .connection()
        .query_row("SELECT count(*) FROM quarantine", [], |r| r.get(0))
        .unwrap();
    upgrade::accept(&mut s, &m, &[PackRule::Mw2], 0).unwrap();
    let after: i64 = s
        .connection()
        .query_row("SELECT count(*) FROM quarantine", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "acceptance releases nothing by itself");
    let e = load(&m, &s);
    assert!(!e.pack_versions.state(PackRule::Mw2).pending());
    assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
    assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
}

#[test]
fn shadow_records_both_directions_and_actions() {
    // Narrows A away and widens to B.
    let m = mw2_bump(Direction::Mixed, Action::Hide, only_widen);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let e = load(&m, &s);
    assert_eq!(shadow_of(&e, A)[0].side(), Some(Side::AppliedOnly));
    assert_eq!(shadow_of(&e, B)[0].side(), Some(Side::ShadowOnly));
    assert!(!acceptance_strengthens(
        &e.pack_versions,
        PackRule::Mw2,
        &[]
    ));
    // A heavier action, and a lighter one.
    for (action, heavier) in [(Action::Drop, true), (Action::Demote, false)] {
        let dir = if heavier {
            Direction::ActionHeavier
        } else {
            Direction::ActionLighter
        };
        let m = mw2_bump(dir, action, real_mw2);
        let mut s = store();
        seeded_with_malware(&mut s, &m);
        let e = load(&m, &s);
        let sh = shadow_of(&e, A);
        assert_eq!(sh[0].side(), Some(Side::Both));
        assert_eq!((sh[0].heavier(), sh[0].lighter()), (heavier, !heavier));
        assert_eq!(
            decide(&e, A),
            Some((MW2, Action::Hide)),
            "still the applied one"
        );
        assert_eq!(
            acceptance_strengthens(&e.pack_versions, PackRule::Mw2, &sh),
            heavier
        );
    }
}

#[test]
fn a_mislabelled_direction_makes_acceptance_weakening() {
    // Labelled `widens`, but it no longer holds A.
    fn narrowed(c: &PackCtx<'_>) -> bool {
        real_mw2(c) && c.name().contains("KEEP")
    }
    let m = mw2_bump(Direction::Widens, Action::Hide, narrowed);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let e = load(&m, &s);
    let sh = shadow_of(&e, A);
    assert_eq!(sh[0].side(), Some(Side::AppliedOnly));
    assert!(!acceptance_strengthens(
        &e.pack_versions,
        PackRule::Mw2,
        &sh
    ));
    assert!(
        acceptance_strengthens(&e.pack_versions, PackRule::Mw2, &[]),
        "the record alone"
    );
    // Labelled `action-heavier`, but it also matches B.
    fn heavier_and_wider(c: &PackCtx<'_>) -> bool {
        widened(c)
    }
    let m = mw2_bump(Direction::ActionHeavier, Action::Drop, heavier_and_wider);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let e = load(&m, &s);
    let a = shadow_of(&e, A);
    let b = shadow_of(&e, B);
    assert!(acceptance_strengthens(&e.pack_versions, PackRule::Mw2, &a));
    assert_eq!(b[0].side(), Some(Side::ShadowOnly));
    assert!(!acceptance_strengthens(&e.pack_versions, PackRule::Mw2, &b));
    // Labelled `action-heavier`, with the same action.
    let m = mw2_bump(Direction::ActionHeavier, Action::Hide, real_mw2);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let e = load(&m, &s);
    let a = shadow_of(&e, A);
    assert!(!acceptance_strengthens(&e.pack_versions, PackRule::Mw2, &a));
}

#[test]
fn a_rule_added_after_rp0_is_pending_until_off_then_on() {
    let m = with_new_rule();
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    s.set_pack_switch("lures", true).unwrap();
    let e = load(&m, &s);
    let st = e.pack_versions.state(PackRule::Lu4);
    assert_eq!(
        (st.applied, st.apply.is_empty(), st.shadow),
        (None, true, Some(1))
    );
    assert_eq!(
        st.direction,
        Some(Direction::Widens),
        "a first version widens"
    );
    assert_eq!(decide(&e, LURE), None, "never on unpreviewed");
    assert_eq!(shadow_of(&e, LURE)[0].side(), Some(Side::ShadowOnly));
    assert!(acceptance_strengthens(
        &e.pack_versions,
        PackRule::Lu4,
        &shadow_of(&e, LURE)
    ));
    // A wizard re-run (on → on) accepts nothing.
    assert!(
        upgrade::set_pack_switch(&mut s, &m, "lures", true, 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(decide(&load(&m, &s), LURE), None);
    // The rule switched off, then back on: its current version.
    upgrade::set_pack_switch(&mut s, &m, "LU-4", false, 0).unwrap();
    assert_eq!(
        upgrade::set_pack_switch(&mut s, &m, "LU-4", true, 0).unwrap(),
        [PackRule::Lu4]
    );
    assert_eq!(decide(&load(&m, &s), LURE), Some((LU4, Action::Hide)));
    // Or its pack switched off, then on.
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    s.set_pack_switch("lures", true).unwrap();
    upgrade::set_pack_switch(&mut s, &m, "lures", false, 0).unwrap();
    let on = upgrade::set_pack_switch(&mut s, &m, "lures", true, 0).unwrap();
    assert!(on.contains(&PackRule::Lu4));
    assert_eq!(decide(&load(&m, &s), LURE), Some((LU4, Action::Hide)));
    // MW-2's pending widening was untouched by the lures pack.
    assert!(load(&m, &s).pack_versions.state(PackRule::Mw2).pending());
}

#[test]
fn full_state_writes_and_undo_accept_nothing_and_leave_versions() {
    let m = mw2_bump(Direction::Widens, Action::Hide, widened);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    // A full-state write (Undo, redo) that leaves the pack on.
    let set = s.rule_set().unwrap();
    s.replace_rule_set(&set).unwrap();
    let mut off = set.clone();
    off.pack_switches.retain(|(id, _)| id != "malware");
    s.replace_rule_set(&off).unwrap();
    s.replace_rule_set(&set).unwrap();
    assert!(load(&m, &s).pack_versions.state(PackRule::Mw2).pending());
    // Undo after an acceptance leaves the versions.
    upgrade::accept(&mut s, &m, &[PackRule::Mw2], 0).unwrap();
    let accepted = rows(&s);
    s.replace_rule_set(&off).unwrap();
    s.replace_rule_set(&set).unwrap();
    assert_eq!(rows(&s), accepted);
    assert!(!load(&m, &s).pack_versions.state(PackRule::Mw2).pending());
}

#[test]
fn the_interim_mode_never_drops_what_only_an_unaccepted_version_holds() {
    // MW-4 drops. Version 1's code is gone; version 2 narrowed; 3 is the
    // current one.
    fn v2(c: &PackCtx<'_>) -> bool {
        c.matches(PackRule::Mw4) && c.name().contains("TWO")
    }
    let m = manifest(vec![RuleEntry::new(
        PackRule::Mw4,
        true,
        vec![
            v(1, Direction::Widens, Action::Drop, None),
            v(2, Direction::Narrows, Action::Drop, Some(Code::Fn(v2))),
            v(3, Direction::Widens, Action::Drop, Some(Code::Current)),
        ],
    )]);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let e = load(&m, &s);
    let st = e.pack_versions.state(PackRule::Mw4);
    assert_eq!(st.apply, [2, 3]);
    assert_eq!(st.unaccepted, [2, 3]);
    assert!(
        e.pack_versions
            .conditions
            .contains(&Condition::Skipped(PackRule::Mw4))
    );
    assert!(
        e.pack_versions
            .conditions
            .contains(&Condition::Narrowed(PackRule::Mw4)),
        "a skipped narrowing is reported"
    );
    assert!(
        e.pack_versions
            .labels()
            .contains(&"packs.narrowed.MW-4".to_owned())
    );
    assert!(!acceptance_strengthens(
        &e.pack_versions,
        PackRule::Mw4,
        &[]
    ));
    let tiny = (
        "Movie 2019 1080p",
        &[("movie.mkv", 90_000), ("player.exe", 1 << 20)][..],
    );
    assert_eq!(
        decide(&e, tiny).map(|d| d.1),
        Some(Action::Quarantine),
        "held, never dropped"
    );
    // Accepted, it drops again.
    upgrade::accept(&mut s, &m, &[PackRule::Mw4], 0).unwrap();
    let e = load(&m, &s);
    assert!(e.pack_versions.conditions.is_empty());
    assert_eq!(decide(&e, tiny).map(|d| d.1), Some(Action::Drop));
}

#[test]
fn both_rows_lost_never_drops_by_an_unprovable_version() {
    let m = manifest(vec![RuleEntry::new(
        PackRule::Mw4,
        true,
        vec![
            v(
                1,
                Direction::Widens,
                Action::Drop,
                Some(Code::Fn(|c| c.matches(PackRule::Mw4))),
            ),
            v(2, Direction::Widens, Action::Drop, Some(Code::Current)),
        ],
    )]);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    corrupt(&s, APPLIED_KEYS[0]);
    corrupt(&s, APPLIED_KEYS[1]);
    let e = load(&m, &s);
    let st = e.pack_versions.state(PackRule::Mw4);
    assert_eq!(
        (st.apply.clone(), st.unaccepted.clone()),
        (vec![1, 2], vec![2])
    );
    let tiny = (
        "Movie 2019 1080p",
        &[("movie.mkv", 90_000), ("player.exe", 1 << 20)][..],
    );
    // Version 1 (seeded, so accepted) matches too: it still drops.
    assert_eq!(decide(&e, tiny).map(|d| d.1), Some(Action::Drop));
}

#[test]
fn differing_rows_apply_the_stronger_per_rule() {
    let m = mw2_bump(Direction::Mixed, Action::Hide, only_widen);
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let seed = upgrade::seed_record(&m).encode();
    let two = seed.replace("MW-2@1", "MW-2@2");
    write_rows(&mut s, &seed, &two);
    let e = load(&m, &s);
    assert_eq!(e.pack_versions.conditions, [Condition::Differ]);
    let st = e.pack_versions.state(PackRule::Mw2);
    assert_eq!(st.apply, [1, 2]);
    // Both versions hold: A by version 1, B by version 2.
    assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
    assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
    assert!(e.pack_versions.in_doubt(PackRule::Mw2));
    // An off → on write keeps both rows' views of other rules.
    upgrade::set_pack_switch(&mut s, &m, "lures", true, 0).unwrap();
    let [a, b] = rows(&s);
    assert_ne!(a, b);
    assert_eq!(load(&m, &s).pack_versions.conditions, [Condition::Differ]);
    // Accepting another rule never picks one of MW-2's two versions.
    upgrade::accept(&mut s, &m, &[PackRule::Mw1], 0).unwrap();
    let e = load(&m, &s);
    assert_eq!(e.pack_versions.conditions, [Condition::Differ]);
    assert_eq!(e.pack_versions.state(PackRule::Mw2).apply, [1, 2]);
    // Accepting MW-2 writes one record again.
    upgrade::accept(&mut s, &m, &[PackRule::Mw2], 0).unwrap();
    let [a, b] = rows(&s);
    assert_eq!(a, b);
    assert!(load(&m, &s).pack_versions.conditions.is_empty());
}

#[test]
fn a_damaged_readable_record_fails_closed() {
    let m = mw2_bump(Direction::Narrows, Action::Hide, only_widen);
    // An empty record, and one that lost MW-2's line.
    for text in [
        String::new(),
        upgrade::seed_record(&m).encode().replace("MW-2@1", ""),
    ] {
        let mut s = store();
        seeded_with_malware(&mut s, &m);
        write_rows(&mut s, &text, &text);
        let e = load(&m, &s);
        assert!(
            e.pack_versions
                .conditions
                .contains(&Condition::Unknown(PackRule::Mw2)),
            "{text:?}"
        );
        // The interim mode: both versions hold, nothing is dropped.
        assert_eq!(e.pack_versions.state(PackRule::Mw2).apply, [1, 2]);
        assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
        assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
        assert!(!acceptance_strengthens(
            &e.pack_versions,
            PackRule::Mw2,
            &[]
        ));
    }
}

#[test]
fn a_downgrade_applies_the_builds_own_drop() {
    let m = baseline();
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    s.set_pack_switch("fakes", true).unwrap();
    let text = upgrade::seed_record(&m)
        .encode()
        .replace("MW-4@1", "MW-4@5")
        .replace("FK-4@1", "FK-4@3");
    write_rows(&mut s, &text, &text);
    let e = load(&m, &s);
    assert_eq!(
        e.pack_versions.conditions,
        [
            Condition::Downgrade(PackRule::Fk4),
            Condition::Downgrade(PackRule::Mw4)
        ]
    );
    assert!(!e.pack_versions.state(PackRule::Mw4).pending());
    assert!(e.pack_versions.state(PackRule::Fk4).plain);
    let tiny = (
        "Movie 2019 1080p",
        &[("movie.mkv", 90_000), ("player.exe", 1 << 20)][..],
    );
    assert_eq!(
        decide(&e, tiny),
        Some((RuleRef::Pack(PackRule::Mw4), Action::Drop))
    );
    let empty = ("Empty 2019", &[("pad.bin", 0)][..]);
    assert_eq!(
        decide(&e, empty),
        Some((RuleRef::Pack(PackRule::Fk4), Action::Drop))
    );
}

#[test]
fn a_downgrade_applies_the_builds_version_and_the_upgrade_back_keeps_the_accepted_one() {
    let newer = mw2_bump(Direction::Widens, Action::Hide, widened);
    let older = baseline();
    let mut s = store();
    seeded_with_malware(&mut s, &newer);
    upgrade::accept(&mut s, &newer, &[PackRule::Mw2], 0).unwrap();
    let accepted = rows(&s);
    // The older build: its own version, the row left as it is.
    let e = load(&older, &s);
    assert_eq!(e.pack_versions.state(PackRule::Mw2).apply, [1]);
    assert_eq!(
        e.pack_versions.conditions,
        [Condition::Downgrade(PackRule::Mw2)]
    );
    assert_eq!(e.pack_versions.labels(), ["packs.downgrade.MW-2"]);
    assert_eq!(decide(&e, A), Some((MW2, Action::Hide)));
    assert_eq!(decide(&e, B), None);
    // Its writes merge per rule and keep the newer version.
    upgrade::set_pack_switch(&mut s, &older, "malware", false, 0).unwrap();
    upgrade::set_pack_switch(&mut s, &older, "malware", true, 0).unwrap();
    upgrade::accept(&mut s, &older, &[PackRule::Mw2], 0).unwrap();
    assert!(rows(&s)[0].as_deref().unwrap().contains("MW-2@2"));
    // Turning malware on recorded MW-6, which the older build ships and
    // the newer one's acceptance hadn't: the rest is as accepted.
    let mut now = upgrade::Applied::decode(rows(&s)[0].as_deref().unwrap()).unwrap();
    assert_eq!(now.0.remove("MW-6"), Some(1));
    assert_eq!(
        Some(now),
        upgrade::Applied::decode(accepted[0].as_deref().unwrap())
    );
    // The upgrade back.
    let e = load(&newer, &s);
    assert!(e.pack_versions.conditions.is_empty());
    assert_eq!(e.pack_versions.state(PackRule::Mw2).apply, [2]);
    assert_eq!(decide(&e, B), Some((MW2, Action::Hide)));
}

#[test]
fn unknown_switches_and_versions_are_kept_and_ignored() {
    let m = baseline();
    let mut s = store();
    seeded_with_malware(&mut s, &m);
    let seed = upgrade::seed_record(&m).encode();
    let with = format!("{seed}\nZZ-9@4");
    write_rows(&mut s, &with, &with);
    s.set_pack_switch("ZZ-9", true).unwrap();
    s.set_pack_switch("newpack", true).unwrap();
    let e = load(&m, &s);
    assert!(e.pack_versions.conditions.is_empty());
    assert!(
        e.pack_versions
            .pending()
            .all(|(r, _)| lodestar_filter::packs::manifest::ADDED_AFTER_RP0.contains(&r))
    );
    upgrade::set_pack_switch(&mut s, &m, "lures", true, 0).unwrap();
    upgrade::accept(&mut s, &m, &[PackRule::Mw1], 0).unwrap();
    for r in rows(&s) {
        assert!(r.unwrap().contains("ZZ-9@4"));
    }
    let set = s.rule_set().unwrap();
    assert!(set.pack_switches.contains(&("ZZ-9".into(), true)));
    assert!(set.pack_switches.contains(&("newpack".into(), true)));
}
