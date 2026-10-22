//! `/parse` and `/search` (§8, §9): the query language over the index,
//! with display rows read from SQLite.

use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use hmac::{Hmac, KeyInit, Mac};
use lodestar_classify::{Category, Composition, Record};
use lodestar_search::{
    CancelToken, Cursor, Error as SearchError, Lease, Node, ParseError, Sort, Start, parse,
};
use lodestar_store::Enriched;
use rand::RngExt;
use sha2::{Digest, Sha256};
use utoipa::OpenApi;

use crate::dto::search::{
    CompositionShare, ParseQuery, ParseResponse, SearchHit, SearchRequest, SearchResponse,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json, Query, same_origin_json};
use crate::{ApiState, auth, torrents};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(parse_query, search_get, search_post))]
pub(crate) struct ApiDoc;

/// Results per page, at most.
pub const MAX_LIMIT: usize = 200;
/// How deep offset paging may go; deeper pages follow `next_cursor`,
/// which reads the same snapshot through a searcher lease (§8).
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
        ("offset" = Option<usize>, Query, description = "`offset + limit` may not pass 10,000. Not with `cursor`."),
        ("hidden" = Option<bool>, Query, description = "Include torrents a filter rule hid (§20.3)."),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`; send the same `q`, `sort` and `hidden` with it. An expired one is a 410 `cursor_expired`, a tampered one a 400 `bad_cursor`."),
        ("exact" = Option<bool>, Query, description = "`1` or `true`: count every match exactly, within a time budget."),
    ),
    responses((status = 200, body = SearchResponse))
)]
pub(crate) async fn search_get(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Query(p): Query<SearchRequest>,
) -> ApiResult<axum::Json<SearchResponse>> {
    if p.ast.is_some() {
        return Err(ApiError::bad_request("ast_needs_post", "Send an AST with POST.").field("ast"));
    }
    run_search(s, &headers, p).await
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
    run_search(s, &headers, p).await
}

async fn run_search(
    s: ApiState,
    headers: &HeaderMap,
    p: SearchRequest,
) -> ApiResult<axum::Json<SearchResponse>> {
    let Some(search) = s.index.clone() else {
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
    if p.cursor.is_some() && p.offset.is_some() {
        return Err(
            ApiError::bad_request("cursor_and_offset", "Send cursor or offset, not both.")
                .field("offset"),
        );
    }
    let offset = p.offset.unwrap_or(0);
    // `offset` is the client's: saturate rather than overflow.
    if offset.saturating_add(limit) > MAX_WINDOW {
        return Err(ApiError::bad_request(
            "window_too_deep",
            format!("Offsets past {MAX_WINDOW} aren't paged; page with next_cursor."),
        )
        .field("offset"));
    }
    let shown = if p.hidden.unwrap_or(false) {
        ast.clone()
    } else {
        ast.clone().without_hidden()
    };
    let fingerprint = fingerprint(&shown, sort);
    let client = client_of(headers);
    let cursor = p
        .cursor
        .as_deref()
        .map(|c| open_cursor(c, &fingerprint))
        .transpose()?;
    let exact = p.exact.unwrap_or(false);
    let reader = s.reader.clone();
    // Axum drops this future when the client disconnects; the guard then
    // cancels the search running on the blocking pool.
    let cancel = CancelToken::new();
    let _guard = cancel.cancel_on_drop();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let (lease, start, served, id) = match cursor {
            Some(c) => {
                let lease = search
                    .leases()
                    .get(&c.lease, client)
                    .map_err(search_error)?;
                if !lease.fits(&c.position) {
                    return Err(bad_cursor());
                }
                (lease, Start::After(c.position), c.served, Some(c.lease))
            }
            None => {
                // The index serving now, once for this request; the lease
                // keeps it for the pages after.
                let lease = Lease::new(search.index(), &shown, unix_now(), sort, &cancel)
                    .map_err(search_error)?;
                (Arc::new(lease), Start::Offset(offset), offset as u64, None)
            }
        };
        let total = if exact {
            lease.exact_total(&cancel).map_err(search_error)?
        } else {
            lease.total()
        };
        let page = lease.page(start, limit, &cancel).map_err(search_error)?;
        // A lease is kept only when there's a next page to read from it.
        let next_cursor = page.next.map(|position| {
            let lease_id = id.unwrap_or_else(|| search.leases().insert(lease.clone(), client));
            seal_cursor(&Cursor {
                lease: lease_id,
                fingerprint,
                sort: lease.sort(),
                served: served + page.hits.len() as u64,
                position,
            })
        });
        let store = reader.lock().expect("reader lock");
        let mut hits: Vec<SearchHit> = Vec::with_capacity(page.hits.len());
        for h in &page.hits {
            // A torrent deleted since indexing just drops out of the page.
            if let Some(row) = store.torrent(h.id as i64).map_err(internal)? {
                let matched_files = if ast.has_file_clause() {
                    // Positions in the torrent's file list (§8), from the
                    // lease's index.
                    Some(
                        lease
                            .index()
                            .matched_files(&ast, h.id, MATCHED_FILES)
                            .map_err(internal)?,
                    )
                } else {
                    None
                };
                let mut hit = torrents::hit(&row, h.score, matched_files);
                enrich(&mut hit, store.enrichment(row.id).map_err(internal)?);
                hits.push(hit);
            }
        }
        Ok(axum::Json(SearchResponse {
            canonical: ast.to_query(),
            hits,
            limit,
            next_cursor,
            offset: served as usize,
            stemmed: lease.stemmed(),
            took_ms: started.elapsed().as_secs_f64() * 1000.0,
            total: total.value,
            total_approx: total.approx,
        }))
    })
    .await
    .map_err(internal)?
}

fn search_error(e: SearchError) -> ApiError {
    match e {
        SearchError::LeaseExpired => ApiError::new(
            StatusCode::GONE,
            "cursor_expired",
            "These results changed or expired; reload the search.",
        )
        .field("cursor"),
        // Only a client that's gone sees it.
        SearchError::Cancelled => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_cancelled",
            "The search was cancelled.",
        ),
        e => internal(e),
    }
}

fn bad_cursor() -> ApiError {
    ApiError::bad_request(
        "bad_cursor",
        "That cursor isn't one this server gave out; start the search again.",
    )
    .field("cursor")
}

/// The search a cursor continues: the query as run (hidden torrents
/// included or not) and the sort.
fn fingerprint(shown: &Node, sort: Sort) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(b"lodestar-search-v1|");
    h.update(format!("{shown:?}|{sort:?}").as_bytes());
    let d = h.finalize();
    let mut out = [0; 8];
    out.copy_from_slice(&d[..8]);
    out
}

/// Who holds a lease: the session (leases are capped per client, and
/// another session's cursor reads as expired).
fn client_of(headers: &HeaderMap) -> u64 {
    let session = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| auth::cookie_value(c, auth::COOKIE));
    let Some(session) = session else {
        return 0;
    };
    let d = Sha256::digest(session.as_bytes());
    u64::from_le_bytes(d[..8].try_into().expect("8 bytes"))
}

/// This process's cursor keys. Leases live in memory, so a cursor from
/// another process (a restart) names none: its epoch says so, and it
/// reads as expired rather than forged.
struct CursorKeys {
    epoch: [u8; 4],
    mac: [u8; 32],
}

fn cursor_keys() -> &'static CursorKeys {
    static KEYS: OnceLock<CursorKeys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut rng = rand::rng();
        CursorKeys {
            epoch: rng.random(),
            mac: rng.random(),
        }
    })
}

/// Bytes of HMAC-SHA-256 kept in a cursor.
const TAG: usize = 16;
/// Cursors are at most this long (§9 contract: ≤ 256).
const MAX_CURSOR: usize = 256;

fn cursor_mac(body: &[u8]) -> Hmac<Sha256> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(&cursor_keys().mac).expect("any key length");
    mac.update(b"lodestar-cursor-v1|");
    mac.update(body);
    mac
}

/// `epoch ‖ cursor ‖ tag`, in lowercase hex: URL-safe, at most 162
/// characters.
fn seal_cursor(c: &Cursor) -> String {
    let mut body = cursor_keys().epoch.to_vec();
    body.extend(c.to_bytes());
    let tag = cursor_mac(&body).finalize().into_bytes();
    body.extend_from_slice(&tag[..TAG]);
    body.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks a cursor's form, epoch, tag and search, in that order.
fn open_cursor(text: &str, fingerprint: &[u8; 8]) -> ApiResult<Cursor> {
    if text.len() > MAX_CURSOR || text.len() % 2 != 0 {
        return Err(bad_cursor());
    }
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|i| {
            let pair = text.get(i..i + 2)?;
            // Lowercase only, as `seal_cursor` writes it.
            if !pair
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            u8::from_str_radix(pair, 16).ok()
        })
        .collect();
    let Some(bytes) = bytes.filter(|b| b.len() > 4 + TAG) else {
        return Err(bad_cursor());
    };
    let (body, tag) = bytes.split_at(bytes.len() - TAG);
    if body[..4] != cursor_keys().epoch {
        return Err(search_error(SearchError::LeaseExpired));
    }
    cursor_mac(body)
        .verify_truncated_left(tag)
        .map_err(|_| bad_cursor())?;
    let cursor = Cursor::from_bytes(&body[4..]).ok_or_else(bad_cursor)?;
    if cursor.fingerprint != *fingerprint {
        return Err(ApiError::bad_request(
            "bad_cursor",
            "That cursor belongs to another search: send the same q, sort and hidden with it.",
        )
        .field("cursor"));
    }
    Ok(cursor)
}

/// The §10.18 colour groups in the composition bar's order, and the
/// categories each holds.
const GROUPS: [(&str, &[Category]); 7] = [
    ("video", &[Category::Video]),
    ("audio", &[Category::Audio]),
    ("images", &[Category::Image]),
    (
        "documents",
        &[Category::Document, Category::Ebook, Category::Subtitle],
    ),
    ("archives", &[Category::Archive, Category::Disc]),
    ("software", &[Category::Software]),
    ("other", &[Category::Other]),
];

/// `Composition::pct_bytes`'s rounding: any non-zero share is at least
/// 1, and only a complete one is 100.
fn percent(part: u64, total: u64) -> u8 {
    if part == 0 || total == 0 {
        return 0;
    }
    if part >= total {
        return 100;
    }
    ((u128::from(part) * 100 / u128::from(total)) as u8).clamp(1, 99)
}

fn shares(c: &Composition) -> Vec<CompositionShare> {
    let (all_bytes, all_files) = (c.total_bytes(), u64::from(c.total_count()));
    GROUPS
        .iter()
        .filter_map(|(group, cats)| {
            let (files, bytes) = cats.iter().fold((0u32, 0u64), |(f, b), cat| {
                let s = c.get(*cat);
                (f.saturating_add(s.count), b.saturating_add(s.bytes))
            });
            (files > 0).then(|| CompositionShare {
                bytes,
                files,
                group: (*group).to_string(),
                pct_bytes: percent(bytes, all_bytes),
                pct_files: percent(u64::from(files), all_files),
            })
        })
        .collect()
}

/// Fills a hit's classification fields (`docs/metrics.md` §7.1) from the
/// stored enrichment. Without one, or with a record this build can't
/// read, they stay null or empty.
fn enrich(hit: &mut SearchHit, e: Option<Enriched>) {
    let Some(e) = e else {
        return;
    };
    if let Ok(r) = Record::decode(&e.data) {
        hit.title = r.release("title").map(str::to_string);
        hit.resolved_via = r.resolved.map(|v| v.as_str().to_string());
        hit.content_type = Some(r.content_type.as_str().to_string());
        hit.adult_kind = r.adult_kind.map(|k| k.as_str().to_string());
        hit.junk_score = Some(r.junk.score);
        hit.junk_reasons = r
            .junk
            .reasons
            .iter()
            .map(|k| k.as_str().to_string())
            .collect();
    }
    if let Ok(c) = Composition::decode(&e.comp) {
        hit.composition = shares(&c);
    }
}

pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
