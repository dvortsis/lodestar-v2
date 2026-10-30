//! Three content-filter semantics through the full engine (§20.6, §20.9,
//! §20.12), with neutral stand-in words:
//! - an exception to one subscribed list leaves every other list's
//!   action in force, before and after the fetch;
//! - a stored torrent's classification rules read its swarm hashes, as
//!   ingest does;
//! - a neutralized word supplies no evidence inside a phrase, NEAR, a
//!   dotted word, a prefix or a fuzzy word.

use std::collections::HashSet;
use std::sync::Arc;

use lodestar_classify::{ContentType, Correction, FileInfo, classify};
use lodestar_filter::reclassify::reclassify_row;
use lodestar_filter::{
    Action, ClassRule, ClassRules, Core, CsList, Decision, Engine, Exception, Facts, Item,
    ListLayer, Lists, Match, Override, RuleRef, Strength, Switches, TermSet, UserRule,
};
use lodestar_search::parse;
use lodestar_store::TorrentRow;
use proptest::prelude::*;

fn engine() -> Engine {
    let lists = Lists {
        sexual: TermSet::default(),
        child_roles: TermSet::default(),
        codes: TermSet::default(),
    };
    Engine::new(Core::new(lists, CsList::default(), Switches::default()))
}

const H: [u8; 20] = [7; 20];

fn item<'a>(hashes: &'a [[u8; 20]], name: &'a str) -> Item<'a> {
    Item {
        hashes,
        name,
        files: &[],
        content_type: ContentType::Other,
    }
}

fn list(e: &mut Engine, id: u32, action: Action, hashes: &[[u8; 20]]) {
    let layer = ListLayer {
        id,
        action,
        hashes: Arc::new(hashes.iter().copied().collect::<HashSet<_>>()),
    };
    e.set_list(Some(layer), id);
}

fn except_list(id: u32, hashes: &[[u8; 20]]) -> Exception {
    let ex = Exception {
        id,
        rule: RuleRef::List(id),
        what: Match::Hashes(hashes.to_vec()),
    };
    ex.validate().unwrap();
    ex
}

fn decided(e: &Engine, hashes: &[[u8; 20]], name: &str) -> Option<(RuleRef, Action)> {
    e.judge(&item(hashes, name), None, 0)
        .decision
        .map(|d| (d.rule, d.action))
}

// ---------------------------------------------------------------------
// A3-1: an exception covers its own list only.

/// The review's first probe: L-1 quarantines H, L-2 drops it, and an
/// exception for H under L-2 leaves L-1's quarantine.
#[test]
fn an_exception_to_one_list_leaves_the_others() {
    let mut e = engine();
    list(&mut e, 1, Action::Quarantine, &[H]);
    list(&mut e, 2, Action::Drop, &[H]);
    assert_eq!(
        decided(&e, &[H], "Amber Beacon"),
        Some((RuleRef::List(2), Action::Drop))
    );
    e.exceptions.push(except_list(2, &[H]));
    // L-1 doesn't stop the fetch (it quarantines), so this is fetched
    // and judged on its metadata, where L-1 still applies.
    assert!(e.blocked_hash(&[H]).is_none());
    assert_eq!(
        decided(&e, &[H], "Amber Beacon"),
        Some((RuleRef::List(1), Action::Quarantine))
    );
    // Excepted from both: nothing left.
    e.exceptions.push(except_list(1, &[H]));
    assert_eq!(decided(&e, &[H], "Amber Beacon"), None);
}

#[test]
fn prefetch_stays_blocked_by_a_list_that_isnt_excepted() {
    let mut e = engine();
    list(&mut e, 1, Action::Drop, &[H]);
    list(&mut e, 2, Action::Drop, &[H]);
    assert_eq!(e.blocked_hash(&[H]).unwrap().rule, RuleRef::List(2));
    e.exceptions.push(except_list(2, &[H]));
    // L-1 still drops H: no fetch.
    let d = e.blocked_hash(&[H]).unwrap();
    assert_eq!((d.rule, d.action), (RuleRef::List(1), Action::Drop));
    assert_eq!(
        decided(&e, &[H], "x"),
        Some((RuleRef::List(1), Action::Drop))
    );
    // An exception to L-1 by text could still release it: fetch, then
    // judge every list on the metadata.
    e.exceptions.push(Exception {
        id: 3,
        rule: RuleRef::List(1),
        what: Match::Query(parse("amber").unwrap()),
    });
    assert!(e.blocked_hash(&[H]).is_none());
    assert_eq!(
        decided(&e, &[H], "Cedar"),
        Some((RuleRef::List(1), Action::Drop))
    );
    assert_eq!(decided(&e, &[H], "Amber"), None);
    // A list H isn't on never blocks it.
    let other = [8; 20];
    assert!(e.blocked_hash(&[other]).is_none());
}

#[test]
fn a_list_exception_leaves_packs_rules_and_the_core_alone() {
    let mut e = engine();
    list(&mut e, 2, Action::Drop, &[H]);
    e.rules.push(UserRule {
        id: 1,
        query: parse("amber").unwrap(),
        action: Action::Hide,
        enabled: true,
        shadow_until: None,
        unless: None,
    });
    e.exceptions.push(except_list(2, &[H]));
    assert_eq!(
        decided(&e, &[H], "Amber Beacon"),
        Some((RuleRef::User(1), Action::Hide))
    );
    e.core.cs.add_hash(H, lodestar_filter::Origin::Confirmed);
    assert!(matches!(
        e.blocked_hash(&[H]).map(|d| d.rule),
        Some(RuleRef::Core(_))
    ));
}

// ---------------------------------------------------------------------
// A3-2: stored classification rules read the swarm hashes.

fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// A stored single-file video, hybrid when `v2` is given.
fn row(v1: [u8; 20], v2: Option<[u8; 32]>) -> TorrentRow {
    TorrentRow {
        id: 1,
        info_hash: hex(&v1).parse().unwrap(),
        info_hash_v2: v2.map(|h| hex(&h).parse().unwrap()),
        name: "Amber Beacon.mkv".into(),
        name_raw: None,
        size: 1 << 30,
        piece_len: 1 << 20,
        piece_count: 1024,
        file_count: 1,
        info_size: 100,
        flags: 0,
        discovered_at: 0,
        metadata_at: 0,
        last_seen_at: 0,
        sources: 0,
        first_source: None,
    }
}

/// Every swarm hash of the row, as ingest has them: this mirrors
/// `lodestar_store::Prepared::swarm_hashes`, which ingest judges with.
fn swarm(r: &TorrentRow) -> Vec<[u8; 20]> {
    let mut out = vec![r.info_hash.0];
    if let Some(t) = r.info_hash_v2.map(|h| h.truncated()) {
        if t != r.info_hash {
            out.push(t.0);
        }
    }
    out
}

fn class_engine(rules: &[(&str, bool)]) -> Engine {
    let mut e = engine();
    e.class_rules = ClassRules::new(
        rules
            .iter()
            .enumerate()
            .map(|(i, (q, adult))| ClassRule {
                id: i as u32 + 1,
                query: parse(q).unwrap(),
                adult: *adult,
            })
            .collect(),
    );
    e
}

/// What ingest's classification rules say about the row.
fn at_ingest(e: &Engine, r: &TorrentRow, files: &[FileInfo]) -> Option<bool> {
    let class = classify(&r.name, files);
    let facts = Facts {
        class: &class,
        size: r.size,
        piece_len: r.piece_len,
        flags: r.flags,
        discovered_at: r.discovered_at,
        now: 1000,
    };
    let hashes = swarm(r);
    let item = Item {
        hashes: &hashes,
        name: &r.name,
        files,
        content_type: class.content_type,
    };
    e.judge_classified(&item, Some(&facts), 1000).1
}

fn stored(e: &Engine, r: &TorrentRow, own: Correction) -> ContentType {
    let files = vec![FileInfo::new(vec![r.name.clone()], r.size)];
    reclassify_row(r, &files, own, false, Some(e), 1000)
        .class
        .content_type
}

/// The review's fourth probe, with negation, a hybrid's v2 hash and your
/// own correction.
#[test]
fn stored_hash_rules_match_ingest() {
    let v1 = [0xa1; 20];
    let r = row(v1, None);
    let files = vec![FileInfo::new(vec![r.name.clone()], r.size)];
    assert_ne!(
        classify(&r.name, &files).content_type,
        ContentType::Adult,
        "detection calls it a video"
    );
    let e = class_engine(&[(&format!("hash:{}", hex(&v1)), true)]);
    assert_eq!(at_ingest(&e, &r, &files), Some(true));
    assert_eq!(stored(&e, &r, Correction::default()), ContentType::Adult);

    // Negated: true for every other torrent, false for this one.
    let e = class_engine(&[(&format!("beacon -hash:{}", hex(&v1)), true)]);
    assert_eq!(at_ingest(&e, &r, &files), None);
    assert_ne!(stored(&e, &r, Correction::default()), ContentType::Adult);
    let other = row([0xb2; 20], None);
    assert_eq!(
        stored(&e, &other, Correction::default()),
        ContentType::Adult
    );

    // A hybrid is matched by its truncated v2 hash too.
    let v2 = [0xc3; 32];
    let hybrid = row(v1, Some(v2));
    let e = class_engine(&[(&format!("hash:{}", hex(&v2[..20])), true)]);
    assert_eq!(at_ingest(&e, &hybrid, &files), Some(true));
    assert_eq!(
        stored(&e, &hybrid, Correction::default()),
        ContentType::Adult
    );

    // Your own correction comes first.
    let not_adult = Correction {
        adult: Some(false),
        ..Correction::default()
    };
    assert_ne!(stored(&e, &hybrid, not_adult), ContentType::Adult);
}

// ---------------------------------------------------------------------
// A3-3: a neutralized word supplies no evidence.

fn neutralize(word: &str) -> Override {
    let o = Override {
        id: 1,
        what: Match::Keyword(word.into()),
        strength: Strength::Neutralize,
        scope: None,
        expires_at: None,
        shadow_until: None,
    };
    o.validate().unwrap();
    o
}

fn hide(query: &str, unless: Option<&str>) -> UserRule {
    UserRule {
        id: 1,
        query: parse(query).unwrap(),
        action: Action::Hide,
        enabled: true,
        shadow_until: None,
        unless: unless.map(|u| parse(u).unwrap()),
    }
}

/// Whether the one rule hides `name`, with `neutral` neutralized.
fn hides(query: &str, unless: Option<&str>, neutral: Option<&str>, name: &str) -> bool {
    let mut e = engine();
    e.rules.push(hide(query, unless));
    e.overrides.extend(neutral.map(neutralize));
    let d: Option<Decision> = e.judge(&item(&[H], name), None, 0).decision;
    d.is_some_and(|d| d.rule == RuleRef::User(1))
}

/// The review's phrase and NEAR probes, and the other forms a word takes.
#[test]
fn a_neutralized_word_never_completes_a_clause() {
    for q in [
        "\"amber beacon\"",
        "amber NEAR/1 beacon",
        "beacon NEAR/3 amber",
        "amber.beacon",
        "name:\"amber beacon\"",
        "\"cedar amber beacon\" | delta",
        "amb*",
        "~ambar",
        "file:(\"amber beacon\")",
    ] {
        let name = "Cedar Amber Beacon";
        let files = [FileInfo::new(vec!["Amber Beacon.mkv".into()], 1 << 30)];
        let judge = |neutral: Option<&str>| {
            let mut e = engine();
            e.rules.push(hide(q, None));
            e.overrides.extend(neutral.map(neutralize));
            let it = Item {
                files: &files,
                ..item(&[H], name)
            };
            e.judge(&it, None, 0).decision.map(|d| d.rule)
        };
        assert_eq!(judge(None), Some(RuleRef::User(1)), "{q} matches");
        assert_eq!(judge(Some("amber")), None, "{q}: amber is neutralized");
        // Neutralizing another word changes nothing.
        assert_eq!(judge(Some("delta")), Some(RuleRef::User(1)), "{q}");
    }
}

#[test]
fn other_evidence_still_counts() {
    // The phrase still matches where its words appear without help.
    assert!(!hides(
        "\"amber beacon\"",
        None,
        Some("amber"),
        "Amber Beacon"
    ));
    assert!(hides("beacon", None, Some("amber"), "Amber Beacon"));
    assert!(hides(
        "beacon NEAR/2 cedar",
        None,
        Some("amber"),
        "Cedar Amber Beacon"
    ));
    // A prefix or fuzzy word another token satisfies still matches.
    assert!(hides("amb*", None, Some("amber"), "Amber Ambient"));
    assert!(hides("~ambar", None, Some("amber"), "Amber Ambar"));
    // A whole or compound form is a token of its own: neutralizing `x`
    // leaves the word `x265`, which a torrent has as one token too. A
    // phrase reads parts only, so the `x` of `x265` can't complete it.
    assert!(hides("x265 beacon", None, Some("x"), "x265 Beacon"));
    assert!(hides("x265 NEAR/1 beacon", None, Some("x"), "x265 Beacon"));
    assert!(!hides("\"x265 beacon\"", None, Some("x"), "x265 Beacon"));
    // A phrase is never one word: neutralizing `amberbeacon` leaves
    // `"amber beacon"` on a torrent that hasn't that word.
    assert!(hides(
        "\"amber beacon\"",
        None,
        Some("amberbeacon"),
        "Amber Beacon"
    ));
    assert!(hides(
        "-\"amber beacon\"",
        None,
        Some("amberbeacon"),
        "Cedar"
    ));
    assert!(!hides(
        "-\"amber beacon\"",
        None,
        Some("amberbeacon"),
        "Amber Beacon"
    ));
}

/// A query word of several parts can't borrow a neutralized word the
/// torrent has on its own.
#[test]
fn a_word_of_several_parts_needs_its_own_token() {
    for q in [
        "AmberBeacon",
        "\"cedar AmberBeacon\"",
        "AmberBeacon NEAR/1 cedar",
        "amber2",
    ] {
        let name = "Cedar Amber Beacon amber 2";
        assert!(hides(q, None, None, name), "{q}");
        assert!(!hides(q, None, Some("amber"), name), "{q}");
    }
    // The torrent's own word of that spelling still matches.
    assert!(hides(
        "AmberBeacon",
        None,
        Some("amber"),
        "Cedar AmberBeacon"
    ));
    assert!(hides("amber2", None, Some("amber"), "Cedar amber2"));
}

#[test]
fn negation_and_unless_with_a_neutralized_word() {
    let name = "Amber Beacon";
    // A neutralized phrase or NEAR is false, so its negation is true, as
    // for a bare word.
    for q in [
        "beacon -\"amber beacon\"",
        "beacon -(amber NEAR/1 beacon)",
        "beacon -amber",
    ] {
        assert!(!hides(q, None, None, name), "{q}");
        assert!(hides(q, None, Some("amber"), name), "{q}");
    }
    // The rule's own query honours neutralizing beside an `unless`.
    for q in ["\"amber beacon\"", "amber NEAR/1 beacon"] {
        assert!(hides(q, Some("cedar"), None, name), "{q}");
        assert!(!hides(q, Some("cedar"), Some("amber"), name), "{q}");
        assert!(!hides(q, Some("beacon"), None, name), "{q}");
    }
    // An `unless` is an exception to its rule (§20.12): like every
    // exception, it reads the torrent as it is, so a neutralized word
    // can't take a rule's exception away.
    for u in ["\"amber beacon\"", "amber NEAR/1 beacon", "amber"] {
        assert!(!hides("beacon", Some(u), Some("amber"), name), "{u}");
        assert!(hides("beacon", Some(u), Some("amber"), "Beacon"), "{u}");
    }
}

// ---------------------------------------------------------------------
// Properties.

const VOCAB: &[&str] = &["amber", "beacon", "cedar", "delta", "ember"];

fn word() -> impl Strategy<Value = &'static str> {
    prop::sample::select(VOCAB.to_vec())
}

/// A query of every form a word takes, nested.
fn query() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        word().prop_map(str::to_owned),
        (word(), word()).prop_map(|(a, b)| format!("\"{a} {b}\"")),
        (word(), word(), word()).prop_map(|(a, b, c)| format!("\"{a} {b} {c}\"")),
        (word(), word(), 1u32..4).prop_map(|(a, b, n)| format!("{a} NEAR/{n} {b}")),
        (word(), word()).prop_map(|(a, b)| format!("{a}.{b}")),
        (word(), word()).prop_map(|(a, b)| format!("{}{b}", capital(a))),
        (word(), 1u8..10).prop_map(|(w, n)| format!("{w}{n}")),
        (word(), 2usize..5).prop_map(|(w, n)| format!("{}*", &w[..n])),
        word().prop_map(|w| format!("~{}a", &w[..w.len() - 1])),
        (word(), word()).prop_map(|(a, b)| format!("name:\"{a} {b}\"")),
    ];
    leaf.prop_recursive(3, 10, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 2..4).prop_map(|v| v.join(" ")),
            prop::collection::vec(inner.clone(), 2..4).prop_map(|v| format!("({})", v.join(" | "))),
            inner.prop_map(|q| format!("-({q})")),
        ]
    })
}

fn capital(w: &str) -> String {
    let mut c = w.chars();
    c.next()
        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
        .unwrap_or_default()
}

/// A name of vocabulary words between separators, some joined
/// ([`joined`]).
fn name() -> impl Strategy<Value = Vec<(&'static str, &'static str)>> {
    prop::collection::vec(
        (word(), prop::sample::select(vec![" ", ".", "_", "", "2 "])),
        1..8,
    )
}

/// The name, with `swap` replacing every `from`. A neutralized word
/// takes out every token of its text, a part of a joined word too, so
/// `from` is never joined here: the swap then removes exactly what
/// neutralizing does. Other words join with a capital (`CedarBeacon`)
/// or a digit (`cedar2`).
fn joined(words: &[(&str, &str)], swap: Option<(&str, &str)>) -> String {
    let mut out = String::new();
    for (i, (w, sep)) in words.iter().enumerate() {
        let joins = |other: &str| *w == "amber" || other == "amber";
        if i > 0 {
            let prev = words[i - 1].0;
            out.push_str(match *sep {
                "" | "2 " if joins(prev) => " ",
                s => s,
            });
        }
        let w = match swap {
            Some((from, to)) if *w == from => to,
            _ => w,
        };
        if i > 0 && sep.is_empty() && !joins(words[i - 1].0) {
            out.push_str(&capital(w));
        } else {
            out.push_str(w);
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a) Adding an exception for list X never changes the verdict any
    /// other list contributes: the engine judges as if X didn't have the
    /// torrent, before the fetch and after.
    #[test]
    fn a_list_exception_is_the_list_without_the_torrent(
        lists in prop::collection::vec(
            (prop::sample::select(vec![
                Action::Flag, Action::Demote, Action::Hide, Action::Quarantine, Action::Drop,
            ]), any::<bool>()),
            1..6,
        ),
        x in 0usize..6,
        others in prop::collection::vec(any::<bool>(), 6),
    ) {
        let x = (x % lists.len()) as u32 + 1;
        let build = |drop_x: bool, except_x: bool| {
            let mut e = engine();
            for (i, (action, has)) in lists.iter().enumerate() {
                let id = i as u32 + 1;
                let held = *has && !(drop_x && id == x);
                list(&mut e, id, *action, if held { &[H] } else { &[] });
                // Exceptions other lists already have stay in both.
                if id != x && others[i] {
                    e.exceptions.push(except_list(id, &[H]));
                }
            }
            if except_x {
                e.exceptions.push(except_list(x, &[H]));
            }
            e
        };
        let excepted = build(false, true);
        let without = build(true, false);
        prop_assert_eq!(
            decided(&excepted, &[H], "Amber"),
            decided(&without, &[H], "Amber")
        );
        prop_assert_eq!(excepted.blocked_hash(&[H]), without.blocked_hash(&[H]));
        // And never weaker than any list left un-excepted.
        let strongest = lists
            .iter()
            .enumerate()
            .filter(|(i, (_, has))| *has && *i as u32 + 1 != x && !others[*i])
            .map(|(_, (a, _))| *a)
            .max();
        prop_assert_eq!(decided(&excepted, &[H], "Amber").map(|d| d.1), strongest);
    }

    /// (b) A stored torrent's classification equals what ingest's
    /// classification rules make of the same torrent.
    #[test]
    fn stored_classification_equals_ingest(
        v1 in prop::sample::select(vec![[0xa1u8; 20], [0xb2; 20]]),
        v2 in prop::option::of(prop::sample::select(vec![[0xc3u8; 32], [0xa1; 32]])),
        rules in prop::collection::vec(
            (
                prop::sample::select(vec![0usize, 1, 2]),
                any::<bool>(),
                prop::sample::select(vec!["", "beacon ", "cedar | "]),
                any::<bool>(),
            ),
            1..4,
        ),
    ) {
        let hashes = [hex(&[0xa1; 20]), hex(&[0xb2; 20]), hex(&[0xc3; 20])];
        let rules: Vec<(String, bool)> = rules
            .iter()
            .map(|(h, neg, with, adult)| {
                let not = if *neg { "-" } else { "" };
                (format!("{with}{not}hash:{}", hashes[*h]), *adult)
            })
            .collect();
        let refs: Vec<(&str, bool)> = rules.iter().map(|(q, a)| (q.as_str(), *a)).collect();
        let e = class_engine(&refs);
        let r = row(v1, v2);
        let files = vec![FileInfo::new(vec![r.name.clone()], r.size)];
        let says = at_ingest(&e, &r, &files);
        let detected = classify(&r.name, &files).content_type == ContentType::Adult;
        let adult = stored(&e, &r, Correction::default()) == ContentType::Adult;
        prop_assert_eq!(adult, says.unwrap_or(detected), "{:?}", rules);
        // A correction of its own still decides.
        for own in [false, true] {
            let fix = Correction { adult: Some(own), ..Correction::default() };
            prop_assert_eq!(stored(&e, &r, fix) == ContentType::Adult, own);
        }
    }

    /// (c) A neutralized word never supplies evidence, in any form: a
    /// rule judges the torrent as it would with that word replaced by one
    /// no rule mentions, in the same place.
    #[test]
    fn a_neutralized_word_is_no_evidence(q in query(), words in name()) {
        let mut e = engine();
        e.rules.push(hide(&q, None));
        let plain = e.clone();
        e.overrides.push(neutralize("amber"));
        let n = joined(&words, None);
        let swapped = joined(&words, Some(("amber", "zulu")));
        let rule = |e: &Engine, name: &str| e.judge(&item(&[H], name), None, 0).decision;
        prop_assert_eq!(rule(&e, &n), rule(&plain, &swapped), "{} on {}", q, n);
    }
}

/// The security review's note: neutralizing a word an allow query needs
/// absent never makes it release more.
#[test]
fn neutralizing_never_widens_always_allow() {
    for allow in [
        "foo -\"bar baz\"",
        "foo -(bar NEAR/1 baz)",
        "foo -bar.baz",
        "foo -bar",
    ] {
        let mut e = engine();
        list(&mut e, 1, Action::Hide, &[H]);
        e.allow.push(lodestar_filter::Allow {
            id: 1,
            query: parse(allow).unwrap(),
        });
        let listed = Some((RuleRef::List(1), Action::Hide));
        assert_eq!(decided(&e, &[H], "foo bar baz"), listed, "{allow}");
        assert_eq!(decided(&e, &[H], "foo qux"), None, "{allow}");
        e.overrides.push(neutralize("bar"));
        assert_eq!(decided(&e, &[H], "foo bar baz"), listed, "{allow}");
        assert_eq!(decided(&e, &[H], "foo qux"), None, "{allow}");
    }
}

/// The locked floor (CS-1a, CS-1b, CS-8a) comes before everything these
/// fixes touch: neutralizing, Always allow, list exceptions and
/// classification rules never reach it.
#[test]
fn the_floor_is_untouched() {
    use lodestar_filter::{CoreRule, Origin, payload_sig};
    let (confirmed, imported) = ([0x11; 20], [0x22; 20]);
    let payload = vec![
        FileInfo::new(vec!["x".into(), "Amber Beacon.mkv".into()], 734_003_201),
        FileInfo::new(vec!["x".into(), "Amber Beacon.srt".into()], 81_233),
    ];
    let mut e = engine();
    e.core.cs.add_hash(confirmed, Origin::Confirmed);
    e.core.cs.add_hash(imported, Origin::Imported);
    e.core.cs.add_sig(payload_sig(&payload).unwrap().sig);
    // Every release this change touches, all matching the torrent.
    e.overrides.push(neutralize("amber"));
    e.overrides.push(Override {
        id: 2,
        what: Match::Keyword("beacon".into()),
        strength: Strength::Override,
        ..neutralize("amber")
    });
    e.allow.push(lodestar_filter::Allow {
        id: 1,
        query: parse("beacon -\"amber beacon\"").unwrap(),
    });
    for h in [confirmed, imported, H] {
        list(&mut e, 1, Action::Hide, &[h]);
    }
    e.exceptions.push(except_list(1, &[confirmed, imported, H]));
    e.class_rules = class_engine(&[("amber", false)]).class_rules;
    for (hashes, files, rule) in [
        (&[confirmed][..], &[][..], CoreRule::Cs1a),
        (&[imported][..], &[][..], CoreRule::Cs1b),
        (&[H][..], &payload[..], CoreRule::Cs8a),
    ] {
        let it = Item {
            files,
            ..item(hashes, "Amber Beacon")
        };
        let d = e.judge(&it, None, 0).decision.unwrap();
        assert_eq!(d.rule, RuleRef::Core(rule), "{rule:?}");
        let (v, _) = e.judge_classified(&it, None, 0);
        assert_eq!(v.decision.unwrap().rule, RuleRef::Core(rule), "{rule:?}");
        if rule != CoreRule::Cs8a {
            let d = e.blocked_hash(hashes).unwrap();
            assert_eq!(d.rule, RuleRef::Core(rule), "{rule:?}");
        }
    }
}
