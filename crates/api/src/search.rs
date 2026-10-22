//! `/parse` and `/search` (§8, §9): the query language over the index,
//! with display rows read from SQLite.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use lodestar_search::{ParseError, Sort, parse};
use utoipa::OpenApi;

use crate::dto::search::{ParseQuery, ParseResponse, SearchHit, SearchRequest, SearchResponse};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json, Query, same_origin_json};
use crate::{ApiState, auth, torrents};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(parse_query, search_get, search_post))]
pub(crate) struct ApiDoc;

/// Results per page, at most.
pub const MAX_LIMIT: usize = 200;
/// How deep offset paging may go; past this, narrow the query (stable
/// cursors arrive with searcher leases, §8).
pub const MAX_WINDOW: usize = 10_000;
/// Matched files listed per hit for a query with `file:(…)`.
pub const MATCHED_FILES: usize = 20;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/parse", get(parse_query))
            .route("/search", get(search_get).post(search_post)),
    )
}

/// A query syntax error, with the span the UI highlights.
fn parse_error(e: ParseError) -> ApiError {
    ApiError::bad_request("query_syntax", e.message)
        .field("q")
        .span(e.start, e.end)
}

/// Parses a query: the AST and its canonical text, or a `query_syntax`
/// error whose `start`/`end` span the UI highlights.
#[utoipa::path(
    operation_id = "parseQuery",
    get,
    path = "/parse",
    tag = "search",
    params(ParseQuery),
    responses((status = 200, body = ParseResponse))
)]
pub(crate) async fn parse_query(
    Query(p): Query<ParseQuery>,
) -> ApiResult<axum::Json<ParseResponse>> {
    let ast = parse(&p.q).map_err(parse_error)?;
    let canonical = ast.to_query();
    Ok(axum::Json(ParseResponse { ast, canonical }))
}

fn sort_param(s: Option<&str>) -> ApiResult<Sort> {
    Ok(match s.unwrap_or("relevance") {
        "relevance" => Sort::Relevance,
        "discovered" | "-discovered" => Sort::Discovered { desc: true },
        "+discovered" => Sort::Discovered { desc: false },
        "size" | "-size" => Sort::Size { desc: true },
        "+size" => Sort::Size { desc: false },
        "files" | "-files" => Sort::Files { desc: true },
        "+files" => Sort::Files { desc: false },
        other => {
            return Err(ApiError::bad_request(
                "unknown_sort",
                format!(
                    "Unknown sort {other:?}: use relevance, discovered, size or files, with + or - for the order."
                ),
            )
            .field("sort"));
        }
    })
}

/// Searches with query text. To search with an AST, POST it.
#[utoipa::path(
    operation_id = "searchGet",
    get,
    path = "/search",
    tag = "search",
    params(
        ("q" = Option<String>, Query, description = "The query text; empty browses everything."),
        ("sort" = Option<String>, Query, description = "`relevance` (the default), or `discovered`, `size` or `files` with an optional `+` (ascending) or `-` (descending, the default)."),
        ("limit" = Option<usize>, Query, description = "Results per page: 1–200, 50 by default."),
        ("offset" = Option<usize>, Query, description = "`offset + limit` may not pass 10,000."),
        ("hidden" = Option<bool>, Query, description = "Include torrents a filter rule hid (§20.3)."),
    ),
    responses((status = 200, body = SearchResponse))
)]
pub(crate) async fn search_get(
    State(s): State<ApiState>,
    Query(p): Query<SearchRequest>,
) -> ApiResult<axum::Json<SearchResponse>> {
    if p.ast.is_some() {
        return Err(ApiError::bad_request("ast_needs_post", "Send an AST with POST.").field("ast"));
    }
    run_search(s, p).await
}

/// Searches with query text or a query AST.
#[utoipa::path(
    operation_id = "searchPost",
    post,
    path = "/search",
    tag = "search",
    request_body = SearchRequest,
    responses((status = 200, body = SearchResponse))
)]
pub(crate) async fn search_post(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Json(p): Json<SearchRequest>,
) -> ApiResult<axum::Json<SearchResponse>> {
    same_origin_json(&headers)?;
    run_search(s, p).await
}

async fn run_search(s: ApiState, p: SearchRequest) -> ApiResult<axum::Json<SearchResponse>> {
    let Some(index) = s.index.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_disabled",
            "Search is turned off.",
        ));
    };
    let ast = match (p.ast, p.q) {
        (Some(ast), None) => {
            // An AST from a client must be one the parser could produce.
            let text = ast.to_query();
            match parse(&text) {
                Ok(again) if again == ast => ast,
                _ => {
                    return Err(ApiError::bad_request(
                        "invalid_ast",
                        "That query AST isn't one the parser can produce.",
                    )
                    .field("ast"));
                }
            }
        }
        (None, q) => parse(q.as_deref().unwrap_or("")).map_err(parse_error)?,
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "q_and_ast",
                "Send q or ast, not both.",
            ));
        }
    };
    let sort = sort_param(p.sort.as_deref())?;
    let limit = p.limit.unwrap_or(50).clamp(1, MAX_LIMIT);
    let offset = p.offset.unwrap_or(0);
    // `offset` is the client's: saturate rather than overflow.
    if offset.saturating_add(limit) > MAX_WINDOW {
        return Err(ApiError::bad_request(
            "window_too_deep",
            format!("Results past {MAX_WINDOW} aren't paged; narrow the query."),
        )
        .field("offset"));
    }
    let reader = s.reader.clone();
    let shown = if p.hidden.unwrap_or(false) {
        ast.clone()
    } else {
        ast.clone().without_hidden()
    };
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let (results, stemmed) = index
            .search_with_fallback(&shown, unix_now(), sort, limit, offset)
            .map_err(internal)?;
        let store = reader.lock().expect("reader lock");
        let mut hits: Vec<SearchHit> = Vec::with_capacity(results.hits.len());
        for h in &results.hits {
            // A torrent deleted since indexing just drops out of the page.
            if let Some(row) = store.torrent(h.id as i64).map_err(internal)? {
                let matched_files = if ast.has_file_clause() {
                    // Positions in the torrent's file list (§8).
                    Some(
                        index
                            .matched_files(&ast, h.id, MATCHED_FILES)
                            .map_err(internal)?,
                    )
                } else {
                    None
                };
                hits.push(torrents::hit(&row, h.score, matched_files));
            }
        }
        Ok(axum::Json(SearchResponse {
            canonical: ast.to_query(),
            hits,
            limit,
            offset,
            stemmed,
            took_ms: started.elapsed().as_secs_f64() * 1000.0,
            total: results.total,
        }))
    })
    .await
    .map_err(internal)?
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
