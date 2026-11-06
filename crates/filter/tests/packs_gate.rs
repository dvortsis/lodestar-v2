//! The rule-pack gate (RP-G; plan 2026-11-04 §4.3, architecture §20.8):
//! the packs proven together, as an install runs them.
//!
//! - **Corpora:** every pack's corpus and the precision corpus pass, one
//!   rule at a time ([`Corpus::run`], as RP-3 serves them) and again with
//!   every pack on together behind the shipped child-safety core, on a
//!   fresh install and on one an earlier build set up. The counts the
//!   corpora ship with are floors, so losing an example, a precision item
//!   or a rule's coverage is a regression, and a new known gap is too.
//! - **Upgrade without release:** on the install an earlier build set
//!   up, the versions this build changed or added stay pending, judged in
//!   shadow, across restarts and unrelated changes, until accepted.
//!   Accepting releases nothing it held, pauses the overrides that weaken
//!   the new version and keeps the ones that strengthen it.
//!
//! - **The child-safety core and the adult floor:** with every pack on,
//!   every pack rule overridden to its weakest, versions pending and
//!   versions just accepted, the core still decides what it decides alone,
//!   at least as heavily, in prepare and at commit.
//!
//! The budget with every pack on is `props.rs`'s `budget`; the guardrails
//! through the API are `crates/api/tests/packs_gate.rs`.

use std::collections::BTreeMap;

use lodestar_classify::{Classification, classify};
use lodestar_filter::corpus::{self, Corpus, Example, Expect, GUARDED};
use lodestar_filter::packs::manifest::Manifest;
use lodestar_filter::packs::overrides::{PackOverride, PackOverrides, current_base};
use lodestar_filter::packs::upgrade::{self, FRESH_KEY, PackShadow, Side};
use lodestar_filter::packs::{Pack, PackRule};
use lodestar_filter::{
    Action, Core, CsList, Engine, Facts, Item, Lists, PackSwitches, RuleRef, Switches, Verdict,
};
use lodestar_store::{Store, StoreConfig};

fn builtin() -> &'static Corpus {
    match Corpus::builtin() {
        Ok(c) => c,
        Err(e) => panic!("the shipped corpus won't parse: {e}"),
    }
}

fn stateless() -> impl Iterator<Item = PackRule> {
    PackRule::ALL.into_iter().filter(|r| corpus::stateless(*r))
}

/// A store as an install has it: `upgraded`, one an earlier build set up
/// with every pack on (no fresh marker), so the versions this build
/// changed or added are pending; otherwise one created on this start.
fn install(upgraded: bool) -> Store {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    if upgraded {
        s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
        for p in Pack::ALL {
            s.set_pack_switch(p.as_str(), true).unwrap();
        }
    }
    assert!(
        upgrade::ensure_seeded(&mut s, &Manifest::builtin(), 0)
            .unwrap()
            .is_some(),
        "seeded once"
    );
    s
}

/// The engine as start loads it: the shipped core, then the store's rule
/// set and pack versions.
fn load(store: &Store) -> Engine {
    let mut e = Engine::new(Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    ));
    let notes = e.load_rules(&store.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(e.packs, PackSwitches::all(), "every pack on");
    e
}

/// The example's classification, with its `type` if it has one.
fn classified(e: &Example) -> Classification {
    let mut class = classify(&e.name, &e.files);
    if let Some(t) = e.content_type {
        class.content_type = t;
    }
    class
}

/// The engine's verdict on a corpus example, as ingest judges it, with
/// its pending versions' shadow results.
fn judge(engine: &Engine, e: &Example) -> (Verdict, Vec<PackShadow>) {
    let class = classified(e);
    let facts = Facts {
        class: &class,
        size: e.files.iter().map(|f| f.size).fold(0, u64::saturating_add),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[[0x5a; 20]],
        name: &e.name,
        files: &e.files,
        content_type: class.content_type,
    };
    engine.judge_shadowed(&item, Some(&facts), 0)
}

fn decided(v: &Verdict) -> Option<(RuleRef, Action)> {
    v.decision.as_ref().map(|d| (d.rule, d.action))
}

// ---------------------------------------------------------------------
// The corpora.

/// What the corpora shipped with when the gate was set (RP-G,
/// 2026-11-06): `match` and `miss` examples per rule. A rule may gain
/// examples; losing one fails the gate.
const FLOORS: &[(&str, usize, usize)] = &[
    ("MW-1", 4, 3),
    ("MW-2", 11, 6),
    ("MW-3", 3, 4),
    ("MW-4", 3, 3),
    ("MW-5", 3, 3),
    ("MW-6", 4, 4),
    ("LU-1", 3, 3),
    ("LU-2", 3, 3),
    ("LU-3", 3, 3),
    ("LU-4", 3, 3),
    ("FK-1", 3, 3),
    ("FK-2", 10, 5),
    ("FK-4", 3, 3),
    ("FK-5", 3, 3),
    ("SP-1", 3, 3),
    ("SP-2", 3, 4),
    ("SC-1", 3, 4),
    ("SC-2", 3, 3),
    ("UC-1", 7, 5),
    ("UC-2", 3, 4),
    ("UC-3", 3, 4),
];

/// Precision items when the gate was set.
const PRECISION_FLOOR: usize = 38;

#[test]
fn every_corpus_and_the_precision_corpus_pass() {
    let c = builtin();
    let m = Manifest::builtin();
    let report = c.run(&m);
    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the pack gate fails:\n{}",
        failures.join("\n")
    );
    assert!(report.passes());
    // No known gap: a precision item a rule holds fails, it isn't listed.
    assert!(report.gaps().is_empty(), "{:?}", report.gaps());
    assert!(report.directions.iter().all(|d| d.agrees()));
    // Every stateless rule is reported, with its floors.
    let mut counts = BTreeMap::new();
    for r in stateless() {
        let rr = report
            .rule(r)
            .unwrap_or_else(|| panic!("{} reported", r.id()));
        assert_eq!(rr.disagreements(), 0, "{}", r.id());
        counts.insert(r.id(), (rr.count(Expect::Match), rr.count(Expect::Miss)));
    }
    let mut lost = Vec::new();
    for (id, m, x) in FLOORS {
        let (got_m, got_x) = counts.get(id).copied().unwrap_or_default();
        if got_m < *m || got_x < *x {
            lost.push(format!("{id}: {got_m}/{m} match, {got_x}/{x} miss"));
        }
    }
    assert!(lost.is_empty(), "examples lost: {lost:?}");
    let floored: Vec<&str> = FLOORS.iter().map(|(id, ..)| *id).collect();
    let missing: Vec<&&str> = counts.keys().filter(|id| !floored.contains(id)).collect();
    assert!(
        missing.is_empty(),
        "stateless rules without a floor: {missing:?}"
    );
    assert!(
        report.precision.len() >= PRECISION_FLOOR,
        "{} precision items, at least {PRECISION_FLOOR}",
        report.precision.len()
    );
    for r in GUARDED {
        assert!(
            c.precision.iter().any(|p| p.guards.contains(r)),
            "{}'s guard has a precision item",
            r.id()
        );
    }
}

/// With every pack on together behind the shipped core:
/// - each `match` example is held at least as heavily as its rule holds
///   it alone, except one of a rule this install never applied, which is
///   in shadow only until accepted;
/// - no precision item is held, by a pack rule or the core, except by an
///   older version still applied whose pending version lets it through
///   (the guards RP-1b added): accepting that version is what releases
///   such holds, never the upgrade itself.
///
/// Returns the precision items held that way, by line.
fn every_pack_on_together(engine: &Engine) -> Vec<(usize, PackRule)> {
    let c = builtin();
    let state = |r: PackRule| engine.pack_versions.state(r);
    let mut wrong = Vec::new();
    for pack in Pack::ALL {
        for case in c.cases(pack).iter().filter(|x| x.expect == Expect::Match) {
            let (v, shadow) = judge(engine, &case.example);
            let got = decided(&v);
            let rule = case.rule;
            let name = &case.example.name;
            if state(rule).pending() && state(rule).apply.is_empty() {
                if got.is_some_and(|(r, _)| r == RuleRef::Pack(rule)) {
                    wrong.push(format!(
                        "{} applied before it's accepted: {name}",
                        rule.id()
                    ));
                }
                if !shadow
                    .iter()
                    .any(|s| s.rule == rule && s.side() == Some(Side::ShadowOnly))
                {
                    wrong.push(format!("{} not in shadow: {name}", rule.id()));
                }
                continue;
            }
            let action = engine.manifest.entry(rule).current().action;
            if got.is_none_or(|(_, a)| a < action) {
                wrong.push(format!(
                    "{} {got:?}: {name} (its rule alone: {action:?})",
                    rule.id()
                ));
            }
        }
    }
    let mut kept = Vec::new();
    for p in &c.precision {
        let (v, shadow) = judge(engine, &p.example);
        let Some((rule, _)) = decided(&v) else {
            continue;
        };
        let line = p.example.line;
        match rule {
            RuleRef::Pack(r)
                if state(r).pending()
                    && shadow
                        .iter()
                        .any(|s| s.rule == r && s.side() == Some(Side::AppliedOnly)) =>
            {
                kept.push((line, r));
            }
            _ => wrong.push(format!(
                "precision line {line}: {rule:?}: {}",
                p.example.name
            )),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} wrong:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
    kept
}

#[test]
fn a_fresh_install_holds_the_corpora_with_every_pack_on() {
    let e = load(&install(false));
    assert_eq!(e.pack_versions.pending().count(), 0);
    assert!(every_pack_on_together(&e).is_empty());
}

/// The precision items an earlier build's MW-2@1 and FK-2@1 hold, which
/// their version 2 guards let through (RP-1b): music production packs and
/// short episodes.
const KEPT_UNTIL_ACCEPTED: &[(usize, PackRule)] = &[
    (23, PackRule::Mw2),
    (28, PackRule::Mw2),
    (33, PackRule::Mw2),
    (41, PackRule::Mw2),
    (107, PackRule::Fk2),
    (113, PackRule::Fk2),
];

#[test]
fn an_upgraded_install_holds_the_corpora_until_it_accepts() {
    let mut s = install(true);
    let e = load(&s);
    let pending: Vec<PackRule> = e.pack_versions.pending().map(|(r, _)| r).collect();
    for r in [PackRule::Mw2, PackRule::Fk2, PackRule::Sp4] {
        assert!(pending.contains(&r), "{} pending: {pending:?}", r.id());
    }
    assert_eq!(every_pack_on_together(&e), KEPT_UNTIL_ACCEPTED);
    // Every pending version accepted: as a fresh install.
    upgrade::accept(&mut s, &Manifest::builtin(), &pending, 0).unwrap();
    let e = load(&s);
    assert_eq!(e.pack_versions.pending().count(), 0);
    assert!(every_pack_on_together(&e).is_empty());
}

// ---------------------------------------------------------------------
// Upgrade without release.

const MW2: RuleRef = RuleRef::Pack(PackRule::Mw2);
const FK2: RuleRef = RuleRef::Pack(PackRule::Fk2);

/// The first `match` example of `rule` that, with every pack on and
/// nothing overridden, `rule` itself decides at its own action.
fn alone(engine: &Engine, rule: PackRule) -> Option<&'static Example> {
    builtin()
        .cases(rule.pack())
        .iter()
        .filter(|x| x.rule == rule && x.expect == Expect::Match)
        .map(|x| &x.example)
        .find(|e| decided(&judge(engine, e).0) == Some((RuleRef::Pack(rule), rule.action())))
}

fn decided_by(engine: &Engine, rule: PackRule) -> &'static Example {
    alone(engine, rule).unwrap_or_else(|| panic!("an example {} decides alone", rule.id()))
}

/// The overrides the upgrade test sets, against the versions `engine`
/// applies: MW-2 lighter (weakens), FK-2 heavier (strengthens), and
/// `steady` lighter with an `unless` nothing meets (weakens).
fn overrides(engine: &Engine, steady: PackRule) -> Vec<(String, Result<String, Vec<u8>>)> {
    [
        (PackRule::Mw2, Action::Flag, None),
        (PackRule::Fk2, Action::Hide, None),
        (steady, Action::Flag, Some("nothingmatchesthis")),
    ]
    .into_iter()
    .map(|(rule, action, unless)| {
        let row = PackOverride {
            base: current_base(&engine.pack_versions, rule),
            action,
            unless: unless.map(str::to_owned),
        };
        (rule.id().to_owned(), Ok(row.encode()))
    })
    .collect()
}

/// A rule nothing pending touches, heavier than a flag, and an example
/// it decides alone, also once [`overrides`] are set.
fn steady(engine: &Engine) -> (PackRule, &'static Example) {
    stateless()
        .filter(|r| !engine.pack_versions.state(*r).pending() && r.action() > Action::Flag)
        .flat_map(|r| {
            builtin()
                .cases(r.pack())
                .iter()
                .filter(move |x| x.rule == r && x.expect == Expect::Match)
                .map(move |x| (r, &x.example))
        })
        .find(|(r, e)| {
            let mut with = engine.clone();
            with.pack_overrides =
                PackOverrides::resolve(&with.manifest, &with.pack_versions, &overrides(engine, *r));
            decided(&judge(&with, e).0) == Some((RuleRef::Pack(*r), Action::Flag))
        })
        .expect("a steady rule")
}

/// Every row of the tables that hold or release: the review queue, the
/// tombstones, the filter hits and your exceptions.
fn holds(store: &Store) -> Vec<String> {
    [
        "SELECT hex(hash) || ' ' || rule || ' ' || at || ' ' || expires_at AS r FROM quarantine ORDER BY hash",
        "SELECT hex(hash) || ' ' || rule || ' ' || at AS r FROM tombstones ORDER BY hash",
        "SELECT id || ' ' || hex(hash) || ' ' || rule || ' ' || action || ' ' || at AS r FROM filter_hits ORDER BY id",
        "SELECT id || ' ' || rule || ' ' || kind || ' ' || hex(value) AS r FROM filter_exceptions ORDER BY id",
    ]
    .iter()
    .map(|q| {
        let rows: Option<String> = store
            .connection()
            .query_row(&format!("SELECT group_concat(r, ',') FROM ({q})"), [], |r| {
                r.get(0)
            })
            .unwrap();
        rows.unwrap_or_default()
    })
    .collect()
}

#[test]
fn an_upgrade_stays_pending_releases_nothing_and_pauses_weakening_overrides() {
    let m = Manifest::builtin();
    let mut s = install(true);
    let e = load(&s);
    // The applied versions are older than the ones built.
    for r in [PackRule::Mw2, PackRule::Fk2] {
        let st = e.pack_versions.state(r);
        assert_eq!((st.applied, st.shadow), (Some(1), Some(2)), "{}", r.id());
        assert!(st.pending());
    }
    let mw2 = decided_by(&e, PackRule::Mw2);
    let fk2 = decided_by(&e, PackRule::Fk2);
    let (steady, other) = steady(&e);
    let other_ref = RuleRef::Pack(steady);
    // Overrides made against the applied versions: MW-2 lighter (weakens),
    // FK-2 heavier (strengthens), and a rule nothing pending touches,
    // lighter and with an `unless` its example doesn't meet (weakens).
    for (rule, row) in overrides(&e, steady) {
        s.set_pack_override(&rule, Some(row.as_ref().unwrap()))
            .unwrap();
    }
    // What it holds: a quarantined torrent, a tombstone, filter hits.
    let c = s.connection();
    c.execute(
        "INSERT INTO quarantine (hash, rule, sealed, at, expires_at) VALUES (?1, 'MW-2', NULL, 1, 9999999999)",
        [vec![9u8; 20]],
    )
    .unwrap();
    c.execute(
        "INSERT INTO tombstones (hash, rule, at) VALUES (?1, 'MW-4', 1)",
        [vec![8u8; 20]],
    )
    .unwrap();
    for (n, rule, action) in [
        (7u8, "MW-2", Action::Hide),
        (6, "FK-2", Action::Flag),
        (5, "SP-2", Action::Hide),
    ] {
        c.execute(
            "INSERT INTO filter_hits (hash, rule, action, at, evidence) VALUES (?1, ?2, ?3, 1, x'')",
            (vec![n; 20], rule, action as i64),
        )
        .unwrap();
    }
    let before = holds(&s);

    let check_pending = |s: &Store, why: &str| {
        let e = load(s);
        for r in [PackRule::Mw2, PackRule::Fk2, PackRule::Sp4] {
            assert!(
                e.pack_versions.state(r).pending(),
                "{why}: {} pending",
                r.id()
            );
        }
        for (rule, o) in [
            (PackRule::Mw2, "MW-2"),
            (PackRule::Fk2, "FK-2"),
            (steady, steady.id()),
        ] {
            let l = e
                .pack_overrides
                .get(rule)
                .unwrap_or_else(|| panic!("{why}: {o}"));
            assert!(
                !l.changed && !l.paused && !l.unreadable(),
                "{why}: {o} applies"
            );
        }
        assert_eq!(
            decided(&judge(&e, mw2).0),
            Some((MW2, Action::Flag)),
            "{why}"
        );
        assert_eq!(
            decided(&judge(&e, fk2).0),
            Some((FK2, Action::Hide)),
            "{why}"
        );
        assert_eq!(
            decided(&judge(&e, other).0),
            Some((other_ref, Action::Flag)),
            "{why}"
        );
        assert_eq!(holds(s), before, "{why}: nothing released");
    };
    check_pending(&s, "set up");
    // A restart, an unrelated change, and another rule switched off and
    // on (which records only that rule's version): still pending.
    assert!(
        upgrade::ensure_seeded(&mut s, &m, 10).unwrap().is_none(),
        "never seeded again"
    );
    upgrade::offer_new_packs(&mut s).unwrap();
    check_pending(&s, "a restart");
    s.add_rule("unrelated words", Action::Flag as u8, None, None)
        .unwrap();
    check_pending(&s, "a rule of yours");
    upgrade::set_pack_switch(&mut s, &m, "SP-1", false, 20).unwrap();
    upgrade::set_pack_switch(&mut s, &m, "SP-1", true, 21).unwrap();
    check_pending(&s, "SP-1 off and on");

    // Accepted.
    upgrade::accept(
        &mut s,
        &m,
        &[PackRule::Mw2, PackRule::Fk2, PackRule::Sp4],
        30,
    )
    .unwrap();
    assert_eq!(holds(&s), before, "acceptance releases nothing by itself");
    let e = load(&s);
    for r in [PackRule::Mw2, PackRule::Fk2, PackRule::Sp4] {
        let st = e.pack_versions.state(r);
        assert!(!st.pending() && st.plain, "{} applied", r.id());
    }
    // MW-2's lighter override weakens the new version: paused, so the
    // rule applies as built until you confirm it again.
    let o = e.pack_overrides.get(PackRule::Mw2).unwrap();
    assert!(o.changed && o.paused, "{o:?}");
    assert_eq!(decided(&judge(&e, mw2).0), Some((MW2, Action::Hide)));
    // FK-2's heavier one strengthens it: kept, flagged as changed.
    let o = e.pack_overrides.get(PackRule::Fk2).unwrap();
    assert!(o.changed && !o.paused, "{o:?}");
    assert_eq!(decided(&judge(&e, fk2).0), Some((FK2, Action::Hide)));
    // The steady rule's base didn't change: its override still applies.
    let o = e.pack_overrides.get(steady).unwrap();
    assert!(!o.changed && !o.paused, "{o:?}");
    assert_eq!(
        decided(&judge(&e, other).0),
        Some((other_ref, Action::Flag))
    );
    // A restart after acceptance keeps all of it.
    upgrade::ensure_seeded(&mut s, &m, 40).unwrap();
    let again = load(&s);
    assert!(again.pack_overrides.get(PackRule::Mw2).unwrap().paused);
    assert_eq!(
        again.pack_versions.pending().count(),
        e.pack_versions.pending().count()
    );
    assert_eq!(holds(&s), before);
}

// ---------------------------------------------------------------------
// The child-safety core and the adult floor, never loosened by the packs
// (runner's review of #526): with every pack on, with overrides on every
// pack rule, with versions pending and with them just accepted, the core
// still decides what it decides alone, at least as heavily. The core's
// rules are driven with its neutral stand-in words (`lexa`, `rolea`,
// `codex`, as `tests/core.rs` does), so no indicative text is needed; the
// shipped lists' hashed must-match fixtures are `core.rs`'s
// `shipped_must_match_fixtures`, which only the core reads.

const FLOOR_HASH: [u8; 20] = [0xc5; 20];

fn stand_in_core() -> Core {
    use lodestar_filter::{Origin, TermSet, payload_sig};
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut cs = CsList::default();
    cs.add_hash(FLOOR_HASH, Origin::Confirmed);
    cs.add_sig(payload_sig(&floor_payload()).unwrap().sig);
    Core::new(lists, cs, Switches::default())
}

/// A payload CS-8a holds (specific: two files of distinctive size).
fn floor_payload() -> Vec<lodestar_classify::FileInfo> {
    files(&[("x/a.mkv", 734_003_201), ("x/b.srt", 81_233)])
}

fn files(paths: &[(&str, u64)]) -> Vec<lodestar_classify::FileInfo> {
    paths
        .iter()
        .map(|(p, s)| {
            lodestar_classify::FileInfo::new(p.split('/').map(str::to_owned).collect(), *s)
        })
        .collect()
}

/// One torrent the core holds alone: its hashes, name, files and the type
/// it's judged as, and whether its source is always adult.
struct Held {
    what: &'static str,
    hashes: [[u8; 20]; 1],
    name: &'static str,
    files: Vec<lodestar_classify::FileInfo>,
    content_type: lodestar_classify::ContentType,
    adult_source: bool,
}

fn held() -> Vec<Held> {
    use lodestar_classify::ContentType::{Adult, Images, Movie};
    let plain = || files(&[("a.mkv", 1 << 30)]);
    let h = |what, name, files, content_type, adult_source| Held {
        what,
        hashes: [[0x11; 20]],
        name,
        files,
        content_type,
        adult_source,
    };
    let mut floor = h("CS-1a, a confirmed hash", "Anything", plain(), Movie, false);
    floor.hashes = [FLOOR_HASH];
    vec![
        floor,
        h("CS-8a, a payload", "Renamed", floor_payload(), Movie, false),
        h("CS-2a, an age", "Title 15yo lexa", plain(), Movie, false),
        h("CS-3a, a code", "Title codex", plain(), Movie, false),
        h(
            "CS-3b, a code in files",
            "Plain Title",
            files(&[("Folder codex/a.jpg", 1), ("b.jpg", 1)]),
            Images,
            false,
        ),
        h("CS-4a, a role", "rolea Title lexa", plain(), Movie, false),
        h(
            "CS-5a, Adult's wider window",
            "rolea a b c d e f g h i lexa",
            files(&[("a.mp4", 1 << 30)]),
            Adult,
            false,
        ),
        h(
            "CS-5b, Adult's wider window",
            "15yo a b c d e f g h i lexa",
            files(&[("a.mp4", 1 << 30)]),
            Adult,
            false,
        ),
        // The adult floor at commit: a find from a source that's always
        // adult is read as Adult too, so Adult's wider window holds it.
        h(
            "CS-5a, read as Adult for an always-adult source",
            "rolea a b c d e f g h i lexa",
            files(&[("a.mp4", 1 << 30)]),
            Movie,
            true,
        ),
    ]
}

/// What the core alone decides about `t`: as found, or as Adult for an
/// always-adult source.
fn core_alone(core: &Core, t: &Held) -> (lodestar_filter::CoreRule, Action) {
    let item = Item {
        hashes: &t.hashes,
        name: t.name,
        files: &t.files,
        content_type: if t.adult_source {
            lodestar_classify::ContentType::Adult
        } else {
            t.content_type
        },
    };
    let hit = core
        .verdict(&item)
        .unwrap_or_else(|| panic!("the core alone holds {}", t.what));
    (hit.rule, hit.action)
}

/// The engine's decisions on `t`: prepare's, and the writer's at commit
/// (in a flood, as a clone of hidden spam, and for an always-adult source
/// with its Adult reading).
fn engine_decides(e: &Engine, t: &Held) -> Vec<(&'static str, Option<(RuleRef, Action)>)> {
    use lodestar_filter::flood::Level;
    use lodestar_filter::packs::sp4::AtCommit;
    use lodestar_filter::spam_sigs::Original;
    let mut class = classify(t.name, &t.files);
    class.content_type = t.content_type;
    let facts = Facts {
        class: &class,
        size: t.files.iter().map(|f| f.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &t.hashes,
        name: t.name,
        files: &t.files,
        content_type: t.content_type,
    };
    let at = AtCommit {
        flood: Some(Level::Demote),
        clone_of: Some(Original {
            rule: PackRule::Sp2,
            action: Action::Hide,
        }),
    };
    let mut out = vec![(
        "at commit",
        decided(&e.judge_at_commit(&item, &facts, 0, at, t.adult_source)),
    )];
    if !t.adult_source {
        out.push(("prepare", decided(&e.judge(&item, Some(&facts), 0))));
    }
    out
}

/// Every stateless and stateful pack rule overridden to flag, with an
/// `unless` that every held torrent meets: the most an override can
/// weaken.
fn weakest_overrides(store: &mut Store, e: &Engine) {
    for r in PackRule::ALL {
        let row = PackOverride {
            base: current_base(&e.pack_versions, r),
            action: Action::Flag,
            unless: Some("lexa OR codex OR rolea OR anything OR renamed".to_owned()),
        };
        store
            .set_pack_override(r.id(), Some(&row.encode()))
            .unwrap();
    }
}

fn load_with(core: &Core, store: &Store) -> Engine {
    let mut e = Engine::new(core.clone());
    let notes = e.load_rules(&store.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    e.packs = PackSwitches::all();
    e
}

#[test]
fn the_core_and_the_adult_floor_hold_under_every_pack_state() {
    let core = stand_in_core();
    let mut states: Vec<(&str, Engine)> = Vec::new();
    let fresh = install(false);
    states.push(("every pack on", load_with(&core, &fresh)));
    let mut overridden = install(false);
    let e = load_with(&core, &overridden);
    weakest_overrides(&mut overridden, &e);
    let e = load_with(&core, &overridden);
    assert_eq!(e.pack_overrides.iter().count(), PackRule::COUNT);
    states.push(("every pack rule overridden to flag, with an unless", e));
    let mut upgraded = install(true);
    let e = load_with(&core, &upgraded);
    let pending: Vec<PackRule> = e.pack_versions.pending().map(|(r, _)| r).collect();
    assert!(!pending.is_empty());
    weakest_overrides(&mut upgraded, &e);
    states.push((
        "versions pending, overrides set",
        load_with(&core, &upgraded),
    ));
    upgrade::accept(&mut upgraded, &Manifest::builtin(), &pending, 0).unwrap();
    let e = load_with(&core, &upgraded);
    assert_eq!(e.pack_versions.pending().count(), 0);
    states.push(("versions just accepted, overrides set", e));

    let mut wrong = Vec::new();
    for t in held() {
        let (rule, action) = core_alone(&core, &t);
        for (state, e) in &states {
            for (when, got) in engine_decides(e, &t) {
                let ok = got.is_some_and(|(r, a)| r == RuleRef::Core(rule) && a >= action);
                if !ok {
                    wrong.push(format!(
                        "{}: {state}, {when}: {got:?}, the core alone: {rule:?} {action:?}",
                        t.what
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "the core loosened:\n{}", wrong.join("\n"));
}
