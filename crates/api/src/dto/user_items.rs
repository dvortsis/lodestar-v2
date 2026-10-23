//! `/user-items` (§7 `user_items`, §10.5): your marks on torrents.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use utoipa::{IntoParams, ToSchema};

use super::search::SearchHit;

/// A search hit as the examples show it.
pub(crate) fn example_hit() -> Value {
    json!({
        "adult_kind": null,
        "completed": null,
        "composition": [],
        "content_type": "movie",
        "discovered_at": 1_700_000_000,
        "files": 1,
        "flags": ["single_file"],
        "hash": "0123456789abcdef0123456789abcdef01234567",
        "hash_v2": null,
        "id": 1,
        "junk_reasons": [],
        "junk_score": 0,
        "last_scrape_at": null,
        "leechers": null,
        "metadata_at": 1_700_000_001,
        "name": "The.Dark.Knight.2008.1080p.BluRay.x264-GRP",
        "piece_len": 65536,
        "resolved_via": null,
        "score": 0.0,
        "seeders": null,
        "size": 8_589_934_592u64,
        "title": "The Dark Knight",
        "trackers_checked": null,
        "trackers_reporting": null
    })
}

/// Reads a field that may be absent, `null` or a value: `null` is
/// `Some(None)`, a value `Some(Some(v))`, and absent stays `None`
/// (through `#[serde(default)]`).
pub(crate) fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// `GET /user-items`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct UserItemsQuery {
    /// Only torrents you marked as a favourite.
    pub favourite: Option<bool>,
    /// Only torrents you marked Hide (your own mark, not a filter
    /// rule's).
    pub hidden_mark: Option<bool>,
    /// Only torrents carrying this label.
    pub label: Option<String>,
    /// Include torrents a filter rule hid (§20.3), as in search.
    pub hidden: Option<bool>,
    /// Rows per page: 1–200, 50 by default.
    pub limit: Option<usize>,
    /// The previous response's `next_cursor`.
    pub cursor: Option<String>,
}

/// `GET /user-items/labels`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(default)]
#[into_params(parameter_in = Query)]
pub struct LabelsQuery {
    /// Count torrents a filter rule hid too (§20.3).
    pub hidden: Option<bool>,
}

/// One marked torrent: your marks and the torrent beside them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "created_at": 1_700_000_100,
    "favourite": true,
    "hash": "0123456789abcdef0123456789abcdef01234567",
    "hidden": false,
    "labels": ["4K"],
    "note": "Watch with subs",
    "torrent": example_hit(),
    "updated_at": 1_700_000_200
}))]
pub struct UserItemRow {
    pub created_at: i64,
    pub favourite: bool,
    /// 40 hex digits.
    pub hash: String,
    /// Your own Hide mark.
    pub hidden: bool,
    /// Sorted, unique, case-sensitive.
    pub labels: Vec<String>,
    #[schema(required = true)]
    pub note: Option<String>,
    pub torrent: SearchHit,
    pub updated_at: i64,
}

/// `GET /user-items`: marked torrents, newest change first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "items": [],
    "limit": 50,
    "next_cursor": null
}))]
pub struct UserItemsResponse {
    /// Only torrents search would show (§20.3, §20.9).
    pub items: Vec<UserItemRow>,
    pub limit: usize,
    /// Null on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A torrent's marks. Without any, `favourite` and `hidden` are false,
/// `labels` is empty and the rest is null.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "created_at": 1_700_000_100,
    "favourite": true,
    "hash": "0123456789abcdef0123456789abcdef01234567",
    "hidden": false,
    "labels": ["4K"],
    "note": null,
    "updated_at": 1_700_000_200
}))]
pub struct UserMarks {
    /// Null when there are no marks.
    #[schema(required = true)]
    pub created_at: Option<i64>,
    pub favourite: bool,
    /// 40 hex digits.
    pub hash: String,
    pub hidden: bool,
    pub labels: Vec<String>,
    #[schema(required = true)]
    pub note: Option<String>,
    /// Null when there are no marks.
    #[schema(required = true)]
    pub updated_at: Option<i64>,
}

/// `GET /user-items/{hash}`: the marks and the collections it is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "collections": [1],
    "created_at": 1_700_000_100,
    "favourite": true,
    "hash": "0123456789abcdef0123456789abcdef01234567",
    "hidden": false,
    "labels": ["4K"],
    "note": null,
    "updated_at": 1_700_000_200
}))]
pub struct UserItemDetail {
    /// Ids of the collections the torrent is in ("In collections").
    pub collections: Vec<i64>,
    #[schema(required = true)]
    pub created_at: Option<i64>,
    pub favourite: bool,
    pub hash: String,
    pub hidden: bool,
    pub labels: Vec<String>,
    #[schema(required = true)]
    pub note: Option<String>,
    #[schema(required = true)]
    pub updated_at: Option<i64>,
}

/// `PATCH /user-items/{hash}`: what to change. Absent leaves a mark as
/// it is.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct UserItemPatch {
    pub favourite: Option<bool>,
    /// Your own Hide mark.
    pub hidden: Option<bool>,
    /// Replaces the labels (at most 32, 1–64 characters each); an empty
    /// list removes them.
    pub labels: Option<Vec<String>>,
    /// 1–4,000 characters; `null` removes the note.
    #[serde(deserialize_with = "nullable")]
    #[schema(value_type = Option<String>, nullable = true)]
    pub note: Option<Option<String>>,
}

/// One label in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "count": 3, "label": "4K" }))]
pub struct LabelCount {
    /// Visible torrents carrying it.
    pub count: u64,
    pub label: String,
}
