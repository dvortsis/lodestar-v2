//! `/user-items` (§7 `user_items`, §10.5): your favourite, hidden, label
//! and note marks on torrents. Also the lookups [`crate::collections`] and
//! [`crate::saved`] share.
//!
//! **Visibility (content safety):** a torrent by hash goes through
//! [`visible`], which calls [`crate::discover::withheld`] with
//! `hidden_ok = true` (the drawer opens torrents a filter hid), so an
//! Adult torrent while Adult is hidden is the same 404 as an unknown
//! hash. Lists call `withheld` per row with the request's `hidden`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use lodestar_metainfo::InfoHash;
use lodestar_store::{Enriched, Error as StoreError, Store, TorrentRow, UserItem, UserItemUpdate};
use utoipa::OpenApi;

use crate::discover::withheld;
use crate::dto::auth::OkResponse;
use crate::dto::error::ErrorEnvelope;
use crate::dto::search::SearchHit;
use crate::dto::user_items::{
    LabelCount, LabelsQuery, UserItemDetail, UserItemPatch, UserItemRow, UserItemsQuery,
    UserItemsResponse, UserMarks,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, Path, Query, same_origin_json};
use crate::{ApiState, auth, search, torrents, unix_now};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(list, labels, get_one, patch, clear))]
pub(crate) struct ApiDoc;

/// Rows per page, at most.
const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;
/// Rows read from the store at a time while skipping withheld ones.
const BATCH: usize = 200;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/user-items", get(list))
            // Before `{hash}`: the literal wins in axum, but keep it plain.
            .route("/user-items/labels", get(labels))
            .route(
                "/user-items/{hash}",
                get(get_one).patch(patch).delete(clear),
            ),
    )
}

// ---- shared ----------------------------------------------------------------

pub(crate) fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "torrent_not_found",
        "There's no torrent with that hash.",
    )
}

/// The write connection, or the 503 when none is served.
pub(crate) fn user_data(s: &ApiState) -> ApiResult<Arc<Mutex<Store>>> {
    s.user_data.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "user_data_unavailable",
            "Your saved data isn't available.",
        )
    })
}

/// A hash from a path or body, lowercased and parsed.
pub(crate) fn hash_of(text: &str) -> ApiResult<InfoHash> {
    torrents::parse_hash(&text.to_ascii_lowercase())
}

/// A torrent by hash with its enrichment, as long as it may be shown:
/// one that's unknown, or withheld from view, is the same 404.
pub(crate) fn visible(
    store: &Store,
    hash: &InfoHash,
    hidden_ok: bool,
    adult: bool,
) -> ApiResult<(TorrentRow, Option<Enriched>)> {
    let id = store.find(hash).map_err(internal)?.ok_or_else(not_found)?;
    let row = store.torrent(id).map_err(internal)?.ok_or_else(not_found)?;
    let enrichment = store.enrichment(id).map_err(internal)?;
    if withheld(&row, enrichment.as_ref(), hidden_ok, adult) {
        return Err(not_found());
    }
    Ok((row, enrichment))
}

/// A torrent by id as [`visible`]: `None` when it is gone or withheld.
pub(crate) fn shown_row(
    store: &Store,
    id: i64,
    hidden_ok: bool,
    adult: bool,
) -> ApiResult<Option<(TorrentRow, Option<Enriched>)>> {
    let Some(row) = store.torrent(id).map_err(internal)? else {
        return Ok(None);
    };
    let enrichment = store.enrichment(id).map_err(internal)?;
    if withheld(&row, enrichment.as_ref(), hidden_ok, adult) {
        return Ok(None);
    }
    Ok(Some((row, enrichment)))
}

/// A result row for `row`, its classification fields filled as search
/// fills them.
pub(crate) fn hit_row(row: &TorrentRow, e: Option<Enriched>) -> SearchHit {
    let mut hit = torrents::hit(row, 0.0, None);
    search::enrich(&mut hit, e);
    hit
}

/// A refusal from the store as a 422 on `field`; its message names the
/// rule, never the value.
pub(crate) fn invalid(e: StoreError, field: &'static str) -> ApiError {
    match e {
        StoreError::Invalid(rule) => {
            let field = if rule.contains("label") {
                "labels"
            } else if rule.starts_with("note") {
                "note"
            } else if rule.starts_with("name") {
                "name"
            } else if rule.starts_with("query") || rule.starts_with("ast") {
                "q"
            } else if rule.starts_with("sort") {
                "sort"
            } else {
                field
            };
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_value",
                format!("That value isn't allowed: {rule}."),
            )
            .field(field)
        }
        e => internal(e),
    }
}

fn marks_of(hash: &InfoHash, item: Option<&UserItem>) -> UserMarks {
    UserMarks {
        created_at: item.map(|i| i.created_at),
        favourite: item.is_some_and(|i| i.favourite),
        hash: torrents::hex(&hash.0),
        hidden: item.is_some_and(|i| i.hidden),
        labels: item.map(|i| i.labels.clone()).unwrap_or_default(),
        note: item.and_then(|i| i.note.clone()),
        updated_at: item.map(|i| i.updated_at),
    }
}

/// Folds CRLF (and a lone CR from old editors' pastes is left for the
/// store to refuse) to LF, then trims.
pub(crate) fn clean_note(note: &str) -> String {
    note.replace("\r\n", "\n").trim().to_string()
}

/// `"<updated_at>.<torrent_id>"`.
fn parse_cursor(text: &str) -> ApiResult<(i64, i64)> {
    let bad = || {
        ApiError::bad_request(
            "bad_cursor",
            "That cursor isn't one this server gave out; start again.",
        )
        .field("cursor")
    };
    let (at, id) = text.split_once('.').ok_or_else(bad)?;
    Ok((
        at.parse().map_err(|_| bad())?,
        id.parse().map_err(|_| bad())?,
    ))
}

// ---- handlers --------------------------------------------------------------

/// Your marked torrents, newest change first. Only torrents search would
/// show: those a filter rule hid appear with `hidden=true`, and Adult
/// only while "Show adult content" is on. A page is full unless the list
/// ends: withheld rows are skipped, never counted.
#[utoipa::path(
    operation_id = "listUserItems",
    get,
    path = "/user-items",
    tag = "library",
    params(UserItemsQuery),
    responses(
        (status = 200, body = UserItemsResponse),
        (status = 400, body = ErrorEnvelope, description = "`bad_cursor`: a cursor this server didn't give out."),
    )
)]
pub(crate) async fn list(
    State(s): State<ApiState>,
    Query(p): Query<UserItemsQuery>,
) -> ApiResult<Json<UserItemsResponse>> {
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut before = p.cursor.as_deref().map(parse_cursor).transpose()?;
    let adult = s.adult.shown();
    let hidden = p.hidden.unwrap_or(false);
    let (favourite, hidden_mark) = (p.favourite.unwrap_or(false), p.hidden_mark.unwrap_or(false));
    let label = p.label.map(|l| l.trim().to_string());
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        let mut items = Vec::new();
        let mut more = false;
        // The last row returned: a cursor never names a row that was
        // withheld, so it can't tell that one exists.
        let mut last = None;
        'pages: loop {
            let page = store
                .list_user_items(favourite, hidden_mark, label.as_deref(), before, BATCH)
                .map_err(|e| match e {
                    StoreError::Invalid(_) => ApiError::bad_request(
                        "bad_parameter",
                        "A query parameter has the wrong type or value.",
                    )
                    .field("label"),
                    e => internal(e),
                })?;
            let end = page.len() < BATCH;
            for item in page {
                let key = (item.updated_at, item.torrent_id);
                let Some((row, e)) = shown_row(&store, item.torrent_id, hidden, adult)? else {
                    before = Some(key);
                    continue;
                };
                if items.len() == limit {
                    more = true;
                    break 'pages;
                }
                items.push(UserItemRow {
                    created_at: item.created_at,
                    favourite: item.favourite,
                    hash: torrents::hex(&row.info_hash.0),
                    hidden: item.hidden,
                    labels: item.labels,
                    note: item.note,
                    torrent: hit_row(&row, e),
                    updated_at: item.updated_at,
                });
                last = Some(key);
                before = Some(key);
            }
            if end {
                break;
            }
        }
        let cursor = last.filter(|_| more).map(|(at, id)| format!("{at}.{id}"));
        Ok(Json(UserItemsResponse {
            items,
            limit,
            next_cursor: cursor,
        }))
    })
    .await
    .map_err(internal)?
}

/// Every label in use with the number of visible torrents carrying it.
#[utoipa::path(
    operation_id = "listLabels",
    get,
    path = "/user-items/labels",
    tag = "library",
    params(LabelsQuery),
    responses((status = 200, body = Vec<LabelCount>))
)]
pub(crate) async fn labels(
    State(s): State<ApiState>,
    Query(p): Query<LabelsQuery>,
) -> ApiResult<Json<Vec<LabelCount>>> {
    let adult = s.adult.shown();
    let hidden = p.hidden.unwrap_or(false);
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        let mut counts = BTreeMap::<String, u64>::new();
        let mut before = None;
        loop {
            let page = store
                .list_user_items(false, false, None, before, BATCH)
                .map_err(internal)?;
            let end = page.len() < BATCH;
            for item in &page {
                before = Some((item.updated_at, item.torrent_id));
                if item.labels.is_empty()
                    || shown_row(&store, item.torrent_id, hidden, adult)?.is_none()
                {
                    continue;
                }
                for l in &item.labels {
                    *counts.entry(l.clone()).or_default() += 1;
                }
            }
            if end {
                break;
            }
        }
        Ok(Json(
            counts
                .into_iter()
                .map(|(label, count)| LabelCount { count, label })
                .collect(),
        ))
    })
    .await
    .map_err(internal)?
}

/// A torrent's marks, and the collections it is in. A torrent without
/// marks answers with all-false, empty marks; an unknown torrent, or one
/// withheld (Adult while hidden), is `404 torrent_not_found`.
#[utoipa::path(
    operation_id = "getUserItem",
    get,
    path = "/user-items/{hash}",
    tag = "library",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    responses(
        (status = 200, body = UserItemDetail),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`."),
    )
)]
pub(crate) async fn get_one(
    State(s): State<ApiState>,
    Path(hash): Path<String>,
) -> ApiResult<Json<UserItemDetail>> {
    let hash = hash_of(&hash)?;
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        let (row, _) = visible(&store, &hash, true, adult)?;
        let item = store.user_item(row.id).map_err(internal)?;
        let collections = store.collections_of(row.id).map_err(internal)?;
        let m = marks_of(&hash, item.as_ref());
        Ok(Json(UserItemDetail {
            collections,
            created_at: m.created_at,
            favourite: m.favourite,
            hash: m.hash,
            hidden: m.hidden,
            labels: m.labels,
            note: m.note,
            updated_at: m.updated_at,
        }))
    })
    .await
    .map_err(internal)?
}

/// Changes some marks: absent leaves one as it is, `note: null` removes
/// the note. Labels and the note are trimmed first (and CRLF becomes LF).
/// Returns the marks after, all-false when none are left.
#[utoipa::path(
    operation_id = "patchUserItem",
    patch,
    path = "/user-items/{hash}",
    tag = "library",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    request_body = UserItemPatch,
    responses(
        (status = 200, body = UserMarks),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_value`: a label or the note isn't allowed (`field` names which)."),
    )
)]
pub(crate) async fn patch(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
    JsonBody(body): JsonBody<UserItemPatch>,
) -> ApiResult<Json<UserMarks>> {
    same_origin_json(&headers)?;
    let hash = hash_of(&hash)?;
    let adult = s.adult.shown();
    let labels: Option<Vec<String>> = body
        .labels
        .map(|v| v.iter().map(|l| l.trim().to_string()).collect());
    let note: Option<Option<String>> = body.note.map(|n| n.map(|n| clean_note(&n)));
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let id = {
            let store = s.reader.lock().expect("reader lock");
            visible(&store, &hash, true, adult)?.0.id
        };
        let label_refs: Option<Vec<&str>> = labels
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect());
        let update = UserItemUpdate {
            favourite: body.favourite,
            hidden: body.hidden,
            labels: label_refs.as_deref(),
            note: note.as_ref().map(|n| n.as_deref()),
        };
        let item = writer
            .lock()
            .expect("user data lock")
            .set_user_item(id, &update, unix_now())
            .map_err(|e| match e {
                // Gone between the lookup and the write.
                StoreError::NotFound(_) => not_found(),
                e => invalid(e, "labels"),
            })?;
        Ok(Json(marks_of(&hash, item.as_ref())))
    })
    .await
    .map_err(internal)?
}

/// Clears every mark on a torrent.
#[utoipa::path(
    operation_id = "clearUserItem",
    delete,
    path = "/user-items/{hash}",
    tag = "library",
    params(("hash" = String, Path, description = "A 40-character hex info hash.")),
    responses(
        (status = 200, body = OkResponse),
        (status = 404, body = ErrorEnvelope, description = "`torrent_not_found`."),
    )
)]
pub(crate) async fn clear(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Path(hash): Path<String>,
) -> ApiResult<Json<OkResponse>> {
    same_origin_json(&headers)?;
    let hash = hash_of(&hash)?;
    let adult = s.adult.shown();
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let id = {
            let store = s.reader.lock().expect("reader lock");
            visible(&store, &hash, true, adult)?.0.id
        };
        writer
            .lock()
            .expect("user data lock")
            .delete_user_item(id)
            .map_err(internal)?;
        Ok(Json(OkResponse::OK))
    })
    .await
    .map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_parse_or_are_refused() {
        assert_eq!(parse_cursor("1700000000.42").unwrap(), (1_700_000_000, 42));
        for bad in ["", "1", "a.b", "1.", ".1", "1.2.3", "1,2"] {
            let e = parse_cursor(bad).unwrap_err();
            assert_eq!(e.code(), "bad_cursor", "{bad:?}");
            assert_eq!(e.body()["error"]["field"], "cursor");
        }
    }

    #[test]
    fn notes_lose_crlf_and_padding() {
        assert_eq!(clean_note("  a\r\nb \r\n"), "a\nb");
        assert_eq!(clean_note("\r\n"), "");
    }

    #[test]
    fn store_refusals_name_their_field_not_their_value() {
        let e = invalid(
            StoreError::Invalid("a label must be 1-64 characters"),
            "note",
        );
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(e.body()["error"]["field"], "labels");
        let e = invalid(
            StoreError::Invalid("note must be 1-4000 characters"),
            "labels",
        );
        assert_eq!(e.body()["error"]["field"], "note");
        let e = invalid(StoreError::Invalid("ast is not JSON"), "name");
        assert_eq!(e.body()["error"]["field"], "q");
        assert_eq!(e.code(), "invalid_value");
    }

    #[test]
    fn marks_without_a_row_are_empty() {
        let m = marks_of(&InfoHash([0xab; 20]), None);
        assert_eq!(m.hash, "ab".repeat(20));
        assert!(!m.favourite && !m.hidden && m.labels.is_empty());
        assert!(m.note.is_none() && m.created_at.is_none() && m.updated_at.is_none());
    }
}
