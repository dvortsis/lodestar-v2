//! The built-in rule packs (§20.6): malware, lures, fakes, spam and
//! scams. Rules read the classifier's junk reasons and structure
//! fingerprints, so the packs and the junk score never disagree, and each
//! carries its false-positive guards.
//!
//! Not yet built, because they need data that doesn't exist yet: FK-3
//! (pre-release fakes need a TMDB release date), SP-3 (flooding needs
//! per-publisher rates) and SP-4 (clones of hidden spam need a signature
//! index of hidden items).

use std::collections::BTreeSet;

use lodestar_classify::{Category, ContentType, FileInfo, JunkReason, Structure};

use crate::core::{Action, Item};
use crate::matcher::Facts;

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

fn ext(f: &FileInfo) -> String {
    lodestar_classify::ext::extension(f.name()).map_or_else(String::new, |e| e.as_str().to_owned())
}

const EXECUTABLE: &[&str] = &[
    "exe", "scr", "pif", "bat", "cmd", "vbs", "vbe", "js", "jse", "wsf", "hta", "ps1", "msi",
    "lnk", "jar", "apk",
];

const LURE_NAMES: &[&str] = &["codec", "player", "setup", "install", "installer", "unlock"];

const UNLOCK_WORDS: &[&str] = &["unlock", "survey", "verification", "verify"];

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
    /// Content files with their lowercase extensions.
    files: Vec<(&'a FileInfo, String)>,
    name_words: std::cell::OnceCell<Vec<String>>,
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
    pub fn new(item: &'a Item<'a>, facts: &'a Facts<'a>) -> Ctx<'a> {
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
            files,
            name_words: std::cell::OnceCell::new(),
        }
    }

    fn name_words(&self) -> &[String] {
        self.name_words.get_or_init(|| words(self.item.name))
    }

    fn with_ext<'s>(&'s self, set: &'s [&str]) -> impl Iterator<Item = &'s FileInfo> + 's {
        self.files
            .iter()
            .filter(move |(_, e)| set.contains(&e.as_str()))
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
                        words(x.name())
                            .iter()
                            .any(|w| LURE_NAMES.contains(&w.as_str()))
                    })
            }
            Mw4 => junk(JunkReason::TinyVideo) && self.with_ext(EXECUTABLE).next().is_some(),
            Mw5 => !software && self.with_ext(&["scr", "pif"]).next().is_some(),
            Lu1 => junk(JunkReason::LureFile) && junk(JunkReason::ArchiveForMedia),
            Lu2 => {
                let real_media = pct(Category::Video) > 0 || pct(Category::Audio) > 0;
                !real_media
                    && pct(Category::Archive) > 50
                    && self.with_ext(NOTES).any(|x| {
                        let w = words(x.name());
                        HOW_TO.iter().any(|p| w.windows(p.len()).any(|s| s == *p))
                    })
            }
            Lu3 => junk(JunkReason::WebShortcut) && media(t),
            Lu4 => self.with_ext(NOTES).any(|x| {
                let w = words(x.name());
                w.iter().any(|w| UNLOCK_WORDS.contains(&w.as_str()))
                    && (w.iter().any(|w| w == "survey")
                        || w.windows(2)
                            .any(|p| p[0] == "human" && p[1] == "verification")
                        || (w.iter().any(|w| w == "unlock") && w.iter().any(|w| w == "password")))
            }),
            Fk1 => junk(JunkReason::NameMismatch) && !disc && !scene,
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
                let w = self.name_words();
                w.iter().any(|x| SERVICES.contains(&x.as_str()))
                    && w.iter().any(|x| SCAM_WORDS.contains(&x.as_str()))
                    && !w
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

    fn hit(name: &str, paths: &[(&str, u64)]) -> Vec<&'static str> {
        let f: Vec<FileInfo> = paths
            .iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
            .collect();
        let class = classify(name, &f);
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
            files: &f,
            content_type: class.content_type,
        };
        PackRule::ALL
            .into_iter()
            .filter(|r| Ctx::new(&item, &facts).matches(*r))
            .map(PackRule::id)
            .collect()
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
}
