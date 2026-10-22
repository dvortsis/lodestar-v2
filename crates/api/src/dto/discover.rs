//! `/discover` (§10.7), and the hit group it shares with
//! `/torrents/{hash}/siblings`: a section of result rows that may not be
//! available yet, with the reason it isn't.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::search::SearchHit;

/// `GET /discover`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct DiscoverQuery {
    /// The Newest tab: a content type (`movie`, `tv`, `music`,
    /// `audiobook`, `ebook`, `comic`, `software`, `game`, `images`,
    /// `video`, `other`, `adult`); absent is every type.
    #[serde(rename = "type")]
    #[param(rename = "type")]
    pub kind: Option<String>,
    /// Newest rows per page: 1–200, 50 by default.
    pub limit: Option<usize>,
    /// The previous response's `newest.next_cursor`; send the same `type`
    /// with it. It pages Newest only.
    pub cursor: Option<String>,
}

/// `GET /discover`: the three sections of §10.7.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverResponse {
    /// The newest searchable torrents, newest first.
    pub newest: NewestSection,
    /// Ambiguous torrents just identified.
    pub recently_resolved: HitGroup,
    /// The top torrents by swarm trend.
    pub rising: HitGroup,
}

/// Discover's Newest: a page of result rows by `discovered_at`, newest
/// first, paged like search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NewestSection {
    pub hits: Vec<SearchHit>,
    pub limit: usize,
    /// Pass as `cursor` for the next page; null on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Rows before this page.
    pub offset: usize,
    /// Searchable torrents in this tab, not just this page.
    pub total: usize,
    /// `total` is an estimate (shown with "≈").
    pub total_approx: bool,
    /// The tab; null is every type.
    #[serde(rename = "type")]
    #[schema(required = true)]
    pub kind: Option<String>,
}

/// A section of result rows (the row search returns) that may not be
/// available in this build or for this torrent. Unavailable is never an
/// empty list that reads as "none": `status` says which.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HitGroup {
    /// Empty when unavailable.
    pub hits: Vec<SearchHit>,
    /// Why it's unavailable; null when available.
    #[schema(required = true)]
    pub reason: Option<Unavailable>,
    pub status: GroupStatus,
    /// More matched than the group lists.
    pub truncated: bool,
}

impl HitGroup {
    pub fn available(hits: Vec<SearchHit>, truncated: bool) -> HitGroup {
        HitGroup {
            hits,
            reason: None,
            status: GroupStatus::Available,
            truncated,
        }
    }

    pub fn unavailable(reason: Unavailable) -> HitGroup {
        HitGroup {
            hits: Vec::new(),
            reason: Some(reason),
            status: GroupStatus::Unavailable,
            truncated: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupStatus {
    Available,
    Unavailable,
}

/// Why a section isn't available. Stable keys; the UI words them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    /// Search is turned off.
    SearchDisabled,
    /// The torrent has no parsed title to match on (not classified yet,
    /// or the parser found none).
    NoTitle,
    /// Rising needs swarm history (M4).
    NeedsSwarmHistory,
    /// Recently resolved needs the store to record when a torrent's
    /// resolution changed (B8, the M5 migration).
    NeedsResolutionHistory,
    /// Identical-payload clones need the payload signature (M7).
    NeedsPayloadSignature,
    /// The same file elsewhere needs per-file piece roots (M7).
    NeedsPieceRoots,
}
