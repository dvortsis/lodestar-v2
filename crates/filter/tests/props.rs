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
            if r.next().is_multiple_of(3) {
                let year = 1960 + r.next() % 65;
                let name = format!("{title}.{year}.{res}.{src}.{audio}.{codec}-{group}");
                let mut files = vec![FileInfo::new(vec![format!("{name}.mkv")], 4 << 30)];
                if r.next().is_multiple_of(2) {
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
                let subs = r.next().is_multiple_of(2);
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
                let (a, b) = if (i / 3).is_multiple_of(2) {
                    (a, b)
                } else {
                    (b, a)
                };
                format!("{a} NEAR/{} {b}", 1 + i / 11 % 2)
            }
            Shape::Negation => format!("{} -mkv", common(i)),
            // Words of common letters, so the character bounds can't turn
            // them away: half five letters (one edit), half eight (two).
            Shape::Fuzzy => {
                let letters = b"rstnlhdcm";
                let n = if i.is_multiple_of(2) { 5 } else { 8 };
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
/// nanoseconds: an AMD EPYC 7763, the most common hosted runner (10.45 µs,
/// the lowest of the 37 jobs listed in [`REFERENCE`], 2026-10-31..11-05;
/// it was 10.7 µs from four jobs on 2026-10-22). The budget holds on
/// hardware like users run, not only on the fastest runners (owner,
/// 2026-10-23: "Go with whatever makes for the best user experience"); a
/// runner as fast as the Xeon 6973P-C (7.2 µs) reads its times about 1.5×
/// higher.
///
/// To derive it for a runner type, run the budget test on a runner of
/// that type (the log's CPU line names it) and take the `calibration:`
/// line's µs, the fastest pass; take the lowest over a few runs. Re-derive
/// it when the calibration's time moves: a Rust toolchain change (the
/// toolchain isn't pinned, and the calibration is all std: SipHash, the
/// hash set, `sort_unstable`), or a change to [`calibrate`], [`torrents`]
/// or [`COMMON`]. The test prints `rustc --version` to tell.
const CALIB_REF_NS: f64 = 10_450.0;

/// [`calibrate`]'s fastest pass per torrent on a typical reference
/// runner, in nanoseconds: the median over the 37 EPYC 7763 jobs listed
/// in [`REFERENCE`] (10.45-11.92 µs; it was 11.09 µs over 29 jobs on
/// 2026-10-29..30). 7763 hosts calibrate in two bands, about 10.5 and 11.0
/// µs, at the same engine time, and by 2026-11-04 nearly every job drew the
/// faster one (`docs/decisions/2026-11-05-X-filter-budget-noise.md`). The
/// reference reads its raw times, which is what a typical 7763 host runs,
/// so a listed model's host is measured against the typical 7763's
/// calibration, not the fastest's ([`CALIB_REF_NS`], which stays the
/// fallback for unlisted models).
/// Re-derive it on the same triggers as [`CALIB_REF_NS`], and when the
/// 7763's `calibration:` lines move off it for good; its jobs ran rustc
/// 1.99.1.
const CALIB_TYPICAL_NS: f64 = 10_590.0;

/// How a hosted runner model runs the engine compared with the
/// calibration, as a share of the same on the reference runner.
struct EngineSpeed {
    /// Matched against the CPU line's model name.
    model: &'static str,
    /// The median over the model's jobs of the mixed set's raw time (its
    /// third-fastest pass) ÷ the calibration's, ÷ the same on the 7763.
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
/// against: 4.02 µs per calibration µs over these jobs (3.82-4.20; it was
/// 3.93 over five jobs on 2026-10-30). The code range is the first and
/// last job's head, across branches.
const REFERENCE: EngineSpeed = EngineSpeed {
    model: "EPYC 7763",
    skew: 1.0,
    runs: &[
        41945426619,
        41942305919,
        41940903202,
        41935549308,
        41935241853,
        41933423478,
        41932987039,
        41922199112,
        41920437564,
        41919199999,
        41915886307,
        41912491340,
        41908869737,
        41903017195,
        41532566763,
        41489296472,
        41473555316,
        41466645299,
        41458033765,
        41451433046,
        41431099240,
        41359684569,
        41356515182,
        41345438561,
        41330895188,
        41305628508,
        41301948554,
        41246113451,
        41236509974,
        41231734520,
        41222848579,
        41221310305,
        41220229135,
        41210184355,
        41208775894,
        41200425685,
        41194830646,
    ],
    dates: "2026-10-31..11-05",
    code: "a3243b7d..e481c6df",
};

/// The mixed set's raw time ÷ the calibration's on the reference, at the
/// code the gate runs: the median of [`REFERENCE`]'s jobs.
/// Only the drift warning reads it: the skews are ratios taken at the same
/// code, so an engine change moves them alike. Re-derive it when the
/// engine's speed changes much (the test prints the ratio on every job),
/// and on the same triggers as [`CALIB_REF_NS`].
const REFERENCE_RATIO: f64 = 4.02;

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
/// divide each by the reference's median at the same code
/// ([`REFERENCE_RATIO`]); the skew is the median over all its eligible
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
    // 1.070-1.143 over its jobs.
    EngineSpeed {
        model: "8573C",
        skew: 1.097,
        runs: &[
            41919270308,
            41904747201,
            41439674586,
            41433116185,
            41339386827,
        ],
        dates: "2026-11-01..11-04",
        code: "38789bcb..5bebdb89",
    },
    // 0.892-0.965 over its jobs.
    EngineSpeed {
        model: "EPYC 9V45",
        skew: 0.944,
        runs: &[
            41945810509,
            41939178877,
            41920858097,
            41911139684,
            41908317816,
            41517600122,
            41315681318,
            41206854675,
        ],
        dates: "2026-10-31..11-05",
        code: "29201097..b08e161f",
    },
    // 0.877-0.995 over its jobs.
    EngineSpeed {
        model: "EPYC 9V74",
        skew: 0.922,
        runs: &[
            41937126100,
            41935234631,
            41930504493,
            41925822797,
            41916664314,
            41913257284,
            41904743381,
            41465630405,
            41361833457,
            41354931099,
            41352406885,
            41342560026,
            41296743871,
            41253319480,
            41228842689,
            41217026016,
            41204310448,
        ],
        dates: "2026-10-31..11-05",
        code: "5f012f2b..e2f578e8",
    },
    // 1.033-1.048 over its jobs; provisional, from 4 jobs.
    EngineSpeed {
        model: "8370C",
        skew: 1.038,
        runs: &[41937536173, 41931557263, 41914687461, 41908571127],
        dates: "2026-11-04..11-05",
        code: "38fd9309..7e02eacd",
    },
];

/// How far one job's skew (its mixed time ÷ calibration time over
/// [`REFERENCE_RATIO`]) may stray from its model's before the test warns
/// that the skew may need re-deriving. It only warns: the table decides.
const MODEL_DRIFT: f64 = 0.20;

/// One case's seven passes, read for the gate.
#[derive(Debug, Clone, Copy)]
struct Timing {
    /// The third-fastest pass: the time the gate decides on. Noise only
    /// adds time, so it can't read faster than the engine; it takes three
    /// passes that fast, so one lucky pass can't set it either; and up to
    /// four slowed passes of seven leave it alone, where the median moves
    /// once the fourth is slowed (#429's CI filter-budget job).
    at: f64,
    /// How far `at` is above the fastest pass, as a share. A shared host's
    /// neighbours can slow most of a case's passes while the calibrations
    /// either side run clean; then `at` isn't the engine's time, and this
    /// shows it. A slower engine slows every pass alike, so it stays small.
    spread: f64,
    /// How far the slowest pass is above the fastest, as a share: a sign
    /// the host was busy during the case, even when `at` wasn't touched.
    jitter: f64,
}

fn timing(mut passes: Vec<f64>) -> Timing {
    passes.sort_by(f64::total_cmp);
    let fastest = passes[0];
    let at = passes[passes.len() / 3];
    Timing {
        at,
        spread: at / fastest - 1.0,
        jitter: passes[passes.len() - 1] / fastest - 1.0,
    }
}

#[test]
fn a_case_disturbed_mid_run_has_a_wide_spread() {
    // X-filter-budget-8573C's dispatch on a 7763 (the CI job on that slice): the
    // mixed set while the host was busy, and the neutralized case right
    // after. Five of seven passes slowed: `at` moves, and the spread says so.
    let t = timing(vec![42.9, 44.3, 51.1, 56.9, 49.6, 62.0, 52.6]);
    assert_eq!(t.at, 49.6);
    assert!(t.spread > 0.15, "{t:?}");
    let t = timing(vec![43.6, 43.2, 43.2, 43.3, 43.8, 43.4, 43.2]);
    assert_eq!(t.at, 43.2);
    assert!(t.spread < 0.01 && t.jitter < 0.02, "{t:?}");
}

#[test]
fn a_few_slowed_passes_leave_the_time_alone_and_show_as_jitter() {
    // #429's CI job (an EPYC 9V74): three passes near 60 µs and
    // two a little slow (37.2 and 37.9). The median read 37.9 µs, the
    // fourth pass, and the gate failed at 50.8 µs. The third-fastest is
    // 37.2 µs, 2% over the fastest: slowed passes can only raise it.
    let t = timing(vec![37.2, 36.4, 59.8, 37.9, 63.9, 60.9, 36.4]);
    assert_eq!(t.at, 37.2);
    assert!(t.spread < 0.03, "{t:?}");
    assert!(t.jitter > 0.15, "{t:?}");
}

/// A slower or faster engine scales every pass alike: the time scales
/// with it and the spread doesn't move, so a slower runner can't read as
/// a disturbed one.
#[test]
fn a_uniformly_slower_engine_keeps_its_spread() {
    let passes = vec![42.9, 44.3, 51.1, 56.9, 49.6, 62.0, 52.6];
    let base = timing(passes.clone());
    for k in [0.25, 0.5, 2.0, 3.93, 10.0] {
        let t = timing(passes.iter().map(|p| p * k).collect());
        assert!((t.at - base.at * k).abs() < 1e-9 * k, "×{k}: {t:?}");
        assert!((t.spread - base.spread).abs() < 1e-12, "×{k}: {t:?}");
        assert!((t.jitter - base.jitter).abs() < 1e-12, "×{k}: {t:?}");
    }
}

proptest! {
    /// Noise only adds time: adding any non-negative delay to any passes
    /// never makes the gate's time read faster, so noise can only make the
    /// gate stricter. A slower engine slows every pass, and the time
    /// rises at least by the smallest slowdown.
    #[test]
    fn noise_never_makes_a_case_read_faster(
        passes in prop::collection::vec(1.0f64..100.0, 7),
        noise in prop::collection::vec(0.0f64..100.0, 7),
        slower in 1.0f64..2.0,
    ) {
        let clean = timing(passes.clone());
        let noisy = timing(passes.iter().zip(&noise).map(|(p, n)| p + n).collect());
        prop_assert!(noisy.at >= clean.at, "{clean:?} → {noisy:?}");
        let slow = timing(passes.iter().map(|p| p * slower).collect());
        prop_assert!(slow.at >= clean.at * slower * (1.0 - 1e-12), "{clean:?} → {slow:?}");
    }
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
    let s = scaling("AMD EPYC 9999 Processor", 8.36, 40.0);
    assert!((s.speed - 0.8).abs() < 1e-9, "{}", s.speed);
    assert_eq!(s.speed, 8.36e3 / CALIB_REF_NS);
    assert!(s.notes.iter().any(|n| n.contains("isn't in ENGINE_SPEED")));
}

#[test]
fn a_listed_model_scales_by_its_calibration_and_skew() {
    // #210's CI job, as fixed inputs: calibration 9.86 µs, mixed
    // 43.4 µs (its median pass, as the gate read it then).
    // 9.86 ÷ 10.59 × 1.097 = 1.021381.
    let s = scaling("INTEL(R) XEON(R) PLATINUM 8573C", 9.86, 43.4);
    assert!((s.speed - 1.021381).abs() < 1e-6, "{}", s.speed);
    assert!((48.0 / s.speed - 47.00).abs() < 0.01);
    // Its skew (43.4 ÷ 9.86 ÷ 4.02 = 1.095) is within MODEL_DRIFT.
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

/// An engine with the shipped core, every pack on and the pack versions
/// an install has (RP-G), sharing `sigs` as SP-4's set. `upgraded`: a
/// store an earlier build set up with every pack on, so the versions
/// this build changed or added are pending, judged in shadow until
/// accepted; otherwise a fresh store, which applies every current
/// version.
fn installed(
    upgraded: bool,
    sigs: &std::sync::Arc<lodestar_filter::spam_sigs::SpamSigs>,
) -> lodestar_filter::Engine {
    use lodestar_filter::packs::manifest::Manifest;
    use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
    use lodestar_store::{Store, StoreConfig};
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    if upgraded {
        s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
        for p in lodestar_filter::packs::Pack::ALL {
            s.set_pack_switch(p.as_str(), true).unwrap();
        }
    }
    upgrade::ensure_seeded(&mut s, &Manifest::builtin(), 0).unwrap();
    let mut e = lodestar_filter::Engine::new(shipped());
    let notes = e.load_rules(&s.rule_set().unwrap());
    assert!(notes.is_empty(), "{notes:?}");
    e.packs = lodestar_filter::PackSwitches::all();
    e.spam_sigs = sigs.clone();
    let pending: Vec<&str> = e.pack_versions.pending().map(|(r, _)| r.id()).collect();
    if upgraded {
        // The shipped versions an earlier install hadn't accepted.
        for id in ["MW-2", "FK-2", "SP-4"] {
            assert!(pending.contains(&id), "{id} pending: {pending:?}");
        }
        println!("versions pending in shadow: {pending:?}");
    } else {
        assert!(pending.is_empty(), "a fresh install: {pending:?}");
    }
    e
}

/// The writer's second judgment of a commit (RP-4's entry point,
/// [`lodestar_filter::Engine::judge_at_commit`]), at its worst: SP-3 at its
/// demote level, as in a flood, so every torrent pays it, and SP-4 looked
/// up in its set as the writer does (the payload signature, then one map
/// read).
fn at_commit(
    e: &lodestar_filter::Engine,
    item: &Item,
    facts: &lodestar_filter::Facts,
) -> lodestar_filter::Verdict {
    at_commit_as(e, item, facts, false)
}

/// [`at_commit`] for a find from a source that's always adult (`adult`):
/// the engine also reads a torrent not classified Adult as Adult, a second
/// reading of the whole engine (`Engine::judge_at_commit`).
fn at_commit_as(
    e: &lodestar_filter::Engine,
    item: &Item,
    facts: &lodestar_filter::Facts,
    adult: bool,
) -> lodestar_filter::Verdict {
    use lodestar_filter::spam_sigs::usable;
    let clone_of =
        usable(lodestar_filter::payload_sig(item.files)).and_then(|s| e.spam_sigs.get(s));
    let at = lodestar_filter::packs::sp4::AtCommit {
        flood: Some(lodestar_filter::flood::Level::Demote),
        clone_of,
    };
    e.judge_at_commit(item, facts, 0, at, adult)
}

/// The writer's at-commit judgment as [`at_commit_as`] has it, from the
/// first judgment's trail (X-commit-rejudge-reuse,
/// `Engine::judge_at_commit_reusing`).
fn at_commit_reusing(
    e: &lodestar_filter::Engine,
    trail: &lodestar_filter::Trail,
    item: &Item,
    facts: &lodestar_filter::Facts,
    adult: bool,
) -> lodestar_filter::Verdict {
    use lodestar_filter::spam_sigs::usable;
    let clone_of =
        usable(lodestar_filter::payload_sig(item.files)).and_then(|s| e.spam_sigs.get(s));
    let at = lodestar_filter::packs::sp4::AtCommit {
        flood: Some(lodestar_filter::flood::Level::Demote),
        clone_of,
    };
    e.judge_at_commit_reusing(trail, item, facts, 0, at, adult)
}

/// Prepare's judgment as the worker makes it (`judge_kept`), keeping its
/// trail: with `adult`, an always-adult source's find, read as Adult too.
fn prepared(
    e: &lodestar_filter::Engine,
    item: &Item,
    facts: &lodestar_filter::Facts,
    adult: bool,
) -> (lodestar_filter::Verdict, lodestar_filter::Trail) {
    let mut trail = lodestar_filter::Trail::default();
    let v = if adult {
        lodestar_filter::reclassify::judge_source_adult_with(e, item, facts, 0, Some(&mut trail)).0
    } else {
        e.judge_classified_with(item, Some(facts), 0, false, Some(&mut trail))
            .0
    };
    (v, trail)
}

/// None of the budget's 10,000 rules matches its torrents, so every rule
/// is checked to the end.
fn no_rule_of_yours(v: &lodestar_filter::Verdict, what: &str) {
    assert!(
        v.decision
            .as_ref()
            .is_none_or(|d| !matches!(d.rule, lodestar_filter::RuleRef::User(_))),
        "no rule of yours matches the set: {what}: {:?}",
        v.decision
    );
}

/// Distinct signatures, spread as hashes are, for filling SP-4's set.
fn spread(i: u64) -> u64 {
    let mut x = i.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// SP-4's set at its cap (RP-G), shared by every engine of the budget:
/// filler, then the signatures of every tenth torrent of `items` whose
/// payload is specific, added last so the cap keeps them. Many of the
/// budget's torrents share a payload, so most of them are clones: the
/// at-commit cases judge SP-4 inheriting a hide on most torrents.
fn sigs_at_cap(items: &[Item]) -> std::sync::Arc<lodestar_filter::spam_sigs::SpamSigs> {
    use lodestar_filter::spam_sigs::{CAP, Original, SpamSigs, usable};
    let hid = Original {
        rule: lodestar_filter::PackRule::Sp2,
        action: lodestar_filter::Action::Hide,
    };
    let originals: std::collections::BTreeSet<u64> = items
        .iter()
        .step_by(10)
        .filter_map(|i| usable(lodestar_filter::payload_sig(i.files)))
        .collect();
    let sigs = std::sync::Arc::new(SpamSigs::with_cap(CAP));
    for i in 0..(CAP - originals.len()) as u64 {
        sigs.add(spread(i), hid);
    }
    for s in &originals {
        sigs.add(*s, hid);
    }
    assert_eq!(sigs.len(), CAP, "the set at its cap");
    assert!(originals.iter().all(|s| sigs.get(*s) == Some(hid)));
    sigs
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
/// **The rule-pack gate (RP-G, plan 2026-11-04 §4.3):** every engine case
/// runs with every pack on at the versions a fresh install applies, and
/// with SP-4's set at its 1,000,000 cap, shared as the writer shares it
/// ([`sigs_at_cap`]). Each held engine case is timed twice: as prepare
/// judges, and as the writer judges the commit again through RP-4's entry
/// point at its worst ([`at_commit`]: SP-3 at its demote level and SP-4
/// looked up, so the clones inherit their original's hide). The held sets
/// are timed again, both ways, on an install an earlier build set up,
/// whose new versions are pending and judged in shadow ([`installed`]).
/// The budget is per judgment, as RP-4 set it (#495), and a torrent's
/// second judgment and second reading are held too
/// (X-commit-rejudge-reuse, `docs/decisions/2026-11-06-X-commit-rejudge-reuse.md`):
/// a flooded or cloned commit's two judgments together (prepare's, which
/// keeps its trail, and the writer's from it,
/// `Engine::judge_at_commit_reusing`), and an always-adult source's find at
/// commit, judged from the trail prepare kept of both its readings, as
/// found and as Adult. The judgment that reads such a find both ways
/// (prepare's, or the commit's where no trail holds), and that find's two
/// judgments together, are reported, not held: one judgment read twice.
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
/// the test; a held case whose third-fastest pass is more than 15% over
/// its fastest ([`Timing`]) is caught the same way, and either measures
/// again. Where it scales, it never runs beside filter state, so a
/// regression can't slow it and hide itself. Noise that slows the
/// calibration makes a scaled runner read faster than it is, so it takes
/// the fastest of 14 passes, and the 15% drift bound limits what noise on
/// one side can hide. Each case decides on its third-fastest pass of
/// seven, which noise can't make faster
/// (`docs/decisions/2026-11-05-X-filter-budget-noise.md`). An attempt
/// where every held case over budget had a pass more than 15% over its own
/// fastest measures again; one with a steady case over budget fails. From
/// then on, a scaled model is scaled by the fastest calibration of any
/// attempt, the strictest, so a fresh draw can't loosen it. Raw passes, the
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
    // a held case's third-fastest pass and its fastest ([`Timing`]).
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
    // then seven passes over the set. The third-fastest decides
    // ([`Timing`]), so slowed passes on a shared runner don't; with how far
    // it is above the fastest pass, so a run of them is caught.
    let time = |what: &str, f: &dyn Fn(usize)| {
        pass(f);
        let passes: Vec<f64> = (0..7).map(|_| pass(f)).collect();
        println!("{what}: passes {} µs", show(&passes));
        timing(passes)
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
    // trusted attempt decides, and only drift on every attempt fails. A
    // trusted attempt whose every held case over budget shows the host
    // was busy (a pass more than DRIFT over its fastest) measures again
    // too: noise only adds time to a case, so a later attempt passes only
    // if the engine reads under budget. A steady case over budget fails
    // at once.
    // SP-4's set at its cap (RP-G), and how many torrents are clones of
    // an original in it: each of those inherits its hide at commit.
    let sigs = sigs_at_cap(&items);
    let clones = items
        .iter()
        .filter(|i| {
            lodestar_filter::spam_sigs::usable(lodestar_filter::payload_sig(i.files))
                .and_then(|s| sigs.get(s))
                .is_some()
        })
        .count();
    println!(
        "SP-4's set: {} signatures (its cap); {clones} of the {} torrents are clones",
        sigs.len(),
        items.len()
    );
    assert!(clones >= items.len() / 10, "{clones} clones");
    {
        let e = installed(false, &sigs);
        let mut inherited = 0;
        for (i, item) in items.iter().enumerate() {
            let sig = lodestar_filter::spam_sigs::usable(lodestar_filter::payload_sig(item.files));
            if sig.and_then(|s| sigs.get(s)).is_some() {
                // SP-4 decides, unless prepare's own decision was already
                // at least a hide.
                let d = at_commit(&e, item, &facts[i]).decision;
                let before = e.judge(item, Some(&facts[i]), 0).decision;
                let sp4 = d.as_ref().is_some_and(|d| {
                    d.rule == lodestar_filter::RuleRef::Pack(lodestar_filter::PackRule::Sp4)
                });
                let held = before
                    .as_ref()
                    .is_some_and(|d| d.action >= lodestar_filter::Action::Hide);
                assert!(
                    sp4 || held,
                    "a clone inherits its original's hide through SP-4 at commit: {d:?}"
                );
                inherited += usize::from(sp4);
            }
        }
        println!("SP-4 decides {inherited} of the {clones} clones at commit");
        // The second judgment from the trail is the full one, on the
        // budget's own torrents, and reads the trail for every one.
        let mut e = installed(false, &sigs);
        e.rules = mixed_rules().into();
        let e = lodestar_filter::Handle::new(e).current();
        for (i, item) in items.iter().enumerate() {
            for adult in [false, true] {
                let (_, trail) = prepared(&e, item, &facts[i], adult);
                assert!(trail.holds_for(&e, item, &facts[i], 0), "the trail holds");
                assert_eq!(
                    at_commit_reusing(&e, &trail, item, &facts[i], adult),
                    at_commit_as(&e, item, &facts[i], adult),
                    "from the trail as in full"
                );
            }
        }
        // The rest are already hidden in prepare by other packs (the
        // budget's names are scene-style, which UC-3 reads).
        assert!(
            inherited >= items.len() / 10,
            "SP-4 decides a tenth of the set at commit: {inherited}"
        );
    }
    let mut drifted = Vec::new();
    // The fastest calibration of any attempt so far, and whether a busy
    // attempt over budget has measured again: from then on that fastest
    // calibration scales, so a failing scaled run can't pass on a slower
    // calibration draw. Noise can't make a calibration faster.
    let mut fastest_calib = f64::INFINITY;
    let mut busy_retried = false;
    for attempt in 1..=ATTEMPTS {
        println!("attempt {attempt} of {ATTEMPTS}");
        let before = calibration("before");
        let core = shipped();
        let t = time("core", &|i| {
            std::hint::black_box(core.verdict(std::hint::black_box(&items[i])));
        });
        let mut cases = vec![("core".to_owned(), t, true)];

        drop(core);
        // The whole engine (RP-G): the core, every pack at the versions a
        // fresh install applies, SP-4's set at its cap, and 10,000 of your
        // rules, reading the classification as ingest does. Each case is
        // timed as prepare judges ([`Engine::judge`]); a held case is also
        // timed as the writer judges a commit again, through RP-4's entry
        // point ([`at_commit`]).
        let mut engine = installed(false, &sigs);
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
        // Prepare's judgment of each torrent, then (`commit`) the writer's
        // second one. `none`: assert that no rule of yours decides (the
        // rule sets alone; with words neutralized a negation can match).
        let prepare = |e: &lodestar_filter::Engine, what: &str, none: bool| {
            time(what, &|i| {
                let v = e.judge(std::hint::black_box(&items[i]), Some(&facts[i]), 0);
                if none {
                    no_rule_of_yours(&v, what);
                }
                std::hint::black_box(v);
            })
        };
        let commit = |e: &lodestar_filter::Engine, what: &str, none: bool| {
            time(what, &|i| {
                let v = at_commit(e, std::hint::black_box(&items[i]), &facts[i]);
                if none {
                    no_rule_of_yours(&v, what);
                }
                std::hint::black_box(v);
            })
        };
        let counts = |e: &lodestar_filter::Engine, what: &str| {
            let (met, full) = (0..items.len()).fold((0, 0), |(m, f), i| {
                let (a, b) = e.rule_counts(&items[i], Some(&facts[i]));
                (m + a, f + b)
            });
            println!(
                "{what}: each torrent meets {:.0} rules on average; {:.0} get the full match",
                met as f64 / items.len() as f64,
                full as f64 / items.len() as f64
            );
        };
        for (what, rules, held) in &sets {
            engine.rules = rules.clone().into();
            counts(&engine, what);
            cases.push((what.clone(), prepare(&engine, what, true), *held));
            if *held {
                let what = format!("{what}, at commit");
                cases.push((what.clone(), commit(&engine, &what, true), true));
            }
        }
        // The cases below are all on the mixed set, as on `main` before
        // RP-G: the loop above left the last shape's rules in place.
        engine.rules = mixed_rules().into();
        // A flooded or cloned commit pays both judgments: the writer
        // judges again whenever SP-3's level or SP-4's set applies, from
        // the trail prepare's judgment kept, as the worker keeps it.
        let what =
            "engine, packs on, 10,000 rules, mixed, a flooded or cloned commit's two judgments";
        let t = time(what, &|i| {
            let (item, facts) = (std::hint::black_box(&items[i]), &facts[i]);
            let (v, trail) = prepared(&engine, item, facts, false);
            std::hint::black_box(v);
            std::hint::black_box(at_commit_reusing(&engine, &trail, item, facts, false));
        });
        cases.push((what.to_owned(), t, true));
        // An always-adult source's find at commit, flooded or cloned: the
        // writer judges it again from the trail prepare's judgment kept
        // with both readings, as found and as Adult (none of the budget's
        // torrents is classified Adult, so every one has both).
        assert!(
            items
                .iter()
                .all(|i| i.content_type != lodestar_classify::ContentType::Adult),
            "none of the budget's torrents is classified Adult, so each pays the Adult reading"
        );
        let trails: Vec<_> = items
            .iter()
            .zip(&facts)
            .map(|(item, facts)| prepared(&engine, item, facts, true).1)
            .collect();
        let what = "engine, packs on, 10,000 rules, mixed, at commit, from an always-adult source, from prepare's trail";
        let t = time(what, &|i| {
            std::hint::black_box(at_commit_reusing(
                &engine,
                &trails[i],
                std::hint::black_box(&items[i]),
                &facts[i],
                true,
            ));
        });
        cases.push((what.to_owned(), t, true));
        drop(trails);
        // The judgment that reads the find as found and again as Adult:
        // prepare's, and the commit's where no trail holds (a torrent
        // cache's file, or a rule change between). Reported, not held: one
        // judgment read twice (RP-G), the Adult reading sharing the reading
        // as found's work.
        let what = "engine, packs on, 10,000 rules, mixed, an always-adult source's find read as found and as Adult";
        let t = time(what, &|i| {
            std::hint::black_box(at_commit_as(
                &engine,
                std::hint::black_box(&items[i]),
                &facts[i],
                true,
            ));
        });
        cases.push((what.to_owned(), t, false));
        // That find flooded or cloned, both judgments: prepare reads it
        // both ways and keeps the trail, and the writer's second judgment
        // reads both from it. Reported, not held, as the judgment above.
        let what = "engine, packs on, 10,000 rules, mixed, an always-adult source's flooded or cloned find's two judgments";
        let t = time(what, &|i| {
            let (item, facts) = (std::hint::black_box(&items[i]), &facts[i]);
            let (v, trail) = prepared(&engine, item, facts, true);
            std::hint::black_box(v);
            std::hint::black_box(at_commit_reusing(&engine, &trail, item, facts, true));
        });
        cases.push((what.to_owned(), t, false));
        // The mixed set with pack-rule overrides (RP-2): a lighter
        // action, a heavier one, one with an `unless`, and one on a rule
        // that flags.
        let rows: Vec<_> = [
            ("MW-2", "demote", None),
            ("SP-1", "hide", None),
            ("LU-2", "flag", Some("sample OR trailer")),
            ("FK-2", "drop", None),
        ]
        .into_iter()
        .map(|(id, action, unless)| {
            let rule = lodestar_filter::PackRule::from_id(id).expect("a pack rule");
            let row = lodestar_filter::packs::overrides::PackOverride {
                base: lodestar_filter::packs::overrides::current_base(&engine.pack_versions, rule),
                action: lodestar_filter::packs::overrides::action_named(action).unwrap(),
                unless: unless.map(str::to_owned),
            };
            (id.to_owned(), Ok(row.encode()))
        })
        .collect();
        engine.pack_overrides = lodestar_filter::packs::overrides::PackOverrides::resolve(
            &engine.manifest,
            &engine.pack_versions,
            &rows,
        );
        assert_eq!(engine.pack_overrides.iter().count(), 4);
        assert!(
            engine
                .pack_overrides
                .iter()
                .all(|o| !o.unreadable() && !o.changed && !o.paused),
            "every override bound to the version that applies"
        );
        let what = "engine, packs on, 10,000 rules, mixed, 4 pack overrides (1 with unless)";
        cases.push((what.to_owned(), prepare(&engine, what, false), true));
        let what = format!("{what}, at commit");
        cases.push((what.clone(), commit(&engine, &what, false), true));
        engine.pack_overrides = Default::default();
        // The mixed set again with common words neutralized (CSX-5
        // *Neutralize term*): every keyword entry reads the name and
        // paths, and every text clause checks the words.
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
        cases.push((what.to_owned(), prepare(&engine, what, false), true));
        let what = format!("{what}, at commit");
        cases.push((what.clone(), commit(&engine, &what, false), true));
        drop(engine);
        // An install an earlier build set up (RP-G): the versions this
        // build changed or added (MW-2@2, FK-2@2, SP-4) pending, judged in
        // shadow beside the applied ones, for the held sets.
        let mut pending = installed(true, &sigs);
        for (what, rules, held) in sets.iter().filter(|(_, _, held)| *held) {
            pending.rules = rules.clone().into();
            let what = what.replacen("packs on", "packs on, versions pending in shadow", 1);
            cases.push((what.clone(), prepare(&pending, &what, true), *held));
            let what = format!("{what}, at commit");
            cases.push((what.clone(), commit(&pending, &what, true), true));
        }
        drop(pending);
        let after = calibration("after");

        let calib_us = before.min(after);
        fastest_calib = fastest_calib.min(calib_us);
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
            .filter(|(_, t, held)| *held && t.spread > DRIFT)
            .map(|(what, t, _)| {
                format!(
                    "{what}: third-fastest pass {:.0}% over the fastest",
                    t.spread * 100.0
                )
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
            .1
            .at;
        let scale_us = if busy_retried {
            fastest_calib
        } else {
            calib_us
        };
        if scale_us < calib_us {
            println!(
                "after a busy attempt, scaling by the fastest calibration of any attempt, \
                 {scale_us:.2} µs, not this attempt's {calib_us:.2} µs"
            );
        }
        let Scaling { speed, how, notes } = scaling(&cpu, scale_us, mixed);
        for note in notes {
            println!("{note}");
        }
        let mut over = Vec::new();
        // Whether every held case over budget had a pass more than DRIFT
        // over its own fastest: noise in another case doesn't count, so a
        // steady case over budget always fails at once.
        let mut all_busy = true;
        for (what, t, held) in cases {
            let raw = t.at;
            let per = raw / speed;
            println!(
                "{what}: {per:.1} µs per torrent on the reference runner ({raw:.1} µs here, \
                 the third-fastest pass, ÷ {speed:.3}, {how})"
            );
            if per >= BUDGET_US {
                if held {
                    all_busy &= t.jitter > DRIFT;
                    over.push(format!(
                        "{what}: {per:.1} µs ({raw:.1} µs here, slowest pass {:.0}% over the \
                         fastest)",
                        t.jitter * 100.0
                    ));
                } else {
                    println!("{what}: over {BUDGET_US} µs (reported, not held)");
                }
            }
        }
        if !over.is_empty() && all_busy && attempt < ATTEMPTS {
            let note = format!("attempt {attempt}: over budget while busy: {over:?}");
            println!(
                "{note}: the host was busy during each case over budget, so measuring again \
                 (only an attempt under budget passes)"
            );
            drifted.push(note);
            busy_retried = true;
            continue;
        }
        assert!(
            over.is_empty(),
            "over the {BUDGET_US} µs budget on the reference runner, with this runner at \
             {speed:.2}× its time: {over:?}"
        );
        return;
    }
    panic!(
        "no attempt of {ATTEMPTS} could decide: the runner's speed changed under the test, \
         the engine's time varied between passes, or the host was busy during a case over \
         budget, so the measurement can't be trusted: {drifted:?}"
    );
}

/// One judgment of each of the budget's torrents, for callgrind, whose
/// instruction counts don't move with the runner (§20.8). `LODESTAR_PROFILE`
/// picks the case: `core`, `mixed` (the default), `neutralized`, or one
/// shape at 10% ([`shape_rules`]): `near`, `negation`, `fuzzy`, `prefix` or
/// `title`. A warm-up pass first, so the counts are a judgment's, not the
/// index build's:
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
        let shape = Shape::ALL
            .into_iter()
            .find(|s| format!("{s:?}").eq_ignore_ascii_case(&case));
        engine.rules = match shape {
            Some(shape) => shape_rules(shape),
            None => mixed_rules(),
        }
        .into();
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

// ---------------------------------------------------------------------
// X-commit-rejudge-reuse: the writer's second judgment from the first's
// trail, and the Adult reading sharing the reading as found's work, each
// against the full judgment as it was (the `_unshared` oracles).

mod reuse {
    use super::*;
    use lodestar_filter::flood::Level;
    use lodestar_filter::packs::manifest::Manifest;
    use lodestar_filter::packs::overrides::{
        PackOverride, PackOverrides, action_named, current_base,
    };
    use lodestar_filter::packs::sp4::AtCommit;
    use lodestar_filter::packs::upgrade::{self, FRESH_KEY};
    use lodestar_filter::reclassify::{judge_source_adult_unshared, judge_source_adult_with};
    use lodestar_filter::spam_sigs::Original;
    use lodestar_filter::{
        Action, Allow, ClassRule, ClassRules, Engine, Exception, Facts, Handle, ListLayer, Match,
        Origin, Override, Pack, PackRule, PackSwitches, RuleRef, Strength, TermSet, Trail,
        UserRule, Verdict,
    };
    use lodestar_store::{Store, StoreConfig};

    /// The core with its neutral stand-in words, as `tests/core.rs` drives
    /// it, and one confirmed hash on the floor.
    fn core() -> Core {
        let lists = Lists {
            sexual: TermSet::of(&["lexa"]),
            child_roles: TermSet::of(&["rolea"]),
            codes: TermSet::of(&["codex"]),
        };
        let mut c = Core::new(lists, CsList::default(), Switches::default());
        c.cs.add_hash([9; 20], Origin::Confirmed);
        c
    }

    /// An install's packs: a fresh store's versions, or (`pending`) one an
    /// earlier build set up, whose newer versions run in shadow.
    fn installed(pending: bool) -> Engine {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        if pending {
            s.update_settings_text(&[], &[FRESH_KEY]).unwrap();
            for p in Pack::ALL {
                s.set_pack_switch(p.as_str(), true).unwrap();
            }
        }
        upgrade::ensure_seeded(&mut s, &Manifest::builtin(), 0).unwrap();
        let mut e = Engine::new(core());
        assert!(e.load_rules(&s.rule_set().unwrap()).is_empty());
        e.packs = PackSwitches::all();
        e
    }

    /// Words the torrents are made of: the core's stand-ins and an age,
    /// pack triggers (a stuffed name, a site advert, an executable), the
    /// words the rules below read, and plain release words.
    const WORDS: &[&str] = &[
        "lexa",
        "rolea",
        "codex",
        "15yo",
        "documentary",
        "holiday",
        "beacon",
        "amber",
        "sample",
        "web",
        "1080p",
        "x265",
        "free",
        "movie",
        "download",
        "www.site.com",
        "crack",
        "keygen",
        "setup",
        "S01E02",
        "2024",
        "studiox",
    ];
    const FILES: &[&str] = &[
        "a.mkv",
        "b.mp4",
        "setup.exe",
        "readme.txt",
        "song.mp3",
        "pic.jpg",
        "x.url",
        "extras/rolea lexa.mkv",
        "sample.mkv",
        "Season 1/ep1.mkv",
        "password.txt",
    ];

    /// The queries your rules, exceptions, *Always allow* and the
    /// classification rules are drawn from: words, tags (the part that
    /// reads differently as Adult), negations, sizes, dates and times of
    /// day (the parts that read the time).
    const QUERIES: &[&str] = &[
        "holiday",
        "beacon",
        "sample",
        "type:adult",
        "type:movie",
        "type:video holiday",
        "adult:scene",
        "adult:movie type:adult",
        "-sample web",
        "studiox type:adult",
        "size:>1GB",
        "files:>2",
        "discovered:<1d",
        "discovered:>2024-01-01",
        // Within the drawn times, so a date reads differently at commit.
        "discovered:<1m",
        "discovered:>1m holiday",
        "amber | beacon",
        "\"free movie\"",
    ];

    fn query(i: usize) -> lodestar_search::Node {
        lodestar_search::parse(QUERIES[i % QUERIES.len()]).unwrap()
    }

    const ACTIONS: &[Action] = &[
        Action::Flag,
        Action::Demote,
        Action::Hide,
        Action::Quarantine,
        Action::Drop,
    ];

    /// A CSX-5 entry: its word, Neutralize or Override, expiry, end of its
    /// shadow period and scope.
    type OverrideDraw = (usize, bool, Option<i64>, Option<i64>, Option<usize>);

    /// What an engine is given beyond its packs, each drawn by index.
    #[derive(Debug, Clone)]
    struct Setup {
        pending: bool,
        packs_off: Vec<usize>,
        rules: Vec<(usize, usize, Option<i64>, Option<usize>)>,
        stand_in: Option<usize>,
        overrides: Vec<OverrideDraw>,
        allow: Option<usize>,
        exceptions: Vec<(usize, usize)>,
        class: Vec<(usize, bool)>,
        pack_overrides: Vec<(usize, usize, bool)>,
        listed: Option<usize>,
    }

    fn setup() -> impl Strategy<Value = Setup> {
        let time = || prop::option::of(95i64..106);
        (
            any::<bool>(),
            prop::collection::vec(0usize..32, 0..3),
            prop::collection::vec(
                (0usize..64, 0usize..5, time(), prop::option::of(0usize..64)),
                0..6,
            ),
            prop::option::of(0usize..6),
            prop::collection::vec(
                (
                    0usize..8,
                    any::<bool>(),
                    time(),
                    time(),
                    prop::option::of(0usize..64),
                ),
                0..3,
            ),
            prop::option::of(0usize..64),
            prop::collection::vec((0usize..16, 0usize..64), 0..3),
            prop::collection::vec((0usize..64, any::<bool>()), 0..3),
            prop::collection::vec((0usize..6, 0usize..5, any::<bool>()), 0..3),
            prop::option::of(0usize..5),
        )
            .prop_map(
                |(
                    pending,
                    packs_off,
                    rules,
                    stand_in,
                    overrides,
                    allow,
                    exceptions,
                    class,
                    pack_overrides,
                    listed,
                )| {
                    Setup {
                        pending,
                        packs_off,
                        rules,
                        stand_in,
                        overrides,
                        allow,
                        exceptions,
                        class,
                        pack_overrides,
                        listed,
                    }
                },
            )
    }

    const OVERRIDE_WORDS: &[&str] = &[
        "sample", "web", "holiday", "beacon", "lexa", "free", "x265", "amber",
    ];
    const PACK_RULES: &[PackRule] = &[
        PackRule::Sp2,
        PackRule::Sp3,
        PackRule::Sp4,
        PackRule::Mw2,
        PackRule::Fk2,
        PackRule::Sp1,
    ];

    fn build(s: &Setup) -> Engine {
        let mut e = installed(s.pending);
        for &i in &s.packs_off {
            let r = PackRule::ALL[i % PackRule::ALL.len()];
            if i % 5 == 0 {
                e.packs.on.remove(&r.pack());
            } else {
                e.packs.rules_off.insert(r);
            }
        }
        e.rules = s
            .rules
            .iter()
            .enumerate()
            .map(|(n, &(q, a, shadow_until, unless))| UserRule {
                id: n as u32 + 1,
                query: query(q),
                action: ACTIONS[a],
                enabled: true,
                shadow_until,
                unless: unless.map(query),
            })
            .collect::<Vec<_>>()
            .into();
        e.stand_ins = s.stand_in.map(|i| i as u32 + 1).into_iter().collect();
        e.overrides = s
            .overrides
            .iter()
            .enumerate()
            .map(
                |(n, &(w, neutralize, expires_at, shadow_until, scope))| Override {
                    id: n as u32 + 1,
                    what: Match::Keyword(OVERRIDE_WORDS[w].into()),
                    strength: if neutralize {
                        Strength::Neutralize
                    } else {
                        Strength::Override
                    },
                    // A scope can read tags, so the two readings can neutralize
                    // different words.
                    scope: scope.map(query),
                    expires_at,
                    shadow_until,
                },
            )
            .collect();
        e.allow = s
            .allow
            .map(|q| Allow {
                id: 1,
                query: query(q),
            })
            .into_iter()
            .collect();
        e.exceptions = s
            .exceptions
            .iter()
            .enumerate()
            .map(|(n, &(r, q))| Exception {
                id: n as u32 + 1,
                rule: match r {
                    0..6 => RuleRef::Pack(PACK_RULES[r]),
                    6 => RuleRef::Core(lodestar_filter::CoreRule::Cs4a),
                    7 => RuleRef::List(1),
                    8 => RuleRef::Pack(PackRule::Mw6),
                    _ => RuleRef::User(r as u32 - 8),
                },
                what: Match::Query(query(q)),
            })
            .collect();
        e.class_rules = ClassRules::new(
            s.class
                .iter()
                .enumerate()
                .map(|(n, &(q, adult))| ClassRule {
                    id: n as u32 + 1,
                    query: query(q),
                    adult,
                })
                .collect(),
        );
        let rows: Vec<_> = s
            .pack_overrides
            .iter()
            .map(|&(r, a, unless)| {
                let rule = PACK_RULES[r];
                let row = PackOverride {
                    base: current_base(&e.pack_versions, rule),
                    action: action_named(["flag", "demote", "hide", "quarantine", "drop"][a])
                        .unwrap(),
                    unless: unless.then(|| "sample OR holiday".to_owned()),
                };
                (rule.id().to_owned(), Ok(row.encode()))
            })
            .collect();
        e.pack_overrides = PackOverrides::resolve(&e.manifest, &e.pack_versions, &rows);
        if let Some(a) = s.listed {
            e.set_list(
                Some(ListLayer {
                    id: 1,
                    action: ACTIONS[a],
                    hashes: std::sync::Arc::new([[2; 20]].into_iter().collect()),
                }),
                1,
            );
        }
        e
    }

    /// A torrent: its name's words, its files, its hash, and whether it's
    /// already classified Adult.
    fn torrent() -> impl Strategy<Value = (String, Vec<FileInfo>, [u8; 20], bool)> {
        (
            prop::collection::vec(0usize..WORDS.len(), 0..9),
            any::<bool>(),
            prop::collection::vec(0usize..FILES.len(), 1..5),
            prop::sample::select(vec![[1u8; 20], [2; 20], [9; 20]]),
            prop::bool::weighted(0.2),
        )
            .prop_map(|(words, stuffed, files, hash, adult)| {
                let mut name: Vec<&str> = words.into_iter().map(|w| WORDS[w]).collect();
                if stuffed {
                    // A keyword-stuffed name SP-2 hides.
                    name.extend(std::iter::repeat_n(["free", "movie", "download"], 7).flatten());
                }
                let files = files
                    .into_iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let path = FILES[f].split('/').map(str::to_owned).collect();
                        FileInfo::new(path, 1_234_567_891 + i as u64)
                    })
                    .collect();
                (name.join(" "), files, hash, adult)
            })
    }

    /// How the engine changes between prepare and commit, if it does.
    fn change(e: &mut Engine, how: usize) {
        match how % 5 {
            0 => {}
            1 => {
                e.packs.rules_off.insert(PackRule::Mw2);
            }
            2 => {
                let mut rules: Vec<UserRule> = e.rules.to_vec();
                rules.push(UserRule {
                    id: 99,
                    query: query(how),
                    action: Action::Drop,
                    enabled: true,
                    shadow_until: None,
                    unless: None,
                });
                e.rules = rules.into();
            }
            3 => e.exceptions.clear(),
            _ => e.overrides = Vec::<Override>::new().into(),
        }
    }

    /// What prepare makes of the torrent, as the worker judges it
    /// (`lodestar_pipeline::source_adult::judge_kept`), with its trail.
    fn prepare(e: &Engine, item: &Item, facts: &Facts, now: i64, adult: bool) -> (Verdict, Trail) {
        let mut trail = Trail::default();
        let v = if adult {
            judge_source_adult_with(e, item, facts, now, Some(&mut trail)).0
        } else {
            e.judge_classified_with(item, Some(facts), now, false, Some(&mut trail))
                .0
        };
        (v, trail)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(3000))]

        /// The writer's second judgment from the first judgment's trail is
        /// the full judgment as it was, for every input: flood levels and
        /// clones, an always-adult source at prepare and at commit, an Is
        /// adult rule, torrents already Adult, pending versions, pack
        /// overrides and exceptions, the times on either side of a shadow
        /// period or an expiry, a date read, and a rule change between
        /// prepare and commit, which judges in full. So is each first
        /// judgment, whose Adult reading shares the reading as found's work.
        #[test]
        fn the_second_judgment_is_the_full_one(
            s in setup(),
            (name, files, hash, pre_adult) in torrent(),
            (adult_then, adult_now) in (any::<bool>(), any::<bool>()),
            (p, c, d) in (95i64..106, 95i64..106, prop::sample::select(vec![0i64, 95, 100])),
            flood in prop::option::of(prop::sample::select(vec![Level::Flag, Level::Demote])),
            clone_of in prop::option::of((prop::sample::select(vec![PackRule::Sp2, PackRule::Sp3]), prop::sample::select(vec![Action::Demote, Action::Hide]))),
            changed in prop::option::of(0usize..10),
        ) {
            // Prepare and commit share the engine, as the worker and the
            // writer share the handle's.
            let h = Handle::new(build(&s));
            let e = h.current();
            let found = lodestar_classify::classify(&name, &files);
            let class = if pre_adult { found.as_adult() } else { found };
            let hashes = [hash];
            let item = Item { hashes: &hashes, name: &name, files: &files, content_type: class.content_type };
            let facts_at = |now: i64, discovered_at: i64| Facts {
                class: &class,
                size: files.iter().map(|f| f.size).sum(),
                piece_len: 1 << 20,
                flags: 0,
                discovered_at,
                now,
            };
            // Prepare, as the worker does: discovered now.
            let pf = facts_at(p, p);
            let (first, trail) = prepare(&e, &item, &pf, p, adult_then);
            // Each first judgment is the full one.
            if adult_then {
                prop_assert_eq!(&first, &judge_source_adult_unshared(&e, &item, &pf, p).0);
            } else {
                prop_assert_eq!(
                    e.judge_classified_with(&item, Some(&pf), p, false, None),
                    e.judge_classified_unshared(&item, Some(&pf), p)
                );
                prop_assert_eq!(&first, &e.judge_classified_unshared(&item, Some(&pf), p).0);
            }
            // Commit, with the engine as it is then.
            drop(e);
            if let Some(how) = changed {
                h.update_engine(|e| change(e, how));
            }
            let e = h.current();
            let cf = facts_at(c, d);
            let at = AtCommit { flood, clone_of: clone_of.map(|(rule, action)| Original { rule, action }) };
            let full = e.judge_at_commit_unshared(&item, &cf, c, at, adult_now);
            prop_assert_eq!(&e.judge_at_commit(&item, &cf, c, at, adult_now), &full, "shared");
            prop_assert_eq!(
                &e.judge_at_commit_reusing(&trail, &item, &cf, c, at, adult_now),
                &full,
                "from the trail: {:?}", s
            );
            // A trail is read only for the engine that made it.
            let holds = trail.holds_for(&e, &item, &cf, c);
            prop_assert!(!(holds && changed.is_some()), "read across a change of rules");
            prop_assert!(
                holds || changed.is_some() || !trail.is_kept() || trail_misses(&s, p, c),
                "a trail that should hold doesn't: {:?}", s
            );
        }
    }

    /// The floor is judged again on every reading, never taken from a
    /// trail: even where one fits (here the engine is edited in place,
    /// which keeps its generation, as no running engine is), a hash put on
    /// the floor after prepare decides at commit, for the reading as found
    /// and for the Adult reading.
    #[test]
    fn the_floor_is_judged_again_under_a_trail() {
        let e = Handle::new(build(&Setup {
            pending: true,
            packs_off: vec![],
            rules: vec![(0, 2, None, None)],
            stand_in: None,
            overrides: vec![],
            allow: None,
            exceptions: vec![],
            class: vec![],
            pack_overrides: vec![],
            listed: None,
        }))
        .current();
        let name = "holiday 2024 1080p".to_owned();
        let files = vec![FileInfo::new(vec!["a.mkv".into()], 1_234_567_891)];
        let class = lodestar_classify::classify(&name, &files);
        let hashes = [[3u8; 20]];
        let item = Item {
            hashes: &hashes,
            name: &name,
            files: &files,
            content_type: class.content_type,
        };
        let facts = Facts {
            class: &class,
            size: 1,
            piece_len: 1,
            flags: 0,
            discovered_at: 0,
            now: 0,
        };
        let at = AtCommit {
            flood: Some(Level::Demote),
            clone_of: None,
        };
        for adult in [false, true] {
            let (_, trail) = prepare(&e, &item, &facts, 0, adult);
            let mut floored = Engine::clone(&e);
            // A copy is another engine: its trail doesn't fit.
            assert!(!trail.holds_for(&floored, &item, &facts, 0));
            let (_, trail) = prepare(&floored, &item, &facts, 0, adult);
            floored.core.cs.add_hash([3; 20], Origin::Confirmed);
            assert!(
                trail.holds_for(&floored, &item, &facts, 0),
                "edited in place"
            );
            let v = floored.judge_at_commit_reusing(&trail, &item, &facts, 0, at, adult);
            assert_eq!(
                v.decision.as_ref().map(|d| d.rule),
                Some(RuleRef::Core(lodestar_filter::CoreRule::Cs1a)),
                "the floor decides at commit (adult: {adult})"
            );
            assert_eq!(
                v,
                floored.judge_at_commit_unshared(&item, &facts, 0, at, adult)
            );
        }
    }

    /// A trail is read only for the torrent it was kept for: another
    /// torrent's, or the same one with other facts, judges in full.
    #[test]
    fn a_trail_is_never_read_for_another_torrent() {
        let e = Handle::new(build(&Setup {
            pending: false,
            packs_off: vec![],
            rules: vec![(0, 2, None, None)],
            stand_in: None,
            overrides: vec![],
            allow: None,
            exceptions: vec![],
            class: vec![],
            pack_overrides: vec![],
            listed: None,
        }))
        .current();
        let files = vec![FileInfo::new(vec!["a.mkv".into()], 1_234_567_891)];
        let (a, b) = (
            "Plain 2024 1080p".to_owned(),
            "Holiday 2024 1080p".to_owned(),
        );
        let (ca, cb) = (
            lodestar_classify::classify(&a, &files),
            lodestar_classify::classify(&b, &files),
        );
        let hashes = [[3u8; 20]];
        let item_a = Item {
            hashes: &hashes,
            name: &a,
            files: &files,
            content_type: ca.content_type,
        };
        let item_b = Item {
            hashes: &hashes,
            name: &b,
            files: &files,
            content_type: cb.content_type,
        };
        let facts = |class| Facts {
            class,
            size: 1,
            piece_len: 1,
            flags: 0,
            discovered_at: 0,
            now: 0,
        };
        let at = AtCommit {
            flood: Some(Level::Flag),
            clone_of: None,
        };
        let (_, trail) = prepare(&e, &item_a, &facts(&ca), 0, false);
        assert!(trail.holds_for(&e, &item_a, &facts(&ca), 0));
        // Another name: torrent A's trail says nothing of your rule on it.
        assert!(!trail.holds_for(&e, &item_b, &facts(&cb), 0));
        let v = e.judge_at_commit_reusing(&trail, &item_b, &facts(&cb), 0, at, false);
        assert_eq!(
            v,
            e.judge_at_commit_unshared(&item_b, &facts(&cb), 0, at, false)
        );
        assert_eq!(v.decision.map(|d| d.rule), Some(RuleRef::User(1)));
        // Other hashes, flags, size or classification.
        let other = [[4u8; 20]];
        assert!(!trail.holds_for(
            &e,
            &Item {
                hashes: &other,
                ..item_a
            },
            &facts(&ca),
            0
        ));
        let flagged = Facts {
            flags: 1,
            ..facts(&ca)
        };
        assert!(!trail.holds_for(&e, &item_a, &flagged, 0));
        let bigger = Facts {
            size: 2,
            ..facts(&ca)
        };
        assert!(!trail.holds_for(&e, &item_a, &bigger, 0));
        let adult = ca.as_adult();
        assert!(!trail.holds_for(&e, &item_a, &facts(&adult), 0));
    }

    /// A Neutralize entry whose scope reads tags applies in one reading and
    /// not the other: the Adult reading reuses nothing the reading as
    /// found read with other words neutralized. Here `holiday` is
    /// neutralized only as found, so only the Adult reading meets your rule
    /// on it, and its hide stands, as the full judgment has it.
    #[test]
    fn each_reading_neutralizes_its_own_words() {
        let mut e = build(&Setup {
            pending: false,
            packs_off: vec![],
            rules: vec![(0, 2, None, None)],
            stand_in: None,
            overrides: vec![],
            allow: None,
            exceptions: vec![],
            class: vec![],
            pack_overrides: vec![],
            listed: None,
        });
        e.overrides = vec![Override {
            id: 1,
            what: Match::Keyword("holiday".into()),
            strength: Strength::Neutralize,
            scope: Some(lodestar_search::parse("-type:adult").unwrap()),
            expires_at: None,
            shadow_until: None,
        }]
        .into();
        let e = Handle::new(e).current();
        let name = "Holiday 2024 1080p".to_owned();
        let files = vec![FileInfo::new(vec!["a.mkv".into()], 1_234_567_891)];
        let class = lodestar_classify::classify(&name, &files);
        assert_ne!(class.content_type, lodestar_classify::ContentType::Adult);
        let hashes = [[3u8; 20]];
        let item = Item {
            hashes: &hashes,
            name: &name,
            files: &files,
            content_type: class.content_type,
        };
        let facts = Facts {
            class: &class,
            size: 1,
            piece_len: 1,
            flags: 0,
            discovered_at: 0,
            now: 0,
        };
        let at = AtCommit {
            flood: Some(Level::Flag),
            clone_of: None,
        };
        let full = e.judge_at_commit_unshared(&item, &facts, 0, at, true);
        assert_eq!(
            full.decision.as_ref().map(|d| (d.rule, d.action)),
            Some((RuleRef::User(1), Action::Hide)),
            "the Adult reading meets the rule"
        );
        assert_eq!(e.judge_at_commit(&item, &facts, 0, at, true), full);
        let (first, trail) = prepare(&e, &item, &facts, 0, true);
        assert_eq!(first, judge_source_adult_unshared(&e, &item, &facts, 0).0);
        assert_eq!(
            e.judge_at_commit_reusing(&trail, &item, &facts, 0, at, true),
            full
        );
    }

    /// Whether the trail may rightly not hold for a case the property
    /// draws, the engine unchanged: an instant drawn between the two
    /// `now`s, or a rule that reads a date.
    fn trail_misses(s: &Setup, p: i64, c: i64) -> bool {
        let (lo, hi) = (p.min(c), p.max(c));
        let between = |t: &Option<i64>| t.is_some_and(|t| lo < t && t <= hi);
        let dated = |q: usize| QUERIES[q % QUERIES.len()].contains("discovered");
        s.rules
            .iter()
            .any(|r| between(&r.2) || dated(r.0) || r.3.is_some_and(dated))
            || s.overrides
                .iter()
                .any(|o| between(&o.2) || between(&o.3) || o.4.is_some_and(dated))
            || s.allow.is_some_and(dated)
            || s.exceptions.iter().any(|x| dated(x.1))
            || s.class.iter().any(|x| dated(x.0))
    }

    /// The trail is read in most cases the property draws (all but the
    /// floor's hash, a date read, or `now` across an instant), and those
    /// include the Adult reading's and SP-3's and SP-4's decisions, so the
    /// property tests the path it's about.
    #[test]
    fn the_property_reads_trails() {
        // A fixed seed: this test checks that the draws reach the paths the
        // property is about, so the same draws on every run keep its counts
        // steady. Unseeded, SP-3 and SP-4 sometimes fell under 10% (22 of
        // 250 on a Windows run).
        let mut runner = proptest::test_runner::TestRunner::new_with_rng(
            ProptestConfig::with_cases(600),
            proptest::test_runner::TestRng::deterministic_rng(
                proptest::test_runner::RngAlgorithm::ChaCha,
            ),
        );
        let counts = std::cell::Cell::new([0usize; 4]);
        let cases = (setup(), torrent(), 95i64..106, 95i64..106, any::<bool>());
        runner
            .run(&cases, |(s, (name, files, hash, _), p, c, adult)| {
                if hash == [9; 20] {
                    return Ok(());
                }
                let e = Handle::new(build(&s)).current();
                let class = lodestar_classify::classify(&name, &files);
                let hashes = [hash];
                let item = Item {
                    hashes: &hashes,
                    name: &name,
                    files: &files,
                    content_type: class.content_type,
                };
                let facts = |now| Facts {
                    class: &class,
                    size: 1,
                    piece_len: 1,
                    flags: 0,
                    discovered_at: p,
                    now,
                };
                let (_, trail) = prepare(&e, &item, &facts(p), p, adult);
                let at = AtCommit {
                    flood: Some(Level::Demote),
                    clone_of: Some(Original {
                        rule: PackRule::Sp2,
                        action: Action::Hide,
                    }),
                };
                let mut n = counts.get();
                n[0] += 1;
                if trail.holds_for(&e, &item, &facts(c), c) {
                    n[1] += 1;
                    let v = e.judge_at_commit_reusing(&trail, &item, &facts(c), c, at, adult);
                    let sp = |r| matches!(r, RuleRef::Pack(PackRule::Sp3 | PackRule::Sp4));
                    n[2] += usize::from(v.decision.is_some_and(|d| sp(d.rule)));
                    n[3] += usize::from(
                        adult && class.content_type != lodestar_classify::ContentType::Adult,
                    );
                }
                counts.set(n);
                Ok(())
            })
            .unwrap();
        let [all, read, sp, adult] = counts.get();
        assert!(
            read * 2 > all,
            "the trail was read in {read} of {all} cases"
        );
        assert!(sp * 10 > read, "SP-3 or SP-4 decided {sp} of {read}");
        assert!(
            adult * 4 > read,
            "the Adult reading replayed in {adult} of {read}"
        );
    }
}
