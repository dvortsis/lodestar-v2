//! Content type with a confidence (§6 "Classification", `type:` queries):
//! what the torrent *is*, from its composition, structure and effective
//! release, in that order of trust.

use lodestar_release_parser::Release;

use crate::FileInfo;
use crate::composition::Composition;
use crate::effective::Effective;
use crate::ext::{self, Category};
use crate::structure::Structure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ContentType {
    Movie,
    Tv,
    Music,
    Audiobook,
    Ebook,
    Comic,
    Software,
    Game,
    Images,
    /// Video that's neither a recognisable movie nor a show (clips,
    /// home video, unnamed packs).
    Video,
    Other,
    /// Adult content of any kind; [`AdultKind`](crate::AdultKind) says
    /// which (§20.9).
    Adult,
}

impl ContentType {
    pub const ALL: [ContentType; 12] = [
        ContentType::Movie,
        ContentType::Tv,
        ContentType::Music,
        ContentType::Audiobook,
        ContentType::Ebook,
        ContentType::Comic,
        ContentType::Software,
        ContentType::Game,
        ContentType::Images,
        ContentType::Video,
        ContentType::Other,
        ContentType::Adult,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::Movie => "movie",
            ContentType::Tv => "tv",
            ContentType::Music => "music",
            ContentType::Audiobook => "audiobook",
            ContentType::Ebook => "ebook",
            ContentType::Comic => "comic",
            ContentType::Software => "software",
            ContentType::Game => "game",
            ContentType::Images => "images",
            ContentType::Video => "video",
            ContentType::Other => "other",
            ContentType::Adult => "adult",
        }
    }

    /// The `torrents.content_type` code. Fixed forever: new types get new
    /// codes, and codes are never reused.
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<ContentType> {
        Self::ALL.get(usize::from(code)).copied()
    }

    pub fn from_name(name: &str) -> Option<ContentType> {
        Self::ALL.iter().copied().find(|t| t.as_str() == name)
    }
}

/// Scene and repack groups that release games, not other software.
const GAME_GROUPS: &[&str] = &[
    "CODEX",
    "RUNE",
    "PLAZA",
    "SKIDROW",
    "RELOADED",
    "EMPRESS",
    "TENOKE",
    "FLT",
    "DOGE",
    "CPY",
    "HOODLUM",
    "RAZOR1911",
    "TINYISO",
    "DARKSIDERS",
    "PROPHET",
    "GOG",
    "FITGIRL",
    "DODI",
    "ELAMIGOS",
    "KAOS",
    "XATAB",
    "DUPLEX",
    "VENOM",
    "BIGBLUEBOX",
    "SUXXORS",
    "INSOMNI",
    "I_KNOW",
];

const DESKTOP: &[&str] = &["windows", "macos", "linux", "android", "ios"];

pub(crate) fn content_type(
    e: &Effective,
    comp: &Composition,
    structure: Structure,
    files: &[FileInfo],
) -> (ContentType, u8) {
    use ContentType::*;
    let r = &e.release;
    let episodic =
        r.season.is_some() || r.episode.is_some() || r.date.is_some() || e.episodic_files;
    let video_release = r.resolution.is_some() || r.source.is_some() || r.video_codec.is_some();
    let named = !e.ambiguous || e.resolved.is_some();

    if structure.contains(Structure::BLURAY) || structure.contains(Structure::DVD) {
        return if episodic { (Tv, 75) } else { (Movie, 70) };
    }
    let Some(dominant) = comp.dominant() else {
        return (Other, 0);
    };
    match dominant {
        Category::Video => {
            let videos = comp.get(Category::Video).count;
            if episodic {
                (
                    Tv,
                    if r.season.is_some() || r.episode.is_some() || r.date.is_some() {
                        90
                    } else {
                        75
                    },
                )
            } else if r.year.is_some() && named {
                (Movie, 85)
            } else if videos <= 2 && video_release && named {
                (Movie, 65)
            } else {
                (Video, 50)
            }
        }
        Category::Audio => {
            let m4b = files
                .iter()
                .any(|f| ext::extension(f.name()).is_some_and(|x| x.as_str() == "m4b"));
            if m4b || r.author.is_some() {
                (Audiobook, 80)
            } else if r.artist.is_some()
                || r.bitrate.is_some()
                || structure.contains(Structure::CUE)
            {
                (Music, 85)
            } else {
                (Music, 70)
            }
        }
        Category::Ebook => {
            let single_comic = comp.total_count() == 1
                && files.iter().any(|f| {
                    ext::extension(f.name())
                        .is_some_and(|x| matches!(x.as_str(), "cbz" | "cbr" | "cb7"))
                });
            if structure.contains(Structure::COMICS) || single_comic {
                (Comic, 85)
            } else {
                (Ebook, 85)
            }
        }
        Category::Document if r.author.is_some() => (Ebook, 60),
        Category::Image if structure.contains(Structure::PHOTOS) => (Images, 85),
        Category::Image => (Images, 70),
        Category::Software | Category::Disc | Category::Archive => {
            if is_game(r, structure) {
                (Game, 80)
            } else if dominant == Category::Archive
                && (structure.contains(Structure::MULTIPART) || structure.contains(Structure::SFV))
                && (video_release || episodic)
            {
                // A scene release still in its RAR set.
                if episodic { (Tv, 60) } else { (Movie, 60) }
            } else if dominant == Category::Software
                || r.version.is_some()
                || !r.arch.is_empty()
                || r.platforms.iter().any(|p| DESKTOP.contains(p))
            {
                (Software, 75)
            } else {
                (Other, 30)
            }
        }
        _ => (Other, 40),
    }
}

fn is_game(r: &Release, structure: Structure) -> bool {
    let console = r.platforms.iter().any(|p| !DESKTOP.contains(p));
    let group = r
        .group
        .as_deref()
        .is_some_and(|g| GAME_GROUPS.iter().any(|k| g.eq_ignore_ascii_case(k)));
    let package = matches!(r.container, Some("nsp" | "xci"));
    console
        || group
        || package
        || structure.contains(Structure::STEAM)
        || structure.contains(Structure::REPACK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        // Stored in `torrents.content_type`: never renumber.
        assert_eq!(ContentType::Movie.code(), 0);
        assert_eq!(ContentType::Other.code(), 10);
        assert_eq!(ContentType::Adult.code(), 11);
        for t in ContentType::ALL {
            assert_eq!(ContentType::from_code(t.code()), Some(t));
        }
        assert_eq!(ContentType::from_code(12), None);
    }
}
