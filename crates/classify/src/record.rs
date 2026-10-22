//! The stored form of a classification (`torrent_parsed.data`, §7): what
//! the index and the UI read back without classifying again.
//!
//! A header line, then one `key<TAB>value` line per field. Fields at
//! their usual value (not ambiguous, full title confidence, no junk, no
//! structure, depth 0, one extension) are left out, which halves a typical
//! record. Release fields keep [`Release::fields`]' keys under an `r.`
//! prefix, except alternative titles, which get one `r.alt` line each so a
//! comma in a title survives. Values have control characters replaced by
//! spaces, so a line is always one field. Decoding is strict: an unknown
//! key or a bad value is an error.
//!
//! Any change to this format bumps [`CLASSIFY_VERSION`], so stored records
//! are derived again rather than left unreadable.
//!
//! [`Release::fields`]: lodestar_release_parser::Release::fields

use lodestar_release_parser::PARSER_VERSION;

use crate::{
    AdultKind, CLASSIFY_VERSION, Classification, ContentType, Junk, JunkReason, ResolvedVia,
    Structure,
};

const HEADER: &str = "e1";
/// Values a record leaves out.
const TITLE_CONFIDENCE: u8 = 100;
const DEPTH: u16 = 0;
const EXTENSIONS: u16 = 1;

/// The version stored with each record (`torrent_parsed.parser_ver`).
/// Any parser or classifier change makes every stored record outdated, so
/// it's derived again in the background.
pub const ENRICHMENT_VERSION: i64 = ((PARSER_VERSION as i64) << 16) | CLASSIFY_VERSION as i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The effective release, as `(key, value)` in [`Release::fields`]
    /// order, with each alternative title as its own `alt` entry.
    ///
    /// [`Release::fields`]: lodestar_release_parser::Release::fields
    pub release: Vec<(String, String)>,
    pub ambiguous: bool,
    pub resolved: Option<ResolvedVia>,
    pub title_confidence: u8,
    pub content_type: ContentType,
    pub type_confidence: u8,
    /// Set exactly when the type is adult.
    pub adult_kind: Option<AdultKind>,
    pub junk: Junk,
    pub structure: Structure,
    pub depth: u16,
    pub distinct_extensions: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError(pub String);

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid enrichment record: {}", self.0)
    }
}

impl std::error::Error for RecordError {}

impl Record {
    pub fn new(c: &Classification) -> Record {
        let r = &c.title.release;
        let mut release: Vec<(String, String)> = Vec::new();
        for (k, v) in r.fields() {
            if k == "alt" {
                release.extend(r.alt_titles.iter().map(|a| ("alt".to_owned(), a.clone())));
            } else {
                release.push((k.to_owned(), v));
            }
        }
        Record {
            release,
            ambiguous: c.title.ambiguous,
            resolved: c.title.resolved,
            title_confidence: c.title.confidence,
            content_type: c.content_type,
            type_confidence: c.confidence,
            adult_kind: c.adult_kind,
            junk: c.junk.clone(),
            structure: c.structure,
            depth: c.depth,
            distinct_extensions: c.distinct_extensions,
        }
    }

    /// The first value of a release field (`title`, `res`, `year`, …).
    pub fn release(&self, key: &str) -> Option<&str> {
        self.release
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every value of a release field (several for `alt`).
    pub fn release_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.release
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::from(HEADER);
        let mut line = |k: &str, v: &str| {
            out.push('\n');
            out.push_str(k);
            out.push('\t');
            out.extend(v.chars().map(|c| if c.is_control() { ' ' } else { c }));
        };
        line("type", self.content_type.as_str());
        line("type_conf", &self.type_confidence.to_string());
        if let Some(k) = self.adult_kind {
            line("adult", k.as_str());
        }
        if self.ambiguous {
            line("ambiguous", "yes");
        }
        if let Some(r) = self.resolved {
            line("resolved", r.as_str());
        }
        if self.title_confidence != TITLE_CONFIDENCE {
            line("title_conf", &self.title_confidence.to_string());
        }
        if self.junk.score != 0 {
            line("junk", &self.junk.score.to_string());
        }
        if !self.junk.reasons.is_empty() {
            let reasons: Vec<&str> = self.junk.reasons.iter().map(|r| r.as_str()).collect();
            line("reasons", &reasons.join(","));
        }
        if self.structure != Structure::empty() {
            let structure: Vec<&str> = self.structure.names().collect();
            line("structure", &structure.join(","));
        }
        if self.depth != DEPTH {
            line("depth", &self.depth.to_string());
        }
        if self.distinct_extensions != EXTENSIONS {
            line("exts", &self.distinct_extensions.to_string());
        }
        for (k, v) in &self.release {
            line(&format!("r.{k}"), v);
        }
        out.into_bytes()
    }

    pub fn decode(data: &[u8]) -> Result<Record, RecordError> {
        let bad = |what: &str| RecordError(what.to_owned());
        let text = std::str::from_utf8(data).map_err(|_| bad("not UTF-8"))?;
        let mut lines = text.split('\n');
        if lines.next() != Some(HEADER) {
            return Err(bad("unknown header"));
        }
        let mut r = Record {
            release: Vec::new(),
            ambiguous: false,
            resolved: None,
            title_confidence: TITLE_CONFIDENCE,
            content_type: ContentType::Other,
            type_confidence: 0,
            adult_kind: None,
            junk: Junk::default(),
            structure: Structure::empty(),
            depth: DEPTH,
            distinct_extensions: EXTENSIONS,
        };
        let mut seen_type = false;
        for line in lines {
            let (k, v) = line
                .split_once('\t')
                .ok_or_else(|| bad("a line without a tab"))?;
            let num = |what: &str| v.parse::<u64>().map_err(|_| bad(what));
            match k {
                "type" => {
                    r.content_type = ContentType::from_name(v).ok_or_else(|| bad("type"))?;
                    seen_type = true;
                }
                "type_conf" => {
                    r.type_confidence =
                        percent(num("type_conf")?).ok_or_else(|| bad("type_conf"))?
                }
                "adult" => {
                    r.adult_kind = Some(AdultKind::from_name(v).ok_or_else(|| bad("adult"))?)
                }
                // Only written when set.
                "ambiguous" if v == "yes" => r.ambiguous = true,
                "resolved" => {
                    r.resolved = Some(ResolvedVia::from_name(v).ok_or_else(|| bad("resolved"))?)
                }
                "title_conf" => {
                    r.title_confidence =
                        percent(num("title_conf")?).ok_or_else(|| bad("title_conf"))?
                }
                "junk" => r.junk.score = percent(num("junk")?).ok_or_else(|| bad("junk"))?,
                "reasons" => {
                    for key in v.split(',').filter(|k| !k.is_empty()) {
                        r.junk
                            .reasons
                            .push(JunkReason::from_key(key).ok_or_else(|| bad("junk reason"))?);
                    }
                }
                "structure" => {
                    for name in v.split(',').filter(|k| !k.is_empty()) {
                        r.structure
                            .insert(Structure::from_name(name).ok_or_else(|| bad("structure"))?);
                    }
                }
                "depth" => r.depth = u16::try_from(num("depth")?).map_err(|_| bad("depth"))?,
                "exts" => {
                    r.distinct_extensions = u16::try_from(num("exts")?).map_err(|_| bad("exts"))?
                }
                _ => match k.strip_prefix("r.") {
                    Some(field) if !field.is_empty() => {
                        r.release.push((field.to_owned(), v.to_owned()))
                    }
                    _ => return Err(bad("unknown key")),
                },
            }
        }
        if !seen_type {
            return Err(bad("no type"));
        }
        if (r.content_type == ContentType::Adult) != r.adult_kind.is_some() {
            return Err(bad("adult kind"));
        }
        Ok(r)
    }
}

fn percent(v: u64) -> Option<u8> {
    u8::try_from(v).ok().filter(|&p| p <= 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileInfo, classify};

    fn files(paths: &[(&str, u64)]) -> Vec<FileInfo> {
        paths
            .iter()
            .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
            .collect()
    }

    #[test]
    fn round_trip() {
        let f = files(&[
            ("Movie.2019.1080p.mkv", 4_000_000_000),
            ("Movie.2019.1080p.mkv.exe", 1_000),
            ("Sample/sample.mkv", 10),
        ]);
        let c = classify("Мастер / Master, The\t(2019) 1080p", &f);
        let r = Record::new(&c);
        assert!(r.release_all("alt").any(|a| a == "Master, The"), "{r:?}");
        let back = Record::decode(&r.encode()).unwrap();
        assert_eq!(back, r);
        assert!(!r.junk.reasons.is_empty());
    }

    #[test]
    fn adult_round_trip() {
        let f = files(&[("Site.24.01.15.Jane.Doe.XXX.1080p.mp4", 1_500_000_000)]);
        let r = Record::new(&classify("Site.24.01.15.Jane.Doe.XXX.1080p.MP4-GRP", &f));
        assert_eq!(r.content_type, ContentType::Adult);
        assert_eq!(r.adult_kind, Some(AdultKind::Scene));
        let text = String::from_utf8(r.encode()).unwrap();
        assert!(text.contains("\nadult\tscene"), "{text:?}");
        assert_eq!(Record::decode(text.as_bytes()).unwrap(), r);
    }

    #[test]
    fn usual_values_are_left_out() {
        let f = files(&[("Movie.2019.1080p.x264-GRP.mkv", 4_000_000_000)]);
        let r = Record::new(&classify("Movie.2019.1080p.x264-GRP", &f));
        let text = String::from_utf8(r.encode()).unwrap();
        for key in [
            "ambiguous",
            "title_conf",
            "junk",
            "reasons",
            "structure",
            "depth",
            "exts",
        ] {
            assert!(!text.contains(&format!("\n{key}\t")), "{key} in {text:?}");
        }
        assert_eq!(Record::decode(text.as_bytes()).unwrap(), r);
    }

    #[test]
    fn control_characters_cannot_break_lines() {
        let f = files(&[("a\nb.mkv", 1)]);
        let c = classify("Evil\nkey\tvalue 2019", &f);
        let r = Record::new(&c);
        let back = Record::decode(&r.encode()).unwrap();
        assert!(back.release("title").is_some_and(|t| !t.contains('\n')));
    }

    #[test]
    fn strict() {
        for bad in [
            &b""[..],
            b"e9\ntype\tmovie",
            b"e1\njunk\t5",
            b"e1\ntype\tmovie\nwhat\t1",
            b"e1\ntype\tmovie\njunk\t101",
            b"e1\ntype\tmovie\nreasons\tnope",
            b"e1\ntype\tmovie\nambiguous\tno",
            b"e1\ntype\tmovie\nno tab",
            b"e1\ntype\tadult",
            b"e1\ntype\tmovie\nadult\tscene",
            b"e1\ntype\tadult\nadult\tnope",
            b"\xff",
        ] {
            assert!(
                Record::decode(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
