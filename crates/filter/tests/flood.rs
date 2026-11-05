//! RP-4 (§20.6 SP-3, plan 2026-11-04 Q10): flooding, judged in the
//! writer through [`Engine::judge_flooded`].
//!
//! - **The rate map** ([`Flood`]) never tracks more than its cap, never
//!   counts a name more than once or past the hour, and no single name
//!   reaches a level: property tests.
//! - **Sequence fixtures,** SP-3's must-match and must-not-match cases
//!   (RP-1 exempts it from the per-item corpus): a flood flags from the
//!   20th near-duplicate and demotes from the 100th; different titles,
//!   different publishers and names without a title never count.
//! - **Precedence:** a CSX-5 Override, an exception, *Always allow*, the
//!   switches and a deciding rule of yours each stop SP-3; your override
//!   changes its action; in the pack layer the strongest action wins.
//! - **Versions:** pending on a store seeded before it shipped (shadow
//!   only, agreeing with its recorded `widens`), applied on a fresh one.

use std::collections::HashMap;

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::flood::{
    self, DEMOTE_AT, FLAG_AT, Flood, Level, Origin, Signature, WINDOW_SECS, signature,
};
use lodestar_filter::packs::manifest::{Direction, Manifest};
use lodestar_filter::packs::overrides::PackOverride;
use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
use lodestar_filter::packs::{Pack, PackRule};
use lodestar_filter::{
    Action, Allow, Core, CsList, Engine, Exception, Facts, Item, Lists, Match, Override,
    PackSwitches, RuleRef, Strength, Switches, TermSet, UserRule, Verdict,
};
use lodestar_search::parse;
use lodestar_store::{Store, StoreConfig};
use proptest::prelude::*;

const SP3: RuleRef = RuleRef::Pack(PackRule::Sp3);
const H: [u8; 20] = [7; 20];
const VIDEO: &[(&str, u64)] = &[("e.mkv", 700 << 20)];

fn bare() -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Engine::new(Core::new(lists, CsList::default(), Switches::default()))
}

/// A fresh store's engine, every pack on: SP-3 applies.
fn engine() -> Engine {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    assert!(
        upgrade::ensure_seeded(&mut s, &Manifest::builtin(), 0)
            .unwrap()
            .is_some()
    );
    let mut e = load(&s);
    e.packs = PackSwitches::all();
    assert!(e.pack_versions.state(PackRule::Sp3).plain);
    e
}

fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
    paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect()
}

/// One commit as the writer makes it: the name counted against `origin`
/// at `now`, then judged with SP-3 at the level reached, or as prepare
/// judged it.
fn commit(e: &Engine, map: &mut Flood, origin: Origin<'_>, name: &str, now: i64) -> Verdict {
    let f = files(VIDEO);
    let class = classify(name, &f);
    let facts = Facts {
        class: &class,
        size: 700 << 20,
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes: &[H],
        name,
        files: &f,
        content_type: class.content_type,
    };
    let base = e.judge(&item, Some(&facts), now);
    let level = signature(&class.title.release.title).and_then(|s| map.record(origin, s, now));
    match level {
        Some(level) => {
            let v = e.judge_flooded(&item, &facts, now, level, false);
            // Never weaker than prepare's verdict.
            let weaker = match (&base.decision, &v.decision) {
                (Some(b), Some(f)) => f.action < b.action,
                (Some(_), None) => true,
                _ => false,
            };
            assert!(!weaker, "{name}: {base:?} → {v:?}");
            v
        }
        None => base,
    }
}

fn decided(v: &Verdict) -> Option<(RuleRef, Action)> {
    v.decision.as_ref().map(|d| (d.rule, d.action))
}

/// The `n`th near-duplicate: episode, quality and group differ.
fn episode(n: usize) -> String {
    let q = ["720p", "1080p", "480p"][n % 3];
    format!("Example Show S01E{:02} {q} HDTV x264-G{n}", n % 90 + 1)
}

const PUB: Origin<'static> = Origin::Publisher("uploader");

/// Commits `n` near-duplicates from `origin` in one minute and returns
/// the last verdict.
fn flood_of(e: &Engine, n: usize, origin: Origin<'_>) -> Verdict {
    let mut map = Flood::default();
    let mut last = Verdict::default();
    for i in 0..n {
        last = commit(e, &mut map, origin, &episode(i), 1_000 + (i as i64 % 60));
    }
    last
}

// ---------------------------------------------------------------------
// Sequence fixtures.

#[test]
fn near_duplicates_share_a_signature() {
    let sig = |n| {
        let name = episode(n);
        signature(&classify(&name, &files(VIDEO)).title.release.title).unwrap()
    };
    for n in 1..200 {
        assert_eq!(sig(n), sig(0), "{}", episode(n));
    }
    let title = |name: &str| classify(name, &files(VIDEO)).title.release.title;
    assert_ne!(
        signature(&title("Other Show S01E01 720p")),
        Some(sig(0)),
        "another title"
    );
}

#[test]
fn a_flood_flags_from_20_and_demotes_from_100() {
    let e = engine();
    let mut map = Flood::default();
    for i in 0..120 {
        let v = commit(&e, &mut map, PUB, &episode(i), 1_000 + i as i64);
        let n = i as u32 + 1;
        let want = if n >= DEMOTE_AT {
            Some((SP3, Action::Demote))
        } else if n >= FLAG_AT {
            Some((SP3, Action::Flag))
        } else {
            None
        };
        assert_eq!(decided(&v), want, "name {n}");
    }
    // Prepare's verdict never has it.
    let f = files(VIDEO);
    let name = episode(0);
    let class = classify(&name, &f);
    let facts = Facts {
        class: &class,
        size: 0,
        piece_len: 0,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[H],
        name: &name,
        files: &f,
        content_type: class.content_type,
    };
    assert_eq!(e.judge(&item, Some(&facts), 0).decision, None);
    // An hour after the last, the window is empty and the next passes.
    assert_eq!(
        decided(&commit(&e, &mut map, PUB, &episode(0), 1_120 + WINDOW_SECS)),
        None
    );
}

#[test]
fn what_isnt_a_flood_never_counts() {
    let e = engine();
    let mut map = Flood::default();
    // One publisher, many titles.
    for i in 0..300 {
        let v = commit(&e, &mut map, PUB, &format!("Title{i} Story 720p"), 1_000);
        assert_eq!(decided(&v), None, "title {i}");
    }
    // One title, many publishers and sources.
    for i in 0..300 {
        let p = format!("uploader{i}");
        let v = commit(&e, &mut map, Origin::Publisher(&p), &episode(0), 1_000);
        assert_eq!(decided(&v), None, "publisher {i}");
        let s = Origin::Source((i % 60) as u8 + 1);
        if i < 60 * (FLAG_AT as usize - 1) {
            assert_eq!(decided(&commit(&e, &mut map, s, "Lone Name", 1_000)), None);
        }
    }
    // A name with no title has no signature.
    assert_eq!(signature(""), None);
    // The same title spread over more than an hour: 19 per hour.
    let mut map = Flood::default();
    for i in 0..200 {
        let at = 1_000 + i * (WINDOW_SECS / (FLAG_AT as i64 - 1) + 1);
        assert_eq!(
            decided(&commit(&e, &mut map, PUB, &episode(i as usize), at)),
            None,
            "{i}"
        );
    }
}

// ---------------------------------------------------------------------
// Precedence.

#[test]
fn a_csx5_override_stops_it() {
    let mut e = engine();
    e.overrides.push(Override {
        id: 1,
        what: Match::Keyword("example".into()),
        strength: Strength::Override,
        scope: None,
        expires_at: None,
        shadow_until: None,
    });
    assert_eq!(decided(&flood_of(&e, 150, PUB)), None);
    // A Neutralize entry takes no evidence SP-3 reads: it still holds.
    e.overrides[0].strength = Strength::Neutralize;
    assert_eq!(
        decided(&flood_of(&e, 150, PUB)),
        Some((SP3, Action::Demote))
    );
}

#[test]
fn an_exception_stops_it_and_only_it() {
    let mut e = engine();
    e.exceptions.push(Exception {
        id: 1,
        rule: SP3,
        what: Match::Hashes(vec![H]),
    });
    assert_eq!(decided(&flood_of(&e, 150, PUB)), None);
    e.exceptions[0].rule = RuleRef::Pack(PackRule::Sp1);
    assert_eq!(
        decided(&flood_of(&e, 150, PUB)),
        Some((SP3, Action::Demote))
    );
}

#[test]
fn always_allow_stops_it() {
    let mut e = engine();
    e.allow.push(Allow {
        id: 1,
        query: parse("example").unwrap(),
    });
    assert_eq!(decided(&flood_of(&e, 150, PUB)), None);
}

#[test]
fn the_switches_stop_it() {
    let mut e = engine();
    e.packs.on.remove(&Pack::Spam);
    assert_eq!(decided(&flood_of(&e, 150, PUB)), None);
    let mut e = engine();
    e.packs.rules_off.insert(PackRule::Sp3);
    assert_eq!(decided(&flood_of(&e, 150, PUB)), None);
    let mut e = engine();
    e.packs.rules_off.insert(PackRule::Sp1);
    assert_eq!(
        decided(&flood_of(&e, 150, PUB)),
        Some((SP3, Action::Demote))
    );
}

#[test]
fn your_deciding_rule_comes_first_and_the_strongest_pack_action_wins() {
    let mut e = engine();
    e.rules.push(UserRule {
        id: 9,
        query: parse("example").unwrap(),
        action: Action::Flag,
        enabled: true,
        shadow_until: None,
        unless: None,
    });
    assert_eq!(
        decided(&flood_of(&e, 150, PUB)),
        Some((RuleRef::User(9), Action::Flag)),
        "your rule decides before the packs"
    );
    // SP-1 (a site advert, demote) and SP-3 at the flag level: SP-1's
    // heavier action wins.
    let e = engine();
    let mut map = Flood::default();
    let mut last = Verdict::default();
    for i in 0..30 {
        let name = format!("www.example-site.com - {}", episode(i));
        last = commit(&e, &mut map, PUB, &name, 1_000);
    }
    assert_eq!(
        decided(&last),
        Some((RuleRef::Pack(PackRule::Sp1), Action::Demote))
    );
}

// ---------------------------------------------------------------------
// The store: overrides and versions.

fn load(store: &Store) -> Engine {
    let mut e = bare();
    e.manifest = Manifest::builtin();
    let notes = e.load_rules(&store.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    e
}

/// A store seeded before SP-3 shipped, with the spam pack on.
fn upgraded_store() -> Store {
    let m = Manifest::builtin();
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    let seed = upgrade::seed_record(&m).encode();
    assert!(!seed.contains("SP-3"), "{seed}");
    s.write_pack_versions([Some(&seed), Some(&seed)], Some(("spam", true)))
        .unwrap();
    s
}

#[test]
fn your_override_sets_its_action_at_both_levels() {
    let m = Manifest::builtin();
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_some());
    let row = PackOverride {
        base: 1,
        action: Action::Hide,
        unless: None,
    }
    .encode();
    s.set_pack_override("SP-3", Some(&row)).unwrap();
    let e = load(&s);
    assert_eq!(decided(&flood_of(&e, 25, PUB)), Some((SP3, Action::Hide)));
    assert_eq!(decided(&flood_of(&e, 150, PUB)), Some((SP3, Action::Hide)));
}

#[test]
fn pending_on_an_upgraded_store_applied_on_a_fresh_one() {
    let m = Manifest::builtin();
    assert_eq!(
        m.entry(PackRule::Sp3).direction(None, 1),
        Some(Direction::Widens)
    );
    assert!(!m.entry(PackRule::Sp3).baseline);
    // Upgraded: pending, in shadow only. Its shadow holds what the
    // applied side (nothing) doesn't: it agrees with `widens`.
    let mut s = upgraded_store();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    let e = load(&s);
    let st = e.pack_versions.state(PackRule::Sp3);
    assert!(st.pending() && st.apply.is_empty());
    for (n, action) in [(25, Action::Flag), (150, Action::Demote)] {
        let v = flood_of(&e, n, PUB);
        assert_eq!(decided(&v), None, "nothing applies before acceptance");
        assert!(
            v.shadow.iter().any(|d| d.rule == SP3 && d.action == action),
            "{:?}",
            v.shadow
        );
    }
    let shadow = upgrade::PackShadow {
        rule: PackRule::Sp3,
        version: 1,
        applied: None,
        shadow: Some(Action::Flag),
    };
    assert!(!upgrade::disagrees(Direction::Widens, &shadow));
    // Accepted: it applies.
    upgrade::accept(&mut s, &m, &[PackRule::Sp3], 0).unwrap();
    let e = load(&s);
    assert_eq!(
        decided(&flood_of(&e, 150, PUB)),
        Some((SP3, Action::Demote))
    );
    // Fresh: applied at once.
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    let e = load(&s);
    assert!(e.pack_versions.state(PackRule::Sp3).plain);
    assert_eq!(decided(&flood_of(&e, 25, PUB)), Some((SP3, Action::Flag)));
}

// ---------------------------------------------------------------------
// The map's properties.

fn origin_of(o: u8) -> Origin<'static> {
    const NAMES: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];
    if o < 8 {
        Origin::Publisher(NAMES[o as usize])
    } else {
        Origin::Source(o)
    }
}

fn sig_of(s: u8) -> Signature {
    signature(&format!("title {s}")).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The map never tracks more than its cap, and its links hold.
    #[test]
    fn never_over_its_cap(
        cap in 1usize..40,
        ops in proptest::collection::vec((0u8..64, 0u8..40, 0i64..20_000), 1..600),
    ) {
        let mut map = Flood::with_cap(cap);
        for (o, s, t) in ops {
            map.record(origin_of(o), sig_of(s), t);
            prop_assert!(map.len() <= cap);
        }
        prop_assert!(map.check().is_ok(), "{:?}", map.check());
    }

    /// A level is only ever reached by that many names of the same
    /// signature from the same origin within the last hour, so no single
    /// name, and nothing short of a flood, reaches one.
    #[test]
    fn never_counts_more_than_it_saw(
        cap in 1usize..20,
        ops in proptest::collection::vec((0u8..12, 0u8..24, 0i64..90), 1..1_500),
    ) {
        let mut map = Flood::with_cap(cap);
        let mut seen: HashMap<(u8, u8), Vec<i64>> = HashMap::new();
        let mut now = 0;
        for (o, s, step) in ops {
            now += step;
            let level = map.record(origin_of(o), sig_of(s), now);
            let times = seen.entry((o, s)).or_default();
            times.push(now);
            let recent = times.iter().filter(|&&t| t > now - WINDOW_SECS).count() as u32;
            let counted = map.count(origin_of(o), sig_of(s), now);
            prop_assert!(counted <= recent, "{counted} > {recent}");
            prop_assert_eq!(level, Level::of(counted));
            if recent < FLAG_AT {
                prop_assert_eq!(level, None);
            }
        }
    }
}

/// The cap the plan sets, filled and overrun.
#[test]
fn the_full_cap_holds() {
    let mut map = Flood::default();
    assert_eq!(map.cap(), flood::MAX_ORIGINS);
    let s = sig_of(1);
    for i in 0..3 * flood::MAX_ORIGINS {
        let p = format!("publisher-{i}");
        map.record(Origin::Publisher(&p), s, i as i64);
        assert!(map.len() <= flood::MAX_ORIGINS);
    }
    map.check().unwrap();
    assert_eq!(map.len(), flood::MAX_ORIGINS);
    // A huge publisher string costs no more than a short one.
    let long = "p".repeat(1 << 20);
    map.record(Origin::Publisher(&long), s, 0);
    assert_eq!(map.len(), flood::MAX_ORIGINS);
}
