//! NEAR with words the tokenizer splits into several tokens (`1080p`,
//! `x264`, `web-dl`), checked against a brute-force reference over random
//! token streams, through the rule engine's narrowing, and for cost.
//!
//! A NEAR word matches as its parts in a row, or as one token of a form
//! covering all of them (`webdl` for `web-dl`). Its distance to another
//! word runs between the nearest positions of the two (§16).

use std::time::Instant;

use lodestar_classify::{ContentType, FileInfo, classify};
use lodestar_filter::{
    Action, Core, CsList, Doc, Engine, Facts, Item, Lists, PackSwitches, RuleRef, Switches,
    TermSet, UserRule,
};
use lodestar_search::parse;
use lodestar_search::tokenizer::{TokKind, tokenize};
use proptest::prelude::*;

/// The positions a NEAR word covers at each of its places in `text`,
/// found by trying every start: its parts at consecutive positions, or a
/// token equal to a form that spans all of its parts.
fn places(text: &str, word: &str) -> Vec<Vec<usize>> {
    let toks = tokenize(text);
    let q = tokenize(word);
    let parts: Vec<_> = q.iter().filter(|t| t.kind == TokKind::Part).collect();
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return Vec::new();
    };
    let forms: Vec<&str> = q
        .iter()
        .filter(|t| t.kind != TokKind::Part && t.from <= first.from && t.to >= last.to)
        .map(|t| t.text.as_str())
        .collect();
    let end = toks.iter().map(|t| t.position + 1).max().unwrap_or(0);
    let mut out = Vec::new();
    for start in 0..end {
        let has = |pos: usize, text: &str| toks.iter().any(|t| t.position == pos && t.text == text);
        if parts.iter().zip(start..).all(|(p, pos)| has(pos, &p.text)) {
            out.push((start..start + parts.len()).collect());
        }
        if forms.iter().any(|f| has(start, f)) {
            out.push(vec![start]);
        }
    }
    out
}

/// Brute force: some place of the first word, and some place of every
/// other word, whose closest positions are fewer than `words + slop`
/// apart.
fn reference(text: &str, words: &[&str], slop: u32) -> bool {
    let all: Vec<_> = words.iter().map(|w| places(text, w)).collect();
    let span = words.len() + slop as usize;
    let apart = |a: &[usize], b: &[usize]| {
        a.iter()
            .flat_map(|x| b.iter().map(move |y| x.abs_diff(*y)))
            .min()
            .unwrap_or(usize::MAX)
    };
    all[0]
        .iter()
        .any(|a| all.iter().all(|ps| ps.iter().any(|b| apart(a, b) < span)))
}

/// The previous rule, for words of one token: positions of that token,
/// each within the window of a place of the first.
fn single_token_rule(text: &str, words: &[&str], slop: u32) -> bool {
    let toks = tokenize(text);
    let at: Vec<Vec<usize>> = words
        .iter()
        .map(|w| {
            let w = tokenize(w).remove(0).text;
            toks.iter()
                .filter(|t| t.text == w)
                .map(|t| t.position)
                .collect()
        })
        .collect();
    let span = words.len() + slop as usize;
    at[0].iter().any(|&p0| {
        at.iter()
            .all(|ps| ps.iter().any(|&p| p.abs_diff(p0) < span))
    })
}

fn matches(name: &str, query: &str) -> bool {
    let node = parse(query).unwrap();
    let item = Item {
        hashes: &[[3; 20]],
        name,
        files: &[],
        content_type: ContentType::Other,
    };
    Doc::new(&item, None).matches(&node, &[])
}

/// Words that split into several tokens, their parts, compounds and run
/// together forms, plain words, and CJK (bigrams). `x264+` and `aB-cd`
/// make the tokenizer emit a compound after a later part, so positions
/// come out of order.
const VOCAB: &[&str] = &[
    "1080p",
    "1080",
    "p",
    "x264",
    "x",
    "264",
    "web-dl",
    "WEB-DL",
    "WEBDL",
    "web",
    "dl",
    "DD+",
    "dd",
    "5.1",
    "51",
    "5",
    "1",
    "H.264",
    "h264",
    "ShowName",
    "show",
    "name",
    "S01E02",
    "s01",
    "e02",
    "a",
    "b",
    "c",
    "x264+",
    "aB-cd",
    "ab",
    "cd",
    "abcd",
    "日本語",
    "日本",
    "本語",
    "映画",
];

/// Up to 40 words, so a word can have more places than the plain scans
/// cover and the windowed search runs too.
fn stream() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(prop::sample::select(VOCAB), 1..40),
        prop::collection::vec(prop::sample::select(vec![" ", ".", "-", "_", " "]), 40),
    )
        .prop_map(|(w, s)| w.iter().zip(s).map(|(w, s)| format!("{w}{s}")).collect())
}

fn render(words: &[&str], slop: u32) -> String {
    words
        .iter()
        .map(|w| format!("\"{w}\""))
        .collect::<Vec<_>>()
        .join(&format!(" NEAR/{slop} "))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// NEAR matches exactly when the brute force finds places close
    /// enough, for any words, and its negation is the opposite.
    #[test]
    fn near_is_the_brute_force(
        text in stream(),
        words in prop::collection::vec(prop::sample::select(VOCAB), 2..5),
        slop in 1u32..5,
    ) {
        let q = words.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(&format!(" NEAR/{slop} "));
        let expect = reference(&text, &words, slop);
        prop_assert_eq!(matches(&text, &q), expect, "{} on {:?}", q, text);
        prop_assert_eq!(matches(&text, &format!("-({q})")), !expect, "-({}) on {:?}", q, text);
        // Phrases inside NEAR are their words, each a NEAR word.
        prop_assert_eq!(matches(&text, &render(&words, slop)), expect);
    }

    /// Through the engine: the rule index and the cheap check never stop
    /// a NEAR rule that matches, among rules the torrent doesn't match.
    #[test]
    fn near_rules_reach_the_engine(
        text in stream(),
        words in prop::collection::vec(prop::sample::select(VOCAB), 2..4),
        slop in 1u32..4,
        path in any::<bool>(),
    ) {
        let q = words.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(&format!(" NEAR/{slop} "));
        let mut rules = vec![user(1, &q)];
        rules.extend((2..40).map(|i| user(i, &format!("kzorq{i} | \"{} vmikq{i}\"", words[0]))));
        let mut e = Engine::new(Core::new(
            Lists { sexual: TermSet::of(&[]), child_roles: TermSet::of(&[]), codes: TermSet::of(&[]) },
            CsList::default(),
            Switches::default(),
        ));
        e.rules = rules.into();
        let (name, files) = if path { ("x", vec![(format!("d/{text}.mkv"), 1 << 30)]) } else { (text.as_str(), vec![("a.mkv".to_owned(), 1 << 30)]) };
        let files: Vec<(&str, u64)> = files.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        // NEAR reads the name and every path.
        let expect = reference(name, &words, slop)
            || files.iter().any(|(p, _)| reference(p, &words, slop));
        prop_assert_eq!(judge(&e, name, &files) == Some(RuleRef::User(1)), expect, "{} on {:?}", q, text);
    }

    /// Long names of words whose compounds come after their later parts,
    /// so the windowed search runs on positions out of order.
    #[test]
    fn out_of_order_names_are_the_brute_force(
        words in prop::collection::vec(
            prop::sample::select(vec!["x264+", "aB-cd", "x", "264", "x264", "abcd", "ab", "cd", "b", "zz"]),
            20..60,
        ),
        query in prop::collection::vec(
            prop::sample::select(vec!["x264+", "aB-cd", "x264", "abcd", "x", "264", "cd", "b"]),
            2..4,
        ),
        slop in 1u32..4,
    ) {
        let text = words.join(" ");
        let q = query.join(&format!(" NEAR/{slop} "));
        prop_assert_eq!(matches(&text, &q), reference(&text, &query, slop), "{} on {:?}", q, text);
    }

    /// Words of one token match as they did before.
    #[test]
    fn single_tokens_are_unchanged(
        text in stream(),
        words in prop::collection::vec(
            prop::sample::select(vec!["1080", "p", "x", "264", "webdl", "web", "dl", "dd", "51", "showname", "a", "b"]),
            2..5,
        ),
        slop in 1u32..5,
    ) {
        let q = words.join(&format!(" NEAR/{slop} "));
        prop_assert_eq!(matches(&text, &q), single_token_rule(&text, &words, slop), "{} on {:?}", q, text);
    }
}

#[test]
fn edges() {
    let name = "Show.Name.S01E02.1080p.WEB-DL.DD+5.1.x264-GROUP";
    // show 0, name 1, s 01 e 02 2–5, 1080 p 6–7, web dl 8–9, dd 10,
    // 5 1 11–12, x 264 13–14, group 15.
    for (q, expect) in [
        ("1080p NEAR/1 web-dl", true),
        ("web-dl NEAR/1 1080p", true),
        ("s01e02 NEAR/1 1080p", true),
        ("name NEAR/1 1080p", false),
        ("name NEAR/5 1080p", true),
        ("name NEAR/4 1080p", true),
        ("name NEAR/3 1080p", false),
        ("x264 NEAR/1 group", true),
        ("dd+ NEAR/1 5.1 NEAR/1 x264", true),
        ("1080p NEAR/1 x264", false),
        // An operand inside a compound token.
        ("264 NEAR/1 group", true),
        ("e02 NEAR/1 1080p", true),
        ("e02 NEAR/1 web-dl", false),
        ("\"e 02\" NEAR/1 1080p", true),
        // At the window's edge: x264 ends at 14, group is at 15.
        ("x264 NEAR/1 show", false),
        ("group NEAR/14 show", true),
        ("group NEAR/13 show", false),
        // Repeats.
        ("1080p NEAR/1 1080p", true),
        ("x264 NEAR/1 x NEAR/1 264", true),
        // Out of order or missing parts.
        ("p1080 NEAR/5 show", false),
        ("x265 NEAR/5 group", false),
    ] {
        assert_eq!(matches(name, q), expect, "{q}");
        assert_eq!(matches(name, &format!("-({q})")), !expect, "-({q})");
    }
    // A run-together name matches the hyphenated word, and the reverse.
    assert!(matches("Show WEBDL x264", "web-dl NEAR/1 x264"));
    assert!(matches("Show WEB-DL x264", "webdl NEAR/1 x264"));
    // A compound token sits at its first part's position, as in search:
    // web 0, webdl 0, dl 1, a 2, x264 3.
    assert!(!matches("WEB-DL.a.x264", "webdl NEAR/1 x264"));
    assert!(matches("WEB-DL.a.x264", "web-dl NEAR/1 x264"));
    // A plain word matches a form of only some of its parts; a NEAR word
    // doesn't (§16, a follow-up for the plain word).
    assert!(matches("Show WEBDL", "web-dl.x264"));
    assert!(!matches("Show WEBDL", "web-dl.x264 NEAR/1 show"));
}

fn engine() -> Engine {
    let core = Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    );
    let mut e = Engine::new(core);
    e.packs = PackSwitches::all();
    e
}

fn judge(e: &Engine, name: &str, paths: &[(&str, u64)]) -> Option<RuleRef> {
    let f: Vec<FileInfo> = paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect();
    let class = classify(name, &f);
    let facts = Facts {
        class: &class,
        size: f.iter().map(|x| x.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let item = Item {
        hashes: &[[9; 20]],
        name,
        files: &f,
        content_type: class.content_type,
    };
    e.judge(&item, Some(&facts), 0).decision.map(|d| d.rule)
}

fn user(id: u32, query: &str) -> UserRule {
    UserRule {
        id,
        query: parse(query).unwrap(),
        action: Action::Hide,
        enabled: true,
        shadow_until: None,
        unless: None,
    }
}

/// Through the engine, so the rule index and the cheap check let the
/// rule reach the full match.
#[test]
fn rules_with_such_words_match_through_the_engine() {
    let mut e = engine();
    e.rules = vec![
        user(1, "1080p NEAR/1 web-dl"),
        user(2, "x264 NEAR/1 trailer"),
        user(3, "path:x264 NEAR/1 path:group"),
    ]
    .into();
    let plain: &[(&str, u64)] = &[("a.mkv", 2 << 30)];
    assert_eq!(
        judge(&e, "Show.S01E02.1080p.WEB-DL.x264-GROUP", plain),
        Some(RuleRef::User(1))
    );
    assert_eq!(
        judge(&e, "Show.S01E02.720p.HDTV.x264.Trailer", plain),
        Some(RuleRef::User(2))
    );
    let files: &[(&str, u64)] = &[("Show.720p.HDTV.x264-GROUP.mkv", 2 << 30)];
    assert_eq!(judge(&e, "Something Else", files), Some(RuleRef::User(3)));
    assert_eq!(judge(&e, "Show.S01E02.1080p.WEB.x264", plain), None);
    assert_eq!(judge(&e, "Show.S01E02.1080p.HDTV.AAC.WEB-DL", plain), None);
}

/// The core's rules are built in, not queries: none uses NEAR, so this
/// leaves them as they were. They still match a name full of words NEAR
/// now reads as several tokens.
#[test]
fn the_core_is_unchanged() {
    let core = Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    );
    let files = vec![FileInfo::new(
        vec!["Show.S01E02.1080p.WEB-DL.x264-GROUP.mkv".into()],
        2 << 30,
    )];
    let item = Item {
        hashes: &[[1; 20]],
        name: "Show.S01E02.1080p.WEB-DL.x264-GROUP",
        files: &files,
        content_type: ContentType::Video,
    };
    assert_eq!(core.verdict(&item), None);
    let mut e = engine();
    e.rules = vec![user(1, "1080p NEAR/1 web-dl")].into();
    let v = judge(&e, item.name, &[(&files[0].path[0], 2 << 30)]);
    assert_eq!(v, Some(RuleRef::User(1)));
}

/// What NEAR rules cost (§20.8): 10,000 rules, `share` per cent of them
/// NEAR on words a video set has but never close enough, judged over
/// that set. Measured, not held: `cargo test --release -p
/// lodestar-filter --test near -- --ignored --nocapture near_cost`.
#[test]
#[ignore]
fn near_cost() {
    let set = videos(512);
    let classes: Vec<_> = set.iter().map(|(n, f)| classify(n, f)).collect();
    let items: Vec<Item> = set
        .iter()
        .zip(&classes)
        .map(|((name, files), c)| Item {
            hashes: &[[5; 20]],
            name,
            files,
            content_type: c.content_type,
        })
        .collect();
    let facts: Vec<Facts> = classes
        .iter()
        .zip(&set)
        .map(|(class, (_, f))| Facts {
            class,
            size: f.iter().map(|f| f.size).sum(),
            piece_len: 1 << 22,
            flags: 0,
            discovered_at: 0,
            now: 0,
        })
        .collect();
    const SHAPES: &[(&str, &str)] = &[
        // Several tokens.
        ("1080p NEAR/1 sample", "multi"),
        ("x264 NEAR/1 srt", "multi"),
        ("web-dl NEAR/1 mkv", "multi"),
        ("ddp5.1 NEAR/1 subs", "multi"),
        // One token each.
        ("srt NEAR/1 subs", "single"),
        ("nfo NEAR/1 sample", "single"),
    ];
    for kind in ["single", "multi"] {
        for share in [1usize, 10] {
            let shapes: Vec<&str> = SHAPES.iter().filter(|s| s.1 == kind).map(|s| s.0).collect();
            let rules: Vec<UserRule> = (0..10_000u32)
                .map(|i| {
                    let q = if (i as usize) % 100 < share {
                        shapes[i as usize / 100 % shapes.len()].to_owned()
                    } else {
                        format!("k{}q{i}", ["zor", "vel", "mik", "tas"][i as usize % 4])
                    };
                    user(i + 1, &q)
                })
                .collect();
            let mut e = engine();
            e.rules = rules.into();
            let judge = |i: usize| {
                let v = e.judge(std::hint::black_box(&items[i]), Some(&facts[i]), 0);
                assert!(
                    v.decision
                        .is_none_or(|d| !matches!(d.rule, RuleRef::User(_))),
                    "no rule matches the set: {}",
                    items[i].name
                );
            };
            (0..items.len()).for_each(judge);
            let mut passes: Vec<_> = (0..7)
                .map(|_| {
                    let start = Instant::now();
                    (0..items.len()).for_each(judge);
                    start.elapsed() / items.len() as u32
                })
                .collect();
            passes.sort();
            let (met, full) = (0..items.len()).fold((0, 0), |(m, f), i| {
                let (a, b) = e.rule_counts(&items[i], Some(&facts[i]));
                (m + a, f + b)
            });
            println!(
                "NEAR, {kind} tokens, {share}% of 10,000 rules: {:?} per torrent; \
                 {:.0} rules met, {:.0} fully matched per torrent (passes {passes:?})",
                passes[3],
                met as f64 / items.len() as f64,
                full as f64 / items.len() as f64,
            );
        }
    }
}

/// Video releases like the budget's set (props.rs): films and season
/// packs with samples, subtitles and NFOs.
fn videos(n: usize) -> Vec<(String, Vec<FileInfo>)> {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let pick = |of: &[&'static str], r: u64| of[r as usize % of.len()];
    (0..n)
        .map(|i| {
            let title = format!("Kalo{i}.Mertin");
            let res = pick(&["720p", "1080p", "2160p"], next());
            let src = pick(&["WEB-DL", "BluRay", "WEBRip", "HDTV"], next());
            let audio = pick(&["DDP5.1", "AAC2.0", "FLAC"], next());
            let codec = pick(&["x264", "H.264", "x265", "HEVC"], next());
            let name = format!("{title}.{res}.{src}.{audio}.{codec}-GRP");
            let mut files = Vec::new();
            if next() % 2 == 0 {
                files.push(FileInfo::new(vec![format!("{name}.mkv")], 4 << 30));
                files.push(FileInfo::new(
                    vec!["Sample".into(), "sample.mkv".into()],
                    50 << 20,
                ));
                files.push(FileInfo::new(vec![format!("{name}.nfo")], 4_000));
            } else {
                for e in 1..=(6 + next() % 20) {
                    let ep = format!("{title}.S01E{e:02}.{res}.{src}.{audio}.{codec}-GRP");
                    files.push(FileInfo::new(
                        vec![name.clone(), format!("{ep}.mkv")],
                        1_500_000_000 + e,
                    ));
                    files.push(FileInfo::new(
                        vec![name.clone(), "Subs".into(), format!("{ep}.en.srt")],
                        40_000,
                    ));
                }
            }
            (name, files)
        })
        .collect()
}

/// A hostile name: one word repeated `n` times, three fillers, then the
/// other word `n` times, never close enough for `NEAR/1`.
fn hostile(a: &str, b: &str, n: usize) -> String {
    let mut s = format!("{a} ").repeat(n);
    s.push_str("zz zz zz ");
    s.push_str(&format!("{b} ").repeat(n));
    s
}

/// Seconds to judge `name` against `query` `times` times, tokenizing
/// afresh each time as a new torrent would.
fn time(name: &str, query: &str, times: u32) -> f64 {
    let node = parse(query).unwrap();
    let start = Instant::now();
    for _ in 0..times {
        let item = Item {
            hashes: &[[3; 20]],
            name,
            files: &[],
            content_type: ContentType::Other,
        };
        assert!(!Doc::new(&item, None).matches(&node, &[]));
    }
    start.elapsed().as_secs_f64() / f64::from(times)
}

/// Names from the DHT are untrusted (§6): a word repeated hundreds of
/// times must not make NEAR cost grow faster than the name. Each name
/// here is 2,000 tokens; the bound is far above the linear cost even in
/// a debug build, and far below the quadratic one.
#[test]
fn hostile_names_stay_cheap() {
    for (a, b, q) in [
        ("1080p", "x264", "1080p NEAR/1 x264"),
        ("aa", "bb", "aa NEAR/1 bb"),
        ("web-dl", "x", "web-dl NEAR/1 x"),
    ] {
        let parts = tokenize(a)
            .iter()
            .filter(|t| t.kind == TokKind::Part)
            .count();
        let name = hostile(a, b, 2000 / (parts + 1));
        let secs = time(&name, q, 1);
        assert!(secs < 0.2, "{q}: {secs:.3} s on a 2,000-token name");
    }
}

/// The hostile shape's cost per text, in release: `cargo test --release
/// -p lodestar-filter --test near -- --ignored --nocapture near_hostile`.
#[test]
#[ignore]
fn near_hostile() {
    for (a, b, q, n) in [
        ("1080p", "x264", "1080p NEAR/1 x264", 84),
        ("1080p", "x264", "1080p NEAR/1 x264", 336),
        ("aa", "bb", "aa NEAR/1 bb", 250),
        ("aa", "bb", "aa NEAR/1 bb", 1000),
    ] {
        let name = hostile(a, b, n);
        let tokens = tokenize(&name).len();
        let secs = time(&name, q, 20);
        println!("{q} on {tokens} tokens: {:.1} µs per text", secs * 1e6);
    }
}

/// The switch from plain scans to the windowed search, after 8: the
/// first word's place that matches comes after 7 to 10 that don't, with
/// the other words after it or before it, and in a chain of three.
#[test]
fn the_scan_threshold_changes_nothing() {
    let far = " zz zz zz ";
    for (first, second, third) in [("a", "b", "c"), ("1080p", "x264", "web-dl")] {
        for misses in 7..=10 {
            let lead = format!("{first}{far}").repeat(misses);
            for (tail, words) in [
                (format!("{first} {second}"), vec![first, second]),
                (format!("{second} {first}"), vec![first, second]),
                (
                    format!("{first} {second} {third}"),
                    vec![first, second, third],
                ),
                (
                    format!("{third} {second} {first}"),
                    vec![first, second, third],
                ),
                (format!("{first}{far}{second}"), vec![first, second]),
                (format!("{second}{far}{first}"), vec![first, second]),
            ] {
                let name = format!("{lead}{tail}");
                let q = words.join(" NEAR/1 ");
                let expect = reference(&name, &words, 1);
                assert_eq!(matches(&name, &q), expect, "{q} on {name:?}");
                assert_eq!(expect, !tail.contains("zz"), "{q} on {name:?}");
            }
        }
    }
}
