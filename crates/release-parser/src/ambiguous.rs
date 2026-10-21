//! Uninformative names (§6 "Ambiguity flag"): a hash, `New Folder (2)`,
//! `Complete Season`, `a8f3x.zip.part`. Such torrents get `ambiguous=true`
//! and take their effective title from their files or folder instead.

use crate::lexicon;

/// Names that are generic once numbers and brackets are dropped.
const GENERIC: &[&str] = &[
    "new folder",
    "folder",
    "untitled",
    "untitled folder",
    "video",
    "videos",
    "movie",
    "movies",
    "film",
    "films",
    "music",
    "audio",
    "download",
    "downloads",
    "torrent",
    "torrents",
    "files",
    "file",
    "data",
    "documents",
    "docs",
    "images",
    "pictures",
    "photos",
    "complete",
    "complete season",
    "full season",
    "season",
    "seasons",
    "episode",
    "episodes",
    "disc",
    "disk",
    "cd",
    "dvd",
    "bdmv",
    "video ts",
    "sample",
    "samples",
    "tmp",
    "temp",
    "misc",
    "stuff",
    "pack",
    "collection",
    "new",
    "test",
    "my videos",
    "my music",
    "desktop",
    "output",
    "archive",
    "backup",
    "extras",
    "bonus",
    "subs",
    "subtitles",
    "part",
    "volume",
    "vol",
    "copy",
];

/// Whether a name says nothing about its content, so the effective title
/// must come from somewhere else.
pub fn is_uninformative(name: &str) -> bool {
    let name = strip_extensions(name.trim());
    let alnum = name.chars().filter(|c| c.is_alphanumeric()).count();
    if alnum < 2 {
        return true;
    }
    if is_hash_like(name) || is_random_word(name) {
        return true;
    }
    let mut words = String::new();
    for c in name.chars() {
        let c = if c.is_alphabetic() {
            c.to_ascii_lowercase()
        } else {
            ' '
        };
        if !(c == ' ' && (words.is_empty() || words.ends_with(' '))) {
            words.push(c);
        }
    }
    let words = words.trim_end();
    if words.is_empty() {
        // Only numbers: `1` or `03` says nothing, `1917` is a title.
        return alnum < 4;
    }
    GENERIC.contains(&words)
}

fn strip_extensions(mut name: &str) -> &str {
    for _ in 0..2 {
        match name.rsplit_once('.') {
            Some((rest, ext))
                if !rest.is_empty()
                    && (ext.eq_ignore_ascii_case("part")
                        || lexicon::extension(&ext.to_ascii_lowercase()).is_some()) =>
            {
                name = rest;
            }
            _ => break,
        }
    }
    name
}

/// A hex digest, a base32 info hash, or any long letters-and-digits run.
fn is_hash_like(s: &str) -> bool {
    let hex = s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
    let base32 = s.len() == 32 && s.bytes().all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7'));
    let long_mixed = s.len() >= 16
        && s.bytes().all(|b| b.is_ascii_alphanumeric())
        && s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes().any(|b| b.is_ascii_alphabetic());
    hex || base32 || long_mixed
}

/// One short word mixing letters and digits with no meaning: `a8f3x`,
/// `x7k2`. Marker words (`x264`, `1080p`) aren't names either.
fn is_random_word(s: &str) -> bool {
    let word = s.bytes().all(|b| b.is_ascii_alphanumeric());
    let digits = s.bytes().filter(u8::is_ascii_digit).count();
    let letters = s.bytes().filter(u8::is_ascii_alphabetic).count();
    word && (5..=12).contains(&s.len()) && digits >= 2 && letters >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninformative() {
        for name in [
            "a8f3x.zip.part",
            "New Folder (2)",
            "new folder",
            "Complete Season",
            "Season 1",
            "Disc 2",
            "VIDEO_TS",
            "5f2b3c4d6e7f8091a2b3c4d5e6f70819a2b3c4d5",
            "MFRGGZDFMZTWQ2LKNNWG23TPOBYXE43U",
            "1",
            "--",
            "Untitled (3).mkv",
        ] {
            assert!(is_uninformative(name), "{name}");
        }
    }

    #[test]
    fn informative() {
        for name in [
            "Show.Name.S02E05.2160p.WEB-DL.mkv",
            "The Matrix",
            "1917",
            "Video Games Live",
            "Seinfeld Season 1-9 Complete",
            "Up",
            "R2D2",
        ] {
            assert!(!is_uninformative(name), "{name}");
        }
    }
}
