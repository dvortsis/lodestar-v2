//! Junk and spam score (§6): 0–100 with reasons a person can read.
//! Results above the user's threshold are hidden by default.

use crate::FileInfo;
use crate::composition::Composition;
use crate::effective::Effective;
use crate::ext::{self, Category};
use crate::structure::Structure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JunkReason {
    /// Nothing but BEP 47 padding files.
    PaddingOnly,
    /// Files, but zero bytes in all of them.
    Empty,
    /// A media file name hiding an executable: `movie.mp4.exe`.
    DoubleExtension,
    /// An executable, script or shortcut inside a video or music torrent.
    Executable,
    /// A `.url` web shortcut (usually an ad) inside a media torrent.
    WebShortcut,
    /// `password.txt`, "visit www…", "how to open": a lure to a site.
    LureFile,
    /// The name promises video or music the files don't contain.
    NameMismatch,
    /// The name promises media, the files are one opaque archive.
    ArchiveForMedia,
    /// HD video claimed, but the video files are tiny.
    TinyVideo,
    /// A source's claimed file list contradicts the verified one (§18.5):
    /// see [`claim_mismatch`].
    ClaimMismatch,
    /// Cracked software (owner §2.39): a software release the parser
    /// flags as cracked, named with a crack-specific word after its title
    /// ([`cracked`]). `patch` alone never counts.
    Cracked,
}

impl JunkReason {
    /// Points added to the score. The total is capped at 100.
    pub fn weight(self) -> u8 {
        match self {
            JunkReason::PaddingOnly => 100,
            JunkReason::Empty => 90,
            JunkReason::DoubleExtension => 80,
            JunkReason::Executable => 60,
            JunkReason::WebShortcut => 15,
            JunkReason::LureFile => 40,
            JunkReason::NameMismatch => 40,
            JunkReason::ArchiveForMedia => 30,
            JunkReason::TinyVideo => 40,
            JunkReason::ClaimMismatch => 30,
            JunkReason::Cracked => 40,
        }
    }

    /// Stable machine name, for the API and stored reason lists.
    pub fn as_str(self) -> &'static str {
        match self {
            JunkReason::PaddingOnly => "padding-only",
            JunkReason::Empty => "empty",
            JunkReason::DoubleExtension => "double-extension",
            JunkReason::Executable => "executable",
            JunkReason::WebShortcut => "web-shortcut",
            JunkReason::LureFile => "lure-file",
            JunkReason::NameMismatch => "name-mismatch",
            JunkReason::ArchiveForMedia => "archive-for-media",
            JunkReason::TinyVideo => "tiny-video",
            JunkReason::ClaimMismatch => "claim-mismatch",
            JunkReason::Cracked => "cracked",
        }
    }

    pub fn from_key(key: &str) -> Option<JunkReason> {
        use JunkReason::*;
        [
            PaddingOnly,
            Empty,
            DoubleExtension,
            Executable,
            WebShortcut,
            LureFile,
            NameMismatch,
            ArchiveForMedia,
            TinyVideo,
            ClaimMismatch,
            Cracked,
        ]
        .into_iter()
        .find(|r| r.as_str() == key)
    }

    /// The "why" tooltip text (§10.14 voice: plain, specific).
    pub fn describe(self) -> &'static str {
        match self {
            JunkReason::PaddingOnly => "Contains only padding files",
            JunkReason::Empty => "Every file is empty",
            JunkReason::DoubleExtension => "A media file name hides a program (like movie.mp4.exe)",
            JunkReason::Executable => "Contains a program or script alongside media",
            JunkReason::WebShortcut => "Contains a web shortcut (.url)",
            JunkReason::LureFile => "Contains a file pointing to a website or a password",
            JunkReason::NameMismatch => "The name doesn't match the files",
            JunkReason::ArchiveForMedia => "The name promises media, but the files are an archive",
            JunkReason::TinyVideo => "The video files are too small for the quality claimed",
            JunkReason::ClaimMismatch => "The files don't match what the source listed",
            JunkReason::Cracked => "Cracked software: a crack, keygen or activator",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Junk {
    pub score: u8,
    pub reasons: Vec<JunkReason>,
}

/// Video files smaller than this can't be the HD release the name
/// claims.
const TINY_HD_VIDEO: u64 = 40 * 1024 * 1024;

pub(crate) fn score(
    e: &Effective,
    comp: &Composition,
    structure: Structure,
    files: &[FileInfo],
) -> Junk {
    let mut reasons = Vec::new();
    let r = &e.release;
    let count = comp.total_count();
    if count == 0 {
        if comp.padding.count > 0 {
            reasons.push(JunkReason::PaddingOnly);
        }
        return finish(reasons);
    }
    if comp.total_bytes() == 0 {
        reasons.push(JunkReason::Empty);
    }

    let video_claim = r.resolution.is_some() || r.source.is_some() || r.video_codec.is_some();
    let music_claim = r.artist.is_some() || r.bitrate.is_some() || r.sample_rate.is_some();
    let has = |c: Category| comp.get(c).count > 0;
    let media_dominant = matches!(comp.dominant(), Some(Category::Video | Category::Audio));
    let media = media_dominant || ((video_claim || music_claim) && !is_software_release(r));

    let (mut double, mut executable, mut shortcut, mut lure) = (false, false, false, false);
    let mut largest_video = 0u64;
    for f in files.iter().filter(|f| !f.padding) {
        let name = f.name();
        let ext = ext::extension(name);
        let e = ext.as_ref().map_or("", |x| x.as_str());
        if ext::is_dangerous(e) {
            if hides_media_extension(name, e) {
                double = true;
            } else if e == "url" {
                shortcut = true;
            } else {
                executable = true;
            }
        }
        if ext::category_of(e) == Category::Video {
            largest_video = largest_video.max(f.size);
        }
        if lure_hits(f).next().is_some() {
            lure = true;
        }
    }
    if double {
        reasons.push(JunkReason::DoubleExtension);
    }
    if media && executable {
        reasons.push(JunkReason::Executable);
    }
    if media && shortcut {
        reasons.push(JunkReason::WebShortcut);
    }
    if lure {
        reasons.push(JunkReason::LureFile);
    }

    let archive_set =
        structure.contains(Structure::MULTIPART) || structure.contains(Structure::SFV);
    let disc = structure.contains(Structure::BLURAY)
        || structure.contains(Structure::DVD)
        || has(Category::Disc);
    let video_missing = video_claim && !has(Category::Video) && !disc;
    let music_missing = music_claim && !has(Category::Audio) && !disc;
    if video_missing || music_missing {
        let archive = comp.dominant() == Some(Category::Archive);
        if archive && !archive_set {
            reasons.push(JunkReason::ArchiveForMedia);
        } else if !archive {
            reasons.push(JunkReason::NameMismatch);
        }
    }
    let hd = r
        .resolution
        .is_some_and(|x| !matches!(x.as_str(), "480p" | "576p"));
    if hd && has(Category::Video) && largest_video < TINY_HD_VIDEO {
        reasons.push(JunkReason::TinyVideo);
    }
    finish(reasons)
}

impl Junk {
    /// Adds a reason once and scores again, as [`score`] would have.
    pub fn add(&mut self, reason: JunkReason) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
            *self = finish(std::mem::take(&mut self.reasons));
        }
    }
}

/// The words that name a crack (owner §2.39, MW-6, UC-2): one word, or
/// two in a row. The parser also rates `patch` a weak cracked token; it's
/// left out, so a lawful patch (`Firefox.120.0.1.Patch`) never counts.
/// `k'ed` is read as `k` then `ed`, the way [`words`] splits it, and only
/// counts where the name spells it with an apostrophe.
pub const CRACK_WORDS: &[&[&str]] = &[
    &["crack"],
    &["cracked"],
    &["keygen"],
    &["keymaker"],
    &["activator"],
    &["pre", "activated"],
    &["k", "ed"],
];

/// A name's words as the crack check reads them: runs of letters and
/// digits, lowercased. Packs blank neutralized words in the same list,
/// positions kept, and pass it to [`crack_words_in`].
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        // An ASCII word lowercases the same either way; the ASCII form
        // skips the per-character Unicode lookup (the filter's packs read
        // every torrent's name this way).
        .map(|w| {
            if w.is_ascii() {
                w.to_ascii_lowercase()
            } else {
                w.to_lowercase()
            }
        })
        .collect()
}

/// Words that, right after `crack` or `cracked`, make it physical damage
/// (`Linux.ISO.cracked.screen.fix`, a phone repair guide), not a software
/// crack. Narrow on purpose: `Crack.Fix`, a scene fix for a crack, stays a
/// crack.
const DAMAGE: &[&str] = &["screen", "screens", "glass", "display", "lcd", "phone"];

/// How many crack words ([`CRACK_WORDS`]) `words` holds, each phrase
/// counted where it starts. `k ed` counts only when `name` spells `k'ed`
/// (either apostrophe); `crack` or `cracked` followed by a damage word
/// ([`DAMAGE`]) doesn't count.
pub fn crack_words_in(words: &[String], name: &str) -> usize {
    let low = name.to_lowercase();
    let spelled = low.contains("k'ed") || low.contains("k\u{2019}ed");
    (0..words.len())
        .filter(|&i| {
            CRACK_WORDS.iter().any(|p| {
                words.len() - i >= p.len()
                    && p.iter().zip(&words[i..]).all(|(a, b)| a == b)
                    && (p != &["k", "ed"] || spelled)
                    && !(matches!(p, [w] if *w == "crack" || *w == "cracked")
                        && words
                            .get(i + 1)
                            .is_some_and(|n| DAMAGE.contains(&n.as_str())))
            })
        })
        .count()
}

/// Whether a software release is cracked (MW-6, UC-2): the parser's
/// `cracked` flag **and** a crack word the title doesn't account for. A
/// title like `Crack Attack` names the program, not a crack, so its words
/// are taken off; a crack word must remain after them. `name` must be the
/// text `release` was parsed from: the torrent's name, or for an
/// uninformative name the file or folder name it was resolved from
/// (`Effective::resolved_from`), as `classify_enriched` passes it. The
/// content type is the caller's to check.
pub fn cracked(name: &str, release: &lodestar_release_parser::Release) -> bool {
    use lodestar_release_parser::Flag;
    if !release.flags.contains(&Flag::Cracked) {
        return false;
    }
    let in_title = crack_words_in(&words(&release.title), &release.title);
    crack_words_in(&words(name), name) > in_title
}

/// A claimed file: its path below the root, and its size if the source
/// gave one.
#[derive(Debug, Clone, Copy)]
pub struct ClaimedFile<'a> {
    pub path: &'a str,
    pub size: Option<u64>,
}

/// Whether a source's claimed file list contradicts the verified one
/// (§18.5, the `claim-mismatch` reason). Claims are hints and verified
/// metadata is the truth, so this only *records* the disagreement: it
/// never changes the verified list.
///
/// Files are matched by name (the last path segment, ASCII
/// case-insensitive), since sources differ on the root folder and
/// separators. It's a contradiction when:
/// - fewer than half of the claimed files have a verified file of that
///   name, or
/// - a matched file's claimed size differs from every verified file of
///   that name.
///
/// An empty claimed list asserts nothing and never contradicts. Padding
/// files are ignored on the verified side. Linear in the two lists.
pub fn claim_mismatch(verified: &[FileInfo], claimed: &[ClaimedFile<'_>]) -> bool {
    if claimed.is_empty() {
        return false;
    }
    let mut sizes: std::collections::HashMap<String, Vec<u64>> =
        std::collections::HashMap::with_capacity(verified.len());
    for f in verified.iter().filter(|f| !f.padding) {
        sizes
            .entry(f.name().to_ascii_lowercase())
            .or_default()
            .push(f.size);
    }
    let mut matched = 0usize;
    for c in claimed {
        let name = c
            .path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(c.path)
            .to_ascii_lowercase();
        let Some(found) = sizes.get(&name) else {
            continue;
        };
        if c.size.is_some_and(|s| !found.contains(&s)) {
            return true;
        }
        matched += 1;
    }
    matched * 2 < claimed.len()
}

fn finish(reasons: Vec<JunkReason>) -> Junk {
    let total: u32 = reasons.iter().map(|r| u32::from(r.weight())).sum();
    Junk {
        score: total.min(100) as u8,
        reasons,
    }
}

/// Software releases legitimately ship executables, even when the name
/// mentions a codec (`HEVC Video Extension v2.0 x64`).
fn is_software_release(r: &lodestar_release_parser::Release) -> bool {
    r.version.is_some() || !r.arch.is_empty() || !r.platforms.is_empty()
}

/// `movie.mp4.exe`, `song.mp3 .scr`: a media extension just before the
/// real, dangerous one.
fn hides_media_extension(name: &str, dangerous: &str) -> bool {
    let stem = name[..name.len() - dangerous.len() - 1].trim_end();
    ext::extension(stem).is_some_and(|e| {
        matches!(
            ext::category_of(e.as_str()),
            Category::Video | Category::Audio | Category::Image | Category::Document
        )
    })
}

/// Lure notes are small; a larger file isn't one.
const LURE_MAX_SIZE: u64 = 64 * 1024;

const LURES: &[&str] = &[
    "password",
    "passwd",
    "click here",
    "how to open",
    "how to download",
    "how to unlock",
    "read me to unlock",
    "get the key",
    "visit www",
    "visit http",
    "download here",
];

/// The lure texts a file's name holds, as the `lure-file` reason reads
/// them: ASCII-lowercase substrings, so `MyPassword.txt` holds
/// `password`. None for padding or a file of 64 KiB or more. The filter's
/// LU-1 reads these, so the size and the list live only here (§20.6).
pub fn lure_hits(f: &FileInfo) -> impl Iterator<Item = &'static str> {
    let low = if f.padding || f.size >= LURE_MAX_SIZE {
        String::new()
    } else {
        f.name().to_ascii_lowercase()
    };
    LURES.iter().copied().filter(move |l| low.contains(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_cap_at_100() {
        let j = finish(vec![JunkReason::DoubleExtension, JunkReason::Executable]);
        assert_eq!(j.score, 100);
        assert_eq!(finish(vec![]).score, 0);
    }

    #[test]
    fn double_extension() {
        assert!(hides_media_extension("Movie.2024.1080p.mp4.exe", "exe"));
        assert!(hides_media_extension("Song.mp3 .scr", "scr"));
        assert!(!hides_media_extension("setup.exe", "exe"));
        assert!(!hides_media_extension("Movie.2024.exe", "exe"));
    }

    #[test]
    fn lures() {
        let hits = |name: &str, size: u64| {
            lure_hits(&FileInfo::new(vec!["dir".into(), name.into()], size)).collect::<Vec<_>>()
        };
        assert_eq!(hits("PASSWORD.txt", 90), ["password"]);
        assert_eq!(hits("MyPassword.txt", 90), ["password"]);
        assert_eq!(
            hits("Click here, visit www.example.com.txt", 90),
            ["click here", "visit www"]
        );
        assert!(hits("Movie.nfo", 90).is_empty());
        // Only small files, and never padding.
        assert_eq!(hits("password.txt", LURE_MAX_SIZE - 1), ["password"]);
        assert!(hits("password.txt", LURE_MAX_SIZE).is_empty());
        let mut pad = FileInfo::new(vec!["password.txt".into()], 90);
        pad.padding = true;
        assert!(lure_hits(&pad).next().is_none());
    }

    /// The lure reason and its weight, pinned: the filter's LU-1 reads
    /// [`lure_hits`], and the score must stay what it was.
    #[test]
    fn lure_file_scores_as_before() {
        let junk = |notes: &[(&str, u64, bool)]| {
            let mut files = vec![FileInfo::new(vec!["Movie.2019.1080p.rar".into()], 4 << 30)];
            for (n, size, padding) in notes {
                let mut f = FileInfo::new(vec![(*n).into()], *size);
                f.padding = *padding;
                files.push(f);
            }
            crate::classify("Movie 2019 1080p", &files).junk
        };
        let lure = Junk {
            score: 70,
            reasons: vec![JunkReason::LureFile, JunkReason::ArchiveForMedia],
        };
        let plain = Junk {
            score: 30,
            reasons: vec![JunkReason::ArchiveForMedia],
        };
        assert_eq!(junk(&[("password.txt", 90, false)]), lure);
        assert_eq!(junk(&[("MyPassword.txt", 90, false)]), lure);
        assert_eq!(junk(&[("Click here.txt", LURE_MAX_SIZE - 1, false)]), lure);
        assert_eq!(junk(&[("password.txt", LURE_MAX_SIZE, false)]), plain);
        assert_eq!(junk(&[("password.txt", 90, true)]), plain);
        assert_eq!(junk(&[("readme.txt", 90, false)]), plain);
    }

    #[test]
    fn names_are_unique() {
        use JunkReason::*;
        let all = [
            PaddingOnly,
            Empty,
            DoubleExtension,
            Executable,
            WebShortcut,
            LureFile,
            NameMismatch,
            ArchiveForMedia,
            TinyVideo,
            ClaimMismatch,
        ];
        for r in all {
            assert_eq!(JunkReason::from_key(r.as_str()), Some(r));
        }
        let mut names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len());
    }

    fn verified(files: &[(&str, u64)]) -> Vec<FileInfo> {
        files
            .iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(String::from).collect(), *s))
            .collect()
    }

    fn claimed<'a>(files: &[(&'a str, Option<u64>)]) -> Vec<ClaimedFile<'a>> {
        files
            .iter()
            .map(|&(path, size)| ClaimedFile { path, size })
            .collect()
    }

    #[test]
    fn a_claimed_list_that_agrees_is_no_mismatch() {
        let v = verified(&[("Show/E01.mkv", 900), ("Show/E02.mkv", 800)]);
        // Another root, other separators, other case, sizes left out.
        let c = claimed(&[("show\\e01.MKV", Some(900)), ("E02.mkv", None)]);
        assert!(!claim_mismatch(&v, &c));
        // An empty claimed list asserts nothing.
        assert!(!claim_mismatch(&v, &[]));
        // Half the claimed files found is still agreement.
        let c = claimed(&[("E01.mkv", None), ("extra.nfo", None)]);
        assert!(!claim_mismatch(&v, &c));
    }

    #[test]
    fn a_contradicting_list_is_a_mismatch() {
        let v = verified(&[("Show/E01.mkv", 900), ("Show/E02.mkv", 800)]);
        // Other files altogether.
        let c = claimed(&[("Film.2020.mkv", Some(5)), ("sample.mkv", None)]);
        assert!(claim_mismatch(&v, &c));
        // The right name with a different size.
        let c = claimed(&[("E01.mkv", Some(901)), ("E02.mkv", Some(800))]);
        assert!(claim_mismatch(&v, &c));
        // Padding files are not content.
        let mut v = verified(&[("Show/E01.mkv", 900)]);
        v.push(FileInfo {
            padding: true,
            ..FileInfo::new(vec![".pad".into(), "0".into()], 3)
        });
        assert!(claim_mismatch(&v, &claimed(&[("0", Some(3))])));
    }

    #[test]
    fn adding_a_reason_scores_it_once() {
        let mut j = Junk::default();
        j.add(JunkReason::ClaimMismatch);
        j.add(JunkReason::ClaimMismatch);
        assert_eq!(j.reasons, vec![JunkReason::ClaimMismatch]);
        assert_eq!(j.score, JunkReason::ClaimMismatch.weight());
        j.add(JunkReason::PaddingOnly);
        assert_eq!(j.score, 100, "capped");
    }
}
