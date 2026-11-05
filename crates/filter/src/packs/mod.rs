//! The built-in rule packs (§20.6): malware, lures, fakes, spam, scams and
//! unauthorised-copy. Rules read the classifier's junk reasons, structure
//! fingerprints and the release parser's fields, so the packs and the junk
//! score never disagree, and each carries its false-positive guards. The
//! unauthorised-copy pack and MW-6 are in [`unauthorised_copy`].
//!
//! Each rule has a version in the compiled-in [`manifest`], and the
//! version this install applied is recorded and resolved by [`upgrade`]
//! (RP-0, §20.6 "signed, versioned", §20.10).
//!
//! Not yet built, because they need data that doesn't exist yet: FK-3
//! (pre-release fakes need a TMDB release date), SP-3 (flooding needs
//! per-publisher rates) and SP-4 (clones of hidden spam need a signature
//! index of hidden items).
//!
//! A CSX-5 *Neutralize term* stops a word counting as evidence in the
//! rules that read keyword lists (MW-3, MW-6, LU-2, LU-4, SC-1, SC-2, UC-1,
//! UC-2) and in LU-1's
//! lure notes (§20.5.5), as it does in the core and your rules. The
//! words of a name or file the rules count are read with the neutralized
//! ones blanked, positions kept ([`Ctx::evidence`], [`Ctx::live_lure`]). The guards that keep a rule from matching
//! (FK-2's `trailer`/`sample`, SC-1's and SC-2's `trainer`/`mod`/`offline`) read the
//! torrent as it is, as `unless` does, so neutralizing never widens a
//! rule.

pub mod manifest;
pub mod overrides;
pub mod unauthorised_copy;
pub mod upgrade;

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
    /// Names that follow the conventions of unauthorised copies (owner
    /// §2.39): UC-1 to UC-3.
    UnauthorisedCopy,
}

impl Pack {
    pub const ALL: [Pack; 6] = [
        Pack::Malware,
        Pack::Lures,
        Pack::Fakes,
        Pack::Spam,
        Pack::Scams,
        Pack::UnauthorisedCopy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Pack::Malware => "malware",
            Pack::Lures => "lures",
            Pack::Fakes => "fakes",
            Pack::Spam => "spam",
            Pack::Scams => "scams",
            Pack::UnauthorisedCopy => "unauthorised-copy",
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
    /// "Generator / hack" for online-game currencies or accounts, split
    /// out of SC-1 so it has its own switch (plan 2026-11-04 Q16).
    Sc2,
    /// Cracked software (owner §2.39), in the malware pack so turning
    /// `unauthorised-copy` off doesn't restore it. Added after RP-0, so
    /// declared last: [`PackRule::index`] stays as it was for the others.
    Mw6,
    /// Camcorder, telesync, telecine and screener rips of films.
    Uc1,
    /// Cracked software, as MW-6 reads it.
    Uc2,
    /// Scene-style film and TV rips.
    Uc3,
}

impl PackRule {
    /// In evaluation order: the strongest action within a pack first.
    pub const ALL: [PackRule; 21] = [
        PackRule::Mw4,
        PackRule::Mw1,
        PackRule::Mw2,
        PackRule::Mw3,
        PackRule::Mw5,
        PackRule::Mw6,
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
        PackRule::Sc2,
        PackRule::Uc1,
        PackRule::Uc2,
        PackRule::Uc3,
    ];

    /// How many rules there are.
    pub const COUNT: usize = PackRule::ALL.len();

    /// A dense index, `0..COUNT`, in declaration order.
    pub fn index(self) -> usize {
        self as usize
    }

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
            PackRule::Sc2 => "SC-2",
            PackRule::Mw6 => "MW-6",
            PackRule::Uc1 => "UC-1",
            PackRule::Uc2 => "UC-2",
            PackRule::Uc3 => "UC-3",
        }
    }

    pub fn from_id(id: &str) -> Option<PackRule> {
        PackRule::ALL.into_iter().find(|r| r.id() == id)
    }

    pub fn pack(self) -> Pack {
        use PackRule::*;
        match self {
            Mw1 | Mw2 | Mw3 | Mw4 | Mw5 | Mw6 => Pack::Malware,
            Lu1 | Lu2 | Lu3 | Lu4 => Pack::Lures,
            Fk1 | Fk2 | Fk4 | Fk5 => Pack::Fakes,
            Sp1 | Sp2 => Pack::Spam,
            Sc1 | Sc2 => Pack::Scams,
            Uc1 | Uc2 | Uc3 => Pack::UnauthorisedCopy,
        }
    }

    /// The action of the version this build ships ([`manifest`]).
    pub fn action(self) -> Action {
        use PackRule::*;
        match self {
            Mw4 | Fk4 => Action::Drop,
            Mw1 | Mw2 | Mw3 | Mw5 | Mw6 | Lu1 | Lu4 | Fk1 | Sp2 | Sc1 | Sc2 | Uc1 | Uc2 | Uc3 => {
                Action::Hide
            }
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
    lodestar_classify::crack::words(text)
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
/// generators are scams: SC-2 reads the [`GAMES`] among them, SC-1 the
/// rest, so the two together match what SC-1 alone matched before RP-0.
const SERVICES: &[&str] = &[
    "spotify", "netflix", "robux", "vbucks", "roblox", "fortnite", "disney", "hbo", "deezer",
    "tidal", "onlyfans", "bitcoin", "wallet", "crypto", "paypal",
];

/// The online games and their currencies among [`SERVICES`] (SC-2).
const GAMES: &[&str] = &["robux", "vbucks", "roblox", "fortnite"];

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
        let mut files = Vec::with_capacity(item.files.len().min(crate::MAX_FILES));
        files.extend(
            item.files
                .iter()
                .take(crate::MAX_FILES)
                .filter(|x| !x.padding)
                .map(|x| (x, ext(x))),
        );
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
    /// letters and digits as [`words`] reads them. Unlike
    /// [`Ctx::evidence`], any word is checked, since a lure can sit inside
    /// a word like `password1`; it's blanked only when that whole word is
    /// neutralized.
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
            .filter(move |(_, e)| {
                e.is_some_and(|e| {
                    // Extensions are at most 8 bytes: compared in line,
                    // not by a library call per name in the set.
                    let e = e.as_bytes();
                    set.iter()
                        .any(|s| s.len() == e.len() && s.bytes().zip(e).all(|(a, b)| a == *b))
                })
            })
            .map(|(f, _)| *f)
    }

    /// The torrent's name as it is.
    pub fn name(&self) -> &str {
        self.item.name
    }

    /// Whether a pack rule matches, by the code of the version this
    /// build ships. A version a release replaced keeps its code in the
    /// [`manifest`] while an install may still have it applied.
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
            // MW-2@2: MW-2@1 unless it's a music-production pack.
            Mw2 => mw2_v1(self) && !self.production_pack(),
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
            // FK-2@2: FK-2@1 unless it's a short-episode pack.
            Fk2 => fk2_v1(self) && !self.episode_pack(),
            Fk4 => junk(JunkReason::Empty) || junk(JunkReason::PaddingOnly),
            Fk5 => {
                junk(JunkReason::ArchiveForMedia)
                    && !scene
                    && !structure.contains(Structure::MULTIPART)
            }
            Sp1 => site_advert(self.item.name),
            Sp2 => stuffed(self.item.name) && !matches!(t, ContentType::Music),
            Sc1 => self.scam(|x| !GAMES.contains(&x)),
            Sc2 => self.scam(|x| GAMES.contains(&x)),
            Mw6 | Uc2 => self.cracked_software(),
            Uc1 => self.uc1(),
            Uc3 => self.uc3(),
        }
    }

    /// SC-1 and SC-2: a service `which` takes and a scam word in the
    /// name's evidence, and none of the guards in the name as it is.
    fn scam(&self, which: impl Fn(&str) -> bool) -> bool {
        let w = self.name_evidence();
        w.iter()
            .any(|x| SERVICES.contains(&x.as_str()) && which(x.as_str()))
            && w.iter().any(|x| SCAM_WORDS.contains(&x.as_str()))
            && !self
                .name_words()
                .iter()
                .any(|x| x == "trainer" || x == "mod" || x == "offline")
    }
}

/// MW-2@1, kept while an install may still have it applied (RP-0): an
/// executable in a movie, TV, music, video or adult torrent, except
/// software, games and disc images.
pub(crate) fn mw2_v1(ctx: &Ctx<'_>) -> bool {
    let class = ctx.facts.class;
    let t = class.content_type;
    let disc =
        class.structure.contains(Structure::BLURAY) || class.structure.contains(Structure::DVD);
    class.junk.reasons.contains(&JunkReason::Executable)
        && media(t)
        && !matches!(t, ContentType::Software | ContentType::Game)
        && !disc
}

/// FK-2@1, kept while an install may still have it applied (RP-0): a tiny
/// video claiming HD, except samples and trailers. Its words are a guard
/// only: they read as they are.
pub(crate) fn fk2_v1(ctx: &Ctx<'_>) -> bool {
    let class = ctx.facts.class;
    class.junk.reasons.contains(&JunkReason::TinyVideo)
        && !class.structure.contains(Structure::SAMPLE)
        && !ctx
            .name_words()
            .iter()
            .any(|w| w == "trailer" || w == "sample")
}

/// The installers MW-2@2's music-production guard allows: a plain
/// Windows installer. A script, shortcut, screensaver or archive of code
/// (`.bat`, `.lnk`, `.scr`, `.jar`, …) is never a plug-in installer.
const INSTALLERS: &[&str] = &["exe", "msi"];

/// At most this many installers in a music-production pack (an installer
/// and, say, its 32-bit twin or an uninstaller).
const MAX_INSTALLERS: usize = 2;

/// Preset, instrument and sample-library formats of music software:
/// plug-in presets and banks (`.fxp`, `.fxb`, `.vstpreset`, `.aupreset`,
/// `.h2p`, `.nmsv`), Kontakt and other sampler instruments and their
/// sample containers (`.nki`, `.nkm`, `.nkb`, `.nkc`, `.nkx`, `.nkr`,
/// `.ncw`, `.exs`, `.sfz`, `.sf2`), Ableton devices and racks (`.adg`,
/// `.adv`) and REX loops (`.rx2`). None of them runs.
const PRESETS: &[&str] = &[
    "fxp",
    "fxb",
    "vstpreset",
    "aupreset",
    "h2p",
    "nmsv",
    "nki",
    "nkm",
    "nkb",
    "nkc",
    "nkx",
    "nkr",
    "ncw",
    "exs",
    "sfz",
    "sf2",
    "adg",
    "adv",
    "rx2",
];

/// Folder words of a sample library: audio inside one of them is a
/// sample, not an album track.
const SAMPLE_DIRS: &[&str] = &[
    "samples",
    "presets",
    "patches",
    "multisamples",
    "oneshots",
    "loops",
    "kicks",
    "snares",
    "hats",
    "hihats",
    "claps",
    "cymbals",
    "toms",
    "percussion",
];

/// At least this many audio files in a music-production pack, and this
/// many presets or samples in sample folders for its layout.
const MIN_SOUNDS: usize = 2;

/// FK-2@2's short-episode pack: at least this many episodes.
const MIN_EPISODES: usize = 3;

/// No episode of a short-episode pack is smaller than this. HD video at
/// a typical web bitrate (about 2-6 Mbit/s) runs to 16 MiB in well under
/// a minute; the smallest episode of the precision corpus is 18 MiB, and
/// a fake is usually far smaller.
const MIN_EPISODE_BYTES: u64 = 16 << 20;

impl Ctx<'_> {
    /// Whether the torrent has more files than the rules read
    /// ([`crate::MAX_FILES`]). The classifier reads them all, so a guard
    /// that passes on the files it sees fails closed here: an executable
    /// or a stray video past the cap can't hide behind it.
    fn truncated(&self) -> bool {
        self.item.files.len() > crate::MAX_FILES
    }

    /// MW-2@2's guard (§20.6 "music production packs (VST/samples)"): a
    /// music torrent made mostly of audio and presets, with at most
    /// [`MAX_INSTALLERS`] plain installers beside them and a
    /// plug-in or sample-library layout. All of:
    /// - **music**, as the classifier reads it;
    /// - **every executable is an `.exe` or `.msi`**, and there are at most
    ///   [`MAX_INSTALLERS`]: any other program the pack ([`EXECUTABLE`]) or
    ///   the classifier ([`lodestar_classify::ext::is_dangerous`], which adds
    ///   `.com`) knows keeps it held;
    /// - **sounds**: at least [`MIN_SOUNDS`] audio files, and the audio and
    ///   [`PRESETS`] files together more than half the files and at least
    ///   twice the installers;
    /// - **a production layout**: at least [`MIN_SOUNDS`] [`PRESETS`]
    ///   files, or at least [`MIN_SOUNDS`] audio files inside a
    ///   [`SAMPLE_DIRS`] folder.
    ///
    /// Plug-in binaries (`.vst3`, `.dll`, …) never count as sounds or as
    /// the layout, so code beside one sample is still held. Folder and
    /// file names are read as they are, as every guard is. A torrent
    /// with more files than the rules read never passes ([`Ctx::truncated`]).
    fn production_pack(&self) -> bool {
        if self.facts.class.content_type != ContentType::Music || self.truncated() {
            return false;
        }
        let (mut installers, mut sounds, mut audio, mut presets, mut sampled) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        for (f, e) in &self.files {
            let e = e.as_ref().map_or("", |x| x.as_str());
            // What the pack reads as a program, or what the classifier
            // does (`.com`), so the guard never passes one MW-2@1 holds for.
            if EXECUTABLE.contains(&e) || lodestar_classify::ext::is_dangerous(e) {
                if !INSTALLERS.contains(&e) {
                    return false;
                }
                installers += 1;
            } else if PRESETS.contains(&e) {
                presets += 1;
                sounds += 1;
            } else if lodestar_classify::ext::category_of(e) == Category::Audio {
                sounds += 1;
                audio += 1;
                let dirs = &f.path[..f.path.len().saturating_sub(1)];
                if dirs
                    .iter()
                    .any(|d| words(d).iter().any(|w| SAMPLE_DIRS.contains(&w.as_str())))
                {
                    sampled += 1;
                }
            }
        }
        (1..=MAX_INSTALLERS).contains(&installers)
            && audio >= MIN_SOUNDS
            && sounds * 2 > self.files.len()
            && sounds >= 2 * installers
            && (presets >= MIN_SOUNDS || sampled >= MIN_SOUNDS)
    }

    /// FK-2@2's guard (§20.6 "short-episode pack"): every video is an
    /// episode of one series, and they look like a season's worth. All of:
    /// - at least [`MIN_EPISODES`] video files;
    /// - each named `<series> SxxEyy …`, the same series words before the
    ///   episode marker in every one, the torrent's name starting with
    ///   them, and no episode twice;
    /// - each at least [`MIN_EPISODE_BYTES`], and the largest at most twice
    ///   the smallest.
    ///
    /// One tiny HD video is still flagged, and so is a set of tiny files
    /// of different series, sizes or no episode numbers, and a torrent
    /// with more files than the rules read ([`Ctx::truncated`]).
    fn episode_pack(&self) -> bool {
        if self.truncated() {
            return false;
        }
        let mut series: Option<Vec<String>> = None;
        let mut seen: Vec<(u16, u16)> = Vec::new();
        let (mut small, mut large) = (u64::MAX, 0u64);
        for (f, e) in &self.files {
            let e = e.as_ref().map_or("", |x| x.as_str());
            if lodestar_classify::ext::category_of(e) != Category::Video {
                continue;
            }
            let w = words(f.name());
            let Some((at, ep)) = w
                .iter()
                .enumerate()
                .find_map(|(i, x)| episode(x).map(|ep| (i, ep)))
            else {
                return false;
            };
            if at == 0 || seen.contains(&ep) {
                return false;
            }
            match &series {
                Some(s) if s[..] != w[..at] => return false,
                Some(_) => {}
                None => series = Some(w[..at].to_vec()),
            }
            seen.push(ep);
            small = small.min(f.size);
            large = large.max(f.size);
        }
        let named = series
            .as_ref()
            .is_some_and(|s| self.name_words().starts_with(s));
        named
            && seen.len() >= MIN_EPISODES
            && small >= MIN_EPISODE_BYTES
            && large <= small.saturating_mul(2)
    }
}

/// A word `sNNeNN` (one to three digits each), as season and episode.
fn episode(w: &str) -> Option<(u16, u16)> {
    let rest = w.strip_prefix('s')?;
    let (s, e) = rest.split_once('e')?;
    let num = |x: &str| {
        (!x.is_empty() && x.len() <= 3 && x.bytes().all(|b| b.is_ascii_digit()))
            .then(|| x.parse().ok())
            .flatten()
    };
    Some((num(s)?, num(e)?))
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
        // SC-2 was part of SC-1 before RP-0: the tests written then see
        // the two as one, so SC-1 ∪ SC-2 is checked against today's SC-1
        // (plan 2026-11-04 Q16). `sc_split` tells them apart.
        let mut ids: Vec<&'static str> = PackRule::ALL
            .into_iter()
            .filter(|r| ctx.matches(*r))
            .map(|r| if r == PackRule::Sc2 { "SC-1" } else { r.id() })
            .collect();
        ids.dedup();
        ids
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

    /// The pack rules that match, SC-1 and SC-2 apart.
    fn split_hits(name: &str, paths: &[(&str, u64)]) -> Vec<&'static str> {
        let f = files(paths);
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
        let ctx = Ctx::new(&item, &facts, &[]);
        PackRule::ALL
            .into_iter()
            .filter(|r| ctx.matches(*r))
            .map(PackRule::id)
            .collect()
    }

    /// SC-2 takes the online games out of SC-1, and the two together
    /// match what SC-1 matched before (plan 2026-11-04 Q16).
    #[test]
    fn sc_split() {
        let exe = [("x.exe", 1 << 20)];
        let sc = |name: &str| -> Vec<&'static str> {
            split_hits(name, &exe)
                .into_iter()
                .filter(|r| r.starts_with("SC-"))
                .collect()
        };
        assert_eq!(sc("Free Robux Generator 2024"), ["SC-2"]);
        assert_eq!(sc("Fortnite VBucks Hack"), ["SC-2"]);
        assert_eq!(sc("Spotify Premium Free APK"), ["SC-1"]);
        assert_eq!(sc("Bitcoin Wallet Recovery Generator"), ["SC-1"]);
        assert_eq!(sc("Netflix and Roblox Premium Generator"), ["SC-1", "SC-2"]);
        assert!(sc("Fortnite Trainer Offline Mod").is_empty());
        assert!(sc("Roblox Studio Tutorial").is_empty());
        for w in GAMES {
            assert!(SERVICES.contains(w), "{w} is a service SC-1 read");
        }
        assert_eq!(PackRule::Sc2.pack(), Pack::Scams);
        assert_eq!(PackRule::Sc2.action(), PackRule::Sc1.action());
    }

    const M: u64 = 1 << 20;

    /// MW-2@2's music-production guard at each of its thresholds: what
    /// passes, and the nearest thing that MW-2 still holds.
    #[test]
    fn mw2_music_production_guard() {
        let mw2 = |paths: &[(&str, u64)]| hit("Example Pack WAV", paths).contains(&"MW-2");
        // Two samples in sample folders beside one installer: a pack.
        let pack = [
            ("Kicks/Kick 01.wav", 30 * M),
            ("Snares/Snare 01.wav", 30 * M),
            ("setup.exe", 2 * M),
        ];
        assert!(!mw2(&pack));
        // One sample beside an installer, even in a sample folder.
        assert!(mw2(&[("Samples/Pad.wav", 60 * M), ("setup.exe", 2 * M)]));
        // An album: audio outside any sample folder.
        assert!(mw2(&[
            ("01 - Song.wav", 30 * M),
            ("02 - Song.wav", 30 * M),
            ("setup.exe", 2 * M),
        ]));
        // One song and one preset beside an installer is held: the
        // layout takes two presets, and two audio files are needed anyway.
        assert!(mw2(&[
            ("01 - Song.wav", 60 * M),
            ("Bass.fxp", 40_000),
            ("setup.exe", 2 * M),
        ]));
        assert!(
            mw2(&[
                ("01 - Song.wav", 60 * M),
                ("Bass.fxp", 40_000),
                ("Lead.fxp", 40_000),
                ("Pad.fxp", 40_000),
                ("setup.exe", 2 * M),
            ]),
            "presets don't stand in for audio"
        );
        assert!(!mw2(&[
            ("01 - Song.wav", 30 * M),
            ("02 - Song.wav", 30 * M),
            ("Bass.fxp", 40_000),
            ("Lead.fxp", 40_000),
            ("setup.exe", 2 * M),
        ]));
        // Anything but a plain installer is held.
        for exe in [
            "install.bat",
            "crack.com",
            "run.cmd",
            "open.vbs",
            "x.lnk",
            "a.jar",
            "x.scr",
        ] {
            let mut p = pack.to_vec();
            p[2].0 = exe;
            assert!(mw2(&p), "{exe}");
        }
        assert!(!mw2(&[
            ("Kicks/Kick 01.wav", 30 * M),
            ("Snares/Snare 01.wav", 30 * M),
            ("setup.msi", 2 * M),
        ]));
        // Two installers need four sounds; a third is never a pack.
        let two = [
            ("Kicks/Kick 01.wav", 30 * M),
            ("Kicks/Kick 02.wav", 30 * M),
            ("Snares/Snare 01.wav", 30 * M),
            ("setup.exe", 2 * M),
            ("setup64.exe", 2 * M),
        ];
        assert!(mw2(&two), "three sounds, two installers");
        let mut four = two.to_vec();
        four.push(("Snares/Snare 02.wav", 30 * M));
        assert!(!mw2(&four));
        four.push(("activate.exe", 2 * M));
        assert!(mw2(&four), "three installers");
        // Sounds must be more than half the files: plug-in code and
        // anything else count against them.
        assert!(mw2(&[
            ("Kicks/Kick 01.wav", 60 * M),
            ("Snares/Snare 01.wav", 60 * M),
            ("setup.exe", 2 * M),
            ("plugin.vst3", 2 * M),
            ("plugin.dll", 2 * M),
        ]));
        // Folder words are whole words.
        assert!(mw2(&[
            ("Kickstarter/Kick 01.wav", 30 * M),
            ("Kickstarter/Snare 01.wav", 30 * M),
            ("setup.exe", 2 * M),
        ]));
        // Past the files the rules read, a script could hide: fail closed.
        let mut many: Vec<(String, u64)> = (0..crate::MAX_FILES)
            .map(|i| (format!("Samples/Hit {i:04}.wav"), M))
            .collect();
        many.push(("setup.exe".into(), M));
        let few: Vec<(&str, u64)> = many.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        assert!(!mw2(&few[few.len() - 3..]));
        many.push(("run.bat".into(), 1_000));
        let all: Vec<(&str, u64)> = many.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        assert!(mw2(&all), "a script past the cap");
        // Video lures are untouched: the guard reads music only.
        assert!(
            hit(
                "Example.Movie.2019.1080p.WEB",
                &[
                    ("Example.Movie.2019.1080p.WEB.mkv", 4 << 30),
                    ("Presets/Bass.fxp", 40_000),
                    ("Samples/a.wav", M),
                    ("Samples/b.wav", M),
                    ("setup.exe", 2 * M),
                ],
            )
            .contains(&"MW-2")
        );
    }

    /// FK-2@2's short-episode guard at each of its thresholds.
    #[test]
    fn fk2_short_episode_guard() {
        let fk2 =
            |paths: &[(&str, u64)]| hit("Example Shorts S01 1080p WEB", paths).contains(&"FK-2");
        let ep = |n: u32| format!("Example Shorts S01E{n:02} 1080p WEB.mkv");
        let (e1, e2, e3) = (ep(1), ep(2), ep(3));
        assert!(!fk2(&[(&e1, 30 * M), (&e2, 30 * M), (&e3, 30 * M)]));
        assert!(fk2(&[(&e1, 30 * M)]), "one tiny HD file");
        assert!(fk2(&[(&e1, 30 * M), (&e2, 30 * M)]), "two episodes");
        // The floor and the spread.
        assert!(!fk2(&[(&e1, 16 * M), (&e2, 16 * M), (&e3, 16 * M)]));
        assert!(fk2(&[(&e1, 16 * M - 1), (&e2, 16 * M), (&e3, 16 * M)]));
        assert!(!fk2(&[(&e1, 17 * M), (&e2, 34 * M), (&e3, 20 * M)]));
        assert!(fk2(&[(&e1, 17 * M), (&e2, 34 * M + 1), (&e3, 20 * M)]));
        // The torrent's name must start with the episodes' series.
        assert!(
            hit(
                "Big Movie 2024 2160p",
                &[(&e1, 30 * M), (&e2, 30 * M), (&e3, 30 * M)]
            )
            .contains(&"FK-2")
        );
        assert!(
            !hit(
                "Example.Shorts.S01.1080p",
                &[(&e1, 30 * M), (&e2, 30 * M), (&e3, 30 * M)]
            )
            .contains(&"FK-2")
        );
        // One series, no repeats, every video an episode.
        assert!(fk2(&[
            (&e1, 30 * M),
            (&e2, 30 * M),
            ("Other Show S01E03 1080p WEB.mkv", 30 * M),
        ]));
        assert!(fk2(&[(&e1, 30 * M), (&e1, 30 * M), (&e2, 30 * M)]));
        assert!(fk2(&[
            (&e1, 30 * M),
            (&e2, 30 * M),
            (&e3, 30 * M),
            ("Example Shorts 1080p WEB.mkv", 30 * M),
        ]));
        assert!(
            fk2(&[
                ("S01E01.mkv", 30 * M),
                ("S01E02.mkv", 30 * M),
                ("S01E03.mkv", 30 * M),
            ]),
            "no series words"
        );
        // Other files don't count, and seasons may differ.
        assert!(!fk2(&[
            (&e1, 30 * M),
            (&e2, 30 * M),
            ("Example Shorts S02E01 1080p WEB.mkv", 30 * M),
            ("cover.jpg", M),
        ]));
        let many_ep = |n: u32| {
            format!(
                "Example Shorts S{:02}E{:02} 1080p WEB.mkv",
                n / 100,
                n % 100
            )
        };
        let mut many: Vec<String> = (0..crate::MAX_FILES as u32).map(many_ep).collect();
        many.push("Other Show S01E01 1080p WEB.mkv".into());
        let all: Vec<(&str, u64)> = many.iter().map(|p| (p.as_str(), 30 * M)).collect();
        assert!(!fk2(&all[..5]));
        assert!(!fk2(&all[..crate::MAX_FILES]), "a full pack at the cap");
        assert!(fk2(&all), "a stray video past the cap");
        assert_eq!(episode("s01e02"), Some((1, 2)));
        assert_eq!(episode("s1e100"), Some((1, 100)));
        for w in [
            "s01",
            "e01",
            "s01e",
            "se01",
            "s01e01e02",
            "s0001e01",
            "sxe1",
            "s01x02",
        ] {
            assert_eq!(episode(w), None, "{w}");
        }
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
        // A plain scene release passes every pack but UC-3, which hides
        // scene-style film rips (owner §2.39).
        assert_eq!(
            hit(
                "The.Matrix.1999.1080p.BluRay.x264-GRP",
                &[
                    ("The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 8 << 30),
                    ("grp.nfo", 4000)
                ]
            ),
            ["UC-3"]
        );
        // Without a release group it isn't scene-style.
        assert!(
            hit(
                "The Matrix 1999 1080p BluRay",
                &[("The Matrix 1999 1080p BluRay.mkv", 8 << 30)]
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
            (
                "MW-6",
                "Example Tool v1.0 Keygen",
                vec![("setup.exe", 80 << 20)],
                "keygen",
            ),
            (
                "UC-2",
                "Example Tool v1.0 Keygen",
                vec![("setup.exe", 80 << 20)],
                "keygen",
            ),
            (
                "MW-6",
                "Example Disk Tool v4.1 K'ed",
                vec![("setup.exe", 80 << 20)],
                "ed",
            ),
            (
                "UC-1",
                "Example.Movie.2024.HDCAM.x264-GRP",
                vec![("Example.Movie.2024.mkv", 1 << 30)],
                "hdcam",
            ),
            (
                "UC-1",
                "Example Movie (2024) [TS] 720p",
                vec![("Example.Movie.2024.mkv", 1 << 30)],
                "ts",
            ),
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
            // A neutralized crack word in the program's title takes
            // nothing from the crack word after it.
            (
                "MW-6",
                "Crack Attack v1.2 Keygen",
                vec![("setup.exe", 50 << 20)],
                &["crack"],
            ),
            (
                "UC-2",
                "Crack Attack v1.2 Keygen",
                vec![("setup.exe", 50 << 20)],
                &["crack"],
            ),
            // Another marker still counts.
            (
                "UC-1",
                "Example.Movie.2024.HDCAM.TS.x264-GRP",
                vec![("Example.Movie.2024.mkv", 1 << 30)],
                &["hdcam"],
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

        /// Words of lure notes, and words beside them.
        const LURE_VOCAB: &[&str] = &[
            "password",
            "PASSWD",
            "click",
            "here",
            "visit",
            "www",
            "how",
            "to",
            "open",
            "MyPassword",
            "movie",
        ];

        /// Whether to add an archive for the media and lure notes, the
        /// notes, and lure words to neutralize.
        #[allow(clippy::type_complexity)]
        fn lure_case() -> impl Strategy<Value = (bool, Vec<Vec<&'static str>>, Vec<&'static str>)> {
            let words = || prop::collection::vec(prop::sample::select(LURE_VOCAB), 1..4);
            (
                any::<bool>(),
                prop::collection::vec(words(), 1..3),
                prop::sample::subsequence(LURE_VOCAB.to_vec(), 2..6),
            )
        }

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
            /// exactly when it matches both ways. SP-1 and SP-2 read no
            /// keywords, so they match as they do on the torrent as it is.
            #[test]
            fn neutralizing_removes_only_sole_evidence(
                lead in prop::sample::select(LEADS),
                rest in text(0..5),
                files in prop::collection::vec(
                    (text(1..5), prop::sample::select(EXTS), prop::sample::select(SIZES)),
                    1..5,
                ),
                neutral in prop::sample::subsequence(VOCAB.to_vec(), 0..5),
                lure in lure_case(),
            ) {
                // LU-1 needs an HD name, an archive standing in for the
                // media and a small lure note, which random files rarely
                // give together: half the cases carry them.
                let (afm, notes, lure_neutral) = lure;
                let mut name: Vec<&str> = lead.iter().copied().chain(rest).collect();
                let mut files = files;
                let mut neutral = neutral;
                if afm {
                    name.splice(0..0, ["movie", "1080p"]);
                    files.insert(0, (vec!["movie", "1080p"], "rar", 4 << 30));
                    files.extend(notes.into_iter().map(|w| (w, "txt", 90)));
                    neutral.extend(lure_neutral);
                }
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
                // SP-1 and SP-2 read the name as it is by design (no
                // keywords), so they're expected as they are.
                let both: Vec<_> = as_is
                    .iter()
                    .copied()
                    .filter(|r| masked.contains(r) || ["SP-1", "SP-2"].contains(r))
                    .collect();
                prop_assert_eq!(&neutralized, &both);
            }

            /// `episode` never panics on any word, and reads back every
            /// `sNNeNN` of one to three digits each, leading zeros or not.
            #[test]
            fn episode_markers_round_trip(
                any_word in "\\PC{0,24}",
                s in 0u16..1000,
                e in 0u16..1000,
                sw in 1usize..=3,
                ew in 1usize..=3,
            ) {
                let _ = super::super::episode(&any_word);
                let w = format!("s{s:0sw$}e{e:0ew$}");
                prop_assert_eq!(super::super::episode(&w), Some((s, e)));
            }
        }
    }
}
