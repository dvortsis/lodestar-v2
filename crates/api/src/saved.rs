//! `/saved-searches` (§8 "Saved searches → alerts", §10.8): a query kept
//! under a name, with whether it alerts, serves RSS, or is paused.
//!
//! The notifier's channels and the watermark are the store's and B9's:
//! the DTOs don't carry them, and an edit keeps the channels it didn't
//! touch. A saved search holds a query, not torrents, so no visibility
//! rule applies here; its alerts (B9) do apply them.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use lodestar_search::Node;
use lodestar_store::{SavedSearchInput, SavedSearchSummary, Store};
use utoipa::OpenApi;

use crate::dto::auth::OkResponse;
use crate::dto::error::ErrorEnvelope;
use crate::dto::saved::{SavedSearchCreate, SavedSearchPatch, SavedSearchView};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, Path, same_origin_json};
use crate::user_items::{invalid, shown_row, user_data};
use crate::{ApiState, auth, search, unix_now};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(list, create, get_one, patch, remove))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/saved-searches", get(list).post(create))
            .route(
                "/saved-searches/{id}",
                get(get_one).patch(patch).delete(remove),
            ),
    )
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "saved_search_not_found",
        "There's no saved search with that id.",
    )
}

/// The index's watermark, or the store's highest torrent id when search
/// is off, so a new or resumed search alerts only on what is indexed from
/// now on.
fn watermark(s: &ApiState, store: &Store) -> ApiResult<i64> {
    match &s.index {
        Some(handle) => handle.index().watermark().map_err(internal),
        None => store.max_id().map_err(internal),
    }
}

/// A search's alerts on torrents search would show: `(all, unread, when
/// the newest was recorded)`. The store counts every alert, Adult ones
/// included, so these are counted again (§20.9: the preference applies to
/// Alerts).
fn visible_alerts(store: &Store, id: i64, adult: bool) -> ApiResult<(u64, u64, Option<i64>)> {
    let (mut all, mut unread, mut newest) = (0, 0, None);
    let mut before = None;
    loop {
        let page = store.alerts(id, before, 200).map_err(internal)?;
        let end = page.len() < 200;
        before = page.last().map(|a| a.torrent_id);
        for a in page {
            if shown_row(store, a.torrent_id, false, adult)?.is_none() {
                continue;
            }
            all += 1;
            unread += u64::from(a.read_at.is_none());
            newest = newest.max(Some(a.created_at));
        }
        if end {
            return Ok((all, unread, newest));
        }
    }
}

fn view(store: &Store, adult: bool, s: SavedSearchSummary) -> ApiResult<SavedSearchView> {
    let (alerts, unread, last_alert_at) = if s.alerts == 0 {
        (0, 0, None)
    } else {
        visible_alerts(store, s.search.id, adult)?
    };
    let ast: Node = serde_json::from_str(&s.search.ast).map_err(internal)?;
    Ok(SavedSearchView {
        alert: s.search.alert,
        alerts,
        ast,
        created_at: s.search.created_at,
        id: s.search.id,
        last_alert_at,
        name: s.search.name,
        paused: s.search.paused,
        q: s.search.query,
        rss: s.search.rss,
        sort: s.search.sort,
        unread,
        updated_at: s.search.updated_at,
    })
}

fn summary_of(store: &Store, adult: bool, id: i64) -> ApiResult<SavedSearchView> {
    store
        .saved_searches()
        .map_err(internal)?
        .into_iter()
        .find(|s| s.search.id == id)
        .ok_or_else(not_found)
        .and_then(|s| view(store, adult, s))
}

/// The query as stored: the text sent (or the AST's canonical text) and
/// the AST as JSON. Errors from the parser keep their span.
fn query_of(ast: Option<Node>, q: Option<String>) -> ApiResult<(String, String)> {
    let text = q.clone();
    let node = search::request_ast(ast, q)?;
    let json = serde_json::to_string(&node).map_err(internal)?;
    Ok((text.unwrap_or_else(|| node.to_query()), json))
}

/// A sort the search endpoints accept, as sent.
fn sort_of(sort: Option<String>) -> ApiResult<Option<String>> {
    sort.map(|s| {
        let s = s.trim().to_string();
        search::sort_param(Some(&s)).map(|_| s)
    })
    .transpose()
}

/// Every saved search, oldest first, with its alert counts.
#[utoipa::path(
    operation_id = "listSavedSearches",
    get,
    path = "/saved-searches",
    tag = "library",
    responses((status = 200, body = Vec<SavedSearchView>))
)]
pub(crate) async fn list(State(s): State<ApiState>) -> ApiResult<Json<Vec<SavedSearchView>>> {
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        store
            .saved_searches()
            .map_err(internal)?
            .into_iter()
            .map(|s| view(&store, adult, s))
            .collect::<ApiResult<Vec<_>>>()
            .map(Json)
    })
    .await
    .map_err(internal)?
}

/// Saves a search. It alerts only on torrents indexed from now on.
#[utoipa::path(
    operation_id = "createSavedSearch",
    post,
    path = "/saved-searches",
    tag = "library",
    request_body = SavedSearchCreate,
    responses(
        (status = 201, body = SavedSearchView),
        (status = 400, body = ErrorEnvelope, description = "`query_syntax` (with `start` and `end`), `q_and_ast`, `invalid_ast` or `unknown_sort`."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: the name or query isn't allowed."),
    )
)]
pub(crate) async fn create(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<SavedSearchCreate>,
) -> ApiResult<(StatusCode, Json<SavedSearchView>)> {
    same_origin_json(&headers)?;
    let (query, ast) = query_of(body.ast, body.q)?;
    let sort = sort_of(body.sort)?;
    let name = body.name.trim().to_string();
    let writer = user_data(&s)?;
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        let mark = watermark(&s, &store)?;
        let input = SavedSearchInput {
            name: &name,
            query: &query,
            ast: &ast,
            sort: sort.as_deref(),
            alert: body.alert,
            rss: body.rss,
            channels: 0,
            paused: body.paused.unwrap_or(false),
        };
        let id = store
            .create_saved_search(&input, mark, unix_now())
            .map_err(|e| invalid(e, "name"))?;
        Ok((StatusCode::CREATED, Json(summary_of(&store, adult, id)?)))
    })
    .await
    .map_err(internal)?
}

/// One saved search.
#[utoipa::path(
    operation_id = "getSavedSearch",
    get,
    path = "/saved-searches/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The saved search's id.")),
    responses(
        (status = 200, body = SavedSearchView),
        (status = 404, body = ErrorEnvelope, description = "`saved_search_not_found`."),
    )
)]
pub(crate) async fn get_one(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<SavedSearchView>> {
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        summary_of(&store, adult, id).map(Json)
    })
    .await
    .map_err(internal)?
}

/// Changes a saved search. Any subset: a field not sent keeps its value,
/// the notifier's channels included. Resuming a paused search skips what
/// was indexed while it was paused; editing the query does not move
/// where it left off.
#[utoipa::path(
    operation_id = "patchSavedSearch",
    patch,
    path = "/saved-searches/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The saved search's id.")),
    request_body = SavedSearchPatch,
    responses(
        (status = 200, body = SavedSearchView),
        (status = 400, body = ErrorEnvelope, description = "`query_syntax` (with `start` and `end`), `q_and_ast`, `invalid_ast` or `unknown_sort`."),
        (status = 404, body = ErrorEnvelope, description = "`saved_search_not_found`."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: the name or query isn't allowed."),
    )
)]
pub(crate) async fn patch(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<SavedSearchPatch>,
) -> ApiResult<Json<SavedSearchView>> {
    same_origin_json(&headers)?;
    let changes_query = body.q.is_some() || body.ast.is_some();
    let new_query = changes_query
        .then(|| query_of(body.ast, body.q))
        .transpose()?;
    let new_sort = body.sort.map(sort_of).transpose()?;
    let writer = user_data(&s)?;
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        let current = store
            .saved_search(id)
            .map_err(internal)?
            .ok_or_else(not_found)?;
        let mark = watermark(&s, &store)?;
        let name = body
            .name
            .map_or(current.name.clone(), |n| n.trim().to_string());
        let (query, ast) = new_query.unwrap_or((current.query.clone(), current.ast.clone()));
        let sort = new_sort.unwrap_or(current.sort.clone());
        let input = SavedSearchInput {
            name: &name,
            query: &query,
            ast: &ast,
            sort: sort.as_deref(),
            alert: body.alert.unwrap_or(current.alert),
            rss: body.rss.unwrap_or(current.rss),
            channels: current.channels,
            paused: body.paused.unwrap_or(current.paused),
        };
        if !store
            .update_saved_search(id, &input, mark, unix_now())
            .map_err(|e| invalid(e, "name"))?
        {
            return Err(not_found());
        }
        summary_of(&store, adult, id).map(Json)
    })
    .await
    .map_err(internal)?
}

/// Deletes a saved search with its alerts and feed tokens.
#[utoipa::path(
    operation_id = "deleteSavedSearch",
    delete,
    path = "/saved-searches/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The saved search's id.")),
    responses(
        (status = 200, body = OkResponse),
        (status = 404, body = ErrorEnvelope, description = "`saved_search_not_found`."),
    )
)]
pub(crate) async fn remove(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<OkResponse>> {
    same_origin_json(&headers)?;
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let deleted = writer
            .lock()
            .expect("user data lock")
            .delete_saved_search(id)
            .map_err(internal)?;
        if deleted {
            Ok(Json(OkResponse::OK))
        } else {
            Err(not_found())
        }
    })
    .await
    .map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_is_the_text_sent_or_the_asts() {
        let (q, ast) = query_of(None, Some("  knight  1080p".into())).unwrap();
        assert_eq!(q, "  knight  1080p");
        assert!(ast.contains("knight"));
        let node = lodestar_search::parse("knight 1080p").unwrap();
        let (q, _) = query_of(Some(node), None).unwrap();
        assert_eq!(q, "knight 1080p");
        let (q, ast) = query_of(None, None).unwrap();
        assert_eq!(q, "");
        assert!(serde_json::from_str::<Node>(&ast).is_ok());
    }

    #[test]
    fn bad_queries_and_sorts_are_refused_with_codes() {
        let e = query_of(None, Some("year:".into())).unwrap_err();
        assert_eq!(e.code(), "query_syntax");
        assert_eq!(
            query_of(Some(Node::All), Some("x".into()))
                .unwrap_err()
                .code(),
            "q_and_ast"
        );
        assert_eq!(
            sort_of(Some("-size".into())).unwrap().as_deref(),
            Some("-size")
        );
        assert_eq!(
            sort_of(Some("bogus".into())).unwrap_err().code(),
            "unknown_sort"
        );
        assert_eq!(sort_of(None).unwrap(), None);
    }
}
