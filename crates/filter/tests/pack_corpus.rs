//! The pack corpora and the pack precision corpus (RP-1, §20.6, §20.8),
//! run through the engine with only each rule on.
//!
//! **SP-3 and SP-4 are exempt** from the per-item corpus: they depend on
//! state (a publisher's rate, an index of hidden payloads), so their
//! must-match and must-not-match cases are sequence fixtures run through
//! their own engine entry point in RP-4's and RP-5's tests, which check
//! their versions' directions too. The parser refuses examples for them.

use std::sync::Arc;

use lodestar_filter::corpus::{
    self, Corpus, DirectionCheck, Expect, MAX_BYTES, MAX_FILES, MAX_ITEMS, MAX_LINE, Observed,
    STATEFUL,
};
use lodestar_filter::packs::manifest::{Code, Direction, Manifest, RuleEntry, Version};
use lodestar_filter::packs::{Ctx, Pack, PackRule};
use lodestar_filter::{Action, RuleRef};
use proptest::prelude::*;

fn builtin() -> &'static Corpus {
    match Corpus::builtin() {
        Ok(c) => c,
        Err(e) => panic!("the shipped corpus won't parse: {e}"),
    }
}

/// The built rules that take per-item examples.
fn stateless() -> Vec<PackRule> {
    PackRule::ALL
        .into_iter()
        .filter(|r| corpus::stateless(*r))
        .collect()
}

#[test]
fn the_stateful_rules_are_exempt_and_refused() {
    assert_eq!(STATEFUL, ["SP-3", "SP-4"]);
    for id in STATEFUL {
        // Not built yet; once RP-4 and RP-5 add them, they stay exempt.
        if let Some(r) = PackRule::from_id(id) {
            assert!(!corpus::stateless(r));
        }
        let text = format!("rule {id}\nmatch Example\n");
        let e = corpus::parse_pack(Pack::Spam, "spam.txt", &text).unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.message.contains("depends on state"), "{e}");
    }
}

#[test]
fn every_stateless_rule_has_three_of_each() {
    let c = builtin();
    for r in stateless() {
        for expect in [Expect::Match, Expect::Miss] {
            let n = c
                .cases(r.pack())
                .iter()
                .filter(|x| x.rule == r && x.expect == expect)
                .count();
            assert!(
                n >= 3,
                "{} has {n} `{}` examples in {}; it needs at least 3",
                r.id(),
                expect.as_str(),
                corpus::pack_text(r.pack()).0
            );
        }
    }
    // SC-2 is covered by its own id, not through SC-1.
    assert!(c.cases(Pack::Scams).iter().any(|x| x.rule == PackRule::Sc2));
}

#[test]
fn the_whole_corpus_passes() {
    let c = builtin();
    let report = c.run(&Manifest::builtin());
    let failures = report.failures();
    assert!(
        failures.is_empty(),
        "the pack corpus fails:\n{}",
        failures.join("\n")
    );
    assert!(report.passes());
    // Results per pack, per rule and per item, for `GET /filters/fixtures`.
    assert_eq!(report.packs.len(), Pack::ALL.len());
    let mut items = 0;
    for r in stateless() {
        let rr = report.rule(r).expect("every stateless rule is reported");
        assert_eq!(rr.version, 1);
        assert_eq!(rr.action, r.action());
        assert_eq!(rr.disagreements(), 0);
        assert!(rr.count(Expect::Match) >= 3 && rr.count(Expect::Miss) >= 3);
        for i in rr.items.iter().filter(|i| i.expect == Expect::Match) {
            assert_eq!(i.got, Some((RuleRef::Pack(r), r.action())), "{}", i.name);
        }
        items += rr.items.len();
    }
    let parsed: usize = c.packs.iter().map(|(_, _, x)| x.len()).sum();
    assert_eq!(items, parsed, "every example is judged once");
    // One direction check per version of every stateless rule.
    let m = Manifest::builtin();
    let versions: usize = stateless()
        .iter()
        .map(|r| m.entry(*r).versions().len())
        .sum();
    assert_eq!(report.directions.len(), versions);
    assert!(report.directions.iter().all(DirectionCheck::agrees));
}

/// Every rule §20.6 gives a false-positive guard has legitimate items in
/// the precision corpus.
#[test]
fn the_precision_corpus_covers_every_guard() {
    use PackRule::*;
    const GUARDED: &[PackRule] = &[Mw2, Mw3, Mw4, Mw5, Lu1, Lu2, Fk1, Fk2, Fk5, Sp2, Sc1, Sc2];
    let c = builtin();
    for r in GUARDED {
        let n = c.precision.iter().filter(|p| p.guards.contains(r)).count();
        assert!(n >= 1, "no precision item exercises {}'s guard", r.id());
    }
    assert!(c.precision.len() >= 20);
}

/// The guards §20.6 names that aren't built yet: the items they'd pass
/// are held today, listed with `gap` lines and reported. When a guard is
/// built (with its version bump), its items pass and this list shrinks.
#[test]
fn known_gaps_are_listed() {
    let report = builtin().run(&Manifest::builtin());
    let mut gaps: Vec<&str> = report.gaps().iter().map(|(r, _)| r.id()).collect();
    gaps.sort_unstable();
    assert_eq!(gaps, ["FK-2", "MW-2", "MW-2"]);
    for p in report.precision.iter().filter(|p| !p.gaps.is_empty()) {
        let held: Vec<PackRule> = p.held.iter().map(|(r, _)| *r).collect();
        assert_eq!(held, p.gaps, "{}", p.name);
    }
}

// ---------------------------------------------------------------------
// Deliberately broken guards: test-only manifests whose current version
// is a weakened copy of a shipped rule. No shipped rule is edited.

const SERVICES: &[&str] = &[
    "spotify", "netflix", "disney", "hbo", "deezer", "tidal", "onlyfans", "bitcoin", "wallet",
    "crypto", "paypal",
];
const SCAM_WORDS: &[&str] = &[
    "generator",
    "hack",
    "free",
    "premium",
    "unlocked",
    "activator",
    "recovery",
    "adder",
];

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// SC-1 without its `trainer` / `mod` / `offline` guard.
fn sc1_unguarded(c: &Ctx<'_>) -> bool {
    let w = words(c.name());
    w.iter().any(|x| SERVICES.contains(&x.as_str()))
        && w.iter().any(|x| SCAM_WORDS.contains(&x.as_str()))
}

/// SP-2 without its music guard: any long name of repeated words.
fn sp2_unguarded(c: &Ctx<'_>) -> bool {
    let name = c.name();
    if name.chars().count() <= 300 {
        return false;
    }
    let w = words(name);
    let distinct: std::collections::HashSet<&String> = w.iter().collect();
    !w.is_empty() && distinct.len() * 3 < w.len()
}

/// The built-in manifest with `rule`'s versions replaced.
fn manifest_with(rule: PackRule, versions: Vec<Version>) -> Arc<Manifest> {
    let builtin = Manifest::builtin();
    let entries = PackRule::ALL
        .into_iter()
        .map(|r| {
            if r == rule {
                RuleEntry::new(r, true, versions.clone())
            } else {
                builtin.entry(r).clone()
            }
        })
        .collect();
    Arc::new(Manifest::new(entries).expect("a valid test manifest"))
}

fn v(version: u32, direction: Direction, action: Action, code: Option<Code>) -> Version {
    Version {
        version,
        direction,
        action,
        code,
    }
}

/// A rule's matching code.
type RuleFn = fn(&Ctx<'_>) -> bool;

#[test]
fn a_broken_guard_fails_the_corpus() {
    let c = builtin();
    let cases: [(PackRule, RuleFn, &[&str]); 2] = [
        (
            PackRule::Sc1,
            sc1_unguarded,
            &["Spotify Premium Offline Mod", "Netflix Offline Downloader"],
        ),
        (
            PackRule::Sp2,
            sp2_unguarded,
            &["Sample Band - Album 01", "Sample Band - Discography"],
        ),
    ];
    for (rule, weak, expect) in cases {
        let m = manifest_with(
            rule,
            vec![v(1, Direction::Widens, rule.action(), Some(Code::Fn(weak)))],
        );
        let report = c.run(&m);
        assert!(!report.passes(), "{}'s weakened guard passed", rule.id());
        let failures = report.failures();
        for name in expect {
            assert!(
                failures
                    .iter()
                    .any(|f| f.contains(name) && f.contains(rule.id())),
                "{} weakened: no failure for {name:?} in\n{}",
                rule.id(),
                failures.join("\n")
            );
        }
    }
}

// ---------------------------------------------------------------------
// Directions: each version's record against what the corpus shows.

fn sc1_same(c: &Ctx<'_>) -> bool {
    c.matches(PackRule::Sc1)
}

fn direction_of(m: &Manifest, version: u32) -> DirectionCheck {
    builtin()
        .run(&Arc::new(m.clone()))
        .directions
        .into_iter()
        .find(|d| d.rule == PackRule::Sc1 && d.version == version)
        .expect("checked")
}

#[test]
fn directions_are_checked_against_the_corpus() {
    use Direction::*;
    let hide = Action::Hide;
    // The guard added in version 2: it matches less.
    let narrowing = |d| {
        manifest_with(
            PackRule::Sc1,
            vec![
                v(1, Widens, hide, Some(Code::Fn(sc1_unguarded))),
                v(2, d, hide, Some(Code::Current)),
            ],
        )
    };
    let check = direction_of(&narrowing(Narrows), 2);
    assert_eq!(check.observed, Observed::Shown(Narrows));
    assert!(check.agrees());
    assert!(!direction_of(&narrowing(Widens), 2).agrees(), "mislabelled");
    assert!(
        direction_of(&narrowing(Mixed), 2).agrees(),
        "mixed is weakening anyway"
    );
    // The guard dropped in version 2: it matches more.
    let widening = manifest_with(
        PackRule::Sc1,
        vec![
            v(1, Widens, hide, Some(Code::Fn(sc1_same))),
            v(2, Widens, hide, Some(Code::Fn(sc1_unguarded))),
        ],
    );
    assert_eq!(direction_of(&widening, 2).observed, Observed::Shown(Widens));
    assert!(direction_of(&widening, 2).agrees());
    // The same matching with a heavier action, then a lighter one.
    let action = |from, to, d| {
        manifest_with(
            PackRule::Sc1,
            vec![
                v(1, Widens, from, Some(Code::Fn(sc1_same))),
                v(2, d, to, Some(Code::Current)),
            ],
        )
    };
    let heavier = direction_of(&action(Action::Demote, hide, ActionHeavier), 2);
    assert_eq!(heavier.observed, Observed::Shown(ActionHeavier));
    assert!(heavier.agrees());
    assert!(!direction_of(&action(Action::Demote, hide, ActionLighter), 2).agrees());
    let lighter = direction_of(&action(hide, Action::Flag, ActionLighter), 2);
    assert_eq!(lighter.observed, Observed::Shown(ActionLighter));
    assert!(lighter.agrees());
    // Matching more and a heavier action: only `mixed` describes it.
    let both = manifest_with(
        PackRule::Sc1,
        vec![
            v(1, Widens, Action::Demote, Some(Code::Fn(sc1_same))),
            v(2, Widens, hide, Some(Code::Fn(sc1_unguarded))),
        ],
    );
    assert_eq!(direction_of(&both, 2).observed, Observed::Shown(Mixed));
    assert!(!direction_of(&both, 2).agrees());
    // A bump the corpus can't see never agrees: it needs an example.
    let unseen = direction_of(&action(hide, hide, Widens), 2);
    assert_eq!(unseen.observed, Observed::NoChange);
    assert!(!unseen.agrees());
    let report = builtin().run(&action(hide, hide, Widens));
    assert!(
        report
            .failures()
            .iter()
            .any(|f| f.contains("SC-1@2") && f.contains("no change")),
        "{:?}",
        report.failures()
    );
    // A version whose code is gone can't be compared.
    let dropped = manifest_with(
        PackRule::Sc1,
        vec![
            v(1, Widens, hide, None),
            v(2, Narrows, hide, Some(Code::Current)),
        ],
    );
    assert_eq!(direction_of(&dropped, 1).observed, Observed::NotChecked);
    assert_eq!(direction_of(&dropped, 2).observed, Observed::NotChecked);
    assert!(direction_of(&dropped, 2).agrees());
    // A first version widens against nothing, shown by its examples.
    let first = direction_of(&Manifest::builtin(), 1);
    assert_eq!(first.observed, Observed::Shown(Widens));
    // The run judges the current version: version 2's guard holds, so
    // the corpus passes apart from the mislabelled direction.
    let report = builtin().run(&narrowing(Widens));
    assert_eq!(report.failures().len(), 1, "{:?}", report.failures());
}

// ---------------------------------------------------------------------
// The parser.

fn pack_err(text: &str) -> (usize, String) {
    let e = corpus::parse_pack(Pack::Malware, "m.txt", text).unwrap_err();
    assert!(
        e.to_string().starts_with(&format!("m.txt:{}: ", e.line)),
        "{e}"
    );
    (e.line, e.message)
}

fn precision_err(text: &str) -> (usize, String) {
    let e = corpus::parse_precision("p.txt", text).unwrap_err();
    assert!(
        e.to_string().starts_with(&format!("p.txt:{}: ", e.line)),
        "{e}"
    );
    (e.line, e.message)
}

#[test]
fn the_parser_reads_the_format() {
    let text = "# comment\r\n\r\nrule MW-2\r\n  match  Example Movie 1080p \r\n  file 4G Folder/Example Movie.mkv\n  file 900K Codec Setup.exe\n  pad 16K .pad/0\n  type movie\nmiss Other\n";
    let cases = corpus::parse_pack(Pack::Malware, "m.txt", text).unwrap();
    assert_eq!(cases.len(), 2);
    let a = &cases[0];
    assert_eq!(a.rule, PackRule::Mw2);
    assert_eq!(a.expect, Expect::Match);
    assert_eq!(a.example.line, 4);
    assert_eq!(a.example.name, "Example Movie 1080p");
    assert_eq!(a.example.files.len(), 3);
    assert_eq!(a.example.files[0].path, ["Folder", "Example Movie.mkv"]);
    assert_eq!(a.example.files[0].size, 4 << 30);
    assert_eq!(a.example.files[1].size, 900 << 10);
    assert!(!a.example.files[1].padding && a.example.files[2].padding);
    assert_eq!(
        a.example.content_type,
        Some(lodestar_classify::ContentType::Movie)
    );
    assert_eq!(cases[1].expect, Expect::Miss);
    assert!(cases[1].example.files.is_empty());

    let p =
        corpus::parse_precision("p.txt", "item X\n guards MW-2 MW-3 MW-2\n gap MW-2\n").unwrap();
    assert_eq!(p[0].guards, [PackRule::Mw2, PackRule::Mw3]);
    assert_eq!(p[0].gaps, [PackRule::Mw2]);
    assert!(
        corpus::parse_pack(Pack::Malware, "m.txt", "")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_parser_rejects_malformed_lines() {
    let long = format!("rule MW-1\nmatch {}\n", "x".repeat(MAX_LINE));
    let many_files = format!("rule MW-1\nmatch X\n{}", "file 1 a\n".repeat(MAX_FILES + 1));
    let many_items = format!("rule MW-1\n{}", "match X\n".repeat(MAX_ITEMS + 1));
    let pack: &[(&str, usize, &str)] = &[
        ("rule MW-1\nmatch X\nbogus 1\n", 3, "unknown keyword"),
        ("match X\n", 1, "before any `rule`"),
        ("file 1 a\n", 1, "before any example"),
        ("rule SC-1\n", 1, "in the scams pack, not malware"),
        ("rule MW-9\n", 1, "no pack rule"),
        ("rule\n", 1, "no pack rule"),
        ("rule MW-1\nmatch\n", 2, "needs a name"),
        ("rule MW-1\nmatch X\nfile 12Q a\n", 3, "isn't a size"),
        ("rule MW-1\nmatch X\nfile -1 a\n", 3, "isn't a size"),
        ("rule MW-1\nmatch X\nfile K a\n", 3, "isn't a size"),
        (
            "rule MW-1\nmatch X\nfile 99999999999999999999 a\n",
            3,
            "isn't a size",
        ),
        ("rule MW-1\nmatch X\nfile 99999999T a\n", 3, "isn't a size"),
        ("rule MW-1\nmatch X\nfile 4G\n", 3, "a size and a path"),
        ("rule MW-1\nmatch X\nfile 4G a//b\n", 3, "an empty folder"),
        ("rule MW-1\nmatch X\nfile 4G /a\n", 3, "an empty folder"),
        ("rule MW-1\nmatch X\nfile 4G a/\n", 3, "an empty folder"),
        (
            "rule MW-1\nmatch X\ntype movie\ntype tv\n",
            4,
            "a second `type`",
        ),
        ("rule MW-1\nmatch X\ntype film\n", 3, "no content type"),
        (
            "rule MW-1\nmatch X\nguards MW-1\n",
            3,
            "for the precision corpus",
        ),
        (
            "rule MW-1\nmatch X\ngap MW-1\n",
            3,
            "for the precision corpus",
        ),
        ("item X\n", 1, "for the precision corpus"),
        (&long, 2, "longer than"),
        (&many_files, 3 + MAX_FILES, "more than"),
        (&many_items, 2 + MAX_ITEMS, "more than"),
    ];
    for (text, line, message) in pack {
        let (l, m) = pack_err(text);
        assert_eq!(
            (l, m.contains(message)),
            (*line, true),
            "{text:?}: {l}: {m}"
        );
    }
    let precision: &[(&str, usize, &str)] = &[
        ("item X\n", 1, "needs a `guards` line"),
        ("item X\nguards MW-1\nitem Y\n", 3, "needs a `guards` line"),
        ("item X\nguards\n", 2, "needs a rule"),
        ("item X\nguards MW-1\nguards MW-2\n", 3, "a second `guards`"),
        (
            "item X\nguards MW-1\ngap MW-1\ngap MW-2\n",
            4,
            "a second `gap`",
        ),
        ("item X\nguards MW-1 XX-1\n", 2, "no pack rule"),
        ("item X\nguards SP-3\n", 2, "depends on state"),
        ("rule MW-1\n", 1, "for pack corpora"),
        ("match X\n", 1, "for pack corpora"),
    ];
    for (text, line, message) in precision {
        let (l, m) = precision_err(text);
        assert_eq!(
            (l, m.contains(message)),
            (*line, true),
            "{text:?}: {l}: {m}"
        );
    }
    let big = "#".repeat(MAX_BYTES + 1);
    assert_eq!(pack_err(&big).0, 0);
    // Errors cut long values short.
    let (_, m) = pack_err(&format!("{}\n", "y".repeat(1000)));
    assert!(m.len() < 100, "{m}");
}

/// One example as the format writes it.
fn render(rule: &str, expect: Expect, name: &str, files: &[(u64, String, bool)]) -> String {
    let mut s = format!("rule {rule}\n{} {name}\n", expect.as_str());
    for (size, path, pad) in files {
        s.push_str(&format!(
            "  {} {size} {path}\n",
            if *pad { "pad" } else { "file" }
        ));
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any text: an error or a corpus, never a panic.
    #[test]
    fn the_parser_never_panics(text in "\\PC{0,400}") {
        let _ = corpus::parse_pack(Pack::Scams, "s.txt", &text);
        let _ = corpus::parse_precision("p.txt", &text);
    }

    /// Lines built from the format's own keywords, in any order.
    #[test]
    fn keyword_soup_never_panics(lines in prop::collection::vec(
        (prop::sample::select(vec!["rule", "match", "miss", "item", "file", "pad", "type", "guards", "gap", "#", ""]),
         "[ -~]{0,20}"),
        0..40,
    )) {
        let text: String = lines.iter().map(|(k, v)| format!("{k} {v}\n")).collect();
        let _ = corpus::parse_pack(Pack::Malware, "m.txt", &text);
        let _ = corpus::parse_precision("p.txt", &text);
    }

    /// A written example reads back as it was written.
    #[test]
    fn examples_round_trip(
        rule in prop::sample::select(vec!["MW-1", "MW-2", "MW-5"]),
        miss in any::<bool>(),
        name in "[A-Za-z0-9][A-Za-z0-9 .()\\[\\]-]{0,60}[A-Za-z0-9]",
        files in prop::collection::vec(
            (0u64..1 << 50, prop::collection::vec("[A-Za-z0-9][A-Za-z0-9 ._-]{0,20}[A-Za-z0-9]", 1..4), any::<bool>()),
            0..8,
        ),
    ) {
        let expect = if miss { Expect::Miss } else { Expect::Match };
        let files: Vec<(u64, String, bool)> =
            files.into_iter().map(|(s, p, pad)| (s, p.join("/"), pad)).collect();
        let text = render(rule, expect, &name, &files);
        let cases = corpus::parse_pack(Pack::Malware, "m.txt", &text).unwrap();
        prop_assert_eq!(cases.len(), 1);
        let c = &cases[0];
        prop_assert_eq!(c.rule.id(), rule);
        prop_assert_eq!(c.expect, expect);
        prop_assert_eq!(&c.example.name, name.trim());
        prop_assert_eq!(c.example.files.len(), files.len());
        for (f, (size, path, pad)) in c.example.files.iter().zip(&files) {
            prop_assert_eq!(f.size, *size);
            prop_assert_eq!(f.path.join("/"), path.trim());
            prop_assert_eq!(f.padding, *pad);
        }
    }
}
