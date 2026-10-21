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
        }
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
        if f.size < 64 * 1024 && is_lure_name(name) {
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

fn is_lure_name(name: &str) -> bool {
    let low = name.to_ascii_lowercase();
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
    LURES.iter().any(|l| low.contains(l))
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
        assert!(is_lure_name("PASSWORD.txt"));
        assert!(is_lure_name("Visit www.example.com for more.txt"));
        assert!(!is_lure_name("Movie.nfo"));
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
        ];
        let mut names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len());
    }
}
