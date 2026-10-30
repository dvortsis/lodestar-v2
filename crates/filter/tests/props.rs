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
    rules(|i| (i % 100 < share).then(|| on_common(i)))
}

/// A word no torrent has.
fn unique(i: usize, salt: &str) -> String {
    format!("{salt}{}q{i}", ["zor", "vel", "mik", "tas"][i % 4])
}

fn common(i: usize) -> &'static str {
    COMMON[i / 5 % COMMON.len()]
}

/// One of [`ten_thousand_rules`]' shapes on common words.
fn on_common(i: usize) -> String {
    match i % 5 {
        0 => format!("{} {}", common(i), unique(i, "u")),
        1 => {
            let (a, b) = REVERSED[i / 5 % REVERSED.len()];
            format!("\"{a} {b}\"")
        }
        2 => format!("{} size:>{}TB", common(i), i % 7 + 2),
        3 => format!("file:({} size:>{}TB)", common(i), i % 7 + 2),
        _ => format!("{} type:software", common(i)),
    }
}

/// 10,000 rules: rule `i` is `pick(i)`'s, or else one of the rest
/// [`ten_thousand_rules`] describes, none matching the set.
fn rules(pick: impl Fn(usize) -> Option<String>) -> Vec<lodestar_filter::UserRule> {
    let word = unique;
    (0..10_000)
        .map(|i| {
            let q = pick(i).unwrap_or_else(|| match i % 100 {
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
            });
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

/// The rule shapes the cheap check can't decide, each built on words
/// the set has, so the narrowing and the cheap check leave it to the
/// full match, and matching nothing.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Two common words always further apart than two tokens.
    Near,
    /// A common word, without a word every torrent has (`mkv`, the only
    /// one, so these share their negated word, as `-sample` rules do).
    Negation,
    /// A made-up word of common letters within one or two edits of
    /// nothing, which the character bounds can't rule out.
    Fuzzy,
    /// A prefix whose first four letters the set's words have.
    Prefix,
    /// A common word in the title, where titles never have it.
    Title,
}

/// Pairs of the set's words never within three tokens of each other.
const FAR: &[(&str, &str)] = &[
    ("mkv", "1080p"),
    ("mkv", "720p"),
    ("srt", "2160p"),
    ("nfo", "web"),
    ("sample", "x264"),
    ("mkv", "bluray"),
    ("srt", "1080p"),
    ("sample", "2160p"),
];

/// Starts of the set's words, letters only (a prefix with digits splits).
const LETTER_PREFIXES: &[&str] = &[
    "webd", "blur", "samp", "hevc", "atmo", "subs", "true", "webr",
];

impl Shape {
    const ALL: [Shape; 5] = [
        Shape::Near,
        Shape::Negation,
        Shape::Fuzzy,
        Shape::Prefix,
        Shape::Title,
    ];

    fn rule(self, i: usize) -> String {
        match self {
            // 32 different NEARs: a pair, either way round, one or two
            // apart.
            Shape::Near => {
                let (a, b) = FAR[i / 7 % FAR.len()];
                let (a, b) = if i / 3 % 2 == 0 { (a, b) } else { (b, a) };
                format!("{a} NEAR/{} {b}", 1 + i / 11 % 2)
            }
            Shape::Negation => format!("{} -mkv", common(i)),
            // Words of common letters, so the character bounds can't turn
            // them away: half five letters (one edit), half eight (two).
            Shape::Fuzzy => {
                let letters = b"rstnlhdcm";
                let n = if i % 2 == 0 { 5 } else { 8 };
                let mut x = i as u64 * 0x9e37_79b9_7f4a_7c15 + 1;
                let word: String = (0..n)
                    .map(|_| {
                        x ^= x >> 29;
                        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
                        char::from(letters[(x >> 32) as usize % letters.len()])
                    })
                    .collect();
                format!("~{word}")
            }
            // Hundreds of different prefixes, each starting like a word
            // the set has, then with letters none has there.
            Shape::Prefix => {
                let base = LETTER_PREFIXES[i / 7 % LETTER_PREFIXES.len()];
                let letter = |n: usize| char::from(b'a' + (n % 26) as u8);
                format!("{base}{}{}*", ["x", "z", "q"][i % 3], letter(i / 3))
            }
            Shape::Title => format!("title:{}", common(i)),
        }
    }
}

/// The held mixed set: of every 100 rules, 10 on common words (the 10%
/// case), 1 NEAR, 2 negations, 2 fuzzy words, 3 prefixes and 1 `title:`
/// word, all on the set's words, and the rest as in
/// [`ten_thousand_rules`]. Most rules people write are words, phrases
/// and groups; the expensive shapes are a few of them each (§2.21).
fn mixed_rules() -> Vec<lodestar_filter::UserRule> {
    rules(|i| match i % 100 {
        0..10 => Some(on_common(i)),
        10 => Some(Shape::Near.rule(i)),
        11..13 => Some(Shape::Negation.rule(i)),
        13..15 => Some(Shape::Fuzzy.rule(i)),
        15..18 => Some(Shape::Prefix.rule(i)),
        18 => Some(Shape::Title.rule(i)),
        _ => None,
    })
}

/// 10% of the rules of one shape, 1% on common words, and the rest.
fn shape_rules(shape: Shape) -> Vec<lodestar_filter::UserRule> {
    rules(|i| match i % 100 {
        0..10 => Some(shape.rule(i)),
        10 => Some(on_common(i)),
        _ => None,
    })
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
/// nanoseconds: an AMD EPYC 7763, the most common hosted runner (10.7 µs,
/// the lowest of jobs 120136591749, 120136592052, 120136592177 and
/// 120136592216). The budget holds on hardware like users run, not only
/// on the fastest runners (owner, 2026-10-23: "Go with whatever makes for
/// the best user experience"); a runner as fast as the Xeon 6973P-C
/// (7.2 µs) reads its times about 1.5× higher.
///
/// To derive it for a runner type, run the budget test on a runner of
/// that type (the log's CPU line names it) and take the `calibration:`
/// line's µs, the fastest pass; take the lowest over a few runs. Re-derive
/// it when the calibration's time moves: a Rust toolchain change (the
/// toolchain isn't pinned, and the calibration is all std: SipHash, the
/// hash set, `sort_unstable`), or a change to [`calibrate`], [`torrents`]
/// or [`COMMON`]. The test prints `rustc --version` to tell.
const CALIB_REF_NS: f64 = 10_700.0;

/// [`calibrate`]'s fastest pass per torrent on a typical reference
/// runner, in nanoseconds: the median over the 29 EPYC 7763 jobs listed
/// in [`REFERENCE`] (9.83-11.34 µs). The reference reads its raw times,
/// which is what a typical 7763 host runs, so a listed model's host is
/// measured against the typical 7763's calibration, not the fastest's
/// ([`CALIB_REF_NS`], which stays the fallback for unlisted models).
/// Re-derive it on the same triggers as [`CALIB_REF_NS`]; its jobs ran
/// rustc 1.99.1.
const CALIB_TYPICAL_NS: f64 = 11_090.0;

/// How a hosted runner model runs the engine compared with the
/// calibration, as a share of the same on the reference runner.
struct EngineSpeed {
    /// Matched against the CPU line's model name.
    model: &'static str,
    /// The median over the model's jobs of the mixed set's raw time ÷ the
    /// calibration's, ÷ the same on the 7763 at the same code.
    skew: f64,
    /// The `filter budget` jobs it's derived from (run IDs), when they
    /// ran, and the code they ran.
    runs: &'static [u64],
    dates: &'static str,
    code: &'static str,
}

impl EngineSpeed {
    /// Derived from fewer than five jobs: the gate still holds it, warns,
    /// and it's re-derived once more exist (runner, 2026-10-30).
    fn provisional(&self) -> bool {
        self.runs.len() < 5
    }
}

/// The reference runner, read raw: its times are the budget's
/// microseconds. Its jobs set [`CALIB_TYPICAL_NS`] and the mixed set's
/// time ÷ the calibration's that every [`ENGINE_SPEED`] skew is measured
/// against: 4.09 µs per calibration µs at `2d5e761d..f2ee0951` before
/// #196 (24 jobs), 3.93 after (5 jobs).
const REFERENCE: EngineSpeed = EngineSpeed {
    model: "EPYC 7763",
    skew: 1.0,
    runs: &[
        41060388132,
        41055449266,
        41054094982,
        41049498646,
        41044878037,
        41044132796,
        41043907080,
        41041666063,
        41040631957,
        41040270140,
        41035649874,
        41032038396,
        41029625857,
        41027904941,
        41024264027,
        41022335477,
        41018406044,
        41018370367,
        41016853875,
        41016836536,
        41016556547,
        41013719520,
        41011318136,
        41010444426,
        41009227540,
        41008256126,
        41005542890,
        41004752665,
        40845396409,
    ],
    dates: "2026-10-29..30",
    code: "2d5e761d..2b61d89d, before and after #196",
};

/// The mixed set's raw time ÷ the calibration's on the reference, at the
/// code the gate runs: the median of [`REFERENCE`]'s jobs since #196.
/// Only the drift warning reads it: the skews are ratios taken at the same
/// code, so an engine change moves them alike. Re-derive it when the
/// engine's speed changes much (the test prints the ratio on every job),
/// and on the same triggers as [`CALIB_REF_NS`].
const REFERENCE_RATIO: f64 = 3.93;

/// The engine's speed on each other hosted runner model seen. A job on a
/// listed model is scaled by its own calibration against
/// [`CALIB_TYPICAL_NS`], times the model's skew; any other model by
/// [`calibrate`] alone, against [`CALIB_REF_NS`].
///
/// Both parts are needed. The calibration scales the engine unevenly
/// across models (`docs/decisions/2026-10-30-F-filters-xcpu.md`): the
/// mixed set's time ÷ the calibration's is tight within a model and up to
/// 29% apart between them, which the skew corrects. And hosts of one
/// model differ: the 8573C's calibrations fall in two bands, 8.1-8.7 and
/// 9.5-9.9 µs, and the engine's time follows them, which a fixed factor
/// per model can't follow and the calibration does
/// (`docs/decisions/2026-10-30-X-filter-budget-8573C.md`).
///
/// To add a model, take its jobs' raw `mixed` µs ÷ `calibration:` µs and
/// divide each by the reference's median at the same code (the eras
/// above, 4.09 and 3.93); the skew is the median over all its eligible
/// jobs. Re-deriving uses the same formula over all eligible jobs, in a
/// PR with its own decisions file. Re-derive them all when the toolchain
/// or the calibration changes: the test prints each job's ratio, and
/// warns when a listed model's job is more than [`MODEL_DRIFT`] off its
/// skew.
///
/// Adding or changing an entry needs a `docs/decisions/` entry giving the
/// runs and the arithmetic (runner, 2026-10-30): the table never moves to
/// make a run pass.
const ENGINE_SPEED: &[EngineSpeed] = &[
    // 1.112-1.181 over its jobs.
    EngineSpeed {
        model: "8573C",
        skew: 1.126,
        runs: &[
            41065412960,
            41038382885,
            41036553722,
            41015045891,
            41014951807,
            41008099448,
            41006514676,
            41005580320,
            41005421892,
        ],
        dates: "2026-10-30",
        code: "e8f50d58..6c7bcef3, before and after #196",
    },
    // 0.861-0.945.
    EngineSpeed {
        model: "EPYC 9V45",
        skew: 0.894,
        runs: &[
            41055481261,
            41054208363,
            41051280541,
            41048269928,
            41035050970,
            41029959417,
            41028071535,
            41025649623,
            41024152492,
            41014655186,
            41013412376,
            41006310085,
            40660530001,
        ],
        dates: "2026-10-28..30",
        code: "2d5e761d..f2ee0951, before and after #196",
    },
    // 0.905-0.934.
    EngineSpeed {
        model: "EPYC 9V74",
        skew: 0.913,
        runs: &[
            41062154866,
            41059977277,
            41057808450,
            41057663596,
            41046740404,
            41046258692,
            41041114029,
            41040381547,
            41028659383,
            41008473155,
        ],
        dates: "2026-10-30",
        code: "926e0529..00da08a4, before and after #196",
    },
    // 1.022-1.110; provisional, from 3 jobs.
    EngineSpeed {
        model: "8370C",
        skew: 1.034,
        runs: &[41047639541, 41033584211, 41014261444],
        dates: "2026-10-30",
        code: "47fe54e0..5c88c659, before and after #196",
    },
];

/// How far one job's skew (its mixed time ÷ calibration time over
/// [`REFERENCE_RATIO`]) may stray from its model's before the test warns
/// that the skew may need re-deriving. It only warns: the table decides.
const MODEL_DRIFT: f64 = 0.20;

/// A case's median pass, and how far it is above the fastest pass, as a
/// share. A shared host's neighbours can slow most of one case's passes
/// while the calibrations either side run clean; then the median isn't
/// the engine's time, and the spread shows it. A slower engine slows
/// every pass alike, so its spread stays small.
fn spread(mut passes: Vec<f64>) -> (f64, f64) {
    passes.sort_by(f64::total_cmp);
    let median = passes[passes.len() / 2];
    (median, median / passes[0] - 1.0)
}

#[test]
fn a_case_disturbed_mid_run_has_a_wide_spread() {
    // This slice's dispatch on a 7763 (job 124112699853): the mixed set
    // while the host was busy, and the neutralized case right after.
    let (median, wide) = spread(vec![42.9, 44.3, 51.1, 56.9, 49.6, 62.0, 52.6]);
    assert_eq!(median, 51.1);
    assert!(wide > 0.15, "{wide}");
    let (median, narrow) = spread(vec![43.6, 43.2, 43.2, 43.3, 43.8, 43.4, 43.2]);
    assert_eq!(median, 43.3);
    assert!(narrow < 0.01, "{narrow}");
}

/// How a job's times become reference-runner microseconds: each case's
/// raw time ÷ `speed`.
struct Scaling {
    speed: f64,
    /// Where `speed` came from, for each case's line.
    how: String,
    /// Lines for the log; `::warning::` ones show on the check.
    notes: Vec<String>,
}

/// The scaling for a job on `cpu` (the CPU line's model name), from its
/// fastest calibration pass and the mixed set's raw time, both in µs per
/// torrent.
fn scaling(cpu: &str, calib_us: f64, mixed: f64) -> Scaling {
    // The mixed set's time ÷ the calibration's, printed on every job so a
    // skew can be re-derived or checked from the log.
    let ratio = mixed / calib_us;
    let mut notes = vec![format!("mixed ÷ calibration: {ratio:.3}")];
    if cpu.contains(REFERENCE.model) {
        notes.push(format!(
            "CPU {cpu}: the reference model, so its raw times decide"
        ));
        return Scaling {
            speed: 1.0,
            how: format!("the {}, raw", REFERENCE.model),
            notes,
        };
    }
    let Some(m) = ENGINE_SPEED.iter().find(|m| cpu.contains(m.model)) else {
        let speed = calib_us * 1e3 / CALIB_REF_NS;
        notes.push(format!(
            "CPU {cpu}: not in ENGINE_SPEED, so scaling by the calibration"
        ));
        notes.push(format!(
            "::warning::{cpu} isn't in ENGINE_SPEED: scaled by the calibration"
        ));
        return Scaling {
            speed,
            how: "the calibration".to_owned(),
            notes,
        };
    };
    let host = calib_us * 1e3 / CALIB_TYPICAL_NS;
    let speed = host * m.skew;
    let provisional = if m.provisional() { ", provisional" } else { "" };
    notes.push(format!(
        "CPU {cpu}: scaling by the calibration against a typical {}'s {:.2} µs ({host:.3}×), \
         times the {}'s skew {:.3} from {} jobs, {}, {}{provisional}: {speed:.3}× the \
         reference's time",
        REFERENCE.model,
        CALIB_TYPICAL_NS / 1e3,
        m.model,
        m.skew,
        m.runs.len(),
        m.dates,
        m.code,
    ));
    let here = ratio / REFERENCE_RATIO;
    notes.push(format!(
        "this job's skew: {here:.3} (mixed ÷ calibration over the {}'s {REFERENCE_RATIO})",
        REFERENCE.model
    ));
    if (here / m.skew - 1.0).abs() > MODEL_DRIFT {
        notes.push(format!(
            "::warning::this job's skew ({here:.3}) is more than {:.0}% off the {}'s ({:.3}): \
             the skew, or REFERENCE_RATIO after an engine change, may need re-deriving \
             (ENGINE_SPEED)",
            MODEL_DRIFT * 100.0,
            m.model,
            m.skew
        ));
    }
    if m.provisional() {
        notes.push(format!(
            "::warning::the {}'s skew is provisional, from {} job(s): re-derive it once more \
             exist (ENGINE_SPEED)",
            m.model,
            m.runs.len()
        ));
    }
    Scaling {
        speed,
        how: format!("the calibration × the {}'s skew{provisional}", m.model),
        notes,
    }
}

#[test]
fn the_reference_reads_raw() {
    for calib_us in [5.0, 9.9, 20.0] {
        let s = scaling("AMD EPYC 7763 64-Core Processor", calib_us, 43.3);
        assert_eq!(s.speed, 1.0);
        assert!(s.notes.iter().all(|n| !n.starts_with("::warning::")));
    }
}

#[test]
fn an_unlisted_model_scales_by_the_calibration() {
    let s = scaling("AMD EPYC 9999 Processor", 8.56, 40.0);
    assert!((s.speed - 0.8).abs() < 1e-9, "{}", s.speed);
    assert_eq!(s.speed, 8.56e3 / CALIB_REF_NS);
    assert!(s.notes.iter().any(|n| n.contains("isn't in ENGINE_SPEED")));
}

#[test]
fn a_listed_model_scales_by_its_calibration_and_skew() {
    // #210's job 124095631213: calibration 9.86 µs, mixed 43.4 µs.
    // 9.86 ÷ 11.09 × 1.126 = 1.001115.
    let s = scaling("INTEL(R) XEON(R) PLATINUM 8573C", 9.86, 43.4);
    assert!((s.speed - 1.001115).abs() < 1e-6, "{}", s.speed);
    assert!((48.0 / s.speed - 47.95).abs() < 0.01);
    // Its skew (43.4 ÷ 9.86 ÷ 3.93 = 1.120) is within MODEL_DRIFT.
    assert!(s.notes.iter().all(|n| !n.starts_with("::warning::")));
}

#[test]
fn a_skew_off_by_more_than_model_drift_warns() {
    let skew = ENGINE_SPEED
        .iter()
        .find(|m| m.model == "8573C")
        .unwrap()
        .skew;
    // The mixed time that gives this job a skew `off` times the table's.
    let warns = |off: f64| {
        scaling(
            "INTEL(R) XEON(R) PLATINUM 8573C",
            10.0,
            10.0 * REFERENCE_RATIO * skew * off,
        )
        .notes
        .iter()
        .any(|n| n.contains("% off the 8573C's"))
    };
    assert!(warns(1.0 + MODEL_DRIFT + 0.01));
    assert!(warns(1.0 - MODEL_DRIFT - 0.01));
    assert!(!warns(1.0 + MODEL_DRIFT - 0.01));
    assert!(!warns(1.0 - MODEL_DRIFT + 0.01));
    assert!(!warns(1.0));
}

#[test]
fn a_provisional_skew_still_scales_and_warns() {
    let m = ENGINE_SPEED
        .iter()
        .find(|m| m.provisional())
        .expect("a provisional entry exists; drop this test when none does");
    let s = scaling(m.model, 10.0, 10.0 * REFERENCE_RATIO * m.skew);
    assert!((s.speed - 10e3 / CALIB_TYPICAL_NS * m.skew).abs() < 1e-12);
    assert!(s.notes.iter().any(|n| n.contains("provisional")));
    assert!(s.notes.iter().all(|n| !n.contains("% off")));
    // The drift warning stays live for it.
    let s = scaling(m.model, 10.0, 10.0 * REFERENCE_RATIO * m.skew * 1.25);
    assert!(s.notes.iter().any(|n| n.contains("% off")));
}

/// §20.8 and the owner's budget (§1.6.6): under 50 µs per torrent at
/// 10,000 rules. Ignored in debug test runs; CI runs it in release (the
/// `filter budget` job): `cargo test --release -p lodestar-filter --test
/// props -- --ignored --nocapture budget`.
///
/// Each pass judges every torrent of a varied set once, so caches warm
/// only as they would on ingest. The engine is measured with 1%, 10% and
/// 25% of the rules built on words the torrents have, with a mixed set
/// that adds the shapes the cheap check can't decide ([`mixed_rules`]),
/// with 10% of each such shape ([`Shape`]), and with the mixed set and
/// five common words neutralized. The budget holds the core, 1%, 10%,
/// the mixed set and the mixed set neutralized; 25% and each shape at 10%
/// are measured and reported, not held. Every rule shape counts (§2.21,
/// owner-delegated, 2026-10-23).
///
/// The budget is in reference-runner microseconds (owner, 2026-10-22;
/// `docs/decisions/2026-10-22-filter-budget.md`). Hosted runners differ
/// by 1.5-1.9× in speed. On the reference model ([`REFERENCE`]) the raw
/// times decide. On a model [`ENGINE_SPEED`] lists, each case's time is
/// divided by how much slower than [`CALIB_TYPICAL_NS`] [`calibrate`]'s
/// fastest pass ran, times the model's skew. On any other, it's divided
/// by how much slower than [`CALIB_REF_NS`] that pass ran.
/// [`calibrate`] runs either way, before any filter code runs and again
/// once the engine is gone, to catch a runner whose speed changes under
/// the test; a held case whose median pass is more than 15% over its
/// fastest ([`spread`]) is caught the same way, and either measures
/// again. Where it scales, it never runs beside filter state, so a
/// regression can't slow it and hide itself, and noise only adds time, so
/// its fastest pass can only make the gate stricter. Raw times, the
/// divisor and where it came from are printed too.
///
/// This is a budget gate, not a regression gate: a slowdown that keeps a
/// case under 50 µs passes (about +40% at 10% on today's code).
/// Catching smaller ones needs a bench against a stored baseline.
#[test]
#[ignore]
fn budget() {
    const BUDGET_US: f64 = 50.0;
    // Outside this, the measurement is wrong rather than the runner slow:
    // the runners seen took 0.67× (Xeon 6973P-C) to 1.1× (an Intel build
    // VM) the reference's time.
    const SPEED: std::ops::RangeInclusive<f64> = 0.4..=2.0;
    // The fastest calibration passes before and after must agree this
    // closely, or the runner's speed changed under the test; and so must
    // a held case's median pass and its fastest ([`spread`]).
    const DRIFT: f64 = 0.15;
    // Attempts at a measurement the drift doesn't spoil.
    const ATTEMPTS: usize = 3;
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
    // pass on a shared runner doesn't; with how far it is above the
    // fastest pass, so a run of them is caught.
    let time = |what: &str, f: &dyn Fn(usize)| {
        pass(f);
        let passes: Vec<f64> = (0..7).map(|_| pass(f)).collect();
        println!("{what}: passes {} µs", show(&passes));
        spread(passes)
    };
    // The fastest of seven calibration passes, after a warm-up.
    let calibration = |when: &str| {
        pass(&calib);
        let passes: Vec<f64> = (0..7).map(|_| pass(&calib)).collect();
        println!("calibration {when}: passes {} µs", show(&passes));
        passes.into_iter().fold(f64::INFINITY, f64::min)
    };

    // A runner's speed can change under the test (a shared host's
    // neighbours), which the calibrations before and after catch, or
    // during one case, which that case's spread catches. Such a run
    // measures again rather than failing, up to three times; only a
    // trusted attempt decides, and only drift on every attempt fails.
    let mut drifted = Vec::new();
    for attempt in 1..=ATTEMPTS {
        println!("attempt {attempt} of {ATTEMPTS}");
        let before = calibration("before");
        let core = shipped();
        let (per, spread) = time("core", &|i| {
            std::hint::black_box(core.verdict(std::hint::black_box(&items[i])));
        });
        let mut cases = vec![("core".to_owned(), per, true, spread)];

        // The whole engine: the core, every pack, and 10,000 of your rules,
        // reading the classification as ingest does.
        let mut engine = lodestar_filter::Engine::new(core);
        engine.packs = lodestar_filter::PackSwitches::all();
        let mut sets: Vec<(String, Vec<lodestar_filter::UserRule>, bool)> =
            [(1, true), (10, true), (25, false)]
                .into_iter()
                .map(|(share, held)| {
                    let what = format!("engine, packs on, 10,000 rules, {share}% on common words");
                    (what, ten_thousand_rules(share), held)
                })
                .collect();
        sets.push((
            "engine, packs on, 10,000 rules, mixed (10% common, 1% NEAR, 2% negation, 2% fuzzy, 3% prefix, 1% title)".into(),
            mixed_rules(),
            true,
        ));
        for shape in Shape::ALL {
            let what = format!("engine, packs on, 10,000 rules, 10% {shape:?}");
            sets.push((what, shape_rules(shape), false));
        }
        for (what, rules, held) in sets {
            engine.rules = rules.into();
            let (met, full) = (0..items.len()).fold((0, 0), |(m, f), i| {
                let (a, b) = engine.rule_counts(&items[i], Some(&facts[i]));
                (m + a, f + b)
            });
            println!(
                "{what}: each torrent meets {:.0} rules on average; {:.0} get the full match",
                met as f64 / items.len() as f64,
                full as f64 / items.len() as f64
            );
            let (per, spread) = time(&what, &|i| {
                let v = engine.judge(std::hint::black_box(&items[i]), Some(&facts[i]), 0);
                assert!(
                    v.decision
                        .as_ref()
                        .is_none_or(|d| !matches!(d.rule, lodestar_filter::RuleRef::User(_))),
                    "no rule of yours matches the set: {what}: {:?}",
                    v.decision
                );
            });
            cases.push((what, per, held, spread));
        }
        // The mixed set again with common words neutralized (CSX-5
        // *Neutralize term*): every keyword entry reads the name and
        // paths, and every text clause checks the words.
        engine.rules = mixed_rules().into();
        engine.overrides = ["web", "webrip", "hevc", "mkv", "sample"]
            .iter()
            .zip(1..)
            .map(|(w, id)| lodestar_filter::Override {
                id,
                what: lodestar_filter::Match::Keyword((*w).into()),
                strength: lodestar_filter::Strength::Neutralize,
                scope: None,
                expires_at: None,
                shadow_until: None,
            })
            .collect();
        let what = "engine, packs on, 10,000 rules, mixed, 5 common words neutralized";
        let (per, spread) = time(what, &|i| {
            std::hint::black_box(engine.judge(std::hint::black_box(&items[i]), Some(&facts[i]), 0));
        });
        cases.push((what.to_owned(), per, true, spread));
        drop(engine);
        let after = calibration("after");

        let calib_us = before.min(after);
        // How many times the reference's time this runner takes, by the
        // calibration.
        let calibrated = calib_us * 1e3 / CALIB_REF_NS;
        println!(
            "calibration: {calib_us:.2} µs per torrent, {calibrated:.2}× the reference's {:.2} µs",
            CALIB_REF_NS / 1e3
        );
        let drift = before.max(after) / calib_us - 1.0;
        if drift > DRIFT {
            let note = format!(
                "attempt {attempt}: the fastest calibration pass was {before:.2} µs before and \
                 {after:.2} µs after, {:.0}% apart",
                drift * 100.0
            );
            println!("{note}: the runner's speed changed under the test, so measuring again");
            drifted.push(note);
            continue;
        }
        let unsteady: Vec<_> = cases
            .iter()
            .filter(|(_, _, held, spread)| *held && *spread > DRIFT)
            .map(|(what, _, _, spread)| {
                format!("{what}: median {:.0}% over the fastest", spread * 100.0)
            })
            .collect();
        if !unsteady.is_empty() {
            let note = format!("attempt {attempt}: {unsteady:?}");
            println!("{note}: the runner's speed changed during a held case, so measuring again");
            drifted.push(note);
            continue;
        }
        assert!(
            SPEED.contains(&calibrated),
            "the runner took {calibrated:.2}× the reference's time to calibrate, outside \
             {SPEED:?}: the measurement can't be trusted"
        );
        let mixed = cases
            .iter()
            .find(|(what, ..)| what.contains("mixed ("))
            .expect("the mixed case")
            .1;
        let Scaling { speed, how, notes } = scaling(&cpu, calib_us, mixed);
        for note in notes {
            println!("{note}");
        }
        let mut over = Vec::new();
        for (what, raw, held, _) in cases {
            let per = raw / speed;
            println!(
                "{what}: {per:.1} µs per torrent on the reference runner ({raw:.1} µs here \
                 ÷ {speed:.3}, {how})"
            );
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
        return;
    }
    panic!(
        "the runner's speed changed under the test on all {ATTEMPTS} attempts, so the \
         measurement can't be trusted: {drifted:?}"
    );
}

/// One judgment of each of the budget's torrents, for callgrind, whose
/// instruction counts don't move with the runner (§20.8). `LODESTAR_PROFILE`
/// picks the case: `core`, `mixed` (the default) or `neutralized`. A
/// warm-up pass first, so the counts are a judgment's, not the index
/// build's:
/// `valgrind --tool=callgrind --toggle-collect='*profile_pass*' target/release/deps/props-* --ignored --exact profile`.
#[test]
#[ignore]
fn profile() {
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
    let case = std::env::var("LODESTAR_PROFILE").unwrap_or_else(|_| "mixed".into());
    let mut engine = lodestar_filter::Engine::new(shipped());
    if case != "core" {
        engine.packs = lodestar_filter::PackSwitches::all();
        engine.rules = mixed_rules().into();
    }
    if case == "neutralized" {
        engine.overrides = ["web", "webrip", "hevc", "mkv", "sample"]
            .iter()
            .zip(1..)
            .map(|(w, id)| lodestar_filter::Override {
                id,
                what: lodestar_filter::Match::Keyword((*w).into()),
                strength: lodestar_filter::Strength::Neutralize,
                scope: None,
                expires_at: None,
                shadow_until: None,
            })
            .collect();
    }
    #[inline(never)]
    fn profile_pass(
        engine: &lodestar_filter::Engine,
        items: &[Item],
        facts: &[lodestar_filter::Facts],
    ) {
        for (item, facts) in items.iter().zip(facts) {
            std::hint::black_box(engine.judge(std::hint::black_box(item), Some(facts), 0));
        }
    }
    // The warm-up, outside the toggled function.
    for (item, facts) in items.iter().zip(&facts) {
        std::hint::black_box(engine.judge(item, Some(facts), 0));
    }
    profile_pass(&engine, &items, &facts);
}
