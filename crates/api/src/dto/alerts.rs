//! `/saved-searches/{id}/alerts`, `/alerts/*` and `/feeds/{id}` (§8
//! "Saved searches → alerts", §9, §10.8).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::search::SearchHit;

/// One alert: a torrent a saved search matched once it became searchable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlertRow {
    /// When it was recorded, Unix seconds.
    pub created_at: i64,
    /// The alert's id, never reused. `POST /alerts/read` takes the
    /// highest one you saw.
    pub id: i64,
    /// Marked read.
    pub read: bool,
    pub torrent: SearchHit,
}

/// `GET /saved-searches/{id}/alerts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlertsPage {
    /// Newest first: only alerts on torrents search would show.
    pub items: Vec<AlertRow>,
    /// Pass as `cursor` for the next page; null on the last.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// `GET /saved-searches/{id}/alerts` parameters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AlertsQuery {
    /// The previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// 1–200; 50 by default.
    pub limit: Option<usize>,
}

/// `POST /alerts/read`: marks alerts read up to the highest one you saw,
/// so alerts that arrived while the page was open stay unread (§10.8's
/// "New since …" divider).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AlertsRead {
    /// One saved search's alerts; every saved search's when absent.
    #[serde(default)]
    pub saved_search: Option<i64>,
    /// The highest alert id you saw.
    pub through: i64,
}

/// `GET /alerts/unread`: the nav badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(example = json!({ "unread": 3 }))]
pub struct UnreadAlerts {
    /// Unread alerts across every saved search, on torrents search would
    /// show: the sum of the saved searches' `unread`.
    pub unread: u64,
}

/// `GET /feeds/{id}` parameters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FeedQuery {
    /// A feed token (`lsf_…`), for feed readers that can't sign in. A
    /// session or an API key with the `feeds` scope also works.
    pub token: Option<String>,
}
