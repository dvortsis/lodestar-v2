//! `/parse` and `/search` (§8, §9): the query language over the index,
//! with display rows read from SQLite.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use lodestar_search::query::Node;
use lodestar_search::{ParseError, Sort, parse};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json, Query, same_origin_json};
use crate::{ApiState, auth, torrents};

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

#[derive(Deserialize)]
struct QParam {
    #[serde(default)]
    q: String,
}

/// A query syntax error, with the span the UI highlights.
fn parse_error(e: ParseError) -> ApiError {
    ApiError::bad_request("query_syntax", e.message)
        .field("q")
        .span(e.start, e.end)
}

/// `GET /parse?q=` → the AST and its canonical text, or the error with
/// its span (the UI highlights it).
async fn parse_query(Query(p): Query<QParam>) -> ApiResult<axum::Json<Value>> {
    let ast = parse(&p.q).map_err(parse_error)?;
    Ok(axum::Json(
        json!({ "ast": ast, "canonical": ast.to_query() }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchParams {
    q: Option<String>,
    ast: Option<Node>,
    sort: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
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

async fn search_get(
    State(s): State<ApiState>,
    Query(p): Query<SearchParams>,
) -> ApiResult<axum::Json<Value>> {
    if p.ast.is_some() {
        return Err(ApiError::bad_request("ast_needs_post", "Send an AST with POST.").field("ast"));
    }
    run_search(s, p).await
}

async fn search_post(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Json(p): Json<SearchParams>,
) -> ApiResult<axum::Json<Value>> {
    same_origin_json(&headers)?;
    run_search(s, p).await
}

async fn run_search(s: ApiState, p: SearchParams) -> ApiResult<axum::Json<Value>> {
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
    if offset + limit > MAX_WINDOW {
        return Err(ApiError::bad_request(
            "window_too_deep",
            format!("Results past {MAX_WINDOW} aren't paged; narrow the query."),
        )
        .field("offset"));
    }
    let reader = s.reader.clone();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let (results, stemmed) = index
            .search_with_fallback(&ast, unix_now(), sort, limit, offset)
            .map_err(internal)?;
        let store = reader.lock().expect("reader lock");
        let mut hits = Vec::with_capacity(results.hits.len());
        for h in &results.hits {
            // A torrent deleted since indexing just drops out of the page.
            if let Some(row) = store.torrent(h.id as i64).map_err(internal)? {
                let mut v = torrents::summary(&row);
                v["score"] = json!(h.score);
                if ast.has_file_clause() {
                    // Positions in the torrent's file list (§8).
                    v["matched_files"] = json!(
                        index
                            .matched_files(&ast, h.id, MATCHED_FILES)
                            .map_err(internal)?
                    );
                }
                hits.push(v);
            }
        }
        Ok(axum::Json(json!({
            "total": results.total,
            "offset": offset,
            "limit": limit,
            "took_ms": started.elapsed().as_secs_f64() * 1000.0,
            "canonical": ast.to_query(),
            // Few exact matches: plural forms in titles were included too
            // (§8), ranked after the exact ones.
            "stemmed": stemmed,
            "hits": hits,
        })))
    })
    .await
    .map_err(internal)?
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
