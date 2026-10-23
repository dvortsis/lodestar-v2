//! `/suggest` (§8, §9, §10.4): typeahead for the query bar, from the
//! term dictionaries and the title FST (`lodestar_search::suggest`).
//!
//! Every count and existence check runs [`search::base_query`], the
//! query `/search` runs, so nothing is offered that the search would
//! hide: filter-hidden torrents unless `hidden`, Adult unless "Show adult
//! content" is on (§20.3, §20.9). The work runs on the blocking pool
//! under the request's cancel token, which a client disconnect (the UI
//! aborting a stale request) cancels.

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use lodestar_search::query::MAX_QUERY_BYTES;
use lodestar_search::suggest::{self, DEFAULT_LIMIT, MAX_LIMIT, Request, Shown};
use lodestar_search::{CancelToken, Error as SearchError, Node};
use utoipa::OpenApi;

use crate::dto::suggest::{CharRange, SuggestItem, SuggestQuery, SuggestResponse};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::Query;
use crate::search::{Visibility, base_query, search_error, unix_now};
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(suggest_get))]
pub(crate) struct ApiDoc;

/// How long a suggestion may take before the request gives up: far past
/// the 15 ms it takes (§3), so only a runaway query meets it.
pub const BUDGET: Duration = Duration::from_secs(2);

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/suggest", get(suggest_get)))
}

/// Suggestions for the token at the caret: field names, values with
/// counts, titles, smart chips and "did you mean". `replace` ranges are
/// in characters, so the client never tokenises the query again.
#[utoipa::path(
    operation_id = "suggest",
    get,
    path = "/suggest",
    tag = "search",
    params(SuggestQuery),
    responses((status = 200, body = SuggestResponse))
)]
pub(crate) async fn suggest_get(
    State(s): State<ApiState>,
    Query(p): Query<SuggestQuery>,
) -> ApiResult<axum::Json<SuggestResponse>> {
    let Some(search) = s.index.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_disabled",
            "Search is turned off.",
        ));
    };
    if p.q.len() > MAX_QUERY_BYTES {
        return Err(ApiError::bad_request(
            "query_too_long",
            format!("A query is at most {MAX_QUERY_BYTES} bytes."),
        )
        .field("q"));
    }
    if p.pos.is_some_and(|pos| pos > p.q.chars().count()) {
        return Err(
            ApiError::bad_request("bad_pos", "The caret is past the end of the query.")
                .field("pos"),
        );
    }
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let visibility = Visibility {
        hidden: p.hidden.unwrap_or(false),
        adult: s.adult.shown(),
    };
    // Axum drops this future when the client disconnects; the guard then
    // cancels the work on the blocking pool.
    let cancel = CancelToken::new();
    let _guard = cancel.cancel_on_drop();
    let work = cancel.with_budget(BUDGET);
    tokio::task::spawn_blocking(move || {
        let restrict = move |n: Node| base_query(&n, visibility).query();
        let req = Request {
            q: &p.q,
            pos: p.pos,
            limit,
            shown: Shown {
                adult: visibility.adult,
                hidden: visibility.hidden,
            },
            restrict: &restrict,
            now: unix_now(),
        };
        let items = suggest::suggest(&search.index(), &req, &work).map_err(|e| match e {
            SearchError::OutOfTime => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "suggest_timeout",
                "Suggestions took too long.",
            ),
            e => search_error(e),
        })?;
        Ok(axum::Json(SuggestResponse {
            items: items
                .into_iter()
                .map(|i| SuggestItem {
                    count: i.count,
                    insert: i.insert,
                    kind: i.kind.into(),
                    replace: CharRange {
                        end: i.replace.1,
                        start: i.replace.0,
                    },
                    text: i.text,
                })
                .collect(),
        }))
    })
    .await
    .map_err(internal)?
}
