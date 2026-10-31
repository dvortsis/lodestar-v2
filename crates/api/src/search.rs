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
use lodestar_search::natural::{NaturalError, natural};
use lodestar_search::query::TagKey;
use lodestar_search::recovery::{self, MAX_CLAUSES, RECOVERY_BUDGET};
use lodestar_search::{
    CancelToken, Cursor, EXPLAIN_MAX, Error as SearchError, Explanation, Lease, Node, ParseError,
    RebuildProgress, SearchSpec, Sort, Start, WEIGHTS, parse,
};
use lodestar_store::{Enriched, RollupBy, Store};
use rand::RngExt;
use sha2::{Digest, Sha256};
use utoipa::OpenApi;

use crate::dto::events::RebuildPhase;
use crate::dto::search::{
    ClauseRemoval, CompositionShare, ExplainHit, ExplainPhase2, ExplainPriors, ExplainWeights,
    IndexRebuild, NaturalChip, NaturalProposal, ParseQuery, ParseResponse, SearchExplain,
    SearchHit, SearchIndexState, SearchNotice, SearchRecovery, SearchRequest, SearchResponse,
};
use crate::dto::suggest::CharRange;
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json, Query, same_origin_json};
use crate::insights::codes::metric;
use crate::{ApiState, INTEREST_TOP, auth, swarm, torrents};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(parse_query, search_get, search_post, index_state))]
pub(crate) struct ApiDoc;

/// The widest real UTC offsets, in minutes.
const TZ_RANGE: std::ops::RangeInclusive<i32> = -14 * 60..=14 * 60;

/// Results per page, at most.
pub const MAX_LIMIT: usize = 200;
/// How deep offset paging may go; deeper pages follow `next_cursor`,
/// which reads the same snapshot through a searcher lease (§8).
pub const MAX_WINDOW: usize = 10_000;
/// Matched files listed per hit for a query with `file:(…)`.
pub const MATCHED_FILES: usize = 20;

/// Whether "Show adult content" is on (§20.9), read live: the server
/// hands in a reader over the settings registry's `content` channel, so
/// a change applies to the next request with no restart. The default,
/// for tests and tools, keeps Adult hidden.
#[derive(Clone, Default)]
pub struct AdultVisibility(Option<Arc<dyn Fn() -> bool + Send + Sync>>);

impl AdultVisibility {
    /// Follows `shown`, called once per request.
    pub fn follow(shown: impl Fn() -> bool + Send + Sync + 'static) -> AdultVisibility {
        AdultVisibility(Some(Arc::new(shown)))
    }

    /// Whether Adult is shown now.
    pub fn shown(&self) -> bool {
        self.0.as_ref().is_some_and(|f| f())
    }
}

impl std::fmt::Debug for AdultVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AdultVisibility")
            .field(&self.shown())
            .finish()
    }
}

/// What a search may show of the index, besides what the query asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Visibility {
    /// Torrents a filter rule hid are included (`hidden=true`, §20.3).
    pub hidden: bool,
    /// Adult torrents are included ("Show adult content", §20.9).
    pub adult: bool,
}

/// A query as it runs against the index, with what it left out.
///
/// The user's part and the visibility clauses are kept apart because a
/// visibility clause has the same shape as a user's own (`-type:adult`):
/// a pass that edits the user's query, like a facet dropping its own
/// clauses, must never see them. Run [`BaseQuery::query`], or
/// [`BaseQuery::restrict`] on whatever the pass made of `node`.
#[derive(Debug, Clone, PartialEq)]
pub struct BaseQuery {
    /// The user's query, without the torrents a filter rule hid unless
    /// asked. Never run it on its own.
    pub node: Node,
    /// Clauses ANDed onto every query the index runs, after any edit of
    /// `node` (today: `-type:adult` while Adult is hidden).
    pub visibility: Vec<Node>,
    /// What the response tells the user about it.
    pub notices: Vec<SearchNotice>,
}

impl BaseQuery {
    /// `node` as the index runs it.
    pub fn query(&self) -> Node {
        self.restrict(self.node.clone())
    }

    /// `node` (the user's query, or a pass's edit of it) with the
    /// visibility clauses ANDed on.
    pub fn restrict(&self, node: Node) -> Node {
        // The same function `/facets` holds its lease to.
        lodestar_search::with_filters(node, &self.visibility)
    }
}

/// The query every count of the index runs (search, totals, exact
/// counts, cursors and facets): `ast` without the torrents a filter rule
/// hid unless `v.hidden` (or it asks about them itself), and without
/// Adult unless `v.adult`.
///
/// Adult is left out whatever the query asks: `type:adult` or `adult:…`
/// while it's hidden matches nothing and carries the `adult_hidden`
/// notice. Adult follows the stored, effective class (`type:adult` in
/// the index), so your Is adult / Not adult corrections apply once the
/// live indexer picks them up.
///
/// Facets (B6) strip their own clauses from `node`, then run
/// `restrict(stripped)` for every pass, so `type` never counts Adult
/// while it's hidden.
pub fn base_query(ast: &Node, v: Visibility) -> BaseQuery {
    let node = if v.hidden {
        ast.clone()
    } else {
        ast.clone().without_hidden()
    };
    let mut visibility = Vec::new();
    let mut notices = Vec::new();
    if !v.adult {
        if asks_for_adult(ast, true) {
            notices.push(SearchNotice::AdultHidden);
        }
        visibility.push(Node::Not {
            of: Box::new(Node::Tag {
                key: TagKey::Type,
                any: vec![ADULT.to_string()],
            }),
        });
    }
    BaseQuery {
        node,
        visibility,
        notices,
    }
}

/// `type:adult`, as the index stores it.
const ADULT: &str = "adult";

/// Whether a clause that isn't negated asks for Adult (`type:adult`,
/// `adult:…`). `-type:adult` doesn't.
fn asks_for_adult(node: &Node, positive: bool) -> bool {
    match node {
        Node::Tag {
            key: TagKey::Type,
            any,
        } => positive && any.iter().any(|t| t == ADULT),
        Node::Tag {
            key: TagKey::Adult, ..
        } => positive,
        Node::And { of } | Node::Or { of } => of.iter().any(|n| asks_for_adult(n, positive)),
        Node::Not { of } => asks_for_adult(of, !positive),
        _ => false,
    }
}

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/parse", get(parse_query))
            .route("/search", get(search_get).post(search_post))
            .route("/search/state", get(index_state)),
    )
}

/// A query syntax error, with the span the UI highlights.
fn parse_error(e: ParseError) -> ApiError {
    ApiError::bad_request("query_syntax", e.message)
        .field("q")
        .span(e.start, e.end)
}

/// Parses a query: the AST and its canonical text, or a `query_syntax`
/// error whose `start`/`end` span the UI highlights. With
/// `natural=true`, also the query `q` means as natural phrasing
/// (`natural_too_long` past 512 bytes or 32 words).
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
    let natural = p
        .natural
        .then(|| natural(&p.q))
        .transpose()
        .or_else(|e| match e {
            // `q` parsed above, so it can't be this; nothing to propose.
            NaturalError::Syntax => Ok(None),
            e => Err(ApiError::bad_request(
                "natural_too_long",
                format!("Natural phrasing reads at most 512 bytes and 32 words and operators; this is {e}."),
            )
            .field("q")),
        })?
        .map(|n| NaturalProposal {
            ast: n.ast,
            canonical: n.canonical,
            chips: n
                .chips
                .into_iter()
                .map(|c| NaturalChip {
                    from: CharRange {
                        end: c.from.1,
                        start: c.from.0,
                    },
                    text: c.text,
                })
                .collect(),
            text: n.text,
        });
    Ok(axum::Json(ParseResponse {
        ast,
        canonical,
        natural,
    }))
}

pub(crate) fn sort_param(s: Option<&str>) -> ApiResult<Sort> {
    Ok(match s.unwrap_or("relevance") {
        "relevance" => Sort::Relevance,
        "discovered" | "-discovered" => Sort::Discovered { desc: true },
        "+discovered" => Sort::Discovered { desc: false },
        "size" | "-size" => Sort::Size { desc: true },
        "+size" => Sort::Size { desc: false },
        "files" | "-files" => Sort::Files { desc: true },
        "+files" => Sort::Files { desc: false },
        "seeders" | "-seeders" => Sort::Seeders { desc: true },
        "+seeders" => Sort::Seeders { desc: false },
        "leechers" | "-leechers" => Sort::Leechers { desc: true },
        "+leechers" => Sort::Leechers { desc: false },
        "trackers" | "-trackers" => Sort::Trackers { desc: true },
        "+trackers" => Sort::Trackers { desc: false },
        "seen" | "-seen" => Sort::Seen { desc: true },
        "+seen" => Sort::Seen { desc: false },
        other => {
            return Err(ApiError::bad_request(
                "unknown_sort",
                format!(
                    "Unknown sort {other:?}: use relevance, discovered, size, files, seeders, leechers, trackers or seen, with + or - for the order."
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
        ("sort" = Option<String>, Query, description = "`relevance` (the default), or `discovered`, `size`, `files`, `seeders`, `leechers`, `trackers` or `seen` with an optional `+` (ascending) or `-` (descending, the default). Never-scraped torrents sort last either way."),
        ("limit" = Option<usize>, Query, description = "Results per page: 1–200, 50 by default."),
        ("offset" = Option<usize>, Query, description = "`offset + limit` may not pass 10,000. Not with `cursor`."),
        ("hidden" = Option<bool>, Query, description = "Include torrents a filter rule hid (§20.3)."),
        ("tz" = Option<i32>, Query, description = "The user's zone for `time:`, in minutes east of UTC (−840 to 840); 0 by default."),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`; send the same `q`, `sort` and `hidden` with it. An expired one is a 410 `cursor_expired`, a tampered one a 400 `bad_cursor`."),
        ("exact" = Option<bool>, Query, description = "`1` or `true`: count every match exactly, within a time budget."),
        ("explain" = Option<bool>, Query, description = "`1` or `true`: explain each hit's score (`explain` in the response). First page only, at most 100 hits."),
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

/// Searches with query text or a query AST. The body also takes
/// `"explain": true` (as `GET`'s `explain`).
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
        return Err(search_disabled());
    };
    let ast = request_ast(p.ast, p.q)?;
    let sort = sort_param(p.sort.as_deref())?;
    let tz = tz_param(p.tz)?;
    let explain = p.explain.unwrap_or(false);
    let mut limit = p.limit.unwrap_or(50).clamp(1, MAX_LIMIT);
    if explain {
        if p.cursor.is_some() || p.offset.is_some_and(|o| o > 0) {
            return Err(ApiError::bad_request(
                "explain_first_page",
                "Explain covers the first page only: send it without offset or cursor.",
            )
            .field("explain"));
        }
        limit = limit.min(EXPLAIN_MAX);
    }
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
    let (hot, events) = (s.hot.clone(), s.events.clone());
    let visibility = Visibility {
        hidden: p.hidden.unwrap_or(false),
        adult: s.adult.shown(),
    };
    let base = base_query(&ast, visibility);
    let shown = base.query();
    let notices = base.notices;
    let fingerprint = fingerprint(&shown, visibility, sort);
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
        // A search that fails while running counts in the Serve rollups as
        // failed. A bad or expired cursor isn't a failed search.
        let failed = |e: lodestar_search::Error| {
            if let Some(ev) = &events {
                ev.searched(started.elapsed(), None);
            }
            search_error(e)
        };
        // A first page comes from the result cache (§8) unless it's
        // explained, which bypasses it.
        let mut cached = None;
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
            None if offset == 0 && !explain => {
                // The index serving now, once for this request; the
                // cached lease reads the snapshot the page came from.
                let spec = SearchSpec {
                    node: &shown,
                    scope: &scope_of(visibility),
                    sort,
                    limit,
                    now: unix_now(),
                    utc_offset: tz * 60,
                };
                let (got, _) = search
                    .cache()
                    .search(&search.index(), &spec, &cancel)
                    .map_err(failed)?;
                let lease = got.lease.clone();
                cached = Some(got);
                (lease, Start::Offset(0), 0, None)
            }
            None => {
                // The index serving now, once for this request; the lease
                // keeps it for the pages after.
                let lease =
                    Lease::new_at(search.index(), &shown, unix_now(), tz * 60, sort, &cancel)
                        .map_err(failed)?;
                (Arc::new(lease), Start::Offset(offset), offset as u64, None)
            }
        };
        let total = if exact {
            lease.exact_total(&cancel).map_err(failed)?
        } else {
            lease.total()
        };
        let paged = match &cached {
            Some(c) => Ok((c.page.clone(), None)),
            None if explain => {
                lodestar_search::explain(&lease, limit, &cancel).map(|e| (e.page.clone(), Some(e)))
            }
            None => lease.page(start, limit, &cancel).map(|p| (p, None)),
        };
        let (page, explained) = paged.map_err(failed)?;
        // An empty first page: what removing each clause would give.
        let recovery = if served == 0 && total.value == 0 {
            recover(&lease, &ast, visibility, &cancel).map_err(failed)?
        } else {
            None
        };
        if let Some(ev) = &events {
            ev.searched(started.elapsed(), Some(total.value));
        }
        // The first screenful is what people look at: scrape it soon.
        if let (Some(ev), 0) = (&events, served) {
            for h in page.hits.iter().take(INTEREST_TOP) {
                ev.interest(h.id as i64);
            }
        }
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
        for h in page.hits.iter() {
            // A torrent deleted since indexing just drops out of the page.
            if let Some(row) = store.torrent(h.id as i64).map_err(internal)? {
                let enrichment = store.enrichment(row.id).map_err(internal)?;
                // A correction to Is adult reaches the index at the next
                // commit; until then the stored class still keeps it out.
                if !visibility.adult && enrichment.as_ref().is_some_and(is_adult) {
                    continue;
                }
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
                let swarm = swarm::summary(&store, hot.as_deref(), &row).map_err(internal)?;
                let mut hit = torrents::hit(&row, h.score, matched_files, swarm);
                enrich(&mut hit, enrichment);
                hits.push(hit);
            }
        }
        // Only the hits served: nothing the page left out is explained.
        let explain = explained.map(|e| explanation(&e, &hits));
        Ok(axum::Json(SearchResponse {
            canonical: ast.to_query(),
            explain,
            hits,
            limit,
            next_cursor,
            notices,
            offset: served as usize,
            recovery,
            stemmed: lease.stemmed(),
            took_ms: started.elapsed().as_secs_f64() * 1000.0,
            total: total.value,
            total_approx: total.approx,
        }))
    })
    .await
    .map_err(internal)?
}

fn search_disabled() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "search_disabled",
        "Search is turned off.",
    )
}

/// Zero-result recovery for `ast` on `lease`'s snapshot: each removable
/// top-level clause with the total of the query without it, or `None`
/// when the query has fewer than two clauses.
///
/// Each count runs [`base_query`] of the reduced query under the
/// request's own visibility, so it's exactly what `/search` would show
/// for it: Adult stays out while hidden whatever is removed (the
/// visibility clause isn't the user's to remove), and filter-hidden
/// torrents stay out unless `hidden=true` or the reduced query still asks
/// about them itself, which the full query then did too. Quarantined and
/// dropped torrents aren't in the index at all (§20.3).
fn recover(
    lease: &Lease,
    ast: &Node,
    v: Visibility,
    cancel: &CancelToken,
) -> Result<Option<SearchRecovery>, SearchError> {
    let removals = recovery::removals(ast);
    if removals.is_empty() {
        return Ok(None);
    }
    let queries: Vec<Node> = removals
        .iter()
        .map(|(_, without)| base_query(without, v).query())
        .collect();
    let totals = recovery::totals_on(lease, &queries, cancel, RECOVERY_BUDGET)?;
    let top = recovery::top_level(ast);
    let clauses = removals
        .into_iter()
        .zip(totals)
        .map(|((index, without), total)| ClauseRemoval {
            index,
            clause: top[index].to_query(),
            canonical: without.to_query(),
            without,
            total: total.map(|t| t.value),
            total_approx: total.is_some_and(|t| t.approx),
        })
        .collect();
    Ok(Some(SearchRecovery {
        clauses,
        skipped: top.len().saturating_sub(MAX_CLAUSES),
    }))
}

/// Below this many searchable torrents the search page shows its
/// full-page cold start (§10.19, `searching_crawling`).
pub const COLD_START_BELOW: u64 = 1_000;
/// Below this many, the "still growing" banner (decision S-search-2a).
pub const GROWING_BELOW: u64 = 1_000_000;
/// Whole UTC hours `growth_per_hour` looks back over.
const GROWTH_HOURS: i64 = 24;

/// The index's size, growth and rebuild: what the search page's "Cold
/// start" and "Index rebuilding" states show (§10.4). The thresholds are
/// the server's, so the UI only reads the flags.
#[utoipa::path(
    operation_id = "searchIndexState",
    get,
    path = "/search/state",
    tag = "search",
    responses((status = 200, body = SearchIndexState))
)]
pub(crate) async fn index_state(
    State(s): State<ApiState>,
) -> ApiResult<axum::Json<SearchIndexState>> {
    let Some(search) = s.index.clone() else {
        return Err(search_disabled());
    };
    let reader = s.reader.clone();
    tokio::task::spawn_blocking(move || {
        let now = unix_now();
        let index = search.index();
        let searchable = index.num_docs();
        let (growth_per_hour, growth_hours) = {
            let store = reader.lock().expect("reader lock");
            growth(&store, now).map_err(internal)?
        };
        Ok(axum::Json(state_of(
            searchable,
            (growth_per_hour, growth_hours),
            search.progress(),
            index.missing_fields(),
            now,
        )))
    })
    .await
    .map_err(internal)?
}

/// The response, from what was read.
fn state_of(
    searchable: u64,
    (growth_per_hour, growth_hours): (Option<f64>, u32),
    progress: Option<RebuildProgress>,
    missing: &[String],
    now: i64,
) -> SearchIndexState {
    let rebuild = progress.map(|p| {
        use lodestar_search::RebuildPhase as P;
        IndexRebuild {
            done: p.done,
            error: p.error,
            from: p.from.to_string(),
            phase: match p.phase {
                P::Building => RebuildPhase::Building,
                P::Finishing => RebuildPhase::Finishing,
                P::Swapping => RebuildPhase::Swapping,
                P::Failed => RebuildPhase::Failed,
            },
            started_at: p.started_at,
            to: p.to.to_string(),
            total: p.total,
            unavailable: p.unavailable,
        }
    });
    SearchIndexState {
        cold_start: searchable < COLD_START_BELOW,
        growing: searchable < GROWING_BELOW,
        growth_hours,
        growth_per_hour,
        rebuild,
        searchable_torrents: searchable,
        unavailable: missing.to_vec(),
        updated_at: now,
    }
}

/// Torrents made searchable per hour over the last [`GROWTH_HOURS`]
/// whole UTC hours, counting only the hours the index was measured in
/// (a `COVERAGE` row on its index dim), and how many those were. Nothing
/// measured is `None`, not 0.
fn growth(store: &Store, now: i64) -> lodestar_store::Result<(Option<f64>, u32)> {
    let to = now.div_euclid(3600);
    let from = to - GROWTH_HOURS;
    store.read_snapshot(|st| {
        let covered: std::collections::BTreeSet<i64> = st
            .rollup_sums(metric::COVERAGE, from, to, RollupBy::HourDim)?
            .into_iter()
            .filter(|c| c.dim == metric::COVERAGE_INDEX && c.value > 0)
            .map(|c| c.hour)
            .collect();
        let newly: i64 = st
            .rollup_sums(metric::NEWLY_SEARCHABLE, from, to, RollupBy::Hour)?
            .iter()
            .filter(|c| covered.contains(&c.hour))
            .map(|c| c.value)
            .sum();
        let hours = covered.len() as u32;
        Ok(((hours > 0).then(|| newly as f64 / f64::from(hours)), hours))
    })
}

/// A request's `tz`, minutes east of UTC (0 when absent). `/facets`
/// takes it too, so `time:` counts the hours `/search` returns.
pub(crate) fn tz_param(tz: Option<i32>) -> ApiResult<i32> {
    let tz = tz.unwrap_or(0);
    if !TZ_RANGE.contains(&tz) {
        return Err(ApiError::bad_request(
            "bad_tz",
            "tz is minutes east of UTC, from -840 to 840.",
        )
        .field("tz"));
    }
    Ok(tz)
}

/// What a search may see besides its query, for the result cache's key:
/// requests that may see different torrents never share an entry.
pub(crate) fn scope_of(v: Visibility) -> String {
    format!("hidden={},adult={}", v.hidden, v.adult)
}

/// The `explain` block for the hits served, in their order.
fn explanation(e: &Explanation, served: &[SearchHit]) -> SearchExplain {
    let by_id: std::collections::HashMap<u64, &lodestar_search::Explained> =
        e.hits.iter().map(|h| (h.id, h)).collect();
    let hits = served
        .iter()
        .filter_map(|h| by_id.get(&(h.id as u64)))
        .map(|x| ExplainHit {
            id: x.id as i64,
            phase1: x.parts.map(|p| f64::from(p.phase1)),
            phase2: x.parts.map(|p| ExplainPhase2 {
                exact_name: None,
                file_cap: None,
                name: f64::from(p.name),
                phrase: None,
                title: f64::from(p.title),
            }),
            priors: ExplainPriors {
                freshness: x.priors.map(|p| f64::from(p.freshness)),
                junk: x.priors.map(|p| f64::from(p.junk)),
                seeders: x.priors.map(|p| f64::from(p.seeders)),
                swarm_consistency: x.priors.map(|p| f64::from(p.swarm_consistency)),
                trackers: x.priors.map(|p| f64::from(p.trackers)),
            },
            score: f64::from(x.score),
        })
        .collect();
    let ranked_by = match e.sort {
        Sort::Relevance => "relevance",
        Sort::Discovered { .. } => "discovered",
        Sort::Size { .. } => "size",
        Sort::Files { .. } => "files",
        Sort::Seeders { .. } => "seeders",
        Sort::Leechers { .. } => "leechers",
        Sort::Trackers { .. } => "trackers",
        Sort::Seen { .. } => "seen",
    };
    SearchExplain {
        hits,
        ranked_by: ranked_by.into(),
        two_phase: e.two_phase,
        weights: ExplainWeights {
            name: f64::from(WEIGHTS.name),
            path: f64::from(WEIGHTS.path),
            rerank_window: WEIGHTS.rerank_window,
            stem_score: f64::from(WEIGHTS.stem_score),
            title: f64::from(WEIGHTS.title),
        },
    }
}

/// A request's query: from `q`, or an `ast` the parser could have
/// produced. `/facets` reads its query with this and runs it through
/// [`base_query`] too, so it counts exactly what `/search` returns.
pub(crate) fn request_ast(ast: Option<Node>, q: Option<String>) -> ApiResult<Node> {
    let ast = match (ast, q) {
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
    Ok(ast)
}

pub(crate) fn search_error(e: SearchError) -> ApiError {
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

/// The search a cursor continues: the query as run, what it may show
/// (hidden torrents, Adult) and the sort. A cursor from before "Show
/// adult content" changed is refused rather than reading its old
/// snapshot.
pub(crate) fn fingerprint(shown: &Node, v: Visibility, sort: Sort) -> [u8; 8] {
    let mut h = Sha256::new();
    h.update(b"lodestar-search-v2|");
    h.update(format!("{shown:?}|{v:?}|{sort:?}").as_bytes());
    let d = h.finalize();
    let mut out = [0; 8];
    out.copy_from_slice(&d[..8]);
    out
}

/// Who holds a lease: the session (leases are capped per client, and
/// another session's cursor reads as expired).
pub(crate) fn client_of(headers: &HeaderMap) -> u64 {
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
pub(crate) fn open_cursor(text: &str, fingerprint: &[u8; 8]) -> ApiResult<Cursor> {
    if text.len() > MAX_CURSOR || !text.len().is_multiple_of(2) {
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
            "That cursor belongs to another search: send the same q, sort and hidden with it. \
             If Show adult content changed since, start the search again.",
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

/// Whether a stored classification is Adult (its effective class, your
/// correction included).
pub(crate) fn is_adult(e: &Enriched) -> bool {
    e.content_type == lodestar_classify::ContentType::Adult.code()
}

/// Fills a hit's classification fields (`docs/metrics.md` §7.1) from the
/// stored enrichment. Without one, or with a record this build can't
/// read, they stay null or empty.
pub(crate) fn enrich(hit: &mut SearchHit, e: Option<Enriched>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(key: TagKey, v: &str) -> Node {
        Node::Tag {
            key,
            any: vec![v.into()],
        }
    }

    fn word(w: &str) -> Node {
        Node::Word {
            field: lodestar_search::query::TextField::Any,
            word: w.into(),
        }
    }

    fn not(n: Node) -> Node {
        Node::Not { of: Box::new(n) }
    }

    /// What B6's `type` facet does to a query: drop its own top-level
    /// conjuncts, negations included.
    fn without_type(node: &Node) -> Node {
        let owns = |n: &Node| match n {
            Node::Tag { key, .. } => *key == TagKey::Type,
            Node::Not { of } => matches!(
                **of,
                Node::Tag {
                    key: TagKey::Type,
                    ..
                }
            ),
            _ => false,
        };
        match node {
            Node::And { of } => Node::And {
                of: of.iter().filter(|n| !owns(n)).cloned().collect(),
            },
            n if owns(n) => Node::All,
            n => n.clone(),
        }
    }

    const HIDDEN: Visibility = Visibility {
        hidden: true,
        adult: false,
    };

    #[test]
    fn a_facet_pass_cannot_strip_adult_visibility() {
        let no_adult = not(tag(TagKey::Type, ADULT));
        for ast in [
            Node::All,
            tag(TagKey::Type, "movie"),
            not(tag(TagKey::Type, ADULT)),
            Node::And {
                of: vec![word("dark"), tag(TagKey::Type, ADULT)],
            },
        ] {
            let base = base_query(&ast, HIDDEN);
            assert_eq!(base.visibility, vec![no_adult.clone()], "{ast:?}");
            let pass = base.restrict(without_type(&base.node));
            let kept = match &pass {
                Node::And { of } => of.contains(&no_adult),
                n => *n == no_adult,
            };
            assert!(kept, "{ast:?} ran as {pass:?}");
        }
    }

    #[test]
    fn the_query_is_the_users_with_visibility_anded_on() {
        let ast = word("dark");
        let base = base_query(&ast, HIDDEN);
        assert_eq!(base.node, ast);
        assert_eq!(
            base.query(),
            Node::And {
                of: vec![ast.clone(), not(tag(TagKey::Type, ADULT))],
            }
        );
        let shown = base_query(
            &ast,
            Visibility {
                hidden: true,
                adult: true,
            },
        );
        assert!(shown.visibility.is_empty());
        assert_eq!(shown.query(), ast);
        assert_eq!(
            base_query(&Node::All, HIDDEN).query(),
            not(tag(TagKey::Type, ADULT))
        );
    }

    /// `/search` and `/facets` take their leases for `query()`; facets
    /// count `with_filters(node, visibility)` and require the two to be
    /// the same node.
    #[test]
    fn the_query_is_what_facets_hold_a_lease_to() {
        for v in [false, true].map(|adult| Visibility {
            hidden: false,
            adult,
        }) {
            for ast in [
                Node::All,
                word("dark"),
                tag(TagKey::Type, "movie"),
                Node::And {
                    of: vec![word("dark"), not(tag(TagKey::Type, ADULT))],
                },
                Node::Or {
                    of: vec![word("dark"), word("light")],
                },
            ] {
                let base = base_query(&ast, v);
                assert_eq!(
                    base.query(),
                    lodestar_search::with_filters(base.node.clone(), &base.visibility),
                    "{ast:?} {v:?}"
                );
            }
        }
    }

    #[test]
    fn index_state_flags_and_rebuild() {
        use lodestar_search::{IndexVersion, RebuildPhase as P};
        let at = |n| state_of(n, (None, 0), None, &[], 7);
        assert!(at(999).cold_start && at(999).growing);
        assert!(!at(1_000).cold_start && at(1_000).growing);
        assert!(!at(1_000_000).growing);
        assert_eq!(at(5).rebuild, None);
        let from = IndexVersion {
            schema: 3,
            tokenizer: 1,
        };
        let progress = RebuildProgress {
            phase: P::Building,
            done: 40,
            total: 100,
            replayed: 2,
            errors: 0,
            started_at: 1_792_681_000,
            from,
            to: IndexVersion::CURRENT,
            unavailable: vec!["year".into()],
            error: None,
        };
        let missing = ["year".to_string()];
        let got = state_of(84_210, (Some(2_104.5), 24), Some(progress), &missing, 9);
        assert_eq!(
            got.rebuild,
            Some(IndexRebuild {
                done: 40,
                error: None,
                from: from.to_string(),
                phase: RebuildPhase::Building,
                started_at: 1_792_681_000,
                to: IndexVersion::CURRENT.to_string(),
                total: 100,
                unavailable: vec!["year".into()],
            })
        );
        assert_eq!(got.unavailable, missing);
        assert_eq!((got.growth_per_hour, got.growth_hours), (Some(2_104.5), 24));
        assert!(!got.cold_start && got.growing);
    }
}
