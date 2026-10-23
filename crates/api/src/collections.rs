//! `/collections` (§7, §10.5): named, ordered lists of torrents.
//!
//! **Visibility (content safety):** every torrent goes through
//! [`crate::discover::withheld`] (via [`crate::user_items::shown_row`] and
//! [`crate::user_items::visible`]). Every count returned counts visible
//! entries only, so a collection's `items` never reveals an Adult
//! torrent that is hidden.

use std::collections::HashSet;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use lodestar_store::{Collection, Error as StoreError, Store};
use utoipa::OpenApi;

use crate::dto::auth::OkResponse;
use crate::dto::collections::{
    CollectionCreate, CollectionEntry, CollectionPage, CollectionPatch, CollectionQuery,
    CollectionSummary, CollectionsQuery, HashList,
};
use crate::dto::error::ErrorEnvelope;
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, Path, Query, same_origin_json};
use crate::user_items::{
    clean_note, hash_of, hit_row, invalid, not_found, shown_row, user_data, visible,
};
use crate::{ApiState, auth, unix_now};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(list, create, get_one, patch, remove, add_items, remove_items, reorder))]
pub(crate) struct ApiDoc;

const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;
/// Entries read from the store at a time while skipping withheld ones.
const BATCH: usize = 200;
/// Hashes in one add, remove or reorder.
const MAX_HASHES: usize = 500;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/collections", get(list).post(create))
            .route(
                "/collections/{id}",
                get(get_one).patch(patch).delete(remove),
            )
            .route("/collections/{id}/items", post(add_items))
            .route("/collections/{id}/items/remove", post(remove_items))
            .route("/collections/{id}/order", put(reorder)),
    )
}

fn collection_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "collection_not_found",
        "There's no collection with that id.",
    )
}

fn failed(e: StoreError) -> ApiError {
    internal(e)
}

/// Every entry's torrent id in order, past the visibility check or not.
fn all_entries(store: &Store, id: i64) -> ApiResult<Vec<(i64, i64)>> {
    let mut out = Vec::new();
    let mut after = None;
    loop {
        let page = store.collection_items(id, after, BATCH).map_err(failed)?;
        let end = page.len() < BATCH;
        after = page.last().map(|i| i.position);
        out.extend(page.iter().map(|i| (i.torrent_id, i.position)));
        if end {
            return Ok(out);
        }
    }
}

/// How many of a collection's entries are shown: the only count the API
/// returns.
fn visible_count(store: &Store, id: i64, hidden: bool, adult: bool) -> ApiResult<u64> {
    let mut n = 0;
    for (torrent, _) in all_entries(store, id)? {
        if shown_row(store, torrent, hidden, adult)?.is_some() {
            n += 1;
        }
    }
    Ok(n)
}

fn summary(
    store: &Store,
    c: Collection,
    hidden: bool,
    adult: bool,
) -> ApiResult<CollectionSummary> {
    Ok(CollectionSummary {
        created_at: c.created_at,
        id: c.id,
        items: visible_count(store, c.id, hidden, adult)?,
        name: c.name,
        note: c.note,
        updated_at: c.updated_at,
    })
}

fn summary_of(store: &Store, id: i64, hidden: bool, adult: bool) -> ApiResult<CollectionSummary> {
    let c = store
        .collection(id)
        .map_err(failed)?
        .ok_or_else(collection_not_found)?;
    summary(store, c, hidden, adult)
}

/// 1–500 hashes, each parsed; the error names `hashes`.
fn hashes(list: &HashList) -> ApiResult<Vec<lodestar_metainfo::InfoHash>> {
    if list.hashes.is_empty() || list.hashes.len() > MAX_HASHES {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_value",
            "Send 1 to 500 info hashes.",
        )
        .field("hashes"));
    }
    list.hashes
        .iter()
        .map(|h| hash_of(h).map_err(|e| e.field("hashes")))
        .collect()
}

fn store_error(e: StoreError, field: &'static str) -> ApiError {
    match e {
        StoreError::NotFound("collection") => collection_not_found(),
        StoreError::NotFound(_) => not_found().field("hashes"),
        e => invalid(e, field),
    }
}

/// Every collection, most recently changed first, with the number of
/// entries search would show.
#[utoipa::path(
    operation_id = "listCollections",
    get,
    path = "/collections",
    tag = "library",
    params(CollectionsQuery),
    responses((status = 200, body = Vec<CollectionSummary>))
)]
pub(crate) async fn list(
    State(s): State<ApiState>,
    Query(p): Query<CollectionsQuery>,
) -> ApiResult<Json<Vec<CollectionSummary>>> {
    let adult = s.adult.shown();
    let hidden = p.hidden.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        store
            .collections()
            .map_err(failed)?
            .into_iter()
            .map(|c| summary(&store, c, hidden, adult))
            .collect::<ApiResult<Vec<_>>>()
            .map(Json)
    })
    .await
    .map_err(internal)?
}

/// Makes a collection.
#[utoipa::path(
    operation_id = "createCollection",
    post,
    path = "/collections",
    tag = "library",
    request_body = CollectionCreate,
    responses(
        (status = 201, body = CollectionSummary),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: the name or note isn't allowed."),
    )
)]
pub(crate) async fn create(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<CollectionCreate>,
) -> ApiResult<(StatusCode, Json<CollectionSummary>)> {
    same_origin_json(&headers)?;
    let writer = user_data(&s)?;
    let name = body.name.trim().to_string();
    let note = body.note.as_deref().map(clean_note);
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        let id = store
            .create_collection(&name, note.as_deref(), unix_now())
            .map_err(|e| invalid(e, "name"))?;
        let c = summary_of(&store, id, false, false)?;
        Ok((StatusCode::CREATED, Json(c)))
    })
    .await
    .map_err(internal)?
}

/// A collection and a page of its entries in order. Entries search
/// wouldn't show are skipped (a page is full unless the collection ends)
/// and `items` counts the ones it would.
#[utoipa::path(
    operation_id = "getCollection",
    get,
    path = "/collections/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The collection's id."), CollectionQuery),
    responses(
        (status = 200, body = CollectionPage),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
    )
)]
pub(crate) async fn get_one(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
    Query(p): Query<CollectionQuery>,
) -> ApiResult<Json<CollectionPage>> {
    let adult = s.adult.shown();
    let hidden = p.hidden.unwrap_or(false);
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        let c = store
            .collection(id)
            .map_err(failed)?
            .ok_or_else(collection_not_found)?;
        let mut entries = Vec::new();
        let mut after = p.after;
        let mut more = false;
        'pages: loop {
            let page = store.collection_items(id, after, BATCH).map_err(failed)?;
            let end = page.len() < BATCH;
            for item in page {
                let Some((row, e)) = shown_row(&store, item.torrent_id, hidden, adult)? else {
                    after = Some(item.position);
                    continue;
                };
                if entries.len() == limit {
                    more = true;
                    break 'pages;
                }
                entries.push(CollectionEntry {
                    added_at: item.added_at,
                    position: item.position,
                    torrent: hit_row(&row, e),
                });
                after = Some(item.position);
            }
            if end {
                break;
            }
        }
        let items = visible_count(&store, id, hidden, adult)?;
        Ok(Json(CollectionPage {
            created_at: c.created_at,
            entries,
            id: c.id,
            items,
            name: c.name,
            // The last position read before the one that would not fit.
            next_after: after.filter(|_| more),
            note: c.note,
            updated_at: c.updated_at,
        }))
    })
    .await
    .map_err(internal)?
}

/// Renames a collection or changes its note; absent leaves a field as it
/// is, and `note: null` removes the note.
#[utoipa::path(
    operation_id = "patchCollection",
    patch,
    path = "/collections/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The collection's id.")),
    request_body = CollectionPatch,
    responses(
        (status = 200, body = CollectionSummary),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: the name or note isn't allowed."),
    )
)]
pub(crate) async fn patch(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<CollectionPatch>,
) -> ApiResult<Json<CollectionSummary>> {
    same_origin_json(&headers)?;
    let writer = user_data(&s)?;
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        let current = store
            .collection(id)
            .map_err(failed)?
            .ok_or_else(collection_not_found)?;
        if body.name.is_some() || body.note.is_some() {
            let name = body
                .name
                .as_deref()
                .map_or(current.name.clone(), |n| n.trim().to_string());
            let note = match &body.note {
                Some(n) => n.as_deref().map(clean_note),
                None => current.note.clone(),
            };
            if !store
                .update_collection(id, &name, note.as_deref(), unix_now())
                .map_err(|e| invalid(e, "name"))?
            {
                return Err(collection_not_found());
            }
        }
        summary_of(&store, id, false, adult).map(Json)
    })
    .await
    .map_err(internal)?
}

/// Deletes a collection (not the torrents in it).
#[utoipa::path(
    operation_id = "deleteCollection",
    delete,
    path = "/collections/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The collection's id.")),
    responses(
        (status = 200, body = OkResponse),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
    )
)]
pub(crate) async fn remove(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<OkResponse>> {
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let deleted = writer
            .lock()
            .expect("user data lock")
            .delete_collection(id)
            .map_err(failed)?;
        if deleted {
            Ok(Json(OkResponse::OK))
        } else {
            Err(collection_not_found())
        }
    })
    .await
    .map_err(internal)?
}

/// Adds torrents to the end of a collection; ones already in it stay
/// where they are. All or nothing: a hash that is unknown, or that search
/// wouldn't show, is the one `404 torrent_not_found` (`field: "hashes"`)
/// and nothing is added.
#[utoipa::path(
    operation_id = "addToCollection",
    post,
    path = "/collections/{id}/items",
    tag = "library",
    params(("id" = i64, Path, description = "The collection's id.")),
    request_body = HashList,
    responses(
        (status = 200, body = CollectionSummary),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`, or `torrent_not_found` with `field: \"hashes\"`."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: not 1–500 hashes."),
    )
)]
pub(crate) async fn add_items(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<HashList>,
) -> ApiResult<Json<CollectionSummary>> {
    same_origin_json(&headers)?;
    let parsed = hashes(&body)?;
    let adult = s.adult.shown();
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let mut ids = Vec::new();
        {
            let store = s.reader.lock().expect("reader lock");
            if store.collection(id).map_err(failed)?.is_none() {
                return Err(collection_not_found());
            }
            for h in &parsed {
                // The drawer opens torrents a filter hid, so they can be
                // collected; Adult while hidden is the same 404.
                let (row, _) = visible(&store, h, true, adult).map_err(|e| e.field("hashes"))?;
                if !ids.contains(&row.id) {
                    ids.push(row.id);
                }
            }
        }
        let mut store = writer.lock().expect("user data lock");
        store
            .add_to_collection(id, &ids, unix_now())
            .map_err(|e| store_error(e, "hashes"))?;
        summary_of(&store, id, false, adult).map(Json)
    })
    .await
    .map_err(internal)?
}

/// Takes torrents out of a collection. A hash that isn't in it, is
/// unknown or isn't shown is skipped, so nothing about a withheld
/// torrent is revealed.
#[utoipa::path(
    operation_id = "removeFromCollection",
    post,
    path = "/collections/{id}/items/remove",
    tag = "library",
    params(("id" = i64, Path, description = "The collection's id.")),
    request_body = HashList,
    responses(
        (status = 200, body = CollectionSummary),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: not 1–500 hashes."),
    )
)]
pub(crate) async fn remove_items(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<HashList>,
) -> ApiResult<Json<CollectionSummary>> {
    same_origin_json(&headers)?;
    let parsed = hashes(&body)?;
    let adult = s.adult.shown();
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        if store.collection(id).map_err(failed)?.is_none() {
            return Err(collection_not_found());
        }
        let mut ids = Vec::new();
        for h in &parsed {
            if let Some(torrent) = store.find(h).map_err(failed)?
                && shown_row(&store, torrent, true, adult)?.is_some()
            {
                ids.push(torrent);
            }
        }
        store
            .remove_from_collection(id, &ids, unix_now())
            .map_err(|e| store_error(e, "hashes"))?;
        summary_of(&store, id, false, adult).map(Json)
    })
    .await
    .map_err(internal)?
}

/// Puts a collection's entries in the order given. The list must name
/// every entry shown by default once (it may also name ones a filter rule
/// hid); `409 stale_order` otherwise, so a reorder from an old page can't
/// drop or duplicate entries. Entries not shown to the client (Adult while
/// hidden, and filter-hidden ones it did not name) keep their slots.
#[utoipa::path(
    operation_id = "reorderCollection",
    put,
    path = "/collections/{id}/order",
    tag = "library",
    params(("id" = i64, Path, description = "The collection's id.")),
    request_body = HashList,
    responses(
        (status = 200, body = CollectionSummary),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
        (status = 409, body = ErrorEnvelope, description = "`stale_order`: the list isn't exactly the entries."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: not 1–500 hashes."),
    )
)]
pub(crate) async fn reorder(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    JsonBody(body): JsonBody<HashList>,
) -> ApiResult<Json<CollectionSummary>> {
    same_origin_json(&headers)?;
    let parsed = hashes(&body)?;
    let adult = s.adult.shown();
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        if store.collection(id).map_err(failed)?.is_none() {
            return Err(collection_not_found());
        }
        let current: Vec<i64> = all_entries(&store, id)?
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        let mut required = HashSet::new();
        let mut allowed = HashSet::new();
        for &t in &current {
            if shown_row(&store, t, false, adult)?.is_some() {
                required.insert(t);
            }
            if shown_row(&store, t, true, adult)?.is_some() {
                allowed.insert(t);
            }
        }
        let stale = || {
            ApiError::new(
                StatusCode::CONFLICT,
                "stale_order",
                "The collection changed; reload it and try again.",
            )
            .field("hashes")
        };
        let mut given = Vec::new();
        for h in &parsed {
            let t = store.find(h).map_err(failed)?.ok_or_else(stale)?;
            given.push(t);
        }
        let named: HashSet<i64> = given.iter().copied().collect();
        if named.len() != given.len() || !named.is_subset(&allowed) || !required.is_subset(&named) {
            return Err(stale());
        }
        // The named entries take the named entries' slots, in the order
        // given; every other entry keeps its own.
        let mut next = given.into_iter();
        let full: Vec<i64> = current
            .iter()
            .map(|t| {
                if named.contains(t) {
                    next.next().expect("as many names as slots")
                } else {
                    *t
                }
            })
            .collect();
        store
            .reorder_collection(id, &full, unix_now())
            .map_err(|e| match e {
                StoreError::Invalid(_) => stale(),
                e => store_error(e, "hashes"),
            })?;
        summary_of(&store, id, false, adult).map(Json)
    })
    .await
    .map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_lists_are_bounded_and_named() {
        let list = |hashes: Vec<String>| HashList { hashes };
        let e = hashes(&list(vec![])).unwrap_err();
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(e.body()["error"]["field"], "hashes");
        assert!(hashes(&list(vec!["ab".repeat(20); MAX_HASHES + 1])).is_err());
        assert_eq!(
            hashes(&list(vec!["AB".repeat(20); MAX_HASHES]))
                .unwrap()
                .len(),
            MAX_HASHES
        );
        let e = hashes(&list(vec!["nope".into()])).unwrap_err();
        assert_eq!(e.code(), "bad_hash");
        assert_eq!(e.body()["error"]["field"], "hashes");
    }

    #[test]
    fn store_errors_map_to_the_404s() {
        assert_eq!(
            store_error(StoreError::NotFound("collection"), "name").code(),
            "collection_not_found"
        );
        let e = store_error(StoreError::NotFound("torrent"), "name");
        assert_eq!(e.code(), "torrent_not_found");
        assert_eq!(e.body()["error"]["field"], "hashes");
    }
}
