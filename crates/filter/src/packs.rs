//! The built-in rule packs (§20.6): malware, lures, fakes, spam and
//! scams. Rules read the classifier's junk reasons and structure
//! fingerprints, so the packs and the junk score never disagree, and each
//! carries its false-positive guards.
//!
//! Not yet built, because they need data that doesn't exist yet: FK-3
//! (pre-release fakes need a TMDB release date), SP-3 (flooding needs
//! per-publisher rates) and SP-4 (clones of hidden spam need a signature
//! index of hidden items).
//!
//! A CSX-5 *Neutralize term* stops a word counting as evidence in the
//! rules that read keyword lists (MW-3, LU-2, LU-4, SC-1) and in LU-1's
//! lure notes (§20.5.5), as it does in the core and your rules. The words of a name or file the
//! rules count are read with the neutralized ones blanked, positions
//! kept ([`Ctx::evidence`]). The guards that keep a rule from matching
//! (FK-2's `trailer`/`sample`, SC-1's `trainer`/`mod`/`offline`) read the
//! torrent as it is, as `unless` does, so neutralizing never widens a
//! rule.

use std::collections::BTreeSet;

use lodestar_classify::ext::Ext;
use lodestar_classify::{Category, ContentType, FileInfo, JunkReason, Structure};

use crate::core::{Action, Item};
use crate::matcher::{Facts, neutral_token};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pack {
    Malware,
    Lures,
    Fakes,
    Spam,
    Scams,
}

impl Pack {
    pub const ALL: [Pack; 5] = [
        Pack::Malware,
        Pack::Lures,
        Pack::Fakes,
        Pack::Spam,
        Pack::Scams,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Pack::Malware => "malware",
            Pack::Lures => "lures",
            Pack::Fakes => "fakes",
            Pack::Spam => "spam",
            Pack::Scams => "scams",
        }
    }

    pub fn from_name(name: &str) -> Option<Pack> {
        Pack::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PackRule {
    Mw1,
    Mw2,
    Mw3,
    Mw4,
    Mw5,
    Lu1,
    Lu2,
    Lu3,
    Lu4,
    Fk1,
    Fk2,
    Fk4,
    Fk5,
    Sp1,
    Sp2,
    Sc1,
}

impl PackRule {
    /// In evaluation order: the strongest action within a pack first.
    pub const ALL: [PackRule; 16] = [
        PackRule::Mw4,
        PackRule::Mw1,
        PackRule::Mw2,
        PackRule::Mw3,
        PackRule::Mw5,
        PackRule::Lu1,
        PackRule::Lu4,
        PackRule::Lu2,
        PackRule::Lu3,
        PackRule::Fk4,
        PackRule::Fk1,
        PackRule::Fk5,
        PackRule::Fk2,
        PackRule::Sp2,
        PackRule::Sp1,
        PackRule::Sc1,
    ];

    pub fn id(self) -> &'static str {
        match self {
            PackRule::Mw1 => "MW-1",
            PackRule::Mw2 => "MW-2",
            PackRule::Mw3 => "MW-3",
            PackRule::Mw4 => "MW-4",
            PackRule::Mw5 => "MW-5",
            PackRule::Lu1 => "LU-1",
            PackRule::Lu2 => "LU-2",
            PackRule::Lu3 => "LU-3",
            PackRule::Lu4 => "LU-4",
            PackRule::Fk1 => "FK-1",
            PackRule::Fk2 => "FK-2",
            PackRule::Fk4 => "FK-4",
            PackRule::Fk5 => "FK-5",
            PackRule::Sp1 => "SP-1",
            PackRule::Sp2 => "SP-2",
            PackRule::Sc1 => "SC-1",
        }
    }

    pub fn from_id(id: &str) -> Option<PackRule> {
        PackRule::ALL.into_iter().find(|r| r.id() == id)
    }

    pub fn pack(self) -> Pack {
        use PackRule::*;
        match self {
            Mw1 | Mw2 | Mw3 | Mw4 | Mw5 => Pack::Malware,
            Lu1 | Lu2 | Lu3 | Lu4 => Pack::Lures,
            Fk1 | Fk2 | Fk4 | Fk5 => Pack::Fakes,
            Sp1 | Sp2 => Pack::Spam,
            Sc1 => Pack::Scams,
        }
    }

    pub fn action(self) -> Action {
        use PackRule::*;
        match self {
            Mw4 | Fk4 => Action::Drop,
            Mw1 | Mw2 | Mw3 | Mw5 | Lu1 | Lu4 | Fk1 | Sp2 | Sc1 => Action::Hide,
            Lu2 | Lu3 | Fk5 | Sp1 => Action::Demote,
            Fk2 => Action::Flag,
        }
    }
}

/// Which packs are on, and which of their rules you switched off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackSwitches {
    pub on: BTreeSet<Pack>,
    pub rules_off: BTreeSet<PackRule>,
}

impl PackSwitches {
    pub fn all() -> PackSwitches {
        PackSwitches {
            on: Pack::ALL.into_iter().collect(),
            rules_off: BTreeSet::new(),
        }
    }

    pub fn is_on(&self, rule: PackRule) -> bool {
        self.on.contains(&rule.pack()) && !self.rules_off.contains(&rule)
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn ext(f: &FileInfo) -> Option<Ext> {
    lodestar_classify::ext::extension(f.name())
}

const EXECUTABLE: &[&str] = &[
    "exe", "scr", "pif", "bat", "cmd", "vbs", "vbe", "js", "jse", "wsf", "hta", "ps1", "msi",
    "lnk", "jar", "apk",
];

const LURE_NAMES: &[&str] = &["codec", "player", "setup", "install", "installer", "unlock"];

const UNLOCK_WORDS: &[&str] = &["unlock", "survey", "verification", "verify"];

/// LU-4's lures: a survey, "human verification", or "unlock" with
/// "password".
const SURVEY: &str = "survey";
const HUMAN_VERIFICATION: [&str; 2] = ["human", "verification"];
const UNLOCK_PASSWORD: [&str; 2] = ["unlock", "password"];

const HOW_TO: &[&[&str]] = &[
    &["how", "to", "open"],
    &["how", "to", "download"],
    &["how", "to", "extract"],
    &["visit"],
    &["download", "from"],
];

/// Online services and games whose "free premium" or currency
/// generators are scams (SC-1/SC-2).
const SERVICES: &[&str] = &[
    "spotify", "netflix", "robux", "vbucks", "roblox", "fortnite", "disney", "hbo", "deezer",
    "tidal", "onlyfans", "bitcoin", "wallet", "crypto", "paypal",
];

const SCAM_WORDS: &[&str] = &[
    "generator",
    "hack",
    "free",
    "premium",
    "unlocked",
    "activator",
    "recovery",
    "adder",
];

/// Media types for the malware guards.
fn media(t: ContentType) -> bool {
    matches!(
        t,
        ContentType::Movie
            | ContentType::Tv
            | ContentType::Music
            | ContentType::Video
            | ContentType::Adult
    )
}

/// One torrent's pack inputs, computed once for all the pack rules:
/// extensions per file, and words only for the few files rules read.
pub struct Ctx<'a> {
    item: &'a Item<'a>,
    facts: &'a Facts<'a>,
    /// The keys of the torrent's neutralized words
    /// ([`crate::matcher::neutral_key`]).
    neutral: &'a [String],
    /// Content files with their lowercase extensions.
    files: Vec<(&'a FileInfo, Option<Ext>)>,
    name_words: std::cell::OnceCell<Vec<String>>,
    name_evidence: std::cell::OnceCell<Vec<String>>,
}

impl std::fmt::Debug for Ctx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx")
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

/// Small text files a lure or "how to open" note comes as.
const NOTES: &[&str] = &["txt", "url", "html", "htm", "pdf", "nfo"];

impl<'a> Ctx<'a> {
    /// `neutral` is the keys of the Neutralize entries that apply to the
    /// torrent, as the core and your rules get them.
    pub fn new(item: &'a Item<'a>, facts: &'a Facts<'a>, neutral: &'a [String]) -> Ctx<'a> {
        let files = item
            .files
            .iter()
            .take(crate::MAX_FILES)
            .filter(|x| !x.padding)
            .map(|x| (x, ext(x)))
            .collect();
        Ctx {
            item,
            facts,
            neutral,
            files,
            name_words: std::cell::OnceCell::new(),
            name_evidence: std::cell::OnceCell::new(),
        }
    }

    /// The name's words as they are, for guards.
    fn name_words(&self) -> &[String] {
        self.name_words.get_or_init(|| words(self.item.name))
    }

    /// The name's words that count as evidence ([`Ctx::evidence`]).
    fn name_evidence(&self) -> &[String] {
        if self.neutral.is_empty() {
            return self.name_words();
        }
        self.name_evidence
            .get_or_init(|| self.evidence(self.name_words().to_vec()))
    }

    /// A text's words with each neutralized word blanked, positions kept,
    /// so it can't count on its own or as part of a phrase (§20.5.5).
    /// A word is a whole run of letters and digits, so a joined word
    /// (`codecsetup`) is its own word and never lends a neutralized one.
    /// Every keyword the packs read is lowercase ASCII letters
    /// (`keywords_are_plain_words`), and such a word is its own
    /// [`crate::matcher::neutral_key`]; another word can't be evidence,
    /// so it's left as it is.
    fn evidence(&self, mut words: Vec<String>) -> Vec<String> {
        if !self.neutral.is_empty() {
            for w in &mut words {
                if w.bytes().all(|b| b.is_ascii_lowercase()) && neutral_token(w, self.neutral) {
                    w.clear();
                }
            }
        }
        words
    }

    /// A file name's words that count as evidence.
    fn file_evidence(&self, f: &FileInfo) -> Vec<String> {
        self.evidence(words(f.name()))
    }

    /// Whether a lure note still counts for LU-1. With nothing
    /// neutralized, the classifier's `lure-file` reason is the evidence as
    /// it is. Otherwise some lure text a file holds
    /// ([`lodestar_classify::lure_hits`]) must have no neutralized word
    /// over it: a phrase (`click here`) is broken where a neutralized word
    /// stands, as in [`Ctx::evidence`]. Words are whole, so a lure inside
    /// a joined word (`MyPassword`) is broken only by neutralizing that
    /// whole word, never by `password`. Every file is read, not only the
    /// first [`crate::MAX_FILES`], because the classifier reads them all.
    fn live_lure(&self) -> bool {
        if self.neutral.is_empty() {
            return true;
        }
        self.item.files.iter().any(|f| {
            let mut hits = lodestar_classify::lure_hits(f).peekable();
            if hits.peek().is_none() {
                return false;
            }
            let name = f.name();
            let low = name.to_ascii_lowercase();
            let blanked = self.neutral_spans(name);
            // No lure overlaps itself, so non-overlapping finds see them all.
            hits.any(|l| {
                low.match_indices(l).any(|(at, _)| {
                    let end = at + l.len();
                    !blanked.iter().any(|&(s, e)| s < end && at < e)
                })
            })
        })
    }

    /// The byte spans of a text's neutralized words, each a whole run of
    /// letters and digits as [`words`] reads them.
    fn neutral_spans(&self, text: &str) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
            if c.is_alphanumeric() {
                start.get_or_insert(i);
            } else if let Some(s) = start.take()
                && neutral_token(&text[s..i].to_lowercase(), self.neutral)
            {
                spans.push((s, i));
            }
        }
        spans
    }

    fn with_ext<'s>(&'s self, set: &'s [&str]) -> impl Iterator<Item = &'s FileInfo> + 's {
        self.files
            .iter()
            .filter(move |(_, e)| e.is_some_and(|e| set.contains(&e.as_str())))
            .map(|(f, _)| *f)
    }

    /// Whether a pack rule matches.
    pub fn matches(&self, rule: PackRule) -> bool {
        use PackRule::*;
        let f = self.facts;
        let junk = |r: JunkReason| f.class.junk.reasons.contains(&r);
        let structure = f.class.structure;
        let t = f.class.content_type;
        let disc = structure.contains(Structure::BLURAY) || structure.contains(Structure::DVD);
        let scene = structure.contains(Structure::SFV) || structure.contains(Structure::NFO);
        let software = matches!(t, ContentType::Software | ContentType::Game);
        let pct = |c: Category| f.class.composition.pct_bytes(c);
        match rule {
            Mw1 => junk(JunkReason::DoubleExtension),
            Mw2 => junk(JunkReason::Executable) && media(t) && !software && !disc,
            Mw3 => {
                matches!(t, ContentType::Movie | ContentType::Tv | ContentType::Video)
                    && !disc
                    && self.with_ext(EXECUTABLE).any(|x| {
                        self.file_evidence(x)
                            .iter()
                            .any(|w| LURE_NAMES.contains(&w.as_str()))
                    })
            }
            Mw4 => junk(JunkReason::TinyVideo) && self.with_ext(EXECUTABLE).next().is_some(),
            Mw5 => !software && self.with_ext(&["scr", "pif"]).next().is_some(),
            Lu1 => {
                junk(JunkReason::LureFile) && junk(JunkReason::ArchiveForMedia) && self.live_lure()
            }
            Lu2 => {
                let real_media = pct(Category::Video) > 0 || pct(Category::Audio) > 0;
                !real_media
                    && pct(Category::Archive) > 50
                    && self.with_ext(NOTES).any(|x| {
                        let w = self.file_evidence(x);
                        HOW_TO.iter().any(|p| w.windows(p.len()).any(|s| s == *p))
                    })
            }
            Lu3 => junk(JunkReason::WebShortcut) && media(t),
            Lu4 => self.with_ext(NOTES).any(|x| {
                let w = self.file_evidence(x);
                w.iter().any(|w| UNLOCK_WORDS.contains(&w.as_str()))
                    && (w.iter().any(|w| w == SURVEY)
                        || w.windows(2).any(|p| p == HUMAN_VERIFICATION)
                        || UNLOCK_PASSWORD.iter().all(|u| w.iter().any(|w| w == u)))
            }),
            Fk1 => junk(JunkReason::NameMismatch) && !disc && !scene,
            // Its words are a guard only: they read as they are.
            Fk2 => {
                junk(JunkReason::TinyVideo)
                    && !structure.contains(Structure::SAMPLE)
                    && !self
                        .name_words()
                        .iter()
                        .any(|w| w == "trailer" || w == "sample")
            }
            Fk4 => junk(JunkReason::Empty) || junk(JunkReason::PaddingOnly),
            Fk5 => {
                junk(JunkReason::ArchiveForMedia)
                    && !scene
                    && !structure.contains(Structure::MULTIPART)
            }
            Sp1 => site_advert(self.item.name),
            Sp2 => stuffed(self.item.name) && !matches!(t, ContentType::Music),
            Sc1 => {
                let w = self.name_evidence();
                w.iter().any(|x| SERVICES.contains(&x.as_str()))
                    && w.iter().any(|x| SCAM_WORDS.contains(&x.as_str()))
                    && !self
                        .name_words()
                        .iter()
                        .any(|x| x == "trainer" || x == "mod" || x == "offline")
            }
        }
    }
}

/// `www.site.com - Title`, `[site.org] Title` or `Title [site.net]`.
fn site_advert(name: &str) -> bool {
    let lower = name.to_lowercase();
    if lower.starts_with("www.") {
        return true;
    }
    let bracketed = |open: char, close: char| {
        lower
            .split(open)
            .skip(1)
            .filter_map(|s| s.split(close).next())
            .any(|inner| {
                let inner = inner.trim();
                let mut parts = inner.rsplitn(2, '.');
                let tld = parts.next().unwrap_or("");
                let host = parts.next().unwrap_or("");
                !host.is_empty()
                    && !inner.contains(' ')
                    && matches!(
                        tld,
                        "com"
                            | "org"
                            | "net"
                            | "info"
                            | "to"
                            | "cc"
                            | "io"
                            | "me"
                            | "tv"
                            | "xyz"
                            | "ru"
                            | "in"
                            | "se"
                            | "ws"
                            | "ch"
                            | "is"
                            | "biz"
                    )
            })
    };
    bracketed('[', ']') || bracketed('(', ')')
}

/// Over 300 characters, with most words repeated: a keyword-stuffed name.
fn stuffed(name: &str) -> bool {
    if name.chars().count() <= 300 {
        return false;
    }
    let w = words(name);
    let distinct: std::collections::HashSet<&String> = w.iter().collect();
    !w.is_empty() && distinct.len() * 3 < w.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::classify;

    fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
        paths
            .iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
            .collect()
    }

    /// The pack rules that match, classified from `class_of` and read
    /// from `name` and `f`, with `neutral` neutralized.
    fn hit_as(
        class_of: (&str, &[FileInfo]),
        name: &str,
        f: &[FileInfo],
        neutral: &[&str],
    ) -> Vec<&'static str> {
        let class = classify(class_of.0, class_of.1);
        let facts = Facts {
            class: &class,
            size: 0,
            piece_len: 0,
            flags: 0,
            discovered_at: 0,
            now: 0,
        };
        let item = Item {
            hashes: &[[0; 20]],
            name,
            files: f,
            content_type: class.content_type,
        };
        let neutral = keys(neutral);
        let ctx = Ctx::new(&item, &facts, &neutral);
        PackRule::ALL
            .into_iter()
            .filter(|r| ctx.matches(*r))
            .map(PackRule::id)
            .collect()
    }

    /// The keys a Neutralize entry for each word supplies, as
    /// `rules::neutral_sets` keeps them.
    fn keys(words: &[&str]) -> Vec<String> {
        words
            .iter()
            .map(|w| crate::matcher::neutral_key(w))
            .filter(|k| !k.is_empty())
            .collect()
    }

    fn hit_with(name: &str, paths: &[(&str, u64)], neutral: &[&str]) -> Vec<&'static str> {
        let f = files(paths);
        hit_as((name, &f), name, &f, neutral)
    }

    fn hit(name: &str, paths: &[(&str, u64)]) -> Vec<&'static str> {
        hit_with(name, paths, &[])
    }

    #[test]
    fn ids_and_packs() {
        for r in PackRule::ALL {
            assert_eq!(PackRule::from_id(r.id()), Some(r));
            assert!(r.id().starts_with(&r.pack().as_str()[..1].to_uppercase()));
        }
        let s = PackSwitches::all();
        assert!(s.is_on(PackRule::Mw1));
        assert!(!PackSwitches::default().is_on(PackRule::Mw1));
    }

    #[test]
    fn malware() {
        assert!(
            hit(
                "Movie.2019.1080p",
                &[("Movie.2019.1080p.mp4.exe", 2_000_000)]
            )
            .contains(&"MW-1")
        );
        let codec = hit(
            "Movie.2019.1080p.WEB",
            &[
                ("Movie.2019.1080p.WEB.mkv", 4 << 30),
                ("Codec Setup.exe", 900_000),
            ],
        );
        assert!(
            codec.contains(&"MW-2") && codec.contains(&"MW-3"),
            "{codec:?}"
        );
        // Guards: software is allowed executables; disc layouts carry players.
        assert!(
            hit(
                "Tool v1.2 x64",
                &[("setup.exe", 50 << 20), ("readme.txt", 10)]
            )
            .is_empty()
        );
        let disc = hit(
            "Movie 2019 BluRay",
            &[
                ("BDMV/STREAM/00001.m2ts", 20 << 30),
                ("BDMV/index.bdmv", 1000),
                ("PC/player.exe", 1 << 20),
            ],
        );
        assert!(
            !disc.contains(&"MW-2") && !disc.contains(&"MW-3"),
            "{disc:?}"
        );
    }

    #[test]
    fn fakes_and_spam() {
        assert!(hit("Empty", &[("pad", 0)]).contains(&"FK-4"));
        assert!(
            hit(
                "www.example.com - Movie 2019 1080p",
                &[("Movie.2019.1080p.mkv", 4 << 30)]
            )
            .contains(&"SP-1")
        );
        assert!(
            hit(
                "Movie 2019 1080p [example.org]",
                &[("Movie.2019.1080p.mkv", 4 << 30)]
            )
            .contains(&"SP-1")
        );
        assert!(
            !hit(
                "Movie 2019 [Director's Cut] (1080p)",
                &[("Movie.2019.1080p.mkv", 4 << 30)]
            )
            .contains(&"SP-1")
        );
        let stuffed = "free movie download ".repeat(20);
        assert!(hit(&stuffed, &[("a.mkv", 1 << 30)]).contains(&"SP-2"));
        assert!(hit("Free Robux Generator 2024", &[("generator.exe", 1 << 20)]).contains(&"SC-1"));
        assert!(
            !hit("Fortnite Trainer Offline Mod", &[("trainer.exe", 1 << 20)]).contains(&"SC-1")
        );
        // A plain release passes every pack.
        assert!(
            hit(
                "The.Matrix.1999.1080p.BluRay.x264-GRP",
                &[
                    ("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 << 30),
                    ("grp.nfo", 4000)
                ]
            )
            .is_empty()
        );
    }

    // -----------------------------------------------------------------
    // CSX-5 *Neutralize term* (§20.5.5).

    #[test]
    fn keywords_are_plain_words() {
        // `Ctx::evidence` relies on it.
        let all = LURE_NAMES
            .iter()
            .chain(UNLOCK_WORDS)
            .chain(HOW_TO.iter().flat_map(|p| p.iter()))
            .chain(SERVICES)
            .chain(SCAM_WORDS)
            .chain(&[SURVEY])
            .chain(&HUMAN_VERIFICATION)
            .chain(&UNLOCK_PASSWORD);
        for w in all {
            assert!(w.bytes().all(|b| b.is_ascii_lowercase()), "{w}");
            assert_eq!(crate::matcher::neutral_key(w), *w);
        }
    }

    /// A rule, a torrent's name and files, and words to neutralize.
    type Case<W> = (&'static str, &'static str, Vec<(&'static str, u64)>, W);

    /// Each rule's `(rule, name, files)` that matches, and words that are
    /// its sole evidence.
    #[test]
    fn a_neutralized_word_is_no_evidence() {
        let movie = ("Movie.2019.1080p.WEB.mkv", 4u64 << 30);
        let archive = ("Movie.2019.1080p.rar", 4u64 << 30);
        let cases: &[Case<&str>] = &[
            (
                "MW-3",
                "Movie.2019.1080p.WEB",
                vec![movie, ("Codec.exe", 9_000)],
                "codec",
            ),
            (
                "MW-3",
                "Movie.2019.1080p.WEB",
                vec![movie, ("Setup.exe", 9_000)],
                "Setup",
            ),
            (
                "LU-2",
                "Movie 2019 1080p",
                vec![archive, ("How to open.txt", 90)],
                "open",
            ),
            (
                "LU-2",
                "Movie 2019 1080p",
                vec![archive, ("Download from.url", 90)],
                "from",
            ),
            (
                "LU-4",
                "Movie 2019",
                vec![movie, ("Unlock password.txt", 90)],
                "password",
            ),
            (
                "LU-4",
                "Movie 2019",
                vec![movie, ("Human verification.txt", 90)],
                "human",
            ),
            (
                "LU-4",
                "Movie 2019",
                vec![movie, ("Survey.txt", 90)],
                "survey",
            ),
            ("SC-1", "Free Robux", vec![("robux.exe", 1 << 20)], "robux"),
            ("SC-1", "Free Robux", vec![("robux.exe", 1 << 20)], "FREE"),
        ];
        for (rule, name, paths, word) in cases {
            assert!(hit(name, paths).contains(rule), "{rule} {name} {paths:?}");
            let without = hit_with(name, paths, &[word]);
            assert!(!without.contains(rule), "{rule} {word}: {without:?}");
            // Neutralizing a word that isn't there changes nothing.
            assert_eq!(hit_with(name, paths, &["beacon"]), hit(name, paths));
        }
    }

    #[test]
    fn other_evidence_still_counts() {
        let movie = ("Movie.2019.1080p.WEB.mkv", 4u64 << 30);
        let archive = ("Movie.2019.1080p.rar", 4u64 << 30);
        let cases: &[Case<&[&str]>] = &[
            // Another lure name, in the same file or another.
            (
                "MW-3",
                "Movie.2019.1080p.WEB",
                vec![movie, ("Codec Setup.exe", 9_000)],
                &["codec"],
            ),
            (
                "MW-3",
                "Movie.2019.1080p.WEB",
                vec![movie, ("Codec.exe", 9_000), ("Player.exe", 9_000)],
                &["codec"],
            ),
            // Another note, or another phrase in the same one.
            (
                "LU-2",
                "Movie 2019 1080p",
                vec![archive, ("How to open.txt", 90), ("Visit.url", 90)],
                &["open"],
            ),
            (
                "LU-2",
                "Movie 2019 1080p",
                vec![archive, ("How to open or how to extract.txt", 90)],
                &["open"],
            ),
            (
                "LU-4",
                "Movie 2019",
                vec![movie, ("Unlock survey.txt", 90)],
                &["unlock"],
            ),
            (
                "LU-4",
                "Movie 2019",
                vec![movie, ("Verify unlock password.txt", 90)],
                &["verify"],
            ),
            // Another service or scam word.
            (
                "SC-1",
                "Free Robux Generator",
                vec![("a.exe", 1 << 20)],
                &["free"],
            ),
            (
                "SC-1",
                "Free Robux Spotify",
                vec![("a.exe", 1 << 20)],
                &["robux"],
            ),
        ];
        for (rule, name, paths, words) in cases {
            let with = hit_with(name, paths, words);
            assert!(with.contains(rule), "{rule} {words:?}: {with:?}");
        }
        // Rules that don't read keywords are untouched: MW-2 still hides
        // the executable whose lure names are neutralized.
        let codec = hit_with(
            "Movie.2019.1080p.WEB",
            &[movie, ("Codec Setup.exe", 9_000)],
            &["codec", "setup"],
        );
        assert!(
            codec.contains(&"MW-2") && !codec.contains(&"MW-3"),
            "{codec:?}"
        );
    }

    /// A word is a whole run of letters and digits: a joined word never
    /// lends a neutralized one, and a neutralized joined word leaves its
    /// parts written apart. A phrase with a neutralized word is broken
    /// where it stands, but the same phrase elsewhere still counts.
    #[test]
    fn words_are_whole() {
        let archive = ("Movie.2019.1080p.rar", 4u64 << 30);
        let movie = ("Movie.2019.1080p.WEB.mkv", 4u64 << 30);
        // `codecsetup` never was `codec` or `setup`.
        let joined = [movie, ("CodecSetup.exe", 9_000)];
        assert!(!hit("Movie.2019.1080p.WEB", &joined).contains(&"MW-3"));
        // Neutralizing `codecsetup` leaves `Codec-Setup`'s words.
        let apart = [movie, ("Codec-Setup.exe", 9_000)];
        assert!(hit_with("Movie.2019.1080p.WEB", &apart, &["CodecSetup"]).contains(&"MW-3"));
        // `to` neutralized breaks every "how to …" phrase; `visit`
        // elsewhere in the note still counts.
        let notes = [archive, ("How to open.txt", 90), ("How to extract.txt", 90)];
        assert!(!hit_with("Movie 2019 1080p", &notes, &["to"]).contains(&"LU-2"));
        let visit = [archive, ("How to open, visit.txt", 90)];
        assert!(hit_with("Movie 2019 1080p", &visit, &["to"]).contains(&"LU-2"));
        // A blanked word keeps its place: `how open` isn't made of
        // `how to open` with `to` gone.
        let gap = [archive, ("How x open.txt", 90)];
        assert!(!hit("Movie 2019 1080p", &gap).contains(&"LU-2"));
    }

    /// LU-1 reads the classifier's lure notes, with a neutralized word
    /// broken where it stands, as the keyword rules do.
    #[test]
    fn lure_notes_honour_neutralize() {
        let archive = ("Movie.2019.1080p.rar", 4u64 << 30);
        let lu1 = |notes: &[(&str, u64)], neutral: &[&str]| {
            let mut paths = vec![archive];
            paths.extend_from_slice(notes);
            hit_with("Movie 2019 1080p", &paths, neutral).contains(&"LU-1")
        };
        let password = ("PASSWORD.txt", 90);
        let click = ("Click here.url", 90);
        assert!(lu1(&[password], &[]));
        // The sole lure word, neutralized, is no evidence.
        assert!(!lu1(&[password], &["password"]));
        // Another lure note still counts.
        assert!(lu1(&[password, click], &["password"]));
        // A phrase is broken by either of its words, and only by its words.
        assert!(!lu1(&[click], &["click"]));
        assert!(!lu1(&[click], &["HERE"]));
        assert!(lu1(&[click], &["clicks"]));
        assert!(lu1(&[("Click here, or click here now.url", 90)], &["now"]));
        // Words are whole: `pass` isn't `password`, and a lure inside a
        // joined word stays evidence until that whole word is neutralized.
        assert!(lu1(&[password], &["pass"]));
        let joined = ("MyPassword.txt", 90);
        assert!(lu1(&[joined], &[]));
        assert!(lu1(&[joined], &["password"]));
        assert!(!lu1(&[joined], &["MyPassword"]));
        // The note's size is the classifier's: a large file is no note.
        assert!(!lu1(&[("password.txt", 64 * 1024)], &[]));
        // An archive standing in for media is still needed.
        let movie = [("Movie.2019.1080p.mkv", 4u64 << 30), password];
        assert!(!hit_with("Movie 2019 1080p", &movie, &[]).contains(&"LU-1"));
        assert!(!hit_with("Movie 2019 1080p", &movie, &["password"]).contains(&"LU-1"));
        // A lure past the files the other rules read still counts.
        let mut many: Vec<(&str, u64)> = vec![("filler.rar", 1); crate::MAX_FILES];
        many.push(password);
        many.push(click);
        assert!(lu1(&many, &["password"]));
        assert!(!lu1(&many, &["password", "click"]));
    }

    /// Guards read the torrent as it is: neutralizing a guard word never
    /// makes a rule match.
    #[test]
    fn guards_are_never_neutralized() {
        // A tiny file that isn't named a sample, so the name's guard is
        // what keeps FK-2 off.
        let tiny = [("Movie.2019.1080p.mkv", 2_000_000u64)];
        assert!(hit("Movie 2019 1080p", &tiny).contains(&"FK-2"));
        for (name, word) in [
            ("Movie 2019 1080p Sample", "sample"),
            ("Movie 2019 1080p Trailer", "trailer"),
        ] {
            assert!(!hit(name, &tiny).contains(&"FK-2"), "{name}");
            assert!(!hit_with(name, &tiny, &[word]).contains(&"FK-2"), "{name}");
        }
        for word in ["trainer", "mod", "offline"] {
            let name = format!("Free Robux Generator {word}");
            let f = [("a.exe", 1u64 << 20)];
            assert!(!hit(&name, &f).contains(&"SC-1"), "{name}");
            assert!(!hit_with(&name, &f, &[word]).contains(&"SC-1"), "{name}");
        }
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// Pack keywords, guards, extensions, a joined word, a look-alike
        /// and filler, in assorted cases.
        const VOCAB: &[&str] = &[
            "codec",
            "Setup",
            "PLAYER",
            "unlock",
            "survey",
            "human",
            "verification",
            "verify",
            "password",
            "how",
            "to",
            "open",
            "extract",
            "download",
            "from",
            "visit",
            "free",
            "Robux",
            "spotify",
            "generator",
            "hack",
            "trainer",
            "mod",
            "offline",
            "sample",
            "trailer",
            "click",
            "here",
            "www",
            "passwd",
            "MyPassword",
            "exe",
            "txt",
            "CodecSetup",
            "codec2",
            "\u{ff23}odec",
            "movie",
            "2019",
            "1080p",
            "x265",
            "beacon",
        ];
        const EXTS: &[&str] = &["exe", "txt", "url", "nfo", "rar", "mkv", "mp4"];
        const SIZES: &[u64] = &[90, 2_000_000, 50 << 20, 4 << 30];

        /// Words a name may start with, so guarded and caught names come
        /// up often: an HD name for FK-2, with and without its guard, and
        /// SC-1's evidence, with and without its guard.
        const LEADS: &[&[&str]] = &[
            &[],
            &["movie", "1080p"],
            &["movie", "1080p", "sample"],
            &["movie", "1080p", "Trailer"],
            &["free", "robux"],
            &["Free", "Robux", "offline"],
        ];

        fn text(n: std::ops::Range<usize>) -> impl Strategy<Value = Vec<&'static str>> {
            prop::collection::vec(prop::sample::select(VOCAB), n)
        }

        /// The words as a text, with `masked` words (by key) replaced by
        /// a word no rule reads, in place.
        fn joined(words: &[&str], sep: &str, masked: &[String]) -> String {
            words
                .iter()
                .map(|w| {
                    if masked.contains(&crate::matcher::neutral_key(w)) {
                        "qqq"
                    } else {
                        w
                    }
                })
                .collect::<Vec<_>>()
                .join(sep)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2000))]

            /// Neutralizing only takes away matches whose sole evidence
            /// was a neutralized word. It's the same as reading the
            /// torrent with those words replaced by a word no rule reads
            /// (positions kept, the classification left as it was), and
            /// as it is, for guards: a rule matches with neutralizing
            /// exactly when it matches both ways.
            #[test]
            fn neutralizing_removes_only_sole_evidence(
                lead in prop::sample::select(LEADS),
                rest in text(0..5),
                files in prop::collection::vec(
                    (text(1..5), prop::sample::select(EXTS), prop::sample::select(SIZES)),
                    1..5,
                ),
                neutral in prop::sample::subsequence(VOCAB.to_vec(), 0..5),
            ) {
                let name: Vec<&str> = lead.iter().copied().chain(rest).collect();
                let keys = super::keys(&neutral);
                let path = |(w, e, s): &(Vec<&str>, &str, u64), m: &[String]| {
                    (format!("{}.{e}", joined(w, " ", m)), *s)
                };
                let name_as_is = joined(&name, ".", &[]);
                let f = files.iter().map(|x| path(x, &[])).collect::<Vec<_>>();
                let f = super::files(&f.iter().map(|(p, s)| (p.as_str(), *s)).collect::<Vec<_>>());
                let name_masked = joined(&name, ".", &keys);
                let m = files.iter().map(|x| path(x, &keys)).collect::<Vec<_>>();
                let m = super::files(&m.iter().map(|(p, s)| (p.as_str(), *s)).collect::<Vec<_>>());
                let class_of = (name_as_is.as_str(), &f[..]);
                let neutralized = hit_as(class_of, &name_as_is, &f, &neutral);
                let as_is = hit_as(class_of, &name_as_is, &f, &[]);
                // A neutral word no torrent holds: LU-1 then reads the
                // masked notes instead of the classifier's reason, which
                // is of the torrent as it is.
                let masked = hit_as(class_of, &name_masked, &m, &["zzzz"]);
                let both: Vec<_> = as_is.iter().copied().filter(|r| masked.contains(r)).collect();
                prop_assert_eq!(&neutralized, &both);
            }
        }
    }
}
