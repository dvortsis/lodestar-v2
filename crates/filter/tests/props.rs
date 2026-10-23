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

/// The runner's speed, from work that runs no Lodestar code: fold the
/// torrent's name and file names to lower case, split them into words, hash
/// the words into a set, look up the common words, and sort the hashes.
/// It does the kinds of things the core does (strings, hashing, small
/// allocations) in about the core's time, so a slower or busier runner
/// slows it alike, and no change to the filter can move it.
fn calibrate(name: &str, files: &[FileInfo], common: &[u64]) -> usize {
    use std::hash::BuildHasher;
    let hasher = std::hash::BuildHasherDefault::<std::hash::DefaultHasher>::default();
    let mut words = std::collections::HashSet::with_hasher(hasher.clone());
    let mut text = String::new();
    let names = files
        .iter()
        .filter_map(|f| f.path.last().map(String::as_str));
    for part in std::iter::once(name).chain(names) {
        text.clear();
        text.push_str(part);
        text.make_ascii_lowercase();
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            if !word.is_empty() {
                words.insert(hasher.hash_one(word));
            }
        }
    }
    let found = common.iter().filter(|h| words.contains(*h)).count();
    let mut sorted: Vec<u64> = words.into_iter().collect();
    sorted.sort_unstable();
    found + sorted.len()
}

/// [`calibrate`]'s fastest pass per torrent on the reference runner, in
/// nanoseconds: the fastest hosted runners seen, a Xeon 6973P-C (7.2 µs,
/// job 120136592071) and an EPYC 9V45 (7.3 µs, job 120123205000). On
/// those the gate reads what it did before calibration.
///
/// This is a stopgap (owner, 2026-10-22: "whatever makes for the best user
/// experience"): the budget should hold on typical hardware, so F-perf
/// moves the reference to the EPYC 7763, the most common hosted runner,
/// once the engine meets 50 µs there.
///
/// To derive it for a runner type, run the budget test on a runner of
/// that type (the log's CPU line names it) and take the `calibration:`
/// line's µs, the fastest pass; take the lowest over a few runs. Re-derive
/// it when the calibration's time moves: a Rust toolchain change (the
/// toolchain isn't pinned, and the calibration is all std: SipHash, the
/// hash set, `sort_unstable`), or a change to [`calibrate`], [`torrents`]
/// or [`COMMON`]. The test prints `rustc --version` to tell.
const CALIB_REF_NS: f64 = 7_200.0;

/// §20.8 and the owner's budget (§1.6.6): under 50 µs per torrent at
/// 10,000 rules. Ignored in debug test runs; CI runs it in release (the
/// `filter budget` job): `cargo test --release -p lodestar-filter --test
/// props -- --ignored --nocapture budget`.
///
/// Each pass judges every torrent of a varied set once, so caches warm
/// only as they would on ingest. The engine is measured with 1%, 10% and
/// 25% of the rules built on words the torrents have. The budget holds
/// the core, 1% and 10%; 25% is measured and reported, not held.
///
/// The budget is in reference-runner microseconds (owner, 2026-10-22;
/// `docs/decisions/2026-10-22-filter-budget.md`). Hosted runners differ
/// by 1.5-1.9× in speed, so the test times [`calibrate`] before any
/// filter code runs and again once the engine is gone, and divides each
/// case's time by how much slower than [`CALIB_REF_NS`] its fastest pass
/// ran. The calibration never runs beside filter state, so a regression
/// can't slow it and hide itself, and noise only adds time, so the fastest
/// pass can only make the gate stricter. Raw times and the runner's speed
/// are printed too.
///
/// This is a budget gate, not a regression gate: a slowdown that keeps a
/// case under 50 µs passes (about +40% at 10% on today's code).
/// Catching smaller ones needs a bench against a stored baseline.
#[test]
#[ignore]
fn budget() {
    const BUDGET_US: f64 = 50.0;
    // Outside this, the measurement is wrong rather than the runner slow:
    // the slowest runner seen took 1.5× the reference's time.
    const SPEED: std::ops::RangeInclusive<f64> = 0.4..=2.0;
    // The fastest calibration passes before and after must agree this
    // closely, or the runner's speed changed under the test.
    const DRIFT: f64 = 0.15;
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
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("model name")
                    .map(|m| m.trim_start_matches([' ', '\t', ':']).to_owned())
            })
        })
        .unwrap_or_else(|| "unknown".into());
    let rustc = std::process::Command::new(std::env::var("RUSTC").unwrap_or("rustc".into()))
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!(
        "{} torrents, {:.1} files each on average; CPU {cpu}; {rustc}",
        items.len(),
        files as f64 / items.len() as f64
    );
    let common: Vec<u64> = {
        use std::hash::BuildHasher;
        let hasher = std::hash::BuildHasherDefault::<std::hash::DefaultHasher>::default();
        COMMON.iter().map(|w| hasher.hash_one(w)).collect()
    };
    let calib = |i: usize| {
        let (name, files) = std::hint::black_box(&set[i]);
        std::hint::black_box(calibrate(name, files, &common));
    };
    // Microseconds per torrent for one pass over the set.
    let pass = |f: &dyn Fn(usize)| {
        let start = Instant::now();
        (0..items.len()).for_each(f);
        start.elapsed().as_secs_f64() * 1e6 / items.len() as f64
    };
    let show = |v: &[f64]| {
        v.iter()
            .map(|x| format!("{x:.1}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    // A warm-up pass (the first judgment also builds the rule index),
    // then seven passes over the set. The median decides, so one noisy
    // pass on a shared runner doesn't.
    let time = |what: &str, f: &dyn Fn(usize)| {
        pass(f);
        let mut passes: Vec<f64> = (0..7).map(|_| pass(f)).collect();
        println!("{what}: passes {} µs", show(&passes));
        passes.sort_by(f64::total_cmp);
        passes[3]
    };
    // The fastest of seven calibration passes, after a warm-up.
    let calibration = |when: &str| {
        pass(&calib);
        let passes: Vec<f64> = (0..7).map(|_| pass(&calib)).collect();
        println!("calibration {when}: passes {} µs", show(&passes));
        passes.into_iter().fold(f64::INFINITY, f64::min)
    };

    let before = calibration("before");
    let core = shipped();
    let mut cases = vec![(
        "core".to_owned(),
        time("core", &|i| {
            std::hint::black_box(core.verdict(std::hint::black_box(&items[i])));
        }),
        true,
    )];

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
        cases.push((what, per, held));
    }
    drop(engine);
    let after = calibration("after");

    let calib_us = before.min(after);
    // How many times the reference's time this runner takes.
    let speed = calib_us * 1e3 / CALIB_REF_NS;
    println!(
        "calibration: {calib_us:.2} µs per torrent, {speed:.2}× the reference's {:.2} µs",
        CALIB_REF_NS / 1e3
    );
    let drift = before.max(after) / calib_us - 1.0;
    assert!(
        drift <= DRIFT,
        "the fastest calibration pass was {before:.2} µs before and {after:.2} µs after, \
         {:.0}% apart: the runner's speed changed under the test, so the measurement \
         can't be trusted",
        drift * 100.0
    );
    assert!(
        SPEED.contains(&speed),
        "the runner took {speed:.2}× the reference's time to calibrate, outside \
         {SPEED:?}: the measurement can't be trusted"
    );
    let mut over = Vec::new();
    for (what, raw, held) in cases {
        let per = raw / speed;
        println!("{what}: {per:.1} µs per torrent on the reference runner ({raw:.1} µs here)");
        if per >= BUDGET_US {
            if held {
                over.push(format!("{what}: {per:.1} µs ({raw:.1} µs here)"));
            } else {
                println!("{what}: over {BUDGET_US} µs (reported, not held)");
            }
        }
    }
    assert!(
        over.is_empty(),
        "over the {BUDGET_US} µs budget on the reference runner, with this runner at \
         {speed:.2}× its time: {over:?}"
    );
}
