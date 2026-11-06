//! RP-5 (§20.6 SP-4, plan 2026-11-04 Q12): clones of hidden spam, judged
//! in the writer through [`Engine::judge_at_commit`] against the
//! [`SpamSigs`] set.
//!
//! - **Sequence fixtures,** SP-4's must-match and must-not-match cases
//!   (RP-1 exempts it from the per-item corpus): an original SP-2 hid,
//!   or SP-3 demoted, then its clone under another name, which inherits
//!   the original's action; another payload, a payload that isn't
//!   specific, and SP-3's flag never count.
//! - **Precedence:** every SP-3 precedence test has its twin here: a
//!   CSX-5 Override, an exception, *Always allow*, the switches and a
//!   deciding rule of yours each stop SP-4; your override changes its
//!   action; in the pack layer the strongest action wins; the adult
//!   reading is judged too.
//! - **It follows its original:** the original's rule switched off, or an
//!   exception to it covering the clone, lets the clone through; a
//!   release removes the signature.
//! - **Versions:** pending on a store seeded before it shipped (shadow
//!   only, agreeing with its recorded `widens`), applied on a fresh one.
//! - **Storage:** the set survives a restart, one hide writes one row,
//!   a row is compacted once a quarter of it is removed, the oldest is
//!   dropped at the cap, and a row that doesn't read loses only its own
//!   signatures, listed, and is never written over.

use std::collections::{BTreeMap, HashMap, HashSet};

use lodestar_classify::{ContentType, FileInfo, classify};
use lodestar_filter::flood::Level;
use lodestar_filter::packs::manifest::{Direction, Manifest};
use lodestar_filter::packs::overrides::PackOverride;
use lodestar_filter::packs::sp4::AtCommit;
use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
use lodestar_filter::packs::{Pack, PackRule};
use lodestar_filter::spam_sigs::{
    self, CAP, MAX_ROW_BYTES, Original, PREFIX, ROW_BYTES, ROW_ENTRIES, SpamSigs, usable,
};
use lodestar_filter::{
    Action, Allow, Core, CsList, Engine, Exception, Facts, Item, Lists, Match, Override,
    PackSwitches, RuleRef, Strength, Switches, TermSet, UserRule, Verdict, payload_sig,
};
use lodestar_search::parse;
use lodestar_store::{RawSetting, Store, StoreConfig};
use proptest::prelude::*;

const SP2: RuleRef = RuleRef::Pack(PackRule::Sp2);
const SP3: RuleRef = RuleRef::Pack(PackRule::Sp3);
const SP4: RuleRef = RuleRef::Pack(PackRule::Sp4);
const ORIGINAL_HASH: [u8; 20] = [1; 20];
const CLONE_HASH: [u8; 20] = [2; 20];

/// The payload the fixtures share: a film-sized file of an exact size.
const SIZE: u64 = 1_234_567_891;

fn bare() -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Engine::new(Core::new(lists, CsList::default(), Switches::default()))
}

fn load(store: &Store) -> Engine {
    let mut e = bare();
    e.manifest = Manifest::builtin();
    let notes = e.load_rules(&store.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    e
}

/// A fresh store's engine, every pack on: SP-4 applies.
fn engine() -> Engine {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    assert!(
        upgrade::ensure_seeded(&mut s, &Manifest::builtin(), 0)
            .unwrap()
            .is_some()
    );
    let mut e = load(&s);
    e.packs = PackSwitches::all();
    assert!(e.pack_versions.state(PackRule::Sp4).plain);
    e
}

fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
    paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect()
}

/// A keyword-stuffed name SP-2 hides.
fn stuffed() -> String {
    "free movie download ".repeat(20).trim_end().to_owned()
}

/// A torrent as the writer commits it.
struct Torrent {
    hash: [u8; 20],
    name: String,
    files: Vec<FileInfo>,
}

fn torrent(hash: [u8; 20], name: &str, f: &[(&str, u64)]) -> Torrent {
    Torrent {
        hash,
        name: name.to_owned(),
        files: files(f),
    }
}

fn original() -> Torrent {
    torrent(ORIGINAL_HASH, &stuffed(), &[("a.mkv", SIZE)])
}

/// The original's files under another name: SP-2 doesn't match it.
fn clone_named(name: &str) -> Torrent {
    torrent(CLONE_HASH, name, &[("Holiday.2024.mkv", SIZE)])
}

fn clone() -> Torrent {
    clone_named("Holiday Video 2024 1080p")
}

/// One commit as the writer makes it: prepare's verdict, then, when SP-3
/// counted `flood` or the set holds the torrent's signature, the torrent
/// judged again with them, the stricter verdict standing; then the
/// signature added if a spam rule hid it.
fn commit_at(e: &Engine, t: &Torrent, flood: Option<Level>, adult: bool) -> Verdict {
    let class = classify(&t.name, &t.files);
    let facts = Facts {
        class: &class,
        size: t.files.iter().map(|f| f.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 1_000,
        now: 1_000,
    };
    let hashes = [t.hash];
    let item = Item {
        hashes: &hashes,
        name: &t.name,
        files: &t.files,
        content_type: class.content_type,
    };
    let base = e.judge(&item, Some(&facts), 1_000);
    assert!(
        base.decision.as_ref().is_none_or(|d| d.rule != SP4),
        "prepare never has SP-4"
    );
    let sig = usable(payload_sig(&t.files));
    let clone_of = sig
        .filter(|_| e.packs.is_on(PackRule::Sp4))
        .and_then(|s| e.spam_sigs.get(s));
    let at = AtCommit { flood, clone_of };
    let v = if at.any() {
        let v = e.judge_at_commit(&item, &facts, 1_000, at, adult);
        // Never weaker than prepare's verdict.
        let weaker = match (&base.decision, &v.decision) {
            (Some(b), Some(f)) => f.action < b.action,
            (Some(_), None) => true,
            _ => false,
        };
        assert!(!weaker, "{}: {base:?} → {v:?}", t.name);
        v
    } else {
        base
    };
    if let Some(o) = v.decision.as_ref().and_then(|d| match d.rule {
        RuleRef::Pack(r) => Original::of(r, d.action),
        _ => None,
    }) && let Some(s) = sig
    {
        e.spam_sigs.add(s, o);
    }
    v
}

fn commit(e: &Engine, t: &Torrent) -> Verdict {
    commit_at(e, t, None, false)
}

fn decided(v: &Verdict) -> Option<(RuleRef, Action)> {
    v.decision.as_ref().map(|d| (d.rule, d.action))
}

/// The original committed, then its clone's decision.
fn cloned(e: &Engine) -> Option<(RuleRef, Action)> {
    commit(e, &original());
    decided(&commit(e, &clone()))
}

fn sig_of(t: &Torrent) -> u64 {
    usable(payload_sig(&t.files)).expect("a specific payload")
}

// ---------------------------------------------------------------------
// Sequence fixtures.

#[test]
fn a_clone_of_what_sp2_hid_is_hidden() {
    let e = engine();
    assert_eq!(decided(&commit(&e, &clone())), None, "no original yet");
    assert_eq!(decided(&commit(&e, &original())), Some((SP2, Action::Hide)));
    assert_eq!(
        e.spam_sigs.get(sig_of(&original())),
        Some(Original {
            rule: PackRule::Sp2,
            action: Action::Hide
        })
    );
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
    // Paths and names don't matter, sizes do; their order doesn't.
    let two = torrent(
        [3; 20],
        &stuffed(),
        &[("x.mkv", 700_000_001), ("y.nfo", 4_321)],
    );
    commit(&e, &two);
    let reordered = torrent(
        [4; 20],
        "Some Other Name",
        &[("z.nfo", 4_321), ("w.mkv", 700_000_001)],
    );
    assert_eq!(decided(&commit(&e, &reordered)), Some((SP4, Action::Hide)));
    // A clone SP-4 hid adds nothing of its own.
    assert_eq!(e.spam_sigs.len(), 2);
}

#[test]
fn what_isnt_a_clone_never_matches() {
    let e = engine();
    commit(&e, &original());
    // Another payload: one byte more.
    let other = torrent(CLONE_HASH, "Holiday Video 2024", &[("h.mkv", SIZE + 1)]);
    assert_eq!(decided(&commit(&e, &other)), None);
    // An extra file is another payload too.
    let extra = torrent(
        CLONE_HASH,
        "Holiday Video 2024",
        &[("h.mkv", SIZE), ("cover.jpg", 90_001)],
    );
    assert_eq!(decided(&commit(&e, &extra)), None);
    // A payload that isn't specific (small, or only round sizes) never
    // joins, so its clones are never caught: an unrelated torrent could
    // share it by chance.
    let small = torrent([5; 20], &stuffed(), &[("a.txt", 30_001)]);
    assert_eq!(decided(&commit(&e, &small)), Some((SP2, Action::Hide)));
    assert_eq!(usable(payload_sig(&small.files)), None);
    let small_clone = torrent([6; 20], "Notes", &[("b.txt", 30_001)]);
    assert_eq!(decided(&commit(&e, &small_clone)), None);
    assert_eq!(e.spam_sigs.len(), 1);
}

#[test]
fn a_clone_of_what_sp3_demoted_is_demoted_and_its_flag_never_counts() {
    let e = engine();
    let flooded = torrent(
        ORIGINAL_HASH,
        "Example Show S01E01 720p",
        &[("e.mkv", SIZE)],
    );
    // SP-3's flag isn't a hide: nothing joins.
    assert_eq!(
        decided(&commit_at(&e, &flooded, Some(Level::Flag), false)),
        Some((SP3, Action::Flag))
    );
    assert!(e.spam_sigs.is_empty());
    assert_eq!(decided(&commit(&e, &clone())), None);
    // Its demote is: the clone inherits demote.
    assert_eq!(
        decided(&commit_at(&e, &flooded, Some(Level::Demote), false)),
        Some((SP3, Action::Demote))
    );
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Demote)));
    // An SP-2 hide of the same payload later: the newer original's action.
    commit(&e, &original());
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

// ---------------------------------------------------------------------
// Precedence: SP-3's tests' twins (crates/filter/tests/flood.rs).

#[test]
fn a_csx5_override_stops_it() {
    let mut e = engine();
    commit(&e, &original());
    e.overrides.push(Override {
        id: 1,
        what: Match::Keyword("holiday".into()),
        strength: Strength::Override,
        scope: None,
        expires_at: None,
        shadow_until: None,
    });
    assert_eq!(decided(&commit(&e, &clone())), None);
    // A Neutralize entry takes no evidence SP-4 reads: it still holds.
    e.overrides[0].strength = Strength::Neutralize;
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

#[test]
fn an_exception_stops_it_and_only_it() {
    let mut e = engine();
    commit(&e, &original());
    e.exceptions.push(Exception {
        id: 1,
        rule: SP4,
        what: Match::Hashes(vec![CLONE_HASH]),
    });
    assert_eq!(decided(&commit(&e, &clone())), None);
    e.exceptions[0].rule = RuleRef::Pack(PackRule::Sp1);
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

#[test]
fn always_allow_stops_it() {
    let mut e = engine();
    commit(&e, &original());
    e.allow.push(Allow {
        id: 1,
        query: parse("holiday").unwrap(),
    });
    assert_eq!(decided(&commit(&e, &clone())), None);
}

#[test]
fn the_switches_stop_it() {
    let mut e = engine();
    commit(&e, &original());
    e.packs.on.remove(&Pack::Spam);
    assert_eq!(decided(&commit(&e, &clone())), None);
    let mut e = engine();
    commit(&e, &original());
    e.packs.rules_off.insert(PackRule::Sp4);
    assert_eq!(decided(&commit(&e, &clone())), None);
    let mut e = engine();
    commit(&e, &original());
    e.packs.rules_off.insert(PackRule::Sp1);
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

#[test]
fn your_deciding_rule_comes_first_and_the_strongest_pack_action_wins() {
    let mut e = engine();
    commit(&e, &original());
    e.rules.push(UserRule {
        id: 9,
        query: parse("holiday").unwrap(),
        action: Action::Flag,
        enabled: true,
        shadow_until: None,
        unless: None,
    });
    assert_eq!(
        decided(&commit(&e, &clone())),
        Some((RuleRef::User(9), Action::Flag)),
        "your rule decides before the packs"
    );
    // A site advert (SP-1, demote) on a clone of a hidden original:
    // SP-4's heavier hide wins.
    let e = engine();
    commit(&e, &original());
    let advert = clone_named("www.example-site.com - Holiday Video 2024");
    assert_eq!(decided(&commit(&e, &advert)), Some((SP4, Action::Hide)));
    // A clone of a demoted original that MW-1 hides (a double
    // extension): MW-1's heavier hide wins.
    let e = engine();
    let flooded = torrent(
        ORIGINAL_HASH,
        "Example Show S01E01 720p",
        &[("e.mkv", SIZE)],
    );
    commit_at(&e, &flooded, Some(Level::Demote), false);
    let double = torrent(CLONE_HASH, "Holiday Video", &[("Holiday.mp4.exe", SIZE)]);
    assert_eq!(
        decided(&commit(&e, &double)),
        Some((RuleRef::Pack(PackRule::Mw1), Action::Hide))
    );
}

/// The adult reading (an always-adult source's find, or one your Is
/// adult rule makes adult) is judged with SP-4 too, and the stricter
/// verdict stands: a `type:adult` rule of yours decides over SP-4.
#[test]
fn the_adult_reading_is_judged_too() {
    let mut e = engine();
    let flooded = torrent(
        ORIGINAL_HASH,
        "Example Show S01E01 720p",
        &[("e.mkv", SIZE)],
    );
    commit_at(&e, &flooded, Some(Level::Demote), false);
    e.rules.push(UserRule {
        id: 4,
        query: parse("type:adult").unwrap(),
        action: Action::Hide,
        enabled: true,
        shadow_until: None,
        unless: None,
    });
    let c = clone();
    assert_ne!(classify(&c.name, &c.files).content_type, ContentType::Adult);
    // As detected: SP-4's demote.
    assert_eq!(
        decided(&commit_at(&e, &c, None, false)),
        Some((SP4, Action::Demote))
    );
    // An always-adult source found it: the adult reading's rule wins.
    assert_eq!(
        decided(&commit_at(&e, &c, None, true)),
        Some((RuleRef::User(4), Action::Hide))
    );
    // Your Is adult rule makes it adult: the same.
    e.load_class_rules(&[(1, "holiday".into(), true)]);
    assert_eq!(
        decided(&commit_at(&e, &c, None, false)),
        Some((RuleRef::User(4), Action::Hide))
    );
}

// ---------------------------------------------------------------------
// It follows its original.

#[test]
fn it_follows_the_switch_of_the_rule_that_hid_the_original() {
    let mut e = engine();
    commit(&e, &original());
    // SP-2 off: its original isn't hidden any more, nor are its clones.
    e.packs.rules_off.insert(PackRule::Sp2);
    assert_eq!(decided(&commit(&e, &clone())), None);
    // Back on: they are again. The signature stayed.
    e.packs.rules_off.remove(&PackRule::Sp2);
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

#[test]
fn an_exception_to_the_originals_rule_that_covers_the_clone_lets_it_through() {
    // For the payload: every clone.
    let mut e = engine();
    commit(&e, &original());
    e.exceptions.push(Exception {
        id: 1,
        rule: SP2,
        what: Match::Payload(payload_sig(&original().files).unwrap().sig),
    });
    assert_eq!(decided(&commit(&e, &clone())), None);
    // By a word in the clone's name: that clone.
    let mut e = engine();
    commit(&e, &original());
    e.exceptions.push(Exception {
        id: 1,
        rule: SP2,
        what: Match::Keyword("holiday".into()),
    });
    assert_eq!(decided(&commit(&e, &clone())), None);
    // To another rule than the one that hid it: nothing.
    e.exceptions[0].rule = SP3;
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

#[test]
fn a_release_removes_the_signature_and_its_clones_follow() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let e = engine();
    commit(&e, &original());
    let sig = sig_of(&original());
    // An exception to SP-3 leaves what SP-2 hid.
    assert_eq!(
        e.spam_sigs
            .release(&mut store, &[sig], Some(PackRule::Sp3))
            .unwrap(),
        0
    );
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
    assert_eq!(
        e.spam_sigs
            .release(&mut store, &[sig], Some(PackRule::Sp2))
            .unwrap(),
        1
    );
    assert_eq!(decided(&commit(&e, &clone())), None);
    // Written: a restart doesn't bring it back.
    let again = SpamSigs::default();
    again.load(&rows(&store));
    assert_eq!(again.get(sig), None);
    // Hidden again later, it's back.
    commit(&e, &original());
    assert_eq!(decided(&commit(&e, &clone())), Some((SP4, Action::Hide)));
}

#[test]
fn your_override_sets_its_action() {
    let m = Manifest::builtin();
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    assert!(upgrade::ensure_seeded(&mut s, &m, 0).unwrap().is_some());
    let row = PackOverride {
        base: 1,
        action: Action::Flag,
        unless: None,
    }
    .encode();
    s.set_pack_override("SP-4", Some(&row)).unwrap();
    let e = load(&s);
    assert_eq!(cloned(&e), Some((SP4, Action::Flag)));
}

fn upgraded_store() -> Store {
    let m = Manifest::builtin();
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
    let seed = upgrade::seed_record(&m).encode();
    assert!(!seed.contains("SP-4"), "{seed}");
    s.write_pack_versions([Some(&seed), Some(&seed)], Some(("spam", true)))
        .unwrap();
    s
}

#[test]
fn pending_on_an_upgraded_store_applied_on_a_fresh_one() {
    let m = Manifest::builtin();
    assert_eq!(
        m.entry(PackRule::Sp4).direction(None, 1),
        Some(Direction::Widens)
    );
    assert!(!m.entry(PackRule::Sp4).baseline);
    // Upgraded: pending, in shadow only, with the original's action. Its
    // shadow holds what the applied side (nothing) doesn't: it agrees
    // with `widens`.
    let mut s = upgraded_store();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    let e = load(&s);
    let st = e.pack_versions.state(PackRule::Sp4);
    assert!(st.pending() && st.apply.is_empty());
    commit(&e, &original());
    let v = commit(&e, &clone());
    assert_eq!(decided(&v), None, "nothing applies before acceptance");
    assert!(
        v.shadow
            .iter()
            .any(|d| d.rule == SP4 && d.action == Action::Hide),
        "{:?}",
        v.shadow
    );
    let shadow = upgrade::PackShadow {
        rule: PackRule::Sp4,
        version: 1,
        applied: None,
        shadow: Some(Action::Hide),
    };
    assert!(!upgrade::disagrees(Direction::Widens, &shadow));
    // Accepted: it applies.
    upgrade::accept(&mut s, &m, &[PackRule::Sp4], 0).unwrap();
    let e = load(&s);
    assert_eq!(cloned(&e), Some((SP4, Action::Hide)));
    // Fresh: applied at once.
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    upgrade::ensure_seeded(&mut s, &m, 0).unwrap();
    let e = load(&s);
    assert!(e.pack_versions.state(PackRule::Sp4).plain);
    assert_eq!(cloned(&e), Some((SP4, Action::Hide)));
}

// ---------------------------------------------------------------------
// Storage.

fn rows(store: &Store) -> Vec<RawSetting> {
    store.settings_with_prefix_raw(PREFIX).unwrap()
}

/// The rows as stored, text or blob, as bytes.
fn snapshot(store: &Store) -> BTreeMap<String, Vec<u8>> {
    rows(store)
        .into_iter()
        .map(|(k, v)| (k, v.map_or_else(|b| b, String::into_bytes)))
        .collect()
}

const HID: Original = Original {
    rule: PackRule::Sp2,
    action: Action::Hide,
};

/// Distinct signatures, spread as hashes are.
fn sig(i: u64) -> u64 {
    let mut x = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[test]
fn the_set_survives_a_restart() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let s = SpamSigs::default();
    let demoted = Original {
        rule: PackRule::Sp3,
        action: Action::Demote,
    };
    for i in 0..10_000 {
        s.add(sig(i), if i % 3 == 0 { demoted } else { HID });
        if i % 1_000 == 999 {
            s.flush(&mut store).unwrap();
        }
    }
    let mut store_released = 0;
    for i in (0..10_000).step_by(7) {
        store_released += s.release(&mut store, &[sig(i)], None).unwrap();
    }
    assert_eq!(store_released, 1_429);
    s.flush(&mut store).unwrap();
    let again = SpamSigs::default();
    let loaded = again.load(&rows(&store));
    assert_eq!(loaded.len, s.len());
    assert!(loaded.lost.is_empty());
    for i in 0..10_000 {
        assert_eq!(again.get(sig(i)), s.get(sig(i)), "{i}");
    }
    // Blobs of at most 4 KB, of the format: 8 bytes an entry.
    for (k, v) in rows(&store) {
        let v = v.expect_err("a blob");
        assert!(v.len() <= ROW_BYTES, "{k}");
        assert_eq!(v[0], spam_sigs::FORMAT, "{k}");
        assert_eq!((v.len() - 1) % 8, 0, "{k}");
    }
}

#[test]
fn one_hide_writes_exactly_one_row() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let s = SpamSigs::default();
    for i in 0..(ROW_ENTRIES as u64 * 2 + 10) {
        let before = snapshot(&store);
        s.add(sig(i), HID);
        assert_eq!(s.flush(&mut store).unwrap(), 1, "hide {i}");
        let after = snapshot(&store);
        let changed = after
            .iter()
            .filter(|(k, v)| before.get(*k) != Some(*v))
            .count();
        assert_eq!(changed, 1, "hide {i}");
        assert!(
            before.keys().all(|k| after.contains_key(k)),
            "nothing deleted"
        );
    }
    assert_eq!(snapshot(&store).len(), 3, "rows filled then started");
    // Nothing to write: nothing written.
    assert_eq!(s.flush(&mut store).unwrap(), 0);
    // A batch of hides: one row.
    let before = snapshot(&store);
    for i in 0..100 {
        s.add(sig(1_000_000 + i), HID);
    }
    assert_eq!(s.flush(&mut store).unwrap(), 1);
    let after = snapshot(&store);
    assert_eq!(
        after
            .iter()
            .filter(|(k, v)| before.get(*k) != Some(*v))
            .count(),
        1
    );
}

#[test]
fn a_row_is_compacted_once_a_quarter_of_it_is_removed() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let s = SpamSigs::default();
    let full = ROW_ENTRIES as u64;
    for i in 0..full + 100 {
        s.add(sig(i), HID);
    }
    s.flush(&mut store).unwrap();
    let first = format!("{PREFIX}0");
    let len = |store: &Store| (snapshot(store)[&first].len() - 1) / 8;
    assert_eq!(len(&store), ROW_ENTRIES);
    // Releases append removals to the open row; the full one is left as
    // it is until a quarter of it is dead.
    let quarter = ROW_ENTRIES.div_ceil(4) as u64;
    for i in 0..quarter - 1 {
        let before = snapshot(&store)[&first].clone();
        s.release(&mut store, &[sig(i)], None).unwrap();
        assert_eq!(snapshot(&store)[&first], before, "release {i}");
    }
    s.release(&mut store, &[sig(quarter - 1)], None).unwrap();
    assert_eq!(len(&store), ROW_ENTRIES - quarter as usize, "compacted");
    // The removals in the open row cancel nothing still stored, so a
    // reload holds exactly what memory does.
    let again = SpamSigs::default();
    again.load(&rows(&store));
    assert_eq!(again.len(), s.len());
    for i in 0..full + 100 {
        assert_eq!(again.get(sig(i)).is_some(), i >= quarter, "{i}");
    }
    // A row with nothing left is deleted.
    let s = SpamSigs::default();
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    for i in 0..full + 1 {
        s.add(sig(i), HID);
    }
    s.flush(&mut store).unwrap();
    let all: Vec<u64> = (0..full).map(sig).collect();
    s.release(&mut store, &all, None).unwrap();
    let after = snapshot(&store);
    assert!(!after.contains_key(&first), "{:?}", after.keys());
    let again = SpamSigs::default();
    again.load(&rows(&store));
    assert_eq!(again.len(), 1);
    assert!(again.get(sig(full)).is_some());
}

#[test]
fn the_oldest_is_dropped_at_the_cap() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let cap = 1_000;
    let s = SpamSigs::with_cap(cap);
    for i in 0..3_000u64 {
        s.add(sig(i), HID);
        assert!(s.len() <= cap);
        if i % 100 == 0 {
            s.flush(&mut store).unwrap();
        }
    }
    s.flush(&mut store).unwrap();
    for i in 0..3_000u64 {
        assert_eq!(s.get(sig(i)).is_some(), i >= 2_000, "{i}");
    }
    // Added again, a signature counts from then: the oldest is the next
    // one dropped, not it.
    s.add(sig(2_000), HID);
    s.add(sig(5_000), HID);
    assert!(s.get(sig(2_000)).is_some() && s.get(sig(2_001)).is_none());
    s.flush(&mut store).unwrap();
    // On load the cap applies the same way.
    let again = SpamSigs::with_cap(cap);
    again.load(&rows(&store));
    assert_eq!(again.len(), cap);
    for i in 0..6_000u64 {
        assert_eq!(again.get(sig(i)), s.get(sig(i)), "{i}");
    }
    // What the cap dropped is compacted away: the rows hold little more
    // than the cap.
    let (_, entries) = again.shape();
    assert!(entries < cap * 2, "{entries}");
}

/// The build's cap, filled and overrun, as the budget test loads it.
#[test]
fn the_full_cap_holds() {
    let s = SpamSigs::default();
    assert_eq!(s.cap(), CAP);
    for i in 0..(CAP as u64 + 5_000) {
        s.add(sig(i), HID);
    }
    assert_eq!(s.len(), CAP);
    assert!(s.get(sig(4_999)).is_none() && s.get(sig(5_000)).is_some());
    let (rows, entries) = s.shape();
    assert!(rows <= CAP / ROW_ENTRIES + 3, "{rows}");
    assert!(entries < CAP + ROW_ENTRIES * 2, "{entries}");
}

#[test]
fn an_unreadable_row_loses_only_its_own_and_is_never_written_over() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let s = SpamSigs::default();
    let full = ROW_ENTRIES as u64;
    for i in 0..full * 3 {
        s.add(sig(i), HID);
    }
    s.flush(&mut store).unwrap();
    assert_eq!(snapshot(&store).len(), 3);
    // Row 1 in a newer version's format.
    let newer = "2:whatever a newer version writes";
    store
        .set_settings_text(&[(&format!("{PREFIX}1"), newer)])
        .unwrap();
    let again = SpamSigs::default();
    let loaded = again.load(&rows(&store));
    assert_eq!(loaded.lost, ["1"]);
    assert_eq!(again.labels(), ["packs.sp4.row.1"]);
    let mut e = bare();
    e.spam_sigs = std::sync::Arc::new(again);
    assert_eq!(
        e.load_spam_sigs(&rows(&store)),
        ["SP-4 signature row 1 skipped: unreadable"]
    );
    assert_eq!(e.spam_sig_labels(), ["packs.sp4.row.1"]);
    let again = &e.spam_sigs;
    for i in 0..full * 3 {
        let row = i / full;
        assert_eq!(again.get(sig(i)).is_some(), row != 1, "{i}");
    }
    // More hides and releases never touch it.
    for i in 0..full * 2 {
        again.add(sig(10_000_000 + i), HID);
    }
    again
        .release(&mut store, &[sig(0), sig(full + 1)], None)
        .unwrap();
    again.flush(&mut store).unwrap();
    assert_eq!(snapshot(&store)[&format!("{PREFIX}1")], newer.as_bytes());
    // A non-text row, and a key that isn't a row number, the same.
    store
        .update_settings_text(&[(&format!("{PREFIX}x y"), "1:")], &[])
        .unwrap();
    let mut raw = rows(&store);
    raw.push((format!("{PREFIX}2"), Err(vec![0xff])));
    let third = SpamSigs::default();
    let loaded = third.load(&raw);
    assert_eq!(loaded.lost, ["1", "2", "xy"]);
    // The newest readable row before a lost one is never appended to:
    // the next row starts past every row seen.
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let s = SpamSigs::default();
    s.add(sig(1), HID);
    s.flush(&mut store).unwrap();
    store
        .set_settings_text(&[(&format!("{PREFIX}1"), "garbage")])
        .unwrap();
    let s = SpamSigs::default();
    s.load(&rows(&store));
    s.add(sig(2), HID);
    s.flush(&mut store).unwrap();
    let snap = snapshot(&store);
    assert_eq!(snap[&format!("{PREFIX}1")], b"garbage");
    assert!(
        snap.contains_key(&format!("{PREFIX}2")),
        "{:?}",
        snap.keys()
    );
    assert_eq!(
        (snap[&format!("{PREFIX}0")].len() - 1) / 8,
        1,
        "row 0 untouched"
    );
}

#[test]
fn a_lost_row_goes_once_the_cap_has_passed_it() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let cap = ROW_ENTRIES * 2;
    let s = SpamSigs::with_cap(cap);
    for i in 0..ROW_ENTRIES as u64 * 2 {
        s.add(sig(i), HID);
    }
    s.flush(&mut store).unwrap();
    store
        .set_settings_text(&[(&format!("{PREFIX}0"), "lost")])
        .unwrap();
    let s = SpamSigs::with_cap(cap);
    assert_eq!(s.load(&rows(&store)).lost, ["0"]);
    // The cap drops something in row 1, newer than anything row 0 held:
    // row 0 is deleted and no longer listed.
    for i in 0..ROW_ENTRIES as u64 + 1 {
        s.add(sig(1_000_000 + i), HID);
    }
    s.flush(&mut store).unwrap();
    assert!(s.labels().is_empty());
    assert!(!snapshot(&store).contains_key(&format!("{PREFIX}0")));
}

// ---------------------------------------------------------------------
// Properties.

#[derive(Debug, Clone)]
enum Op {
    Add(u8, bool),
    Release(u8),
    Flush,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0u8..80, any::<bool>()).prop_map(|(s, h)| Op::Add(s, h)),
        2 => (0u8..80).prop_map(Op::Release),
        1 => Just(Op::Flush),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Whatever the order of hides, releases and flushes, a reload holds
    /// what memory held, with the same originals; and never a released
    /// signature. Anything more it holds is something the cap dropped,
    /// back after releases took the set below its cap: strict.
    #[test]
    fn a_reload_holds_what_memory_held(
        cap in 1usize..60,
        ops in proptest::collection::vec(op(), 1..400),
    ) {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let s = SpamSigs::with_cap(cap);
        let mut released: HashSet<u8> = HashSet::new();
        let mut model: HashMap<u8, Original> = HashMap::new();
        for op in ops {
            match op {
                Op::Add(i, hide) => {
                    let o = if hide { HID } else {
                        Original { rule: PackRule::Sp3, action: Action::Demote }
                    };
                    s.add(sig(u64::from(i)), o);
                    released.remove(&i);
                    model.insert(i, o);
                }
                Op::Release(i) => {
                    s.release(&mut store, &[sig(u64::from(i))], None).unwrap();
                    released.insert(i);
                    model.remove(&i);
                }
                Op::Flush => {
                    s.flush(&mut store).unwrap();
                }
            }
            prop_assert!(s.len() <= cap);
        }
        s.flush(&mut store).unwrap();
        let again = SpamSigs::with_cap(cap);
        again.load(&rows(&store));
        prop_assert!(again.len() <= cap);
        for i in 0..80u8 {
            let (mem, re) = (s.get(sig(u64::from(i))), again.get(sig(u64::from(i))));
            if let Some(m) = mem {
                prop_assert_eq!(Some(m), re, "{} in memory", i);
                prop_assert_eq!(model.get(&i), Some(&m));
            } else if released.contains(&i) {
                prop_assert_eq!(re, None, "{} was released", i);
            } else if let Some(r) = re {
                prop_assert_eq!(model.get(&i), Some(&r), "{} came back as it was", i);
            }
        }
        for (k, v) in snapshot(&store) {
            prop_assert!(v.len() <= ROW_BYTES, "{}", k);
        }
    }

    /// No row, however damaged and whatever its number, makes a load
    /// panic or lets the set grow past its cap, and adds and releases
    /// after it still work.
    #[test]
    fn a_damaged_row_never_panics(
        rows in proptest::collection::vec(
            (any::<u64>(), any::<bool>(), proptest::collection::vec(any::<u8>(), 0..80)),
            0..8,
        ),
    ) {
        let s = SpamSigs::with_cap(16);
        let rows: Vec<RawSetting> = rows
            .into_iter()
            .map(|(n, tagged, mut v)| {
                if tagged {
                    v.insert(0, spam_sigs::FORMAT);
                }
                (format!("{PREFIX}{}", n % 64 + if n % 3 == 0 { u64::MAX - 64 } else { 0 }), Err(v))
            })
            .collect();
        s.load(&rows);
        prop_assert!(s.len() <= 16);
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        for i in 0..40 {
            s.add(sig(i), HID);
        }
        s.release(&mut store, &[sig(39)], None).unwrap();
        prop_assert!(s.len() <= 16);
        prop_assert!(s.get(sig(38)).is_some());
    }
}

/// The review's cases (#510): a key just below the index's bound, and a
/// readable row just below the band rows are numbered in. The first is a
/// lost key that never sets where numbering starts. After the second the
/// numbers run out: the set stops growing and says so, and never starts
/// a row a restart wouldn't read. Either way a restart reads back
/// everything written, and nothing new is lost.
#[test]
fn keys_near_the_bound_never_break_a_restart() {
    for n in [spam_sigs::MAX_ROW - 1, spam_sigs::ROW_BAND - 1] {
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let near = format!("{PREFIX}{n}");
        store
            .update_settings_raw(&[(&near, &[spam_sigs::FORMAT][..])], &[])
            .unwrap();
        let s = SpamSigs::default();
        let lost = s.load(&rows(&store)).lost;
        let stray = n == spam_sigs::MAX_ROW - 1;
        assert_eq!(
            lost,
            if stray {
                vec![format!("key-{n}")]
            } else {
                vec![]
            }
        );
        for i in 0..(ROW_ENTRIES as u64 * 3) {
            s.add(sig(i), HID);
        }
        s.flush(&mut store).unwrap();
        if stray {
            assert!(
                snapshot(&store)[&near] == [spam_sigs::FORMAT],
                "{n}: left alone"
            );
            assert_eq!(s.len(), ROW_ENTRIES * 3);
        } else {
            // The one row left is filled, then the set stops: listed.
            assert_eq!(s.len(), ROW_ENTRIES);
            assert_eq!(s.labels(), ["packs.sp4.row.numbers"]);
        }
        let again = SpamSigs::default();
        let loaded = again.load(&rows(&store));
        assert_eq!(loaded.lost, lost, "{n}: nothing new lost");
        assert_eq!(again.len(), s.len(), "{n}");
        for i in 0..(ROW_ENTRIES as u64 * 3) {
            assert_eq!(
                again.get(sig(i)).is_some(),
                s.get(sig(i)).is_some(),
                "{n}: {i}"
            );
        }
    }
}

/// A key past the row numbers this build uses (corrupted, or another
/// build's numbering) is listed and left alone: it never sets where new
/// rows start, so they can't wrap or land on it.
#[test]
fn a_row_number_past_the_bound_is_lost_and_left_alone() {
    let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
    let far = format!("{PREFIX}{}", u64::MAX);
    store
        .update_settings_raw(&[(&far, &[spam_sigs::FORMAT][..])], &[])
        .unwrap();
    let s = SpamSigs::with_cap(4);
    let loaded = s.load(&rows(&store));
    // Not a row number this build writes: a key, sanitized, and never
    // a bare number a row's label could take.
    assert_eq!(loaded.lost, [format!("key-{}", u64::MAX)]);
    for i in 0..(ROW_ENTRIES as u64 * 3) {
        s.add(sig(i), HID);
        s.flush(&mut store).unwrap();
    }
    assert_eq!(s.len(), 4);
    let snap = snapshot(&store);
    assert_eq!(snap[&far], [spam_sigs::FORMAT]);
    assert!(snap.contains_key(&format!("{PREFIX}0")) || snap.contains_key(&format!("{PREFIX}2")));
    assert!(
        snap.keys()
            .all(|k| k == &far || k.len() <= PREFIX.len() + 2),
        "{:?}",
        snap.keys()
    );
}

#[test]
fn the_api_constants_hold() {
    assert_eq!(spam_sigs::CAP, 1_000_000);
    assert_eq!(ROW_ENTRIES, 511);
    const { assert!(ROW_BYTES <= MAX_ROW_BYTES) };
    assert_eq!(PREFIX, "filter.sp4.sigs.");
}

// ---------------------------------------------------------------------
// The budget.

/// The filter budget with SP-4's set at its cap (RP-G checks it in
/// `props.rs`): prepare's judgment never reads the set, so a full set
/// costs it nothing; the writer's lookup is one hash of the payload and
/// one map read. Ignored: a measurement. Run: `cargo test --release -p
/// lodestar-filter --test sp4 -- --ignored --nocapture budget`.
#[test]
#[ignore = "a measurement"]
fn budget_with_the_set_at_its_cap() {
    use std::time::Instant;
    let e = engine();
    let set: Vec<Torrent> = (0..512u64)
        .map(|i| {
            torrent(
                [i as u8; 20],
                &format!("Some Show S01E{:02} 1080p WEB x264-GRP{i}", i % 50),
                &[("e.mkv", 1_000_000_007 + i), ("e.nfo", 3_001 + i)],
            )
        })
        .collect();
    let judge_all = |e: &Engine| {
        let t = Instant::now();
        for _ in 0..5 {
            for x in &set {
                let class = classify(&x.name, &x.files);
                let facts = Facts {
                    class: &class,
                    size: 0,
                    piece_len: 0,
                    flags: 0,
                    discovered_at: 0,
                    now: 0,
                };
                let hashes = [x.hash];
                let item = Item {
                    hashes: &hashes,
                    name: &x.name,
                    files: &x.files,
                    content_type: class.content_type,
                };
                std::hint::black_box(e.judge(&item, Some(&facts), 0));
            }
        }
        t.elapsed().as_secs_f64() * 1e6 / (5 * set.len()) as f64
    };
    // A warm-up pass, so both times are warm.
    judge_all(&e);
    let empty = judge_all(&e);
    for i in 0..CAP as u64 {
        e.spam_sigs.add(sig(i), HID);
    }
    assert_eq!(e.spam_sigs.len(), CAP);
    let full = judge_all(&e);
    let t = Instant::now();
    let mut hits = 0;
    for x in set.iter().cycle().take(100_000) {
        hits += usize::from(
            usable(payload_sig(&x.files))
                .and_then(|s| e.spam_sigs.get(s))
                .is_some(),
        );
    }
    let lookup = t.elapsed().as_secs_f64() * 1e6 / 100_000.0;
    println!(
        "judge: {empty:.2} µs with the set empty, {full:.2} µs at its cap ({CAP}); \
         the writer's lookup at the cap: {lookup:.3} µs ({hits} hits)"
    );
    assert!(full < 50.0, "the 50 µs budget");
    assert!(lookup < 1.0, "the lookup");
}

// ---------------------------------------------------------------------
// Memory (X-sp4-followups): the set at its cap.

/// The process's resident memory in bytes (Linux), or `None` elsewhere.
fn resident() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: f64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024.0)
}

/// The rows a set at its cap stores, built directly: full rows of adds,
/// as this build writes them.
fn rows_at_the_cap() -> Vec<RawSetting> {
    (0..CAP as u64)
        .collect::<Vec<_>>()
        .chunks(ROW_ENTRIES)
        .enumerate()
        .map(|(n, c)| {
            let mut b = vec![spam_sigs::FORMAT];
            for i in c {
                b.extend_from_slice(&((sig(*i) & !7) | spam_sigs::HIDE).to_le_bytes());
            }
            (format!("{PREFIX}{n}"), Err(b))
        })
        .collect()
}

/// The set's memory at its cap, loaded from its rows, as resident memory
/// grows (so a load's passing peak counts too); and the time of a lookup
/// that hits and one that misses, over keys spread across the whole set,
/// so the caches don't hide the index's reads. The brief's gate: at most
/// 25 MB at the cap. Linux only. Run alone, in its own process and in
/// release: `cargo test --release -p lodestar-filter --test sp4 --
/// --ignored --nocapture --exact memory_at_the_cap_loaded`.
#[test]
#[ignore = "a measurement: run alone, in release"]
fn memory_at_the_cap_loaded() {
    use std::time::Instant;
    if resident().is_none() {
        return;
    }
    let stored = rows_at_the_cap();
    let before = resident().unwrap();
    let loaded = SpamSigs::default();
    assert_eq!(loaded.load(&stored).len, CAP);
    let mb = (resident().unwrap() - before) / 1e6;
    let probes: Vec<u64> = (0..1_000_000u64)
        .map(|i| sig(sig(i) % CAP as u64))
        .collect();
    let misses: Vec<u64> = (0..1_000_000u64).map(|i| sig(1 << 40 | i)).collect();
    let time = |keys: &[u64]| {
        let t = Instant::now();
        let mut found = 0usize;
        for k in keys {
            found += usize::from(std::hint::black_box(loaded.get(*k)).is_some());
        }
        (t.elapsed().as_secs_f64() * 1e9 / keys.len() as f64, found)
    };
    time(&probes);
    let (hit, found) = time(&probes);
    let (miss, none) = time(&misses);
    assert_eq!((found, none), (probes.len(), 0));
    println!(
        "loaded at the cap ({CAP}): {mb:.1} MB; a lookup: {hit:.0} ns a hit, {miss:.0} ns a miss"
    );
    assert!(mb <= 25.0, "the 25 MB gate");
}

/// The set's memory at its cap, filled by adds (and 10,000 past it, so
/// the cap's evictions and compactions run), as resident memory grows.
/// Run alone, in its own process and in release: `cargo test --release
/// -p lodestar-filter --test sp4 -- --ignored --nocapture --exact
/// memory_at_the_cap_added`.
#[test]
#[ignore = "a measurement: run alone, in release"]
fn memory_at_the_cap_added() {
    if resident().is_none() {
        return;
    }
    let before = resident().unwrap();
    let added = SpamSigs::default();
    for i in 0..(CAP as u64 + 10_000) {
        added.add(sig(i), HID);
    }
    assert_eq!(added.len(), CAP);
    let mb = (resident().unwrap() - before) / 1e6;
    println!("added at the cap ({CAP}): {mb:.1} MB");
    assert!(mb <= 25.0, "the 25 MB gate");
}
