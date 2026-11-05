//! X-filter-perf-1105's differential proof (runner, 2026-11-05): the whole
//! engine's answers on every corpus and on 100,000 generated torrents,
//! digested and compared with the digests the same code gave on `main`
//! before the change (`origin/main` at 0531d024).
//!
//! Each torrent's record is everything the engine and its parts decide:
//! - the core's verdict;
//! - the engine's verdict and pack shadows, with every pack on and a rule
//!   set of every shape (keywords, phrases, fuzzy words, prefixes, NEAR,
//!   `title:`, tags, sizes, files, negations, `unless`, shadow mode), and
//!   again with common words neutralized;
//! - the rules the narrowing returns, and of them the ones that match;
//! - every pack rule's answer, plain and with words neutralized;
//! - the enrichment (tags, titles, numbers) the rules read;
//! - UC-3's allow list on the parsed title.
//!
//! Any change to any of them changes the digest. The digests are pinned
//! below: they were printed by this test run against `main`'s code (with
//! `LODESTAR_DIFF_PRINT=1`) and must not be edited to make it pass. This
//! test is removed in a follow-up once the change has merged.

use std::fmt::Write as _;

use lodestar_classify::{Classification, ContentType, FileInfo, classify};
use lodestar_search::DocEnrichment;

use crate::core::{Core, CsList, Item, Lists, Switches};
use crate::matcher::{Doc, Facts};
use crate::packs::{Ctx, PackRule, PackSwitches};
use crate::rules::{Match, Override, Strength, UserRule};
use crate::{Action, Engine};

/// FNV-1a over the records, so the digest doesn't depend on the
/// toolchain's hasher.
#[derive(Clone, Copy)]
struct Digest {
    h: u64,
    n: usize,
}

impl Digest {
    fn new() -> Digest {
        Digest {
            h: 0xcbf2_9ce4_8422_2325,
            n: 0,
        }
    }

    fn add(&mut self, record: &str) {
        for b in record.bytes().chain([0xff]) {
            self.h = (self.h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
        self.n += 1;
    }
}

/// Words the generated names, files and rules are drawn from: release
/// tokens, pack evidence (lures, scams, cracks, camcorder markers,
/// executables, notes), allow-list titles, and text the tokenizers treat
/// specially (joins, pluses, digits, case, non-ASCII letters, invisible
/// and look-alike characters).
const WORDS: &[&str] = &[
    "1080p",
    "720p",
    "2160p",
    "WEB-DL",
    "WEBRip",
    "BluRay",
    "HDTV",
    "x264",
    "x265",
    "H.264",
    "HEVC",
    "DDP5.1",
    "AAC2.0",
    "DTS-HD",
    "Atmos",
    "DD+",
    "C++",
    "S01",
    "E02",
    "S01E02",
    "2019",
    "1968",
    "2008",
    "GRP",
    "Sample",
    "sample",
    "trailer",
    "Subs",
    "en",
    "PROPER",
    "REPACK",
    "Movie",
    "Example",
    "Show",
    "Big",
    "Buck",
    "Bunny",
    "Night",
    "of",
    "the",
    "Living",
    "Dead",
    "Sintel",
    "Spring",
    "Hero",
    "Dr",
    "Caligari",
    "Cabinet",
    "CAM",
    "HDCAM",
    "TS",
    "HDTS",
    "TC",
    "SCR",
    "DVDSCR",
    "screener",
    "TELESYNC",
    "Crack",
    "cracked",
    "Keygen",
    "Patch",
    "Pre-Activated",
    "activator",
    "screen",
    "fix",
    "codec",
    "player",
    "setup",
    "install",
    "unlock",
    "password",
    "survey",
    "human",
    "verification",
    "how",
    "to",
    "open",
    "download",
    "from",
    "visit",
    "Netflix",
    "Spotify",
    "Steam",
    "Fortnite",
    "free",
    "generator",
    "hack",
    "trainer",
    "mod",
    "offline",
    "www.example.com",
    "[example.org]",
    "(site.net)",
    "xxx",
    "teen",
    "rstnl",
    "hdcmr",
    "lhdcmrst",
    "srtnlhdc",
    "webd",
    "blurx",
    "samplz",
    "İstanbul",
    "ΣΟΦΙΑ",
    "Straße",
    "Мастер",
    "東京",
    "café",
    "naïve",
    "\u{200b}hidden",
    "l33t",
    "h4ck3r",
    "@home",
    "$ale",
    "ＷＥＢ",
    "a",
    "b",
    "I",
    "II",
    "xYz",
    "eAC3",
    "aBcD",
    "v2.1",
    "x64",
    "",
    "Internet",
    "Archive",
];

const SEPARATORS: &[&str] = &[
    " ", ".", "_", "-", " - ", "  ", ".-.", "+", "/", "[", "]", "(", ")",
];

const EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "srt", "nfo", "txt", "url", "html", "pdf", "exe", "scr", "pif", "bat",
    "msi", "lnk", "apk", "jar", "js", "iso", "rar", "7z", "zip", "flac", "mp3", "jpg", "ts", "MKV",
    "EXE", "", "mkv.exe", "xml",
];

/// A small deterministic generator (splitmix64), so `main` and this
/// branch see the same torrents.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len())]
    }

    fn words(&mut self, max: usize) -> String {
        let n = 1 + self.below(max);
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push_str(self.pick(SEPARATORS));
            }
            let w = self.pick(WORDS);
            match self.below(8) {
                0 => s.push_str(&w.to_uppercase()),
                1 => s.push_str(&w.to_lowercase()),
                _ => s.push_str(w),
            }
        }
        s
    }
}

/// One generated torrent: a name and up to seven files in up to two
/// levels of folders, sometimes with Internet Archive item files.
fn generated(r: &mut Rng) -> (String, Vec<FileInfo>) {
    let name = r.words(12);
    let mut files = Vec::new();
    let folder = r.words(4);
    for _ in 0..r.below(8) {
        let mut path = Vec::new();
        for _ in 0..r.below(3) {
            path.push(if r.below(2) == 0 {
                folder.clone()
            } else {
                r.words(3)
            });
        }
        let ext = r.pick(EXTENSIONS);
        let stem = if r.below(4) == 0 {
            name.clone()
        } else {
            r.words(5)
        };
        let file = match r.below(20) {
            0 => format!("{stem}_meta.xml"),
            1 => format!("{stem}_files.xml"),
            _ if ext.is_empty() => stem,
            _ => format!("{stem}.{ext}"),
        };
        path.push(file);
        let size = [0u64, 900, 40_000, 2 << 20, 50 << 20, 700 << 20, 4 << 30][r.below(7)];
        let mut f = FileInfo::new(path, size + r.below(1000) as u64);
        f.padding = r.below(40) == 0;
        files.push(f);
    }
    (name, files)
}

/// Rules of every shape, on the generator's words, so some match.
fn rule_set() -> Vec<UserRule> {
    let mut r = Rng(0x5eed_0001);
    let mut queries: Vec<String> = Vec::new();
    let plain: Vec<String> = WORDS
        .iter()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| w.chars().count() >= 2)
        .collect();
    let p = |r: &mut Rng| plain[r.below(plain.len())].clone();
    for i in 0..400 {
        let q = match i % 16 {
            0 => p(&mut r),
            1 => format!("\"{} {}\"", p(&mut r), p(&mut r)),
            2 => format!("~{}", p(&mut r)),
            3 => {
                let w = p(&mut r);
                let mut c: Vec<char> = w.chars().collect();
                let at = r.below(c.len());
                c[at] = ['q', 'e', 'z', 'a'][r.below(4)];
                format!("~{}", c.into_iter().collect::<String>())
            }
            4 => {
                let w = p(&mut r);
                let n = w.chars().count().min(2 + r.below(3));
                format!("{}*", w.chars().take(n).collect::<String>())
            }
            5 => format!("{} NEAR/{} {}", p(&mut r), 1 + r.below(3), p(&mut r)),
            6 => format!("title:{}", p(&mut r)),
            7 => format!("{} -{}", p(&mut r), p(&mut r)),
            8 => format!("{} | {}", p(&mut r), p(&mut r)),
            9 => format!("group:{}", p(&mut r)),
            10 => format!(
                "{} type:{}",
                p(&mut r),
                ["movie", "tv", "software", "video", "music"][r.below(5)]
            ),
            11 => format!("{} size:>{}MB", p(&mut r), [1, 100, 1000][r.below(3)]),
            12 => format!("file:({} size:<{}MB)", p(&mut r), [1, 100][r.below(2)]),
            13 => format!("ext:{}", EXTENSIONS[r.below(EXTENSIONS.len() - 4)]),
            14 => format!("files:>{} {}", r.below(5), p(&mut r)),
            _ => format!("~{} {}", p(&mut r), p(&mut r)),
        };
        queries.push(q);
    }
    let mut out = Vec::new();
    for (i, q) in queries.iter().enumerate() {
        let Ok(query) = lodestar_search::parse(q) else {
            continue;
        };
        let unless = (i % 23 == 0)
            .then(|| lodestar_search::parse(&p(&mut r)).ok())
            .flatten();
        out.push(UserRule {
            id: i as u32 + 1,
            query,
            action: [Action::Flag, Action::Demote, Action::Hide, Action::Drop][i % 4],
            enabled: i % 31 != 0,
            shadow_until: (i % 17 == 0).then_some(i64::MAX),
            unless,
        });
    }
    assert!(out.len() > 300, "the rule set parses: {}", out.len());
    out
}

/// The engines the records read: every pack on and [`rule_set`], and
/// the same with common words neutralized.
fn engines() -> (Core, Engine, Engine) {
    let core = || {
        Core::new(
            Lists::shipped().unwrap(),
            CsList::default(),
            Switches::default(),
        )
    };
    let mut plain = Engine::new(core());
    plain.packs = PackSwitches::all();
    plain.rules = rule_set().into();
    let mut neutral = Engine::new(core());
    neutral.packs = PackSwitches::all();
    neutral.rules = rule_set().into();
    neutral.overrides = ["sample", "web", "crack", "cam", "survey", "mkv"]
        .iter()
        .zip(1..)
        .map(|(w, id)| Override {
            id,
            what: Match::Keyword((*w).into()),
            strength: Strength::Neutralize,
            scope: None,
            expires_at: None,
            shadow_until: None,
        })
        .collect();
    (core(), plain, neutral)
}

/// Everything decided about one torrent, as text.
fn record(
    (core, plain, neutral): &(Core, Engine, Engine),
    name: &str,
    files: &[FileInfo],
    content_type: Option<ContentType>,
) -> String {
    let class: Classification = classify(name, files);
    let item = Item {
        hashes: &[[7; 20]],
        name,
        files,
        content_type: content_type.unwrap_or(class.content_type),
    };
    let facts = Facts {
        class: &class,
        size: files.iter().map(|f| f.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: 0,
        now: 0,
    };
    let mut s = String::new();
    let _ = write!(s, "{:?}", core.verdict(&item));
    for e in [plain, neutral] {
        let _ = write!(s, "|{:?}", e.judge_shadowed(&item, Some(&facts), 0));
        let _ = write!(s, "|{:?}", e.judge(&item, None, 0));
    }
    let doc = Doc::new(&item, Some(&facts)).with_near(plain.rules.near_terms());
    let candidates = doc.candidates(&plain.rules, &[]);
    let matched: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|&i| {
            let (q, u) = plain.rules.compiled(i);
            doc.matches_compiled(q, &[]) && !u.is_some_and(|u| doc.matches_compiled(u, &[]))
        })
        .collect();
    let _ = write!(s, "|{candidates:?}|{matched:?}");
    let keys: Vec<String> = ["sample", "crack", "cam", "survey", "free"]
        .iter()
        .map(|w| crate::matcher::neutral_key(w))
        .collect();
    for neutral in [&[][..], &keys[..]] {
        let ctx = Ctx::new(&item, &facts, neutral);
        let hits: Vec<&str> = PackRule::ALL
            .into_iter()
            .filter(|r| ctx.matches(*r))
            .map(PackRule::id)
            .collect();
        let _ = write!(s, "|{hits:?}");
    }
    let record = lodestar_classify::Record::new(&class);
    let _ = write!(
        s,
        "|{:?}",
        DocEnrichment::new(&record, Some(&class.composition))
    );
    let r = &class.title.release;
    let _ = write!(
        s,
        "|{}",
        crate::packs::unauthorised_copy::allowed(&r.title, r.year.map(|y| y.first))
    );
    s
}

/// The release parser's golden corpus, by name, with a video file.
fn release_golden() -> Vec<(String, Vec<FileInfo>)> {
    const FILES: &[&str] = &[
        include_str!("../../release-parser/tests/golden/anime.txt"),
        include_str!("../../release-parser/tests/golden/books_other.txt"),
        include_str!("../../release-parser/tests/golden/holdout.txt"),
        include_str!("../../release-parser/tests/golden/movies.txt"),
        include_str!("../../release-parser/tests/golden/music.txt"),
        include_str!("../../release-parser/tests/golden/software_games.txt"),
        include_str!("../../release-parser/tests/golden/tv.txt"),
    ];
    let mut out = Vec::new();
    for text in FILES {
        for line in text.lines() {
            if line.trim().is_empty() || line.starts_with('#') || line.starts_with(' ') {
                continue;
            }
            out.push((line.to_owned(), Vec::new()));
            out.push((
                line.to_owned(),
                vec![FileInfo::new(vec![format!("{line}.mkv")], 2 << 30)],
            ));
        }
    }
    out
}

/// UC-3's allow list as scene releases would name it: each title in
/// three spellings, with its year, a year off and none, and as a film and
/// a show, so every entry is read whole and at its edges.
fn allow_names() -> Vec<(String, Vec<FileInfo>)> {
    let mut out = Vec::new();
    for (t, y) in crate::packs::unauthorised_copy::ALLOW {
        let dotted: String = t
            .split(' ')
            .map(|w| {
                let mut c = w.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            })
            .collect::<Vec<String>>()
            .join(".");
        let spellings = [dotted.clone(), t.to_uppercase(), format!("{t} extra")];
        let year = y.unwrap_or(2008);
        for title in spellings {
            for when in [Some(year), Some(year + 1), None] {
                let stem = match when {
                    Some(w) => format!("{title}.{w}"),
                    None => title.clone(),
                };
                for tail in [".1080p.BluRay.x264-GRP", ".S01E01.720p.WEB-DL.x264-GRP"] {
                    let name = format!("{stem}{tail}");
                    let files = vec![FileInfo::new(vec![format!("{name}.mkv")], 4 << 30)];
                    out.push((name, files));
                }
            }
        }
    }
    out
}

/// Each set's digest, in order: the pack corpora, the precision set, the
/// packs' golden set, the release parser's golden corpus, the allow
/// list's titles, and the generated torrents.
fn digests(generated_n: usize) -> Vec<(&'static str, usize, u64)> {
    let e = engines();
    let corpus = crate::corpus::Corpus::builtin().unwrap();
    let mut out = Vec::new();
    let mut d = Digest::new();
    for (_, _, cases) in &corpus.packs {
        for c in cases {
            let x = &c.example;
            d.add(&record(&e, &x.name, &x.files, x.content_type));
        }
    }
    out.push(("pack corpora", d.n, d.h));
    let mut d = Digest::new();
    for p in &corpus.precision {
        let x = &p.example;
        d.add(&record(&e, &x.name, &x.files, x.content_type));
    }
    out.push(("precision", d.n, d.h));
    let mut d = Digest::new();
    for (name, files) in crate::packs::manifest::tests::golden_set() {
        d.add(&record(&e, &name, &files, None));
    }
    out.push(("pack golden set", d.n, d.h));
    let mut d = Digest::new();
    for (name, files) in release_golden() {
        d.add(&record(&e, &name, &files, None));
    }
    out.push(("release-parser golden", d.n, d.h));
    let mut d = Digest::new();
    for (name, files) in allow_names() {
        d.add(&record(&e, &name, &files, None));
    }
    out.push(("allow list", d.n, d.h));
    let mut d = Digest::new();
    let mut r = Rng(0x0dd5_eed5);
    for _ in 0..generated_n {
        let (name, files) = generated(&mut r);
        d.add(&record(&e, &name, &files, None));
    }
    out.push(("generated", d.n, d.h));
    out
}

/// The digests `main` gave (0531d024), before X-filter-perf-1105.
const MAIN: &[(&str, usize, u64)] = &[
    ("pack corpora", 144, 17300323743453808400),
    ("precision", 35, 9256741853099181663),
    ("pack golden set", 3014, 16357010823402358329),
    ("release-parser golden", 634, 15136584467358822267),
    ("allow list", 684, 18416223319118281401),
    ("generated", 100000, 15644271538990749002),
];

#[test]
fn answers_are_mains() {
    let got = digests(100_000);
    if std::env::var_os("LODESTAR_DIFF_PRINT").is_some() {
        for g in &got {
            println!("{g:?},");
        }
    }
    assert_eq!(got, MAIN, "an answer differs from main's");
}
