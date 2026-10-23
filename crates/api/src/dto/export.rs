//! `/export` (§9, §10.4): a search, a selection or a collection as a
//! CSV, NDJSON or magnet-list download.

use lodestar_search::query::Node;

use super::search::AstNode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `GET /export`'s parameters and `POST /export`'s body. Name one
/// source: a query (`q` or `ast`, with `sort` and `hidden` as `/search`
/// takes them; neither exports everything), `hashes` or `collection`.
/// `ast` and `hashes` need POST.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ExportRequest {
    /// The query text, as `/search` takes it.
    pub q: Option<String>,
    /// A query AST, as `/parse` returns it (POST only).
    #[schema(value_type = Option<AstNode>)]
    pub ast: Option<Node>,
    /// As `/search`'s; only with a query. Selections and collections
    /// keep their own order.
    #[schema(example = "-discovered")]
    pub sort: Option<String>,
    /// Include torrents a filter rule hid (§20.3).
    pub hidden: Option<bool>,
    /// Up to 10,000 info hashes (40 hex digits), such as the bulk bar's
    /// selection (POST only). Repeats are exported once, in the order
    /// first given.
    pub hashes: Option<Vec<String>>,
    /// A collection's id: its torrents in the collection's order.
    pub collection: Option<i64>,
    /// `csv` (the default), `json` (NDJSON) or `magnets`.
    pub format: Option<ExportFormat>,
    /// A comma-separated subset of [`ExportField`] names, written in
    /// that type's order whatever the order given. Every field but
    /// `magnet` by default; `magnets` takes none.
    #[schema(example = "hash,name,size,seeders")]
    pub fields: Option<String>,
}

/// An export's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// RFC 4180 (`text/csv`, CRLF line ends) with a header row. Text
    /// cells that start with `=`, `+`, `-`, `@`, a tab or a CR get a `'`
    /// in front, so a spreadsheet doesn't run them; control and
    /// bidirectional-formatting characters are written `\u{…}`. Lists
    /// are joined with `;`; `composition` is `group:pct_bytes` pairs.
    /// Null (such as a never-scraped swarm count) is an empty cell.
    Csv,
    /// NDJSON (`application/x-ndjson`): one object per line with the
    /// chosen fields (keys sorted, null kept as `null`), then a last
    /// line `{"trailer":{"cap","rows","skipped","truncated"}}`. A file
    /// without the trailer is incomplete.
    Json,
    /// One magnet link per line (`text/plain`), exactly as the detail
    /// view's `magnet`.
    Magnets,
}

/// A field an export can write: the torrent summary of
/// `docs/metrics.md` §7.1 (with its null rules), less the search-only
/// `score` and `matched_files`, plus the detail view's `magnet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportField {
    Id,
    Hash,
    HashV2,
    Name,
    Title,
    ResolvedVia,
    Size,
    Files,
    PieceLen,
    DiscoveredAt,
    MetadataAt,
    Flags,
    ContentType,
    AdultKind,
    Composition,
    JunkScore,
    JunkReasons,
    Seeders,
    Leechers,
    Completed,
    TrackersReporting,
    TrackersChecked,
    LastScrapeAt,
    Magnet,
}

impl ExportField {
    /// Every field, in the order an export writes them.
    pub const ALL: [ExportField; 24] = [
        ExportField::Id,
        ExportField::Hash,
        ExportField::HashV2,
        ExportField::Name,
        ExportField::Title,
        ExportField::ResolvedVia,
        ExportField::Size,
        ExportField::Files,
        ExportField::PieceLen,
        ExportField::DiscoveredAt,
        ExportField::MetadataAt,
        ExportField::Flags,
        ExportField::ContentType,
        ExportField::AdultKind,
        ExportField::Composition,
        ExportField::JunkScore,
        ExportField::JunkReasons,
        ExportField::Seeders,
        ExportField::Leechers,
        ExportField::Completed,
        ExportField::TrackersReporting,
        ExportField::TrackersChecked,
        ExportField::LastScrapeAt,
        ExportField::Magnet,
    ];

    /// Its name: the CSV column and the JSON key.
    pub fn as_str(self) -> &'static str {
        match self {
            ExportField::Id => "id",
            ExportField::Hash => "hash",
            ExportField::HashV2 => "hash_v2",
            ExportField::Name => "name",
            ExportField::Title => "title",
            ExportField::ResolvedVia => "resolved_via",
            ExportField::Size => "size",
            ExportField::Files => "files",
            ExportField::PieceLen => "piece_len",
            ExportField::DiscoveredAt => "discovered_at",
            ExportField::MetadataAt => "metadata_at",
            ExportField::Flags => "flags",
            ExportField::ContentType => "content_type",
            ExportField::AdultKind => "adult_kind",
            ExportField::Composition => "composition",
            ExportField::JunkScore => "junk_score",
            ExportField::JunkReasons => "junk_reasons",
            ExportField::Seeders => "seeders",
            ExportField::Leechers => "leechers",
            ExportField::Completed => "completed",
            ExportField::TrackersReporting => "trackers_reporting",
            ExportField::TrackersChecked => "trackers_checked",
            ExportField::LastScrapeAt => "last_scrape_at",
            ExportField::Magnet => "magnet",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_serde_and_all_is_in_order() {
        for (i, f) in ExportField::ALL.iter().enumerate() {
            assert_eq!(
                serde_json::to_value(f).unwrap(),
                serde_json::Value::from(f.as_str())
            );
            if i > 0 {
                assert!(ExportField::ALL[i - 1] < *f, "{f:?} out of order");
            }
        }
    }
}
