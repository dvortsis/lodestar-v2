//! `/facets` (§8, §10.4): the facet rail's counts for a search, a
//! request of its own that the UI fires beside `/search`.
//!
//! It takes `/search`'s query parameters and builds the same base query
//! with [`search::base_query`], so it counts exactly the torrents the
//! search returns: Adult visibility (§20.9) included, which it passes to
//! the counting apart from the user's query. With `lease` (a search's `next_cursor`) it reads that
//! search's index snapshot; otherwise it takes one as `/search` does.
//! The work runs on the blocking pool under the request's token, which a
//! client disconnect cancels, and [`BUDGET`].

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use lodestar_search::facets::{MAX_LIMIT, SWARM_BUCKETS};
use lodestar_search::{CancelToken, Error as SearchError, Facet, FacetParams, FacetSpec, Facets};
use utoipa::OpenApi;

use crate::dto::facets::{
    FacetValue, FacetsRequest, FacetsResponse, Histogram, HistogramBin, SwarmBuckets, TermFacet,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json, Query, same_origin_json};
use crate::search::{
    Visibility, base_query, client_of, fingerprint, open_cursor, request_ast, scope_of,
    search_error, sort_param, tz_param, unix_now,
};
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(facets_get, facets_post))]
pub(crate) struct ApiDoc;

/// How long facets may take before the request gives up: well past the
/// ~100 ms they take (§3), so only a runaway query meets it.
pub const BUDGET: Duration = Duration::from_secs(5);
/// The store setting that records the crawler's first run.
const FIRST_RUN: &str = "first_run_at";

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/facets", get(facets_get).post(facets_post)))
}

/// Facets for query text. To send an AST, POST it.
#[utoipa::path(
    operation_id = "facetsGet",
    get,
    path = "/facets",
    tag = "search",
    params(
        ("q" = Option<String>, Query, description = "The query text, as `/search` takes it."),
        ("sort" = Option<String>, Query, description = "The search's sort; checked against `lease`."),
        ("hidden" = Option<bool>, Query, description = "Include torrents a filter rule hid (§20.3)."),
        ("tz" = Option<i32>, Query, description = "The user's zone for `time:`, in minutes east of UTC (−840 to 840), as `/search`."),
        ("lease" = Option<String>, Query, description = "A search's `next_cursor`: count its index snapshot. Send the same `q`, `sort` and `hidden`."),
        ("limit.{facet}" = Option<usize>, Query, description = "Values listed for one facet (`limit.ext=20`): 1–100, 8 by default."),
    ),
    responses((status = 200, body = FacetsResponse))
)]
pub(crate) async fn facets_get(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Query(p): Query<BTreeMap<String, String>>,
) -> ApiResult<axum::Json<FacetsResponse>> {
    run_facets(s, &headers, from_params(p)?).await
}

/// Facets for query text or a query AST.
#[utoipa::path(
    operation_id = "facetsPost",
    post,
    path = "/facets",
    tag = "search",
    request_body = FacetsRequest,
    responses((status = 200, body = FacetsResponse))
)]
pub(crate) async fn facets_post(
    State(s): State<ApiState>,
    headers: HeaderMap,
    Json(p): Json<FacetsRequest>,
) -> ApiResult<axum::Json<FacetsResponse>> {
    same_origin_json(&headers)?;
    run_facets(s, &headers, p).await
}

fn bad_parameter(field: String) -> ApiError {
    ApiError::bad_request(
        "bad_parameter",
        "A query parameter has the wrong type or value.",
    )
    .field(field)
}

/// `GET`'s parameters as a request; other parameters are ignored, as
/// `/search` ignores them.
fn from_params(p: BTreeMap<String, String>) -> ApiResult<FacetsRequest> {
    let mut r = FacetsRequest::default();
    for (k, v) in p {
        match k.as_str() {
            "ast" => {
                return Err(
                    ApiError::bad_request("ast_needs_post", "Send an AST with POST.").field("ast"),
                );
            }
            "q" => r.q = Some(v),
            "sort" => r.sort = Some(v),
            "lease" => r.lease = Some(v),
            "tz" => r.tz = Some(v.parse().map_err(|_| bad_parameter(k))?),
            "hidden" => {
                r.hidden = Some(match v.as_str() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(bad_parameter(k)),
                })
            }
            _ => {
                if let Some(name) = k.strip_prefix("limit.") {
                    let n = v.parse().map_err(|_| bad_parameter(k.clone()))?;
                    r.limits.insert(name.to_owned(), n);
                }
            }
        }
    }
    Ok(r)
}

/// The per-facet limits, checked.
fn limits(given: &BTreeMap<String, usize>) -> ApiResult<BTreeMap<Facet, usize>> {
    given
        .iter()
        .map(|(name, n)| {
            let field = format!("limit.{name}");
            let Some(f) = Facet::from_name(name) else {
                return Err(ApiError::bad_request(
                    "unknown_facet",
                    format!(
                        "There's no facet {name:?}: use one of {}.",
                        Facet::ALL.map(Facet::name).join(", ")
                    ),
                )
                .field(field));
            };
            if !(1..=MAX_LIMIT).contains(n) {
                return Err(ApiError::bad_request(
                    "bad_limit",
                    format!("A facet lists 1 to {MAX_LIMIT} values."),
                )
                .field(field));
            }
            Ok((f, *n))
        })
        .collect()
}

async fn run_facets(
    s: ApiState,
    headers: &HeaderMap,
    p: FacetsRequest,
) -> ApiResult<axum::Json<FacetsResponse>> {
    let Some(search) = s.index.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_disabled",
            "Search is turned off.",
        ));
    };
    let ast = request_ast(p.ast, p.q)?;
    let sort = sort_param(p.sort.as_deref())?;
    let limits = limits(&p.limits)?;
    let tz = tz_param(p.tz)?;
    let visibility = Visibility {
        hidden: p.hidden.unwrap_or(false),
        adult: s.adult.shown(),
    };
    let base = base_query(&ast, visibility);
    let shown = base.query();
    let cursor = p
        .lease
        .as_deref()
        .map(|c| open_cursor(c, &fingerprint(&shown, visibility, sort)))
        .transpose()
        .map_err(|e| match e.code() {
            // The parameter is `lease` here.
            "bad_cursor" => ApiError::bad_request(
                "bad_cursor",
                "That lease isn't a cursor this server gave out for this search.",
            )
            .field("lease"),
            _ => e,
        })?;
    let client = client_of(headers);
    let reader = s.reader.clone();
    // Axum drops this future when the client disconnects; the guard then
    // cancels the work on the blocking pool.
    let cancel = CancelToken::new();
    let _guard = cancel.cancel_on_drop();
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        // The budget covers taking the lease (its count) too.
        let budget = cancel.with_budget(BUDGET);
        let timeout = |e: SearchError| match e {
            SearchError::OutOfTime => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "facets_timeout",
                "Counting the facets took too long; narrow the search.",
            ),
            e => search_error(e),
        };
        let lease = match cursor {
            Some(c) => Some(
                search
                    .leases()
                    .get(&c.lease, client)
                    .map_err(|e| search_error(e).field("lease"))?,
            ),
            None => None,
        };
        let first_run = reader
            .lock()
            .expect("reader lock")
            .setting_i64(FIRST_RUN)
            .map_err(internal)?;
        let params = FacetParams::new(limits, first_run, !visibility.adult);
        // Disjunction edits the user's part only; the visibility clauses
        // hold in every pass. From the result cache (§8) when the
        // snapshot has them.
        let spec = FacetSpec {
            query: &shown,
            node: &base.node,
            filters: &base.visibility,
            params: &params,
            scope: &scope_of(visibility),
            sort,
            now: unix_now(),
            utc_offset: tz * 60,
        };
        let (cache, serving) = (search.cache(), search.index());
        let (got, _) = match &lease {
            Some(lease) => cache.facets_on(&serving, lease, &spec, &budget),
            None => cache.facets(&serving, &spec, &budget),
        }
        .map_err(timeout)?;
        let total = got.total;
        Ok(axum::Json(response(
            &got.facets,
            ast.to_query(),
            first_run,
            total.value,
            total.approx,
            started.elapsed().as_secs_f64() * 1000.0,
        )))
    })
    .await
    .map_err(internal)?
}

fn response(
    got: &Facets,
    canonical: String,
    first_run: Option<i64>,
    total: usize,
    total_approx: bool,
    took_ms: f64,
) -> FacetsResponse {
    let term = |t: &lodestar_search::TermCounts| TermFacet {
        approx: t.approx,
        distinct: t.distinct,
        other: t.other,
        values: t
            .values
            .iter()
            .map(|v| FacetValue {
                count: v.count,
                excluded: v.excluded,
                selected: v.selected,
                value: v.value.clone(),
            })
            .collect(),
    };
    // Null, never zeros, until the swarm fields are indexed (M4).
    const { assert!(!SWARM_BUCKETS) };
    let buckets = SwarmBuckets {
        seeders: None,
        trackers: None,
    };
    FacetsResponse {
        backfill_before: first_run,
        buckets,
        canonical,
        facets: got
            .terms
            .iter()
            .map(|(f, t)| (f.name().to_owned(), term(t)))
            .collect(),
        histograms: got
            .histograms
            .iter()
            .map(|(h, g)| {
                (
                    h.name().to_owned(),
                    Histogram {
                        approx: g.approx,
                        bins: g
                            .bins
                            .iter()
                            .map(|b| HistogramBin {
                                backfill: b.backfill,
                                count: b.count,
                                hi: b.hi,
                                lo: b.lo,
                            })
                            .collect(),
                        missing: g.missing,
                        unit: g.unit.as_str().to_owned(),
                    },
                )
            })
            .collect(),
        sample_rate: got.sample_rate,
        sampled: got.sampled(),
        took_ms,
        total,
        total_approx,
        unavailable: got.unavailable.clone(),
    }
}
