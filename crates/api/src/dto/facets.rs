//! `/facets` (§8, §10.4; `docs/metrics.md` §7.4): disjunctive term
//! counts, histograms and the swarm buckets for the documents a search
//! returns.

use std::collections::BTreeMap;

use lodestar_search::query::Node;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::search::AstNode;

/// `POST /facets`'s body. `GET /facets` takes the same fields as query
/// parameters, with `limit.<facet>=` for `limits` (and no `ast`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FacetsRequest {
    /// The query text, as `/search` takes it. Send `q` or `ast`, not
    /// both.
    pub q: Option<String>,
    /// A query AST, as `/parse` returns it.
    #[schema(value_type = Option<AstNode>)]
    pub ast: Option<Node>,
    /// The search's sort; it doesn't change the counts, but a `lease`
    /// is checked against it.
    pub sort: Option<String>,
    /// Include torrents a filter rule hid (§20.3), as `/search`.
    pub hidden: Option<bool>,
    /// The user's zone for `time:`, in minutes east of UTC (−840 to
    /// 840), as `/search`; 0 by default.
    pub tz: Option<i32>,
    /// A search response's `next_cursor`: count the same index snapshot
    /// as its results. Send the same `q`, `sort` and `hidden` with it.
    /// Without one, the index serving now.
    pub lease: Option<String>,
    /// Values listed per facet: 8 by default, 1–100.
    #[schema(example = json!({"ext": 20}))]
    pub limits: BTreeMap<String, usize>,
}

/// `GET`/`POST /facets`. Counts are disjunctive: a facet's counts ignore
/// the query's own clauses on that facet (architecture §16, "B6").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct FacetsResponse {
    /// When the crawler first ran (Unix seconds); discovered bins that
    /// end by then are `backfill`. Null when the store doesn't know.
    #[schema(required = true)]
    pub backfill_before: Option<i64>,
    /// The swarm buckets: null until swarm health is in the index (M4),
    /// never zeros.
    pub buckets: SwarmBuckets,
    /// The query as the server understood it.
    pub canonical: String,
    /// Term facets by name: `type`, `ext`, `res`, `category`, `lang`,
    /// `group`, `codec`, `audio`, `hdr`, `private`, `v2`, `has`,
    /// `ambiguous`, `imported`, `backfill`, and `adult` when there are
    /// adult subtypes to list.
    pub facets: BTreeMap<String, TermFacet>,
    /// `size`, `discovered` and `year`.
    pub histograms: BTreeMap<String, Histogram>,
    /// The share of the documents a sampled group visited (for the "≈"
    /// note); null when nothing was sampled.
    #[schema(required = true)]
    pub sample_rate: Option<f64>,
    /// Some group was computed on a sample (above 1,000,000 matches, or
    /// 300,000 when a segment holds more than 300,000 torrents).
    pub sampled: bool,
    pub took_ms: f64,
    /// The search's result count, as `/search` reports it.
    pub total: usize,
    pub total_approx: bool,
    /// Groups this index can't serve: fields an older index lacks while
    /// it's rebuilt, and the swarm buckets until M4.
    pub unavailable: Vec<String>,
}

/// Seeders and trackers reporting, as `≥1`, `≥5`, `≥50` and
/// `never_scraped` values (§10.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SwarmBuckets {
    #[schema(required = true)]
    pub seeders: Option<TermFacet>,
    #[schema(required = true)]
    pub trackers: Option<TermFacet>,
}

/// One term facet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TermFacet {
    /// Computed on a sample: show the counts with "≈".
    pub approx: bool,
    /// Distinct values with a count, listed or not.
    pub distinct: u64,
    /// The counts of the values not listed, summed (a torrent with two
    /// such values counts twice).
    pub other: u64,
    /// The top values by count (ties by value), then any value the query
    /// selects or excludes that isn't among them. Flags list `yes` and
    /// `no`.
    pub values: Vec<FacetValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FacetValue {
    pub count: u64,
    /// The query excludes it (`-res:480p`).
    pub excluded: bool,
    /// The query selects it.
    pub selected: bool,
    pub value: String,
}

/// A histogram with explicit bins; empty bins are present with count 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Histogram {
    pub approx: bool,
    pub bins: Vec<HistogramBin>,
    /// Matches without a value (no year parsed); the bins plus this sum
    /// to the result count.
    pub missing: u64,
    /// What the edges measure: `bytes`; `hour`, `day`, `week` (from a
    /// Thursday), `month` or `year` in UTC seconds (`discovered`, whose
    /// first and last bins may be irregular edge bins); or `year` (the
    /// release year).
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HistogramBin {
    /// Ends by the crawler's first run (`backfill_before`).
    pub backfill: bool,
    pub count: u64,
    /// Exclusive; null for the open top bin.
    #[schema(required = true)]
    pub hi: Option<i64>,
    /// Inclusive.
    pub lo: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_refuse_unknown_fields() {
        let ok: FacetsRequest =
            serde_json::from_str(r#"{"q":"x","limits":{"ext":3},"hidden":true}"#).unwrap();
        assert_eq!(ok.limits["ext"], 3);
        assert!(serde_json::from_str::<FacetsRequest>(r#"{"q":"x","limit":3}"#).is_err());
    }
}
