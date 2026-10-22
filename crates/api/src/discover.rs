//! `GET /discover` (§10.7): Newest, with content-type tabs; Rising; and
//! Recently resolved. Every section shows only what search would: no
//! torrent a filter rule hid, and no Adult while Adult is hidden (§20.9:
//! excluded entirely, not blurred).
//!
//! Also the visibility rules [`crate::compare`] shares for siblings and
//! Compare.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use lodestar_classify::ContentType;
use lodestar_search::Node;
use lodestar_store::{Enriched, TorrentRow, flags};
use utoipa::OpenApi;

use crate::dto::discover::{DiscoverQuery, DiscoverResponse, HitGroup, NewestSection, Unavailable};
use crate::dto::search::SearchRequest;
use crate::error::{ApiError, ApiResult};
use crate::extract::Query;
use crate::search::Visibility;
use crate::{ApiState, auth, search};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(discover))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/discover", get(discover)))
}

/// What may be shown of the index: `node` through B17's base query, the
/// one search runs, so it leaves out the torrents a filter rule hid and,
/// unless `adult` ("Show adult content", read once per request), Adult.
pub(crate) fn shown(node: &Node, adult: bool) -> Node {
    search::base_query(
        node,
        Visibility {
            hidden: false,
            adult,
        },
    )
    .query()
}

/// Whether the stored state keeps a torrent out of view, whatever the
/// index says (it may lag a reclassification): Adult unless `adult`
/// shows it, or hidden by a filter rule unless `hidden_ok`.
pub(crate) fn withheld(
    row: &TorrentRow,
    e: Option<&Enriched>,
    hidden_ok: bool,
    adult: bool,
) -> bool {
    (!adult && e.is_some_and(search::is_adult)) || (!hidden_ok && row.flags & flags::HIDDEN != 0)
}

/// Newest's tab, checked: a content type, and never query text.
fn tab(kind: Option<&str>) -> ApiResult<Option<&'static str>> {
    let Some(kind) = kind else {
        return Ok(None);
    };
    ContentType::ALL
        .iter()
        .map(|t| t.as_str())
        .find(|t| *t == kind)
        .map(Some)
        .ok_or_else(|| {
            ApiError::bad_request(
                "unknown_type",
                format!(
                    "Unknown type {kind:?}: use movie, tv, music, audiobook, ebook, comic, software, game, images, video, other or adult."
                ),
            )
            .field("type")
        })
}

/// Discover (§10.7): the newest searchable torrents, paged like search
/// and optionally one content type's; the top torrents by swarm trend;
/// and ambiguous torrents just identified. A section this build can't
/// serve is `unavailable` with its reason, never an empty list.
#[utoipa::path(
    operation_id = "getDiscover",
    get,
    path = "/discover",
    tag = "discover",
    params(DiscoverQuery),
    responses((status = 200, body = DiscoverResponse))
)]
pub(crate) async fn discover(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Query(p): Query<DiscoverQuery>,
) -> ApiResult<Json<DiscoverResponse>> {
    let kind = tab(p.kind.as_deref())?;
    if s.index.is_none() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_disabled",
            "Search is turned off.",
        ));
    }
    // Newest is a search sorted by discovery, so it gets search's base
    // query (hidden torrents, and Adult unless shown), leases, cursors
    // and notices.
    let q = kind.map_or_else(String::new, |t| format!("type:{t}"));
    let request = SearchRequest {
        q: Some(q),
        sort: Some("-discovered".to_string()),
        limit: p.limit,
        cursor: p.cursor,
        ..SearchRequest::default()
    };
    let page = search::search_get(State(s), headers, Query(request))
        .await?
        .0;
    Ok(Json(DiscoverResponse {
        newest: NewestSection {
            hits: page.hits,
            limit: page.limit,
            next_cursor: page.next_cursor,
            notices: page.notices,
            offset: page.offset,
            total: page.total,
            total_approx: page.total_approx,
            kind: kind.map(str::to_string),
        },
        // TODO(B8): the store records no time a torrent's resolution
        // changed (`torrent_parsed` keeps no timestamp, `reindex_log` no
        // time or reason); B8's migration adds one (§16, B13).
        recently_resolved: HitGroup::unavailable(Unavailable::NeedsResolutionHistory),
        // TODO(M4): swarm history and the trend arrive with #48's
        // continuation; then Rising is the top N by `swarm_trend`.
        rising: HitGroup::unavailable(Unavailable::NeedsSwarmHistory),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_search::parse;

    #[test]
    fn shown_follows_hidden_and_adult() {
        assert_eq!(shown(&Node::All, false).to_query(), "hidden:no -type:adult");
        assert_eq!(
            shown(&parse("knight").unwrap(), false).to_query(),
            "knight hidden:no -type:adult"
        );
        assert_eq!(
            shown(&parse("knight").unwrap(), true).to_query(),
            "knight hidden:no"
        );
    }

    #[test]
    fn tabs_are_content_types_only() {
        assert_eq!(tab(None).unwrap(), None);
        assert_eq!(tab(Some("movie")).unwrap(), Some("movie"));
        for bad in ["Movie", "movie -x", "", "type:movie"] {
            assert_eq!(tab(Some(bad)).unwrap_err().code(), "unknown_type", "{bad}");
        }
    }
}
