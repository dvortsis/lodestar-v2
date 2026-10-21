//! The stored form of a classification (`torrent_parsed.data`, §7): what
//! the index and the UI read back without classifying again.
//!
//! A header line, then one `key<TAB>value` line per field. Release fields
//! keep [`Release::fields`]' keys under an `r.` prefix, except alternative
//! titles, which get one `r.alt` line each so a comma in a title survives.
//! Values have control characters replaced by spaces, so a line is always
//! one field. Decoding is strict: an unknown key or a bad value is an
//! error, and the caller derives the record again.
//!
//! [`Release::fields`]: lodestar_release_parser::Release::fields

use lodestar_release_parser::PARSER_VERSION;

use crate::{
    CLASSIFY_VERSION, Classification, ContentType, FileInfo, Junk, JunkReason, ResolvedVia,
    Structure,
};

const HEADER: &str = "lodestar-enrichment 1";

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
    pub junk: Junk,
    pub structure: Structure,
    /// Size of the largest file.
    pub largest: u64,
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
    pub fn new(c: &Classification, files: &[FileInfo]) -> Record {
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
            junk: c.junk.clone(),
            structure: c.structure,
            largest: c.largest.map_or(0, |i| files[i].size),
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
        line("ambiguous", if self.ambiguous { "yes" } else { "no" });
        if let Some(r) = self.resolved {
            line("resolved", r.as_str());
        }
        line("title_conf", &self.title_confidence.to_string());
        line("junk", &self.junk.score.to_string());
        let reasons: Vec<&str> = self.junk.reasons.iter().map(|r| r.as_str()).collect();
        line("reasons", &reasons.join(","));
        let structure: Vec<&str> = self.structure.names().collect();
        line("structure", &structure.join(","));
        line("largest", &self.largest.to_string());
        line("depth", &self.depth.to_string());
        line("exts", &self.distinct_extensions.to_string());
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
            title_confidence: 0,
            content_type: ContentType::Other,
            type_confidence: 0,
            junk: Junk::default(),
            structure: Structure::empty(),
            largest: 0,
            depth: 0,
            distinct_extensions: 0,
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
                "ambiguous" => {
                    r.ambiguous = match v {
                        "yes" => true,
                        "no" => false,
                        _ => return Err(bad("ambiguous")),
                    }
                }
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
                "largest" => r.largest = num("largest")?,
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
        Ok(r)
    }
}

fn percent(v: u64) -> Option<u8> {
    u8::try_from(v).ok().filter(|&p| p <= 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify;

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
        let r = Record::new(&c, &f);
        assert_eq!(r.largest, 4_000_000_000);
        assert!(r.release_all("alt").any(|a| a == "Master, The"), "{r:?}");
        let back = Record::decode(&r.encode()).unwrap();
        assert_eq!(back, r);
        assert!(!r.junk.reasons.is_empty());
    }

    #[test]
    fn control_characters_cannot_break_lines() {
        let f = files(&[("a\nb.mkv", 1)]);
        let c = classify("Evil\nkey\tvalue 2019", &f);
        let r = Record::new(&c, &f);
        let back = Record::decode(&r.encode()).unwrap();
        assert!(back.release("title").is_some_and(|t| !t.contains('\n')));
    }

    #[test]
    fn strict() {
        for bad in [
            &b""[..],
            b"lodestar-enrichment 9\ntype\tmovie",
            b"lodestar-enrichment 1\njunk\t5",
            b"lodestar-enrichment 1\ntype\tmovie\nwhat\t1",
            b"lodestar-enrichment 1\ntype\tmovie\njunk\t101",
            b"lodestar-enrichment 1\ntype\tmovie\nreasons\tnope",
            b"lodestar-enrichment 1\ntype\tmovie\nno tab",
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
