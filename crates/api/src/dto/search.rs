//! `/parse` and `/search` (§8, §9), and the query AST they carry.

use lodestar_search::query::Node;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};

/// `GET /parse?q=`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct ParseQuery {
    /// The query text; empty matches everything.
    pub q: String,
}

/// `GET /parse`: the AST and the canonical text it prints as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ParseResponse {
    #[serde(serialize_with = "super::sorted")]
    #[schema(value_type = AstNode)]
    pub ast: Node,
    pub canonical: String,
}

/// `POST /search`'s body, and `GET /search`'s parameters (where `ast` is
/// refused: send an AST with POST).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SearchRequest {
    /// The query text. Send `q` or `ast`, not both.
    pub q: Option<String>,
    /// A query AST, as `/parse` returns it; it must be one the parser
    /// could produce.
    #[schema(value_type = Option<AstNode>)]
    pub ast: Option<Node>,
    /// `relevance` (the default), or `discovered`, `size` or `files` with
    /// an optional `+` (ascending) or `-` (descending, the default).
    #[schema(example = "-discovered")]
    pub sort: Option<String>,
    /// Results per page: 1–200, 50 by default.
    pub limit: Option<usize>,
    /// `offset + limit` may not pass 10,000. Not with `cursor`.
    pub offset: Option<usize>,
    /// Include torrents a filter rule hid (§20.3).
    pub hidden: Option<bool>,
    /// The previous page's `next_cursor`, to read the next page from the
    /// same index snapshot. Send the same `q` (or `ast`), `sort` and
    /// `hidden` with it.
    pub cursor: Option<String>,
    /// Count every match exactly, even above 100,000 (within a time
    /// budget; `total_approx` says whether it made it). `1` or `true`.
    #[serde(deserialize_with = "flag")]
    #[schema(value_type = Option<bool>)]
    pub exact: Option<bool>,
    /// Explain how each hit was scored (`explain` in the response): the
    /// first page only (no `offset` or `cursor`), and at most 100 hits,
    /// so `limit` is capped at 100. `1` or `true`.
    #[serde(deserialize_with = "flag")]
    #[schema(value_type = Option<bool>)]
    pub explain: Option<bool>,
}

/// A boolean that a query string may also spell `1` or `0`.
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    struct Flag;
    impl<'de> serde::de::Visitor<'de> for Flag {
        type Value = Option<bool>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("true, false, 1 or 0")
        }

        fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
            Ok(Some(v))
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            match v {
                "true" | "1" => Ok(Some(true)),
                "false" | "0" => Ok(Some(false)),
                _ => Err(E::invalid_value(serde::de::Unexpected::Str(v), &self)),
            }
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<E: Deserializer<'de>>(self, d: E) -> Result<Self::Value, E::Error> {
            d.deserialize_any(Flag)
        }
    }
    d.deserialize_any(Flag)
}

/// `GET`/`POST /search`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchResponse {
    /// The query as the server understood it.
    pub canonical: String,
    /// Present only when asked for with `explain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub explain: Option<SearchExplain>,
    pub hits: Vec<SearchHit>,
    pub limit: usize,
    /// Pass as `cursor` for the next page, read from the same index
    /// snapshot (§8); null on the last page. Opaque, URL-safe, at most
    /// 256 characters, valid for 10 minutes after its last use.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// What the server left out of these results, for the UI to say:
    /// `adult_hidden` when the query asks for Adult (`type:adult`,
    /// `adult:…`) while "Show adult content" is off, so it matched
    /// nothing adult (§20.9).
    pub notices: Vec<SearchNotice>,
    /// Hits before this page.
    pub offset: usize,
    /// Few exact matches: plural forms in titles were included too (§8),
    /// ranked after the exact ones.
    pub stemmed: bool,
    pub took_ms: f64,
    /// Matches in all, not just this page.
    pub total: usize,
    /// `total` is an estimate (above 100,000 matches; shown with "≈");
    /// ask with `exact` for the exact count.
    pub total_approx: bool,
}

/// A note on what a search left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchNotice {
    /// The query asks for Adult, which is hidden: turn on "Show adult
    /// content" in Settings to see it.
    AdultHidden,
}

/// One search result: the torrent summary (`docs/metrics.md` §7.1), its
/// relevance and, for a query with a file clause, the files that matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchHit {
    /// `scene`, `movie`, `photos`, `comics`, `game`, `vr` or `audio`;
    /// set only when `content_type` is `adult`.
    #[schema(required = true)]
    pub adult_kind: Option<String>,
    /// Null = never scraped.
    #[schema(required = true)]
    pub completed: Option<u32>,
    /// What the torrent is made of, by colour group, in the composition
    /// bar's order; groups without files are left out, and it's empty
    /// before classification.
    pub composition: Vec<CompositionShare>,
    /// `movie`, `tv`, `music`, `audiobook`, `ebook`, `comic`, `software`,
    /// `game`, `images`, `video`, `other` or `adult`; null = not
    /// classified.
    #[schema(required = true)]
    pub content_type: Option<String>,
    pub discovered_at: i64,
    pub files: u64,
    #[schema(example = json!(["single_file"]))]
    pub flags: Vec<String>,
    /// 40 hex digits.
    pub hash: String,
    /// 64 hex digits; null for a v1-only torrent.
    #[schema(required = true)]
    pub hash_v2: Option<String>,
    pub id: i64,
    /// Stable reason keys (`docs/metrics.md` §7.1); empty when there are
    /// none or before classification.
    #[schema(example = json!(["double-extension"]))]
    pub junk_reasons: Vec<String>,
    /// 0–100; null = not classified.
    #[schema(required = true)]
    pub junk_score: Option<u8>,
    /// Null = never scraped.
    #[schema(required = true)]
    pub last_scrape_at: Option<i64>,
    /// Null = never scraped.
    #[schema(required = true)]
    pub leechers: Option<u32>,
    /// Positions in the torrent's file list; present only when the query
    /// has a file clause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub matched_files: Option<Vec<u64>>,
    pub metadata_at: i64,
    pub name: String,
    pub piece_len: u64,
    /// How the title was found when the name wasn't informative:
    /// `files`, `folder` or `structure`; null = from the name itself, or
    /// not classified.
    #[schema(required = true)]
    pub resolved_via: Option<String>,
    /// Relevance; 0 under any sort but `relevance`.
    pub score: f64,
    /// Null = never scraped.
    #[schema(required = true)]
    pub seeders: Option<u32>,
    pub size: u64,
    /// The resolved title; null = not resolved.
    #[schema(required = true)]
    pub title: Option<String>,
    /// Trackers that answered in the last answered round; null = never
    /// scraped.
    #[schema(required = true)]
    pub trackers_checked: Option<u32>,
    /// Of those, trackers that reported anyone; null = never scraped.
    #[schema(required = true)]
    pub trackers_reporting: Option<u32>,
}

/// How a first page was ranked (`?explain=1`, architecture §8). Field
/// names are stable. Relevance is `phase1 + phase2.name + phase2.title`
/// (to float rounding); the other features and the priors aren't part of
/// ranking yet and are null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SearchExplain {
    /// One per hit, in the hits' order.
    pub hits: Vec<ExplainHit>,
    /// `relevance`, or the field the hits are sorted by (`discovered`,
    /// `size`, `files`), when no hit has relevance parts.
    pub ranked_by: String,
    /// Whether phase 2 re-ranked the phase-1 candidates: the query has
    /// unfielded words.
    pub two_phase: bool,
    pub weights: ExplainWeights,
}

/// The ranking's weights, as this server runs them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ExplainWeights {
    /// Boost of a word found in the name, in phase 1 (on the combined
    /// field) and phase 2.
    pub name: f64,
    /// Boost of a word found in a file or folder name, in phase 1.
    pub path: f64,
    /// Phase-1 candidates re-ranked together.
    pub rerank_window: usize,
    /// Phase-1 score of a word matched only by its stem in the title
    /// (the plurals fallback, when `stemmed`).
    pub stem_score: f64,
    /// Phase-2 boost of a word found in the parsed title.
    pub title: f64,
}

/// One hit's score in parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ExplainHit {
    pub id: i64,
    /// BM25 over the name and paths (`text`), fielded words and
    /// filters; null under a field sort.
    #[schema(required = true)]
    pub phase1: Option<f64>,
    /// Null under a field sort.
    #[schema(required = true)]
    pub phase2: Option<ExplainPhase2>,
    pub priors: ExplainPriors,
    /// The score the hit was ranked by, as in `hits[].score`.
    pub score: f64,
}

/// Phase 2's features (§8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ExplainPhase2 {
    /// Exact-name bonus: not ranked yet, always null.
    #[schema(required = true)]
    pub exact_name: Option<f64>,
    /// The cap on file-only matches' contribution: not ranked yet,
    /// always null.
    #[schema(required = true)]
    pub file_cap: Option<f64>,
    /// The unfielded words' name boosts, summed (0 without phase 2).
    pub name: f64,
    /// Phrase and proximity bonus: not ranked yet, always null.
    #[schema(required = true)]
    pub phrase: Option<f64>,
    /// The unfielded words' parsed-title boosts, summed.
    pub title: f64,
}

/// The quality priors (§8), multiplied onto relevance once ranking has
/// them (with swarm health, M4); until then each is null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ExplainPriors {
    #[schema(required = true)]
    pub freshness: Option<f64>,
    #[schema(required = true)]
    pub junk: Option<f64>,
    #[schema(required = true)]
    pub seeders: Option<f64>,
    #[schema(required = true)]
    pub swarm_consistency: Option<f64>,
    #[schema(required = true)]
    pub trackers: Option<f64>,
}

/// One colour group's share of a torrent (`docs/metrics.md` §4, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompositionShare {
    /// Bytes in the group, padding excluded.
    pub bytes: u64,
    /// Files in the group, padding excluded.
    pub files: u32,
    /// `video`, `audio`, `images`, `documents`, `archives`, `software` or
    /// `other`.
    pub group: String,
    /// Whole percent of the non-padding bytes: any non-zero share is at
    /// least 1, and only a complete one is 100.
    pub pct_bytes: u8,
    /// The same, of the non-padding files.
    pub pct_files: u8,
}

// The AST's schema. `Node` lives in `lodestar-search`, which knows
// nothing of OpenAPI; these mirror its serde shape for the schema only,
// and a test round-trips every kind of node through them.

/// A parsed query (§8), tagged by `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum AstNode {
    /// Matches everything (an empty query: browse).
    All,
    And {
        #[schema(no_recursion)]
        of: Vec<AstNode>,
    },
    Or {
        #[schema(no_recursion)]
        of: Vec<AstNode>,
    },
    Not {
        #[schema(no_recursion)]
        of: Box<AstNode>,
    },
    /// A word, tokenized when compiled.
    Word {
        field: AstTextField,
        word: String,
    },
    Phrase {
        field: AstTextField,
        text: String,
    },
    Near {
        field: AstTextField,
        words: Vec<String>,
        slop: u32,
    },
    Prefix {
        field: AstTextField,
        prefix: String,
    },
    Fuzzy {
        field: AstTextField,
        word: String,
        distance: u8,
    },
    /// Inclusive bounds; null is open.
    Range {
        field: AstNumField,
        #[schema(required = true)]
        lo: Option<u64>,
        #[schema(required = true)]
        hi: Option<u64>,
    },
    /// Inclusive bounds; null is open.
    Date {
        field: AstDateField,
        #[schema(required = true)]
        from: Option<AstWhen>,
        #[schema(required = true)]
        to: Option<AstWhen>,
    },
    /// File extensions; matches any.
    Ext {
        any: Vec<String>,
    },
    Flag {
        name: String,
        value: bool,
    },
    /// 40 lowercase hex digits.
    Hash {
        hash: String,
    },
    /// A keyword facet; matches any of the values.
    Tag {
        key: AstTagKey,
        any: Vec<String>,
    },
    /// Torrents with at least one file matching every clause of `of`.
    File {
        #[schema(no_recursion)]
        of: Box<AstNode>,
    },
    /// A category's share of the files or bytes, inclusive percentages.
    Share {
        by: AstShareBy,
        cat: String,
        lo: u8,
        hi: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AstTextField {
    Any,
    Name,
    Path,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AstNumField {
    Size,
    Files,
    Largest,
    Piece,
    Year,
    Season,
    Episode,
    Junk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AstTagKey {
    Type,
    Res,
    Hdr,
    Codec,
    Audio,
    Lang,
    Group,
    Has,
    Resolved,
    Adult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AstShareBy {
    Count,
    Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AstDateField {
    Discovered,
}

/// One end of a date range: seconds before the search (`ago`), or a Unix
/// time (`at`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AstWhen {
    Ago(i64),
    At(i64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_search::parse;
    use serde_json::Value;

    /// Every kind of node, nested.
    const QUERIES: &[&str] = &[
        "",
        "(dark | knight) -cam \"dark knight\" dark NEAR/3 knight comput* ~knigth",
        "name:dark path:sub title:\"dark knight\"",
        "size:700MB..1.5GB files:>=10 largest:<1GB piece:16MB year:2019..2021 s:2 e:>5 junk:<20",
        "discovered:<7d discovered:2026-01-01..2026-03-31",
        "ext:mkv,mp4 private:no hidden:yes",
        "38a68a3bbc7cdb158e44c404c52c7d1ea6aa2cff",
        "type:movie,tv res:4K hdr:dv codec:x265 audio:DD+ lang:fr group:NTb has:nfo resolved:files adult:scene",
        "file:(ext:flac size:>50MB) -file:sample",
        "cat:video>=80% bytes:video=10..50",
    ];

    #[test]
    fn the_ast_schema_mirrors_every_node() {
        let mut ops = std::collections::BTreeSet::new();
        for q in QUERIES {
            let node = parse(q).unwrap_or_else(|e| panic!("{q}: {e:?}"));
            let json = serde_json::to_value(&node).unwrap();
            collect_ops(&json, &mut ops);
            let mirror: AstNode =
                serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("{q}: {e}\n{json}"));
            assert_eq!(serde_json::to_value(&mirror).unwrap(), json, "{q}");
            // And back into the real thing.
            let again: Node =
                serde_json::from_value(serde_json::to_value(&mirror).unwrap()).unwrap();
            assert_eq!(again, node, "{q}");
        }
        let all = [
            "all", "and", "date", "ext", "file", "flag", "fuzzy", "hash", "near", "not", "or",
            "phrase", "prefix", "range", "share", "tag", "word",
        ];
        assert_eq!(ops.into_iter().collect::<Vec<_>>(), all.map(String::from));
    }

    fn collect_ops(v: &Value, ops: &mut std::collections::BTreeSet<String>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(op)) = m.get("op") {
                    ops.insert(op.clone());
                }
                m.values().for_each(|v| collect_ops(v, ops));
            }
            Value::Array(a) => a.iter().for_each(|v| collect_ops(v, ops)),
            _ => {}
        }
    }
}
