//! A text that holds a character the matching once read as a word break
//! is read both ways (docs/decisions/2026-11-02-F-normalize-invisible.md):
//! nothing matches less than it did. The text with each such character a
//! space reads exactly as the matching before did (the normalize unit
//! tests check that), so it stands in for the earlier matching here.
//! Stand-in terms as in `engine.rs`: `lexa` sexual, `rolea` child role.

use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::{
    Action, Core, CsList, Engine, Item, Lists, Match, Override, Strength, Switches, TermSet,
};
use proptest::prelude::*;

/// The characters the matching once read as word breaks and now ignores,
/// at the ends of each range.
const NEWLY: [char; 12] = [
    '\u{2065}',
    '\u{FFF0}',
    '\u{FFF8}',
    '\u{1BCA0}',
    '\u{1BCA3}',
    '\u{1D173}',
    '\u{1D17A}',
    '\u{E0000}',
    '\u{E0001}',
    '\u{E007F}',
    '\u{E0080}',
    '\u{E0FFF}',
];

fn engine(overrides: Vec<Override>) -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa", "alexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut e = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
    e.overrides = overrides.into();
    e
}

fn keyword(k: &str, strength: Strength) -> Override {
    Override {
        id: 1,
        what: Match::Keyword(k.into()),
        strength,
        scope: None,
        expires_at: None,
        shadow_until: None,
    }
}

/// The decision's action, `None` for no decision.
fn action(e: &Engine, name: &str, path: &str) -> Option<Action> {
    let files = [FileInfo::new(
        path.split('/').map(str::to_owned).collect(),
        1 << 30,
    )];
    let item = Item {
        hashes: &[[7; 20]],
        name,
        files: &files,
        content_type: ContentType::Video,
    };
    e.judge(&item, None, 1000).decision.map(|d| d.action)
}

fn spaced(s: &str) -> String {
    s.chars()
        .map(|c| if NEWLY.contains(&c) { ' ' } else { c })
        .collect()
}

#[test]
fn a_term_split_off_still_hits() {
    let e = engine(Vec::new());
    for c in NEWLY {
        // `a<c>lexa`: `alexa` read whole, `lexa` read apart; both hit.
        let name = format!("Title rolea a{c}lexa");
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{:04X}", c as u32);
        // Joined, the term hides inside a word; read apart, it hits.
        let name = format!("Title rolea zz{c}lexa");
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{:04X}", c as u32);
        // Read apart, nothing; read whole, a term.
        let name = format!("Title rolea le{c}xa");
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{:04X}", c as u32);
    }
}

#[test]
fn a_release_must_match_both_ways() {
    for c in NEWLY {
        let at = c as u32;
        let name = format!("Title rolea zz{c}lexa");
        // `zzlexa` matches only the joined reading, where nothing hits:
        // the hit stands in the reading the override doesn't cover.
        let e = engine(vec![keyword("zzlexa", Strength::Override)]);
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{at:04X}");
        // Neutralizing `lexa` matches only the earlier reading.
        let e = engine(vec![keyword("lexa", Strength::Neutralize)]);
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{at:04X}");
        // An override that matches both ways still releases.
        let e = engine(vec![keyword("title", Strength::Override)]);
        assert_eq!(action(&e, &name, "a.mkv"), None, "U+{at:04X}");
        // A Neutralize keyword that is two words in the earlier reading
        // couldn't have been saved then, so it never applies.
        let e = engine(vec![keyword(&format!("le{c}xa"), Strength::Neutralize)]);
        assert!(
            action(&e, "Title rolea lexa", "a.mkv").is_some(),
            "U+{at:04X}"
        );
    }
}

fn entry() -> impl Strategy<Value = Option<Override>> {
    let words = prop::sample::select(vec![
        "title",
        "lexa",
        "alexa",
        "rolea",
        "zzlexa",
        "zz",
        "a",
        "titlerolea",
    ]);
    let strength = prop::sample::select(vec![Strength::Override, Strength::Neutralize]);
    prop::option::of((words, strength).prop_map(|(w, s)| keyword(w, s)))
}

fn piece() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "title", "lexa", "alexa", "a", "zz", "le", "xa", "rolea", "15yo", "yo", "é",
    ])
}

/// Pieces with separators and, now and then, marks.
fn run() -> impl Strategy<Value = String> {
    let sep = prop::sample::select(vec![" ", ".", "", "/"]);
    let mark = prop::option::weighted(0.3, prop::sample::select(NEWLY.to_vec()));
    prop::collection::vec((piece(), sep, mark), 0..4).prop_map(|v| {
        v.into_iter()
            .map(|(p, s, m)| format!("{p}{}{s}", m.map(String::from).unwrap_or_default()))
            .collect::<String>()
    })
}

/// [`run`]s around a pair of pieces joined by a mark, so a word is read
/// both ways.
fn text() -> impl Strategy<Value = String> {
    let joined = (piece(), prop::sample::select(NEWLY.to_vec()), piece());
    (run(), joined, run()).prop_map(|(a, (p, m, q), b)| format!("{a} {p}{m}{q} {b}"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// Inserting newly ignored characters never makes the decision
    /// weaker than the earlier matching's: a core term that hit still
    /// hits, and an override or neutralize entry applies no more.
    #[test]
    fn never_looser_than_before(
        name in text(),
        path in text(),
        overrides in prop::collection::vec(entry(), 1..4),
    ) {
        let overrides: Vec<Override> = overrides.into_iter().flatten().collect();
        let e = engine(overrides);
        let path = if path.is_empty() { "a.mkv".to_owned() } else { path };
        let now = action(&e, &name, &path);
        let before = action(&e, &spaced(&name), &spaced(&path));
        prop_assert!(now >= before, "{name:?} / {path:?}: {now:?} < {before:?}");
    }
}
