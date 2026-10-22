//! Hostile input (§6 "Hostile-input limits"): any text, any file list.

use std::time::Instant;

use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::{Core, CsList, Fixture, Item, Lists, Switches, normalize};
use proptest::prelude::*;

fn shipped() -> Core {
    Core::new(
        Lists::shipped().unwrap(),
        CsList::default(),
        Switches::default(),
    )
}

proptest! {
    #[test]
    fn fixtures_round_trip(
        name in "\\PC{0,200}",
        paths in prop::collection::vec("\\PC{0,60}", 0..20),
        ct in 0u8..12,
    ) {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let f = Fixture::of(&name, &paths, ContentType::from_code(ct).unwrap());
        let bytes = f.encode();
        prop_assert_eq!(Fixture::decode(&bytes), Some(f));
        // Any truncation or trailing byte is refused, never misread.
        for cut in 0..bytes.len() {
            prop_assert_eq!(Fixture::decode(&bytes[..cut]), None);
        }
        let mut longer = bytes.clone();
        longer.push(0);
        prop_assert_eq!(Fixture::decode(&longer), None);
    }

    #[test]
    fn fixture_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = Fixture::decode(&bytes);
        let mut v1 = vec![1u8, 0, 0xff, 0xff];
        v1.extend_from_slice(&bytes);
        prop_assert_eq!(Fixture::decode(&v1), None, "file count over the cap");
    }

    #[test]
    fn tokens_are_bounded(text in "\\PC{0,600}") {
        let mut out = Vec::new();
        normalize::tokens(&text, &mut out);
        prop_assert!(out.len() <= normalize::MAX_TOKENS);
        let words = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).count();
        // Each word adds its parts, its whole form and one alternative.
        prop_assert!(out.iter().all(|t| (t.pos as usize) <= words * 32 + 1));
    }

    #[test]
    fn core_never_panics(
        name in "\\PC{0,300}",
        paths in prop::collection::vec(("\\PC{0,80}", 0u64..u64::MAX), 0..40),
        adult in any::<bool>(),
    ) {
        let core = shipped();
        let files: Vec<FileInfo> = paths
            .into_iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), s))
            .collect();
        let item = Item {
            hashes: &[[0; 20]],
            name: &name,
            files: &files,
            content_type: if adult { ContentType::Adult } else { ContentType::Video },
        };
        let verdict = core.verdict(&item);
        let hits = core.hits(&item);
        prop_assert_eq!(verdict, hits.into_iter().find(|h| h.on));
    }
}

/// A deterministic generator, so every run judges the same set.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.next() as usize % of.len()]
    }

    /// A made-up title word, so titles differ from torrent to torrent and
    /// the tokenizer's per-word caches aren't always warm.
    fn word(&mut self) -> String {
        let syl = [
            "ka", "lo", "mer", "tin", "sa", "vor", "el", "dra", "qui", "zan", "pe", "ru",
        ];
        let n = 2 + self.next() as usize % 3;
        let mut w: String = (0..n).map(|_| self.pick(&syl)).collect();
        w[..1].make_ascii_uppercase();
        w
    }
}

/// A varied set of video releases: films and season packs with 1 to 60
/// files, extras, subtitles and samples, from different sources, codecs
/// and groups, every one with `.mkv` files.
fn torrents(n: usize) -> Vec<(String, Vec<FileInfo>)> {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    (0..n)
        .map(|_| {
            let title = (0..1 + r.next() as usize % 3)
                .map(|_| r.word())
                .collect::<Vec<_>>()
                .join(".");
            let res = r.pick(&["720p", "1080p", "2160p", "480p"]);
            let src = r.pick(&["WEB-DL", "BluRay", "WEBRip", "HDTV", "BDRip"]);
            let codec = r.pick(&["x264", "H.264", "x265", "HEVC", "AV1"]);
            let audio = r.pick(&[
                "DDP5.1",
                "AAC2.0",
                "DTS-HD.MA.5.1",
                "TrueHD.7.1.Atmos",
                "FLAC",
            ]);
            let group = r.word().to_uppercase();
            if r.next() % 3 == 0 {
                let year = 1960 + r.next() % 65;
                let name = format!("{title}.{year}.{res}.{src}.{audio}.{codec}-{group}");
                let mut files = vec![FileInfo::new(vec![format!("{name}.mkv")], 4 << 30)];
                if r.next() % 2 == 0 {
                    files.push(FileInfo::new(
                        vec!["Sample".into(), "sample.mkv".into()],
                        50 << 20,
                    ));
                    files.push(FileInfo::new(vec![format!("{name}.nfo")], 4_000));
                }
                (name, files)
            } else {
                let season = 1 + r.next() % 12;
                let name = format!("{title}.S{season:02}.{res}.{src}.{audio}.{codec}-{group}");
                let episodes = [6u64, 10, 13, 22, 30][r.next() as usize % 5];
                let subs = r.next() % 2 == 0;
                let mut files = Vec::new();
                for e in 1..=episodes {
                    let ep = format!(
                        "{title}.S{season:02}E{e:02}.{}.{res}.{src}.{audio}.{codec}-{group}",
                        r.word()
                    );
                    files.push(FileInfo::new(
                        vec![name.clone(), format!("{ep}.mkv")],
                        1_500_000_000 + e,
                    ));
                    if subs {
                        files.push(FileInfo::new(
                            vec![name.clone(), "Subs".into(), format!("{ep}.en.srt")],
                            40_000,
                        ));
                    }
                }
                (name, files)
            }
        })
        .collect()
}

/// Words the set's names and paths have: resolutions, sources, codecs,
/// audio, extensions, folders.
const COMMON: &[&str] = &[
    "mkv", "1080p", "720p", "2160p", "web", "dl", "webrip", "bluray", "hdtv", "x264", "x265", "h",
    "264", "hevc", "ddp5", "aac2", "dts", "atmos", "sample", "subs", "srt", "en", "nfo",
];

/// Prefixes whose first four characters the set's words have, matching
/// none of them.
const PREFIXES: &[&str] = &[
    "1080z", "2160z", "720px", "webdz", "blurx", "x264z", "ddp5z", "samplz",
];

/// Pairs of those words in an order release names don't use.
const REVERSED: &[(&str, &str)] = &[
    ("mkv", "1080p"),
    ("dl", "web"),
    ("264", "h"),
    ("srt", "subs"),
    ("1080p", "dl"),
    ("x264", "ddp5"),
    ("srt", "en"),
    ("mkv", "sample"),
];

/// 10,000 rules of the kinds people write, `share` per cent of them
/// built on the words the set has, so the narrowing can't rule them out
/// by those words, and matching nothing:
/// - a common word with one no torrent has (the review's `1080p <unique>`);
/// - a phrase of two common words in an order no name has;
/// - a common word with a size no torrent reaches;
/// - a `file:(…)` clause with a common word and a size no file reaches;
/// - a common word with a type the set doesn't have.
///
/// The rest are keywords, phrases, release groups, narrowed keywords,
/// alternatives, and a few rules no word narrows (sizes, prefixes and
/// fuzzy words, which get the full match), none matching the set.
fn ten_thousand_rules(share: usize) -> Vec<lodestar_filter::UserRule> {
    let word = |i: usize, salt: &str| format!("{salt}{}q{i}", ["zor", "vel", "mik", "tas"][i % 4]);
    let common = |i: usize| COMMON[i / 5 % COMMON.len()];
    (0..10_000)
        .map(|i| {
            let q = if i % 100 < share {
                match i % 5 {
                    0 => format!("{} {}", common(i), word(i, "u")),
                    1 => {
                        let (a, b) = REVERSED[i / 5 % REVERSED.len()];
                        format!("\"{a} {b}\"")
                    }
                    2 => format!("{} size:>{}TB", common(i), i % 7 + 2),
                    3 => format!("file:({} size:>{}TB)", common(i), i % 7 + 2),
                    _ => format!("{} type:software", common(i)),
                }
            } else {
                match i % 100 {
                    0..=53 => word(i, "k"),
                    54..=73 => format!("\"{} {}\"", word(i, "a"), word(i, "b")),
                    74..=83 => format!("group:{}", word(i, "g")),
                    84..=93 => format!("{} type:movie -{}", word(i, "c"), word(i, "d")),
                    94..=98 => format!("{} | {}", word(i, "e"), word(i, "f")),
                    // Rules no word narrows: sizes, prefixes that start
                    // like the set's words but match none, fuzzy words.
                    _ => match i / 100 % 3 {
                        0 => format!("size:>{}TB files:>{}", i % 7 + 2, 50_000 + i),
                        1 => format!("{}*", PREFIXES[i / 300 % PREFIXES.len()]),
                        _ => format!("~{}", word(i, "z")),
                    },
                }
            };
            lodestar_filter::UserRule {
                id: i as u32 + 1,
                query: lodestar_search::parse(&q).unwrap(),
                action: lodestar_filter::Action::Hide,
                enabled: true,
                shadow_until: None,
                unless: None,
            }
        })
        .collect()
}

/// §20.8 and the owner's budget (§1.6.6): under 50 µs per torrent at
/// 10,000 rules. Ignored in debug test runs; CI runs it in release (the
/// `filter budget` job): `cargo test --release -p lodestar-filter --test
/// props -- --ignored --nocapture budget`.
///
/// Each pass judges every torrent of a varied set once, so caches warm
/// only as they would on ingest. The engine is measured with 1%, 10% and
/// 25% of the rules built on words the torrents have. The budget holds
/// the core, 1% and 10%; 25% is measured and reported, not held.
#[test]
#[ignore]
fn budget() {
    const BUDGET_US: u128 = 50;
    let set = torrents(512);
    let classes: Vec<_> = set
        .iter()
        .map(|(n, f)| lodestar_classify::classify(n, f))
        .collect();
    let items: Vec<Item> = set
        .iter()
        .enumerate()
        .map(|(i, (name, files))| Item {
            hashes: std::slice::from_ref(Box::leak(Box::new([i as u8; 20]))),
            name,
            files,
            content_type: classes[i].content_type,
        })
        .collect();
    let facts: Vec<_> = classes
        .iter()
        .zip(&set)
        .map(|(class, (_, f))| lodestar_filter::Facts {
            class,
            size: f.iter().map(|f| f.size).sum(),
            piece_len: 1 << 22,
            flags: 0,
            discovered_at: 0,
            now: 0,
        })
        .collect();
    let files: usize = set.iter().map(|(_, f)| f.len()).sum();
    println!(
        "{} torrents, {:.1} files each on average",
        items.len(),
        files as f64 / items.len() as f64
    );
    // A warm-up pass (the first judgment also builds the rule index),
    // then the median of seven passes over the set, so one noisy pass on
    // a shared runner doesn't decide. Each pass's mean is printed.
    let time = |what: &str, f: &dyn Fn(usize)| {
        (0..items.len()).for_each(f);
        let mut passes: Vec<_> = (0..7)
            .map(|_| {
                let start = Instant::now();
                (0..items.len()).for_each(f);
                start.elapsed() / items.len() as u32
            })
            .collect();
        println!("{what}: passes {passes:?}");
        passes.sort();
        let per = passes[3];
        println!("{what}: {per:?} per torrent");
        per
    };
    let core = shipped();
    let mut over = Vec::new();
    let per = time("core", &|i| {
        std::hint::black_box(core.verdict(std::hint::black_box(&items[i])));
    });
    if per.as_micros() >= BUDGET_US {
        over.push(format!("core: {per:?}"));
    }

    // The whole engine: the core, every pack, and 10,000 of your rules,
    // reading the classification as ingest does.
    let mut engine = lodestar_filter::Engine::new(core);
    engine.packs = lodestar_filter::PackSwitches::all();
    for (share, held) in [(1, true), (10, true), (25, false)] {
        engine.rules = ten_thousand_rules(share).into();
        let what = format!("engine, packs on, 10,000 rules, {share}% on common words");
        let (met, full) = (0..items.len()).fold((0, 0), |(m, f), i| {
            let (a, b) = engine.rule_counts(&items[i], Some(&facts[i]));
            (m + a, f + b)
        });
        println!(
            "{what}: each torrent meets {:.0} rules on average; {:.0} get the full match",
            met as f64 / items.len() as f64,
            full as f64 / items.len() as f64
        );
        let per = time(&what, &|i| {
            let v = engine.judge(std::hint::black_box(&items[i]), Some(&facts[i]), 0);
            assert!(
                v.decision
                    .is_none_or(|d| !matches!(d.rule, lodestar_filter::RuleRef::User(_))),
                "no rule of yours matches the set"
            );
        });
        if per.as_micros() >= BUDGET_US {
            if held {
                over.push(format!("{what}: {per:?}"));
            } else {
                println!("{what}: over {BUDGET_US} µs (reported, not held)");
            }
        }
    }
    assert!(over.is_empty(), "over the {BUDGET_US} µs budget: {over:?}");
}
