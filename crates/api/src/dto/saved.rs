//! `/saved-searches` (§8 "Saved searches → alerts", §10.8).

use lodestar_search::Node;
use serde::{Deserialize, Serialize};

use utoipa::ToSchema;

use super::search::AstNode;
use super::user_items::nullable;

/// A saved search with what the list view shows beside it. The
/// notifier's channels and the watermark are not part of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({
    "alert": true,
    "alerts": 4,
    "ast": { "op": "all" },
    "created_at": 1_700_000_100,
    "id": 1,
    "last_alert_at": 1_700_000_300,
    "name": "New 4K releases",
    "paused": false,
    "q": "",
    "rss": false,
    "sort": "-discovered",
    "unread": 2,
    "updated_at": 1_700_000_200
}))]
pub struct SavedSearchView {
    /// Alert in the app about new hits.
    pub alert: bool,
    /// Alerts recorded in all.
    pub alerts: u64,
    #[serde(serialize_with = "super::sorted")]
    #[schema(value_type = AstNode)]
    pub ast: Node,
    pub created_at: i64,
    pub id: i64,
    /// When the newest alert was recorded; null before the first.
    #[schema(required = true)]
    pub last_alert_at: Option<i64>,
    pub name: String,
    /// Skipped by the notifier; its channels are kept.
    pub paused: bool,
    /// The query text.
    pub q: String,
    /// Serve the hits as an RSS feed.
    pub rss: bool,
    /// The sort the results open with; null = relevance.
    #[schema(required = true)]
    pub sort: Option<String>,
    pub unread: u64,
    pub updated_at: i64,
}

/// `POST /saved-searches`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SavedSearchCreate {
    /// 1–200 characters.
    pub name: String,
    /// The query text. Send `q` or `ast`, not both; with neither it
    /// matches everything.
    #[serde(default)]
    pub q: Option<String>,
    /// A query AST, as `/parse` returns it.
    #[serde(default)]
    #[schema(value_type = Option<AstNode>)]
    pub ast: Option<Node>,
    /// As search's `sort`.
    #[serde(default)]
    pub sort: Option<String>,
    /// Alert in the app about new hits.
    pub alert: bool,
    /// Serve the hits as an RSS feed.
    pub rss: bool,
    /// Start paused; false by default.
    #[serde(default)]
    pub paused: Option<bool>,
}

/// `PATCH /saved-searches/{id}`: any subset; absent leaves a field as it
/// is (the notifier's channels too).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SavedSearchPatch {
    pub name: Option<String>,
    /// Send `q` or `ast`, not both.
    pub q: Option<String>,
    #[schema(value_type = Option<AstNode>)]
    pub ast: Option<Node>,
    /// `null` opens with relevance.
    #[serde(deserialize_with = "nullable")]
    #[schema(value_type = Option<String>, nullable = true)]
    pub sort: Option<Option<String>>,
    pub alert: Option<bool>,
    pub rss: Option<bool>,
    /// Pausing and resuming is a toggle; resuming skips what was
    /// indexed meanwhile.
    pub paused: Option<bool>,
}
