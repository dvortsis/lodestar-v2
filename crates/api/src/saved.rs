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

/// Saved searches kept, paused ones included: each one runs against
/// every commit's new torrents (B9), so the count bounds that work.
pub(crate) const MAX_SAVED_SEARCHES: u64 = 1_000;

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
    // One read, so one sort: no index orders a search's alerts by id, and
    // paging would sort them all again for every page (SRC-2b).
    for a in store.alerts(id, None, usize::MAX).map_err(internal)? {
        if shown_row(store, a.torrent_id, false, adult)?.is_none() {
            continue;
        }
        all += 1;
        unread += u64::from(a.read_at.is_none());
        newest = newest.max(Some(a.created_at));
    }
    Ok((all, unread, newest))
}

fn view(store: &Store, adult: bool, s: SavedSearchSummary) -> ApiResult<SavedSearchView> {
    let (alerts, unread, last_alert_at) = if s.alerts == 0 {
        (0, 0, None)
    } else {
        visible_alerts(store, s.search.id, adult)?
    };
    let ast: Node = serde_json::from_str(&s.search.ast).map_err(internal)?;
    let q = ast.to_query_typed();
    Ok(SavedSearchView {
        alert: s.search.alert,
        alerts,
        ast,
        created_at: s.search.created_at,
        id: s.search.id,
        last_alert_at,
        name: s.search.name,
        paused: s.search.paused,
        // From the stored AST every time, as the user would type it
        // (§10.12, U12): shown, and what "Open in search" sends to
        // `/search?q=`, which reads it as typed. A search saved before
        // typed units has raw text like `size:>10GB` and a 10 GiB AST; its
        // `q` is `size:>10GiB`, so the link finds what the alert does.
        q,
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

/// The query as stored: the AST's canonical text and the AST as JSON.
/// Errors from the parser keep their span.
///
/// `q` reads as typed (`10GB` is 10 × 1000³ bytes), but what's stored is
/// [`Node::to_query`]: bytes or IEC units only, which every release
/// reads the same way (§10.12). Typed text is never persisted.
fn query_of(ast: Option<Node>, q: Option<String>) -> ApiResult<(String, String)> {
    let node = search::request_ast(ast, q)?;
    let text = node.to_query();
    if text.len() > lodestar_search::query::MAX_QUERY_BYTES {
        return Err(ApiError::bad_request(
            "query_syntax",
            format!(
                "The query is over {} bytes once written out in full.",
                lodestar_search::query::MAX_QUERY_BYTES
            ),
        )
        .field("q"));
    }
    let json = serde_json::to_string(&node).map_err(internal)?;
    Ok((text, json))
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
        (status = 409, body = ErrorEnvelope, description = "`too_many_saved_searches`: 1,000 are kept at most."),
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
        // Every saved search percolates after every commit (B9).
        if store.saved_search_count().map_err(internal)? >= MAX_SAVED_SEARCHES {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "too_many_saved_searches",
                "You have the most saved searches Lodestar keeps (1,000); delete one first.",
            ));
        }
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
    fn the_query_is_the_asts_canonical_text() {
        let (q, ast) = query_of(None, Some("  knight  1080p".into())).unwrap();
        assert_eq!(q, "knight 1080p");
        assert!(ast.contains("knight"));
        let node = lodestar_search::parse("knight 1080p").unwrap();
        let (q, _) = query_of(Some(node), None).unwrap();
        assert_eq!(q, "knight 1080p");
        let (q, ast) = query_of(None, None).unwrap();
        assert_eq!(q, "");
        assert!(serde_json::from_str::<Node>(&ast).is_ok());
    }

    /// What's stored for a typed `size:>10GB` reads the same set under the
    /// stored reading, the typed reading and the parser before typed
    /// units (which read bytes and IEC units exactly as `Units::Stored`
    /// does; `lodestar_search::query`'s tests pin that).
    #[test]
    fn a_typed_size_is_stored_canonical() {
        use lodestar_search::{Units, parse_with};
        for typed in [
            "size:>10GB",
            "largest:1.5GB..<2TB name:x",
            "size:<1GB file:(size:>50MB)",
        ] {
            let (q, json) = query_of(None, Some(typed.into())).unwrap();
            let ast: Node = serde_json::from_str(&json).unwrap();
            assert_eq!(ast, parse_with(typed, Units::Typed).unwrap());
            assert!(
                !q.contains("GB") && !q.contains("MB") && !q.contains("TB"),
                "{q}"
            );
            assert_eq!(parse_with(&q, Units::Stored).unwrap(), ast, "{q}");
            assert_eq!(parse_with(&q, Units::Typed).unwrap(), ast, "{q}");
        }
        let (q, _) = query_of(None, Some("size:>10GB".into())).unwrap();
        assert_eq!(q, "size:>9765625KiB");
    }

    #[test]
    fn a_query_too_long_once_written_out_is_refused() {
        // Within the limit as typed, past it as canonical text: `res:4k`
        // is written `res:2160p`.
        let max = lodestar_search::query::MAX_QUERY_BYTES;
        let mut typed = "res:4k ".repeat(60);
        typed.push_str(&"x".repeat(max - typed.len()));
        assert_eq!(typed.len(), max);
        assert!(lodestar_search::parse_with(&typed, lodestar_search::Units::Typed).is_ok());
        let e = query_of(None, Some(typed)).unwrap_err();
        assert_eq!(e.code(), "query_syntax");
        assert!(e.to_string().contains("written out in full"), "{e}");
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
