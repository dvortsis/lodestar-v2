//! The core reads a torrent whole or holds it (§20.5, §20.8): within its
//! work budget every word of the name and of every path is judged, and a
//! torrent past the budget is held as too large to judge (CS-0a). Stand-in
//! terms as in `core.rs`: `lexa` for a sexual term, `rolea` for a child
//! role, `codex` for a code term.

use lodestar_classify::{ContentType, FileInfo, classify};
use lodestar_filter::{
    Action, Allow, BUDGET, Core, CoreRule, CsList, Engine, Evidence, Exception, Facts, Field, Item,
    Lists, MAX_FILES, Match, Origin, Override, RuleRef, Strength, Switches, TermSet, read_cost,
    read_whole_by_v1, too_large,
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

/// Every word of a name or a path, and every file, is read: a pair at
/// the end of each, after any number of other words or files, is caught,
/// over a sweep of sizes from one up through every power of two the
/// budget allows. The evidence says where.
#[test]
fn the_last_word_and_the_last_file_are_always_read() {
    let c = core();
    let sizes = || (0..).map(|k| 1usize << k);
    for words in sizes().take_while(|&w| w <= 1 << 10) {
        let text = format!("{} rolea lexa", plain(words, words));
        // In the name.
        let files = [file("a.mkv")];
        let hit = c.verdict(&item(&text, &files)).expect("the name's pair");
        assert_eq!(hit.rule, CoreRule::Cs4a, "{words} words");
        assert!(matches!(
            hit.evidence[0],
            Evidence::ChildRole {
                field: Field::Name,
                ..
            }
        ));
        // In a file name, and in a folder.
        for path in [format!("Folder/{text}.mkv"), format!("{text}/a.mkv")] {
            let files = [file("a.mkv"), file(&path)];
            let hit = c.verdict(&item("Title", &files)).expect("the path's pair");
            assert_eq!(hit.rule, CoreRule::Cs4a, "{words} words");
            assert!(matches!(
                hit.evidence[0],
                Evidence::ChildRole {
                    field: Field::File(1),
                    ..
                }
            ));
        }
        // A code term at the end of the name, and of a path.
        let code = format!("{} codex", plain(words, 1));
        assert_eq!(
            c.verdict(&item(&code, &[file("a.mkv")])).map(|h| h.rule),
            Some(CoreRule::Cs3a)
        );
        let files = [file(&format!("{code}/a.mkv"))];
        assert_eq!(
            c.verdict(&item("Title", &files)).map(|h| h.rule),
            Some(CoreRule::Cs3b)
        );
    }
    for n in sizes().take_while(|&n| n <= 1 << 13) {
        let mut files: Vec<FileInfo> = (0..n).map(|i| file(&format!("e{i}.mkv"))).collect();
        files[n - 1] = file("rolea lexa.mkv");
        let it = item("Title", &files);
        assert!(!too_large(&it), "{n} short files are within the budget");
        let hit = c.verdict(&it).expect("the last file's pair");
        assert_eq!(hit.rule, CoreRule::Cs4a, "{n} files");
        assert!(matches!(
            hit.evidence[0],
            Evidence::ChildRole { field: Field::File(i), .. } if i as usize == n - 1
        ));
    }
}

/// A file the torrent marks as padding is read like any other: its path
/// is shown, so it's judged.
#[test]
fn a_padding_files_path_is_judged() {
    let c = core();
    let mut padding = file("Folder/rolea lexa.pad");
    padding.padding = true;
    let files = [file("Folder/a.mkv"), padding];
    let hit = c.verdict(&item("Title", &files)).expect("caught");
    assert_eq!(hit.rule, CoreRule::Cs4a);
    assert!(matches!(
        hit.evidence[0],
        Evidence::ChildRole {
            field: Field::File(1),
            ..
        }
    ));
    let e = Engine::new(core());
    assert_eq!(
        judge(&e, "Title", &files),
        Some(RuleRef::Core(CoreRule::Cs4a))
    );
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

/// A torrent the check before reading lets through but whose read
/// passes the budget is held as too large to judge by every way the
/// engine judges, before any override, Always allow or exception, read as
/// found or as Adult too.
#[test]
fn every_judgment_holds_what_its_read_finds_too_large() {
    use lodestar_filter::Trail;
    use lodestar_filter::packs::sp4::AtCommit;
    let mut e = Engine::new(core());
    e.overrides = vec![Override {
        id: 1,
        what: Match::Keyword("title".into()),
        strength: Strength::Override,
        scope: None,
        expires_at: None,
        shadow_until: None,
    }]
    .into();
    e.allow.push(Allow {
        id: 1,
        query: parse("title").unwrap(),
    });
    // Costlier to read than the budget, though not past it unread.
    let costly: Vec<FileInfo> = (0..400)
        .map(|i| file(&distinct_words(4_000, i + 1)))
        .collect();
    let it = item("Title", &costly);
    assert!(c_floor(&e, &it).is_none(), "not past the budget unread");
    assert!(too_large(&it));
    let class = classify("Title", &costly);
    let facts = Facts {
        class: &class,
        size: 1 << 30,
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 1000,
        now: 1000,
    };
    let cs0a = Some(RuleRef::Core(CoreRule::Cs0a));
    let rule = |v: lodestar_filter::Verdict| v.decision.map(|d| d.rule);
    assert_eq!(rule(e.judge(&it, Some(&facts), 1000)), cs0a);
    assert_eq!(rule(e.judge(&it, None, 1000)), cs0a);
    assert_eq!(rule(e.judge_shadowed(&it, Some(&facts), 1000).0), cs0a);
    assert_eq!(rule(e.judge_classified(&it, Some(&facts), 1000).0), cs0a);
    assert_eq!(
        rule(e.judge_classified_unshared(&it, Some(&facts), 1000).0),
        cs0a
    );
    for adult in [false, true] {
        let mut trail = Trail::default();
        let (v, _) = e.judge_classified_with(&it, Some(&facts), 1000, adult, Some(&mut trail));
        assert_eq!(rule(v), cs0a, "adult {adult}");
        let at = AtCommit::default();
        assert_eq!(rule(e.judge_at_commit(&it, &facts, 1000, at, adult)), cs0a);
        assert_eq!(
            rule(e.judge_at_commit_unshared(&it, &facts, 1000, at, adult)),
            cs0a
        );
        assert_eq!(
            rule(e.judge_at_commit_reusing(&trail, &it, &facts, 1000, at, adult)),
            cs0a,
            "adult {adult}"
        );
        let level = lodestar_filter::flood::Level::Demote;
        assert_eq!(rule(e.judge_flooded(&it, &facts, 1000, level, adult)), cs0a);
    }
    // Read as Adult from the start.
    let adult_item = Item {
        content_type: ContentType::Adult,
        ..it
    };
    assert_eq!(rule(e.judge(&adult_item, None, 1000)), cs0a);
}

fn c_floor(e: &Engine, it: &Item) -> Option<lodestar_filter::Hit> {
    e.core.floor_hit(it)
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

/// The in-repo corpora are judged whole: every torrent of the rule
/// packs' corpora and every release name of the release parser's golden
/// files is within the budget. These are small, made-up torrents, so it
/// says what an ordinary torrent costs, not what share of a real crawl
/// the budget holds. Prints the counts.
#[test]
fn the_repos_corpora_are_judged_whole() {
    let corpus = lodestar_filter::corpus::Corpus::builtin().expect("the shipped corpus parses");
    let mut examples: Vec<&lodestar_filter::corpus::Example> = corpus
        .packs
        .iter()
        .flat_map(|(_, _, cases)| cases.iter().map(|c| &c.example))
        .collect();
    examples.extend(corpus.precision.iter().map(|p| &p.example));
    let mut held = 0;
    for ex in &examples {
        let it = Item {
            hashes: &[H],
            name: &ex.name,
            files: &ex.files,
            content_type: ex.content_type.unwrap_or(ContentType::Other),
        };
        held += usize::from(too_large(&it));
    }
    let golden =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../release-parser/tests/golden");
    let mut names = 0;
    for entry in std::fs::read_dir(&golden).expect("the golden files") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "txt") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for name in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with([' ', '#']))
        {
            names += 1;
            held += usize::from(too_large(&item(name, &[])));
        }
    }
    println!(
        "held: {held} of {} pack-corpus torrents and {names} release names",
        examples.len()
    );
    // Every example was read (the corpora only grow).
    assert!(
        examples.len() >= 198 && names >= 317,
        "{} {names}",
        examples.len()
    );
    assert_eq!(held, 0);
}

/// What the earlier core read whole: a short torrent was, and the
/// re-judge reads it in its second phase. A long name or path, many files,
/// a padding file, or a text that reads as long only in the earlier
/// reading wasn't, so the re-judge reads it first. Sizes stay far from any
/// limit; the exact-input tests land after the release
/// (docs/security/v1/README.md §5).
#[test]
fn what_the_earlier_core_read_whole() {
    let words = |n: usize| "word ".repeat(n);
    let whole = |name: &str, files: &[FileInfo]| read_whole_by_v1(&item(name, files));
    let files = |n: usize| -> Vec<FileInfo> { (0..n).map(|i| file(&format!("{i}"))).collect() };
    assert!(whole("Some.Film.2001.1080p", &files(3)));
    assert!(!whole(&words(5_000), &[]));
    assert!(!whole("Title", &[file(&words(5_000))]));
    assert!(!whole("Title", &[file(&format!("dir/{}", words(5_000)))]));
    assert!(!whole("Title", &files(50_000)));
    let mut padded = files(2);
    padded[1].padding = true;
    assert!(!whole("Title", &padded), "its path wasn't read");
    // One word in the reading now, many in the earlier one.
    let marked = "word\u{FFF9}".repeat(5_000);
    assert!(!whole(&marked, &[]));
}

/// One file more than the core reads, so the torrent is CS-0a's.
fn too_many_files() -> Vec<FileInfo> {
    (0..=MAX_FILES).map(|i| file(&format!("{i}"))).collect()
}

/// A stored exception for a locked rule, a row that never passed
/// validation, is skipped when the rules load, with a note, and the
/// torrent it names is still held.
#[test]
fn a_stored_exception_to_a_locked_rule_is_skipped() {
    use lodestar_filter::load::KIND_HASHES;
    use lodestar_store::{ExceptionRow, RuleSet};
    let mut e = Engine::new(core());
    let set = RuleSet {
        exceptions: vec![ExceptionRow {
            id: 7,
            rule: "CS-0a".into(),
            kind: KIND_HASHES,
            value: H.to_vec(),
        }],
        ..RuleSet::default()
    };
    let notes = e.load_rules(&set);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("exception 7") && n.contains("locked")),
        "{notes:?}"
    );
    assert!(e.exceptions.is_empty());
    let files = too_many_files();
    assert_eq!(
        judge(&e, "Title", &files),
        Some(RuleRef::Core(CoreRule::Cs0a))
    );
}

/// An exception set on an engine directly, past validation and loading,
/// applies to a rule that isn't locked: the control for the engine's own
/// locked-rule guard, which is tested in `engine.rs` (no public judgment
/// reaches it, since every one holds an over-budget torrent first).
#[test]
fn an_engine_applies_an_exception_to_a_rule_that_isnt_locked() {
    let pair = [file("rolea lexa")];
    assert_eq!(
        judge(&Engine::new(core()), "Title", &pair),
        Some(RuleRef::Core(CoreRule::Cs4a))
    );
    let mut e = Engine::new(core());
    e.exceptions.push(Exception {
        id: 1,
        rule: RuleRef::Core(CoreRule::Cs4a),
        what: Match::Hashes(vec![H]),
    });
    assert_ne!(
        judge(&e, "Title", &pair),
        Some(RuleRef::Core(CoreRule::Cs4a))
    );
}

/// Text drawn from a generic mix of letters, digits, spaces, dots and the
/// characters once read as word breaks, the same for the same seed.
fn mixed(chars: usize, seed: u64) -> String {
    const MIX: &[char] = &[
        'a', 'e', 'k', 'q', 'z', '0', '3', '7', ' ', ' ', '.', '\u{2028}', '\u{2029}',
    ];
    let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..chars)
        .map(|_| {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            MIX[r as usize % MIX.len()]
        })
        .collect()
}

/// Both readings of a torrent (with the characters once read as word
/// breaks read so, and not) reach the end of every text, or it's held:
/// a pair at the end of a long path is caught or held, over a sweep of
/// path depths and lengths of generic text before it, by the core and
/// the engine alike, and held exactly where a reading stops short.
/// Either reading can take more tokens than the other, so neither one's
/// bound covers both.
#[test]
fn every_reading_reaches_the_end_or_holds() {
    let c = core();
    let e = Engine::new(core());
    let both = |rule: Option<CoreRule>| matches!(rule, Some(CoreRule::Cs4a | CoreRule::Cs0a));
    let mut held = [0usize; 2];
    for segments in (0..=8).step_by(2).map(|k| 1usize << k) {
        for chars in std::iter::once(0).chain((0..=16).step_by(2).map(|k| 1usize << k)) {
            let mut path: Vec<String> = (0..segments).map(|i| mixed(64, i as u64 + 1)).collect();
            path.push(mixed(chars, chars as u64 + 7));
            path.push("rolea\u{2028}lexa".into());
            let files = [FileInfo::new(path, 1)];
            let it = item("Title", &files);
            let by_core = c.verdict(&it).map(|h| h.rule);
            assert!(both(by_core), "{segments}/{chars}: {by_core:?}");
            let by_engine = match judge(&e, "Title", &files) {
                Some(RuleRef::Core(r)) => Some(r),
                _ => None,
            };
            assert!(both(by_engine), "{segments}/{chars}: {by_engine:?}");
            assert_eq!(by_core == Some(CoreRule::Cs0a), too_large(&it));
            held[usize::from(by_core == Some(CoreRule::Cs0a))] += 1;
        }
    }
    // The sweep reaches both: caught whole, and held.
    assert!(held[0] > 0 && held[1] > 0, "caught, held: {held:?}");
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
