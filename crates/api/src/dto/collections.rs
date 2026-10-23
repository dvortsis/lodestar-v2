//! `/collections` (§7, §10.5): named, ordered lists of torrents.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::search::SearchHit;
use super::user_items::{example_hit, nullable};

/// A collection. `items` counts the torrents search would show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "created_at": 1_700_000_100,
    "id": 1,
    "items": 2,
    "name": "Weekend watchlist",
    "note": null,
    "updated_at": 1_700_000_200
}))]
pub struct CollectionSummary {
    pub created_at: i64,
    pub id: i64,
    pub items: u64,
    pub name: String,
    #[schema(required = true)]
    pub note: Option<String>,
    pub updated_at: i64,
}

/// `POST /collections`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CollectionCreate {
    /// 1–200 characters.
    pub name: String,
    /// 1–4,000 characters.
    #[serde(default)]
    pub note: Option<String>,
}

/// `PATCH /collections/{id}`: absent leaves a field as it is.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct CollectionPatch {
    pub name: Option<String>,
    /// `null` removes the note.
    #[serde(deserialize_with = "nullable")]
    #[schema(value_type = Option<String>, nullable = true)]
    pub note: Option<Option<String>>,
}

/// `GET /collections`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct CollectionsQuery {
    /// Count torrents a filter rule hid too (§20.3).
    pub hidden: Option<bool>,
}

/// `GET /collections/{id}`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct CollectionQuery {
    /// The previous response's `next_after`.
    pub after: Option<i64>,
    /// Include torrents a filter rule hid (§20.3).
    pub hidden: Option<bool>,
    /// Entries per page: 1–200, 50 by default.
    pub limit: Option<usize>,
}

/// One torrent in a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "added_at": 1_700_000_150,
    "position": 1,
    "torrent": example_hit()
}))]
pub struct CollectionEntry {
    pub added_at: i64,
    /// Ascending; gaps are possible where entries are not shown.
    pub position: i64,
    pub torrent: SearchHit,
}

/// `GET /collections/{id}`: the collection and a page of its entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "created_at": 1_700_000_100,
    "entries": [],
    "id": 1,
    "items": 0,
    "name": "Weekend watchlist",
    "next_after": null,
    "note": null,
    "updated_at": 1_700_000_200
}))]
pub struct CollectionPage {
    pub created_at: i64,
    /// In order, only torrents search would show.
    pub entries: Vec<CollectionEntry>,
    pub id: i64,
    pub items: u64,
    pub name: String,
    /// Send as `after` for the next page; null on the last.
    #[schema(required = true)]
    pub next_after: Option<i64>,
    #[schema(required = true)]
    pub note: Option<String>,
    pub updated_at: i64,
}

/// A list of torrents by info hash: `POST /collections/{id}/items`,
/// `…/items/remove` and `PUT …/order`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct HashList {
    /// 1–500 40-character hex info hashes.
    pub hashes: Vec<String>,
}
