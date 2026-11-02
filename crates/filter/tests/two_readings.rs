//! A text that holds a character the matching once read as a word break
//! is judged in both readings, and the stricter verdict stands
//! (docs/decisions/2026-11-02-F-normalize-invisible.md): nothing is
//! decided more weakly than it was. The text with each such character a
//! space reads exactly as the matching before did (the normalize unit
//! tests check that), so it stands in for the earlier matching here.
//! Stand-in terms as in `engine.rs`: `lexa` sexual, `rolea` child role.

use std::collections::HashSet;
use std::sync::Arc;

use lodestar_classify::{FileInfo, classify};
use lodestar_filter::{
    Action, Core, CoreRule, CsList, Engine, Exception, Facts, Item, ListLayer, Lists, Match,
    Override, PackSwitches, RuleRef, Strength, Switches, TermSet, UserRule,
};
use lodestar_search::parse;
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

const H: [u8; 20] = [7; 20];

/// The decision's action, `None` for no decision. The torrent is
/// classified by `class_of`, the same for both readings: classification
/// isn't the filter's to change.
fn judged(e: &Engine, name: &str, path: &str, class_of: (&str, &str)) -> Option<Action> {
    let file = |p: &str| {
        [FileInfo::new(
            p.split('/').map(str::to_owned).collect(),
            1 << 30,
        )]
    };
    let files = file(path);
    let class = classify(class_of.0, &file(class_of.1));
    let facts = Facts {
        class: &class,
        size: 1 << 30,
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 1000,
        now: 1000,
    };
    let item = Item {
        hashes: &[H],
        name,
        files: &files,
        content_type: class.content_type,
    };
    e.judge(&item, Some(&facts), 1000)
        .decision
        .map(|d| d.action)
}

fn action(e: &Engine, name: &str, path: &str) -> Option<Action> {
    judged(e, name, path, (&spaced(name), &spaced(path)))
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
fn a_release_applies_in_its_own_reading() {
    for c in NEWLY {
        let at = c as u32;
        let name = format!("Title rolea zz{c}lexa");
        // `zzlexa` matches only the joined reading, where nothing hits:
        // the hit stands in the reading the override doesn't cover.
        let e = engine(vec![keyword("zzlexa", Strength::Override)]);
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{at:04X}");
        // Neutralizing `lexa` matches only the earlier reading, where it
        // takes the only hit away, as it did before.
        let e = engine(vec![keyword("lexa", Strength::Neutralize)]);
        assert_eq!(action(&e, &name, "a.mkv"), None, "U+{at:04X}");
        // Neutralizing `zzlexa` matches only the joined reading, where
        // nothing hits: the earlier reading's hit stands.
        let e = engine(vec![keyword("zzlexa", Strength::Neutralize)]);
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
        // Not even where its earlier reading, `le xa`, is in the text: it
        // would neutralize `lexa` there, which no saved entry did.
        assert!(
            action(&e, "Title rolea lexa le xa", "a.mkv").is_some(),
            "U+{at:04X}"
        );
    }
}

#[test]
fn a_keyword_exception_applies_in_its_own_reading() {
    for c in NEWLY {
        let at = c as u32;
        // `alexa` joined, `lexa` read apart: both hit Cs4a. An exception
        // for `alexa` matches only the joined reading, so the earlier
        // reading's hit stands, as it did before.
        let mut e = engine(Vec::new());
        e.exceptions.push(Exception {
            id: 1,
            rule: RuleRef::Core(CoreRule::Cs4a),
            what: Match::Keyword("alexa".into()),
        });
        let name = format!("Title rolea a{c}lexa");
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{at:04X}");
        assert!(action(&e, "Title rolea alexa", "a.mkv").is_none());
    }
}

#[test]
fn a_hit_only_joined_never_pre_empts_a_stronger_layer() {
    for c in NEWLY {
        let at = c as u32;
        // `le<c>xa` hits Cs4a only joined. A list that drops the torrent
        // decided it before, and still does.
        let mut e = engine(Vec::new());
        e.set_list(
            Some(ListLayer {
                id: 9,
                action: Action::Drop,
                hashes: Arc::new(HashSet::from([H])),
            }),
            9,
        );
        let name = format!("Title rolea le{c}xa");
        assert_eq!(action(&e, &name, "a.mkv"), Some(Action::Drop), "U+{at:04X}");
        // A user rule that drops, likewise.
        let mut e = engine(Vec::new());
        e.rules.push(drop_rule("title"));
        assert_eq!(action(&e, &name, "a.mkv"), Some(Action::Drop), "U+{at:04X}");
        // Where the layers below decide less, the joined hit stands.
        let e = engine(Vec::new());
        assert!(action(&e, &name, "a.mkv").is_some(), "U+{at:04X}");
    }
}

fn drop_rule(q: &str) -> UserRule {
    UserRule {
        id: 3,
        query: parse(q).unwrap(),
        action: Action::Drop,
        enabled: true,
        shadow_until: None,
        unless: None,
    }
}

/// An override or neutralize entry, its keyword sometimes holding a mark
/// inside it, as one saved before could.
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
    let mark = prop::option::weighted(0.3, (prop::sample::select(NEWLY.to_vec()), 0usize..8));
    let strength = prop::sample::select(vec![Strength::Override, Strength::Neutralize]);
    prop::option::of((words, mark, strength).prop_map(|(w, m, s)| {
        let mut w = w.to_owned();
        if let Some((c, at)) = m {
            let at = w.char_indices().nth(at % w.len()).map_or(0, |(i, _)| i);
            w.insert(at, c);
        }
        keyword(&w, s)
    }))
}

/// `e` as it read before: each keyword with its marks as spaces.
fn spaced_engine(e: &Engine) -> Engine {
    let mut before = e.clone();
    before.overrides = e
        .overrides
        .iter()
        .map(|o| match &o.what {
            Match::Keyword(k) => Override {
                what: Match::Keyword(spaced(k)),
                ..o.clone()
            },
            _ => o.clone(),
        })
        .collect::<Vec<_>>()
        .into();
    before
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
    /// hits, an override or neutralize entry (its keyword marked or not)
    /// applies no more, and a core
    /// hit only the joined reading has never pre-empts a stronger list,
    /// pack or rule of yours.
    #[test]
    fn never_looser_than_before(
        name in text(),
        path in text(),
        overrides in prop::collection::vec(entry(), 1..4),
        list in prop::option::of(prop::sample::select(vec![Action::Hide, Action::Drop])),
        rule in prop::option::of(prop::sample::select(vec!["title", "lexa", "yo", "zz"])),
        packs in any::<bool>(),
    ) {
        let overrides: Vec<Override> = overrides.into_iter().flatten().collect();
        let mut e = engine(overrides);
        if let Some(action) = list {
            let layer = ListLayer { id: 9, action, hashes: Arc::new(HashSet::from([H])) };
            e.set_list(Some(layer), 9);
        }
        e.rules.extend(rule.map(drop_rule));
        if packs {
            e.packs = PackSwitches::all();
        }
        let path = if path.is_empty() { "a.mkv".to_owned() } else { path };
        let now = action(&e, &name, &path);
        let before = action(&spaced_engine(&e), &spaced(&name), &spaced(&path));
        prop_assert!(now >= before, "{name:?} / {path:?}: {now:?} < {before:?}");
    }
}
