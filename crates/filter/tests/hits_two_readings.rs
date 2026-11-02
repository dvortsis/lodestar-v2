//! `Core::hits` reads a marked text both ways, as `Core::verdict` does, so
//! its first switched-on hit is the verdict
//! (docs/decisions/2026-11-02-F-hits-two-readings.md). The fuzz target
//! `lists` checks the same invariant on shipped lists.

use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::lists;
use lodestar_filter::{Core, CsList, Item, Lists, Switches, TermSet};
use proptest::prelude::*;

/// The characters the matching once read as word breaks and the joined
/// reading now ignores, at the ends of each range: the invisible ones
/// (#381) and the format marks that show, with the line and paragraph
/// separators (docs/decisions/2026-11-02-F-visible-format-two-readings.md).
const MARKS: [char; 16] = [
    '\u{2065}',
    '\u{FFF0}',
    '\u{FFF3}',
    '\u{FFF8}',
    '\u{1BCA0}',
    '\u{1D173}',
    '\u{E0001}',
    '\u{E0FFF}',
    '\u{0600}',
    '\u{0605}',
    '\u{08E2}',
    '\u{110BD}',
    '\u{FFF9}',
    '\u{1343F}',
    '\u{2028}',
    '\u{2029}',
];

/// What the fuzz target checks, for one text read as it reads it.
fn agrees(core: &Core, text: &str) -> Result<(), String> {
    let files: Vec<FileInfo> = text
        .lines()
        .take(64)
        .map(|l| FileInfo::new(l.split('/').map(str::to_owned).collect(), l.len() as u64))
        .collect();
    for content_type in [ContentType::Video, ContentType::Adult] {
        let item = Item {
            hashes: &[[0; 20]],
            name: text,
            files: &files,
            content_type,
        };
        let verdict = core.verdict(&item);
        let first = core.hits(&item).into_iter().find(|h| h.on);
        if verdict != first {
            return Err(format!(
                "{text:?} {content_type:?}: {verdict:?} != {first:?}"
            ));
        }
    }
    Ok(())
}

fn shipped() -> Core {
    Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )
}

/// The input `fuzz (lists)` failed on (on #384, head 14f6d63e): it holds
/// U+FFF3, which the joined reading ignores, and the earlier reading's
/// hit decided the verdict while `hits` read only the joined one.
#[test]
fn the_fuzz_crash_agrees() {
    let data: &[u8] = &[
        109, 116, 103, 97, 101, 97, 55, 88, 88, 179, 52, 58, 97, 101, 97, 55, 88, 88, 179, 52, 58,
        89, 37, 52, 58, 89, 48, 239, 191, 179, 52, 97, 55, 88, 37, 88, 10, 47, 88, 88, 239, 191,
        170, 39, 88, 44, 88, 10, 47, 88, 4,
    ];
    let text = lists::decode(data, 1 << 20).unwrap();
    assert!(text.contains('\u{FFF3}'));
    agrees(&shipped(), &text).unwrap();
}

/// Golden cases for the visible marks: a term only the joined reading
/// has (`le<c>xa`), and one only the earlier reading has (`zz<c>lexa`),
/// each leads the hits as it decides the verdict.
#[test]
fn a_visible_mark_agrees_both_ways() {
    let core = stand_in();
    for c in [
        '\u{0605}',
        '\u{08E2}',
        '\u{FFF9}',
        '\u{13430}',
        '\u{2028}',
        '\u{2029}',
    ] {
        for name in [
            format!("Title rolea le{c}xa"),
            format!("Title rolea zz{c}lexa"),
        ] {
            let files = [FileInfo::new(vec!["a.mkv".into()], 1)];
            let item = Item {
                hashes: &[[0; 20]],
                name: &name,
                files: &files,
                content_type: ContentType::Video,
            };
            let verdict = core.verdict(&item);
            assert!(verdict.is_some(), "U+{:04X}: {name:?}", c as u32);
            assert_eq!(core.hits(&item).into_iter().find(|h| h.on), verdict);
            agrees(&core, &name).unwrap();
        }
    }
}

/// Stand-in terms as in `two_readings.rs`: `lexa` sexual, `rolea` child
/// role, `codex` a code.
fn stand_in() -> Core {
    let lists = Lists {
        sexual: TermSet::of(&["lexa", "alexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    Core::new(lists, CsList::default(), Switches::default())
}

/// Pieces that hit core rules read whole or read apart, joined by
/// separators and, often, marks.
fn marked_text() -> impl Strategy<Value = String> {
    let piece = prop::sample::select(vec![
        "title", "lexa", "alexa", "a", "le", "xa", "rolea", "role", "codex", "co", "dex", "15yo",
        "9", "yo", "years", "old", "15", "x", "é",
    ]);
    let sep = prop::sample::select(vec![" ", ".", "", "/", "\n"]);
    let mark = prop::option::weighted(0.5, prop::sample::select(MARKS.to_vec()));
    prop::collection::vec((piece, mark, sep), 1..10).prop_map(|v| {
        v.into_iter()
            .map(|(p, m, s)| format!("{p}{}{s}", m.map(String::from).unwrap_or_default()))
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    /// The first switched-on hit is the verdict, for marked text on
    /// stand-in terms.
    #[test]
    fn hits_lead_with_the_verdict(text in marked_text()) {
        agrees(&stand_in(), &text).map_err(TestCaseError::fail)?;
    }

    /// And on the shipped lists, for any text with marks spliced in.
    #[test]
    fn hits_lead_with_the_verdict_shipped(
        text in "\\PC{0,120}",
        inserts in prop::collection::vec((prop::sample::select(MARKS.to_vec()), 0usize..128), 0..4),
    ) {
        let mut chars: Vec<char> = text.chars().collect();
        for (c, at) in inserts {
            chars.insert(at % (chars.len() + 1), c);
        }
        let text: String = chars.into_iter().collect();
        agrees(&shipped(), &text).map_err(TestCaseError::fail)?;
    }
}
