//! The `unauthorised-copy` pack (UC-1 to UC-3) and the malware pack's
//! MW-6 (owner §2.39, `docs/decisions/2026-11-04-X-legal-docs.md`,
//! architecture §20.6). They read the release parser's fields, never a
//! list of names: the pack hides names that follow the conventions of
//! unauthorised copies, and it can't know who holds the rights.
//!
//! - **UC-1**, camcorder, telesync, telecine and screener rips of films:
//!   the parser's source is one of those, and the name carries a marker
//!   the parser's weak-token rule alone doesn't prove. A strong marker
//!   (`HDCAM`, `CAMRip`, `HDTS`, `TELESYNC`, `HDTC`, `DVDSCR`, `screener`,
//!   …) counts anywhere; a weak one (`CAM`, `TS`, `TC`, `SCR`) only in
//!   brackets or after the year. So `Nanny Cam 1080p x264-GRP` and
//!   `Little Ts 720p`, which the parser reads as Cam and Telesync, pass.
//! - **UC-2**, cracked software, and **MW-6**, the same test in the
//!   malware pack: the classifier's `cracked` junk reason (the parser's
//!   flag and a crack word after the title, never `patch` alone,
//!   [`lodestar_classify::crack`]) on a software release. MW-6 has its own
//!   switch, so turning this pack off doesn't restore cracked software.
//! - **UC-3**, scene-style film and TV rips: a film or TV classification,
//!   a disc or web source, and a release group. Exempt: Internet Archive
//!   items (their `_meta.xml` and `_files.xml`), and the built-in allow
//!   list of public-domain and Creative Commons films ([`ALLOW`]). Your
//!   Allow rules skip the packs as they always do.
//!
//! Every marker and crack word is read with neutralized words blanked
//! ([`Ctx::evidence`]), so a CSX-5 *Neutralize term* takes away a match
//! whose only evidence it was, as in the other keyword rules.

use lodestar_classify::{ContentType, JunkReason, crack};
use lodestar_release_parser::Source;

use super::Ctx;

/// UC-1's sources: no lawful public release.
const PRE_RELEASE: &[Source] = &[
    Source::Cam,
    Source::Telesync,
    Source::Telecine,
    Source::Screener,
];

/// Words that prove a camcorder, telesync, telecine or screener source
/// wherever they stand (the parser's strong tokens for those sources).
pub(crate) const STRONG_MARKERS: &[&str] = &[
    "camrip", "hdcam", "hdts", "telesync", "pdvd", "hdtc", "telecine", "screener", "dvdscr",
    "bdscr", "webscr",
];

/// Words that are a title word as often as a source (`Nanny Cam`,
/// `Little Ts`): they count only in brackets or after the year.
pub(crate) const WEAK_MARKERS: &[&str] = &["cam", "ts", "tc", "scr"];

/// UC-3's sources: a disc or a web service.
const DISC_OR_WEB: &[Source] = &[
    Source::BluRay,
    Source::Remux,
    Source::Dvd,
    Source::WebDl,
    Source::WebRip,
];

/// The built-in allow list (UC-3): public-domain and Creative Commons
/// films, by title as [`title_key`] reads it, and the year when the title
/// alone is too common to trust (`None`: any year, or none), which must
/// then match exactly, so a commercial film of the same title a year
/// apart isn't let through.
pub const ALLOW: &[(&str, Option<u16>)] = &[
    // Blender Studio's open films (Creative Commons Attribution).
    ("big buck bunny", None),
    ("elephants dream", None),
    ("sintel", None),
    ("tears of steel", None),
    ("cosmos laundromat", None),
    ("caminandes", None),
    ("caminandes llama drama", None),
    ("caminandes gran dillama", None),
    ("caminandes llamigos", None),
    ("glass half", Some(2015)),
    ("agent 327 operation barbershop", None),
    ("hero", Some(2018)),
    ("spring", Some(2019)),
    ("coffee run", Some(2020)),
    ("sprite fright", None),
    ("charge", Some(2022)),
    ("wing it", Some(2023)),
    // Films in the public domain in the United States.
    ("night of the living dead", Some(1968)),
    ("plan 9 from outer space", None),
    ("nosferatu", Some(1922)),
    ("the cabinet of dr caligari", Some(1920)),
    ("a trip to the moon", Some(1902)),
    ("metropolis", Some(1927)),
    ("the general", Some(1926)),
    ("the kid", Some(1921)),
    ("the gold rush", Some(1925)),
    ("sherlock jr", Some(1924)),
    ("safety last", Some(1923)),
    ("steamboat willie", None),
    ("his girl friday", Some(1940)),
    ("detour", Some(1945)),
    ("charade", Some(1963)),
    ("the last man on earth", Some(1964)),
    ("carnival of souls", Some(1962)),
    ("the little shop of horrors", Some(1960)),
    ("house on haunted hill", Some(1959)),
    ("d o a", Some(1949)),
    ("reefer madness", Some(1936)),
];

/// A title as the allow list keys it: words of letters and digits,
/// lowercased, one space apart.
pub fn title_key(title: &str) -> String {
    crack::words(title).join(" ")
}

/// Whether a parsed title and year are on [`ALLOW`]. Word by word, as
/// [`title_key`] reads the title, without building the key: UC-3 asks it
/// of every scene-style film it would hide.
pub fn allowed(title: &str, year: Option<u16>) -> bool {
    let same = |key: &str| {
        let mut words = title
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty());
        key.split(' ').all(|k| {
            words
                .next()
                .is_some_and(|w| w.chars().flat_map(char::to_lowercase).eq(k.chars()))
        }) && words.next().is_none()
    };
    ALLOW.iter().any(|(t, y)| {
        same(t)
            && match (y, year) {
                (None, _) => true,
                (Some(want), Some(got)) => *want == got,
                (Some(_), None) => false,
            }
    })
}

/// A name's words with whether each stands inside brackets or
/// parentheses: the brackets are read from the name as it is, while
/// the words are the ones [`crate::packs::words`] reads, so the two line
/// up word for word.
fn bracketed_words(name: &str) -> Vec<bool> {
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut in_word = false;
    for c in name.chars() {
        if c.is_alphanumeric() {
            if !in_word {
                out.push(depth > 0);
                in_word = true;
            }
            continue;
        }
        in_word = false;
        match c {
            '[' | '(' | '{' => depth = depth.saturating_add(1),
            ']' | ')' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    out
}

impl Ctx<'_> {
    /// UC-1 (module docs).
    pub(super) fn uc1(&self) -> bool {
        let f = self.facts;
        let r = &f.class.title.release;
        if f.class.content_type != ContentType::Movie
            || !r.source.is_some_and(|s| PRE_RELEASE.contains(&s))
        {
            return false;
        }
        let words = self.name_evidence();
        if words.iter().any(|w| STRONG_MARKERS.contains(&w.as_str()))
            || words
                .windows(2)
                .any(|p| p == ["hd", "cam"] || p == ["hd", "ts"])
        {
            return true;
        }
        let brackets = bracketed_words(self.item.name);
        let year = r
            .year
            .map(|y| y.first.to_string())
            .and_then(|y| words.iter().position(|w| *w == y));
        // A name ending `.ts` that is the name of an MPEG transport stream
        // in the torrent (a broadcast or DVB recording) is its extension,
        // not a telesync: the parser doesn't strip `ts` as one. Only then is
        // that last word skipped, so `Movie.2024.TS` still counts.
        let ts_extension = self.item.name.to_ascii_lowercase().ends_with(".ts")
            && self
                .files
                .iter()
                .any(|(_, e)| e.as_ref().is_some_and(|e| e.as_str() == "ts"));
        let last = words.len().saturating_sub(1);
        words.iter().enumerate().any(|(i, w)| {
            WEAK_MARKERS.contains(&w.as_str())
                && !(ts_extension && i == last && w == "ts")
                && (brackets.get(i).copied().unwrap_or(false) || year.is_some_and(|y| i > y))
        })
    }

    /// UC-2 and MW-6 (module docs): the classifier's `cracked` reason on
    /// a software release, with a crack word still evidence once
    /// neutralized words are blanked.
    pub(super) fn cracked_software(&self) -> bool {
        let f = self.facts;
        if f.class.content_type != ContentType::Software
            || !f.class.junk.reasons.contains(&JunkReason::Cracked)
        {
            return false;
        }
        if self.neutral.is_empty() {
            return true;
        }
        let title = &f.class.title.release.title;
        // The title's words blanked the same way, so a neutralized word
        // in the title takes nothing away from another word's evidence.
        let in_title = crack::crack_words_in(&self.evidence(crack::words(title)), title);
        // The text the release was parsed from, as the classifier read it:
        // the name, or the file or folder an uninformative name was
        // resolved from.
        match f.class.title.resolved_from.as_deref() {
            None => crack::crack_words_in(self.name_evidence(), self.item.name) > in_title,
            Some(from) => {
                crack::crack_words_in(&self.evidence(crack::words(from)), from) > in_title
            }
        }
    }

    /// UC-3 (module docs).
    pub(super) fn uc3(&self) -> bool {
        let f = self.facts;
        let r = &f.class.title.release;
        matches!(f.class.content_type, ContentType::Movie | ContentType::Tv)
            && r.source.is_some_and(|s| DISC_OR_WEB.contains(&s))
            && r.group.is_some()
            && !allowed(&r.title, r.year.map(|y| y.first))
            && !self.internet_archive()
    }

    /// An Internet Archive item: its `<id>_meta.xml` and `<id>_files.xml`,
    /// with the same id, at any depth.
    ///
    /// Linear in the files (at most [`crate::MAX_FILES`]): a torrent of
    /// many `_meta.xml` names costs one pass and one set, never a pass
    /// per name, and only a name with one of the two endings is copied.
    fn internet_archive(&self) -> bool {
        // The id before an ASCII `suffix`, any case, lowercased.
        let id_of = |name: &str, suffix: &str| -> Option<String> {
            let at = name.len().checked_sub(suffix.len()).filter(|at| *at > 0)?;
            let (id, end) = (name.get(..at)?, name.get(at..)?);
            end.eq_ignore_ascii_case(suffix)
                .then(|| id.to_ascii_lowercase())
        };
        let mut files_ids = std::collections::HashSet::new();
        let mut meta_ids = Vec::new();
        for (f, _) in &self.files {
            let name = f.name();
            if let Some(id) = id_of(name, "_files.xml") {
                files_ids.insert(id);
            } else if let Some(id) = id_of(name, "_meta.xml") {
                meta_ids.push(id);
            }
        }
        meta_ids.iter().any(|id| files_ids.contains(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_plain_words() {
        for w in STRONG_MARKERS.iter().chain(WEAK_MARKERS) {
            assert!(w.bytes().all(|b| b.is_ascii_lowercase()), "{w}");
            assert_eq!(crate::matcher::neutral_key(w), *w);
        }
        for p in crack::CRACK_WORDS {
            for w in *p {
                assert!(w.bytes().all(|b| b.is_ascii_lowercase()), "{w}");
            }
        }
    }

    #[test]
    fn brackets_line_up_with_words() {
        let name = "Movie (2023) [TS] x264-GRP";
        assert_eq!(
            crate::packs::words(name),
            ["movie", "2023", "ts", "x264", "grp"]
        );
        assert_eq!(bracketed_words(name), [false, true, true, false, false]);
        assert_eq!(bracketed_words("a ] b [c"), [false, false, true]);
    }

    #[test]
    fn internet_archive_items() {
        use lodestar_classify::{FileInfo, classify};
        let ia = |paths: &[&str]| {
            let f: Vec<FileInfo> = paths
                .iter()
                .map(|p| FileInfo::new(p.split('/').map(str::to_owned).collect(), 2_000))
                .collect();
            let class = classify("Example.Film.1950.DVDRip.XviD-GRP", &f);
            let facts = crate::matcher::Facts {
                class: &class,
                size: 0,
                piece_len: 0,
                flags: 0,
                discovered_at: 0,
                now: 0,
            };
            let item = crate::core::Item {
                hashes: &[[0; 20]],
                name: "Example.Film.1950.DVDRip.XviD-GRP",
                files: &f,
                content_type: class.content_type,
            };
            Ctx::new(&item, &facts, &[]).internet_archive()
        };
        assert!(ia(&["film.avi", "film_meta.xml", "FILM_files.xml"]));
        assert!(ia(&["a/film_files.xml", "b/film_meta.xml"]));
        assert!(
            !ia(&["film.avi", "film_meta.xml", "other_files.xml"]),
            "ids differ"
        );
        assert!(!ia(&["film.avi", "film_meta.xml"]));
        assert!(!ia(&["_meta.xml", "_files.xml"]), "no id");
        assert!(!ia(&["é_meta.xml", "x"]));
        // Many look-alikes are one pass.
        let many: Vec<String> = (0..1_000).map(|i| format!("{i}_meta.xml")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(!ia(&refs));
    }

    #[test]
    fn the_allow_list() {
        assert!(allowed("Big Buck Bunny", None));
        assert!(allowed("Big.Buck Bunny", Some(2008)));
        assert!(allowed("Night of the Living Dead", Some(1968)));
        assert!(
            !allowed("Night of the Living Dead", Some(1969)),
            "exact year"
        );
        assert!(
            !allowed("Night of the Living Dead", Some(1990)),
            "the remake"
        );
        assert!(!allowed("Spring", None), "a common title needs its year");
        assert!(!allowed("Big Buck Bunny Returns", None));
        for (t, _) in ALLOW {
            assert_eq!(title_key(t), *t, "{t} is written as its key");
        }
    }
}
