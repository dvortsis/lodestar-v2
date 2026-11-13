//! The core reads a torrent whole or holds it (§20.5, §20.8): within its
//! work budget every word of the name and of every path is judged, and a
//! torrent past the budget is held as too large to judge (CS-0a). Stand-in
//! terms as in `core.rs`: `lexa` for a sexual term, `rolea` for a child
//! role, `codex` for a code term.

use lodestar_classify::{ContentType, FileInfo, classify};
use lodestar_filter::{
    Action, Allow, BUDGET, Core, CoreRule, CsList, Engine, Evidence, Exception, Facts, Field, Item,
    Lists, MAX_FILES, Match, Origin, Override, RuleRef, Strength, Switches, TermSet, read_cost,
    too_large,
};
use lodestar_search::parse;
use proptest::prelude::*;

fn stand_ins() -> Lists {
    Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    }
}

fn core() -> Core {
    Core::new(stand_ins(), CsList::default(), Switches::default())
}

const H: [u8; 20] = [3; 20];

fn item<'a>(name: &'a str, files: &'a [FileInfo]) -> Item<'a> {
    Item {
        hashes: &[H],
        name,
        files,
        content_type: ContentType::Video,
    }
}

fn file(path: &str) -> FileInfo {
    FileInfo::new(path.split('/').map(str::to_owned).collect(), 1 << 20)
}

/// Words that carry no signal, ASCII and not.
const PLAIN: &[&str] = &[
    "kalo",
    "a1",
    "x",
    "S01E02",
    "1080p",
    "h3ll0",
    "z",
    "日本語",
    "привет",
    "e\u{301}t\u{e9}",
    "ｗｅｂ",
    "x264",
    "q.r.s",
    "@@",
    "the",
];

fn plain(words: usize, seed: usize) -> String {
    (0..words)
        .map(|i| PLAIN[(i * 7 + seed) % PLAIN.len()])
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_long_name_is_judged_to_its_end() {
    let c = core();
    for words in [150, 400, 600] {
        let name = format!("{} rolea lexa", plain(words, words));
        assert!(name.len() <= 4096, "a name a torrent can have");
        let hit = c.verdict(&item(&name, &[file("a.mkv")])).unwrap();
        assert_eq!(hit.rule, CoreRule::Cs4a, "{words} words before");
        assert!(matches!(
            hit.evidence[0],
            Evidence::ChildRole {
                field: Field::Name,
                ..
            }
        ));
        // A code term at the end of the name too.
        let name = format!("{} codex", plain(words, 1));
        assert_eq!(
            c.verdict(&item(&name, &[file("a.mkv")])).map(|h| h.rule),
            Some(CoreRule::Cs3a)
        );
    }
}

#[test]
fn a_long_path_is_judged_to_its_end() {
    let c = core();
    for words in [150, 400, 600] {
        let path = format!("Folder/{} rolea lexa.mkv", plain(words, 3));
        let files = [file("a.mkv"), file(&path)];
        let hit = c.verdict(&item("Title", &files)).unwrap();
        assert_eq!(hit.rule, CoreRule::Cs4a, "{words} words before");
        assert!(matches!(
            hit.evidence[0],
            Evidence::ChildRole {
                field: Field::File(1),
                ..
            }
        ));
        let path = format!("{}/codex.mkv", plain(words, 5));
        let files = [file(&path)];
        assert_eq!(
            c.verdict(&item("Title", &files)).map(|h| h.rule),
            Some(CoreRule::Cs3b)
        );
    }
}

#[test]
fn every_file_is_judged() {
    let c = core();
    for (n, at) in [(1_500, 1_499), (4_000, 3_210)] {
        let mut files: Vec<FileInfo> = (0..n).map(|i| file(&format!("e{i}.mkv"))).collect();
        files[at] = file("rolea lexa.mkv");
        let it = item("Title", &files);
        assert!(
            !too_large(&it),
            "{n} files of short paths are within the budget"
        );
        let hit = c.verdict(&it).unwrap();
        assert_eq!(hit.rule, CoreRule::Cs4a, "{n} files");
        assert!(matches!(
            hit.evidence[0],
            Evidence::ChildRole {
                field: Field::File(i),
                ..
            } if i as usize == at
        ));
    }
}

/// Words that each cost a new plan: the costliest ASCII to read.
fn distinct_words(bytes: usize, seed: u64) -> String {
    let mut x = seed | 1;
    let mut s = String::with_capacity(bytes + 8);
    while s.len() < bytes {
        for _ in 0..3 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.push(char::from(
                b"abcdefghijklmnopqrstuvwxyz0123456789"[(x % 36) as usize],
            ));
        }
        s.push(' ');
    }
    s
}

#[test]
fn past_the_budget_is_held() {
    let c = core();
    let held = |it: &Item| {
        assert!(too_large(it));
        assert_eq!(read_cost(it), None);
        let hit = c.verdict(it).expect("held");
        assert_eq!(
            (hit.rule, hit.action, hit.on),
            (CoreRule::Cs0a, Action::Quarantine, true)
        );
        assert!(hit.evidence.is_empty());
        // The previews and the review queue see the same, and nothing
        // else is read of it.
        assert_eq!(c.hits(it), vec![hit]);
    };
    // One file too many, however short the paths: held before reading.
    let files: Vec<FileInfo> = (0..=MAX_FILES).map(|i| file(&format!("{i}"))).collect();
    let it = item("Title", &files);
    held(&it);
    assert_eq!(c.floor_hit(&it).map(|h| h.rule), Some(CoreRule::Cs0a));
    // More bytes than the budget has units: held before reading.
    let long = "w".repeat(BUDGET as usize + 1);
    held(&item(&long, &[]));
    // Within that, but costlier to read than the budget: held as read.
    let costly: Vec<FileInfo> = (0..400)
        .map(|i| file(&distinct_words(4_000, i + 1)))
        .collect();
    let it = item("Title", &costly);
    assert!(c.floor_hit(&it).is_none(), "not past it unread");
    held(&it);
    // Characters that decompose into many.
    let wide: Vec<FileInfo> = (0..400).map(|_| file(&"\u{FDFA}".repeat(1_300))).collect();
    held(&item("Title", &wide));
    // Held with nothing indicative in it, and switched on always.
    let mut c2 = core();
    assert!(c2.switches.set(CoreRule::Cs0a, false).is_err());
    assert!(c2.switches.is_on(CoreRule::Cs0a));
    // A CS-1 hash is the stronger hold.
    c2.cs.add_hash(H, Origin::Confirmed);
    assert_eq!(
        c2.verdict(&item("Title", &costly)).map(|h| h.rule),
        Some(CoreRule::Cs1a)
    );
}

#[test]
fn the_meter_counts_what_reading_costs() {
    let cost = |name: &str, files: &[FileInfo]| read_cost(&item(name, files)).unwrap();
    // More text costs more, and a word read anew more than one seen.
    assert!(cost("abc def", &[]) > cost("abc", &[]));
    assert!(cost("abc xyz", &[]) > cost("abc abc", &[]));
    // A non-ASCII character more than an ASCII byte; one that decomposes
    // into many more than one that doesn't.
    assert!(cost("\u{e9}", &[]) > cost("e", &[]));
    assert!(cost("\u{FDFA}", &[]) > cost("\u{e9}", &[]));
    // Each file and each path segment, however short.
    let one = [FileInfo::new(vec!["a".into()], 1)];
    let two = [file("a"), file("a")];
    let deep = [FileInfo::new(vec!["a".into(); 8], 1)];
    assert!(cost("", &two) > cost("", &one));
    assert!(cost("", &deep) > cost("", &one));
    // The same torrent always costs the same.
    let files: Vec<FileInfo> = (0..50).map(|i| file(&distinct_words(200, i + 1))).collect();
    assert_eq!(cost("Title", &files), cost("Title", &files));
}

/// A path of about `bytes` bytes in `words`' script: a shared folder and a
/// file name numbered `i`.
fn typical_path(words: &[&str], bytes: usize, i: usize) -> FileInfo {
    let fill = |n: usize, from: usize| {
        let mut s = String::new();
        let mut k = from;
        while s.len() + words[k % words.len()].len() < n {
            s.push_str(words[k % words.len()]);
            s.push(' ');
            k += 1;
        }
        s
    };
    let folder = fill(bytes / 3, 0);
    let name = format!("{i:05} {}.mkv", fill(bytes - folder.len() - 11, i));
    FileInfo::new(vec![folder, name], 1 << 30)
}

const SCRIPTS: &[(&str, &[&str])] = &[
    (
        "Latin",
        &[
            "The", "Quick", "Brown", "Fox", "Season", "Episode", "1080p", "WEB-DL", "Title",
        ],
    ),
    (
        "CJK",
        &[
            "日本語",
            "映画",
            "第一話",
            "東京",
            "物語",
            "中文字幕",
            "高清",
            "版本",
        ],
    ),
    (
        "Cyrillic",
        &[
            "Привет",
            "Мир",
            "Сезон",
            "Серия",
            "Фильм",
            "Русский",
            "Перевод",
        ],
    ),
    (
        "Arabic",
        &[
            "مرحبا",
            "بالعالم",
            "الموسم",
            "الحلقة",
            "فيلم",
            "ترجمة",
            "عربية",
        ],
    ),
];

/// Ten thousand files with paths of up to 120 bytes, and a name of 4 KB,
/// in any script, are judged whole: a pair in the last file is caught.
#[test]
fn large_torrents_in_any_script_are_judged() {
    let c = core();
    for (script, words) in SCRIPTS {
        let mut files: Vec<FileInfo> = (0..10_000).map(|i| typical_path(words, 120, i)).collect();
        assert!(
            files
                .iter()
                .all(|f| f.path.iter().map(String::len).sum::<usize>() <= 120)
        );
        files[9_999] = file("Folder/rolea lexa.mkv");
        let mut name = String::new();
        while name.len() < 4_000 {
            name.push_str(words[name.len() % words.len()]);
            name.push(' ');
        }
        let it = item(&name, &files);
        assert!(!too_large(&it), "{script}: {:?}", read_cost(&it));
        let hit = c.verdict(&it).expect(script);
        assert_eq!(hit.rule, CoreRule::Cs4a, "{script}");
        // A 4 KB name in the script, with the pair at its end.
        let name = format!("{name} rolea lexa");
        let it = item(&name, &files[..1]);
        assert_eq!(
            c.verdict(&it).map(|h| h.rule),
            Some(CoreRule::Cs4a),
            "{script}"
        );
    }
}

fn judge(e: &Engine, name: &str, files: &[FileInfo]) -> Option<RuleRef> {
    let class = classify(name, files);
    let facts = Facts {
        class: &class,
        size: files.iter().map(|f| f.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 1000,
        now: 1000,
    };
    let it = Item {
        hashes: &[H],
        name,
        files,
        content_type: class.content_type,
    };
    e.judge(&it, Some(&facts), 1000).decision.map(|d| d.rule)
}

#[test]
fn too_large_to_judge_is_final() {
    let mut e = Engine::new(core());
    // Every entry that can pass the core, and an exception for the rule.
    e.overrides = vec![
        Override {
            id: 1,
            what: Match::Hashes(vec![H]),
            strength: Strength::Override,
            scope: None,
            expires_at: None,
            shadow_until: None,
        },
        Override {
            id: 2,
            what: Match::Keyword("title".into()),
            strength: Strength::Override,
            scope: None,
            expires_at: None,
            shadow_until: None,
        },
    ]
    .into();
    e.allow.push(Allow {
        id: 1,
        query: parse("title").unwrap(),
    });
    let files: Vec<FileInfo> = (0..=MAX_FILES).map(|i| file(&format!("{i}.jpg"))).collect();
    assert_eq!(
        judge(&e, "Title", &files),
        Some(RuleRef::Core(CoreRule::Cs0a))
    );
    // Within the budget, the same entries pass it.
    assert_eq!(judge(&e, "Title", &files[..10]), None);
}

#[test]
fn a_locked_rule_takes_no_exception() {
    let e = Exception {
        id: 1,
        rule: RuleRef::Core(CoreRule::Cs0a),
        what: Match::Hashes(vec![H]),
    };
    assert!(e.validate().is_err());
}

/// Where a term pair sits in a torrent.
#[derive(Debug, Clone, Copy)]
enum Place {
    Name,
    Path,
    Folder,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A pair placed anywhere in a torrent within the budget, after and
    /// before any number of other words, in the name, a file name or a folder
    /// of any file, is always caught.
    #[test]
    fn a_pair_anywhere_within_the_budget_is_caught(
        before in 0usize..400,
        after in 0usize..400,
        seed in 0usize..64,
        files in 1usize..1_500,
        at in any::<prop::sample::Index>(),
        place in prop_oneof![Just(Place::Name), Just(Place::Path), Just(Place::Folder)],
        adult in any::<bool>(),
    ) {
        let c = core();
        let text = format!("{} rolea lexa {}", plain(before, seed), plain(after, seed + 1));
        let at = at.index(files);
        let mut list: Vec<FileInfo> = (0..files)
            .map(|i| file(&format!("Disc {}/e{i}.mkv", i % 4)))
            .collect();
        let name = match place {
            Place::Name => text.clone(),
            Place::Path => {
                list[at] = file(&format!("Disc 1/{text}.mkv"));
                "Title".to_owned()
            }
            Place::Folder => {
                list[at] = file(&format!("{text}/e{at}.mkv"));
                "Title".to_owned()
            }
        };
        let it = Item {
            hashes: &[H],
            name: &name,
            files: &list,
            content_type: if adult { ContentType::Adult } else { ContentType::Video },
        };
        prop_assume!(!too_large(&it));
        let hits = c.hits(&it);
        prop_assert!(
            hits.iter().any(|h| h.rule == CoreRule::Cs4a && h.on),
            "{:?} at {} of {} files: {:?}", place, at, files, hits
        );
        prop_assert!(c.verdict(&it).is_some());
    }

    /// Past the budget, whatever the text, the torrent is held whole.
    #[test]
    fn past_the_budget_always_held(extra in 1usize..10_000, wide in any::<bool>()) {
        let c = core();
        let unit = if wide { "語" } else { "w" };
        let name = unit.repeat(BUDGET as usize / unit.len() + extra);
        let it = item(&name, &[]);
        prop_assert!(too_large(&it));
        prop_assert_eq!(c.verdict(&it).map(|h| h.rule), Some(CoreRule::Cs0a));
    }
}
