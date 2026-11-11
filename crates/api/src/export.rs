//! `/export` (§7 "Read path", §9, §10.4): a search, a selection of
//! hashes or a collection as a CSV, NDJSON, magnet-list or
//! bitmagnet-import download.
//!
//! **What's never exported:** what search wouldn't show (a torrent a
//! filter rule hid, unless `hidden=true`; Adult while "Show adult
//! content" is off), and `private_source` torrents, ever (§18.7).
//! Quarantined and tombstoned items aren't torrents, so nothing finds
//! them. A query runs through search's own [`search::base_query`]; every
//! row read is checked again with [`discover::withheld`] (the index can
//! lag a reclassification) plus the private-source flag. A row left out
//! counts in `skipped` and nothing says which or why: an unknown hash
//! and a withheld one look the same.
//!
//! **The bitmagnet format** (OUT-0, `docs/decisions/2026-11-11-OUT-0.md`)
//! is for another program, which looks up every hash it imports on the
//! DHT, so it leaves out more: hidden torrents whatever `hidden` says
//! (it refuses `hidden=true`), and every torrent a §18.7 privacy gate
//! refuses ([`flags::PRIVACY`]: BEP 27 private, `private_source`, a
//! remote worker's unverified row). Its lines carry only the six fields
//! bitmagnet's import takes, never a setting, key or path.
//!
//! **Adult** follows export's own setting, [`ExportAdult`] (§20.9), not
//! "Show adult content"; without it, as until the server wires
//! `export_adult`, exports never include Adult. It's read when the
//! export starts, and again before each chunk: turning it off mid-export
//! withholds Adult from the next chunk on; turning it on adds none (the
//! query was run without it).
//!
//! **Streamed, chunked, throttled** (§15's WAL risk): ids come a chunk at
//! a time (a page of the export's own searcher lease, a slice of the
//! hashes, a page of the collection), then their rows are read under
//! the reader lock in one short read transaction, which ends before the
//! rows are formatted or sent. No transaction or lock is held across
//! chunks, a pause, the throttle's wait or a slow client, so WAL
//! checkpoints are never starved and other requests run between chunks.
//! The body goes out through a small queue: a client that stops reading
//! stops the export (backpressure). Nothing is written to disk (§2.17).
//!
//! **Every export is a job** (`/jobs`): progress, pause, resume and
//! cancel. A download that ends early (cancelled, failed, past a limit)
//! ends with a transport error, never a clean end, so it isn't taken for
//! a complete file; a capped one ends cleanly, says so in its job, and an
//! NDJSON one in its trailer.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Router};
use lodestar_metainfo::InfoHash;
use lodestar_search::{CancelToken, Lease, Start};
use lodestar_store::{Enriched, HotColumns, Store, TorrentRow, flags};
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use utoipa::OpenApi;

use crate::dto::error::ErrorEnvelope;
use crate::dto::export::{ExportField, ExportFormat, ExportRequest};
use crate::dto::jobs::{BackgroundJobKind, ExportJob};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json, Query, same_origin_json};
use crate::jobs::{Finisher, Job, Jobs, Stop};
use crate::search::{self, Visibility};
use crate::swarm::{self, Summary};
use crate::{AdultVisibility, ApiState, discover, torrents};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(export_get, export_post),
    tags((name = "export", description = "Search results, selections and collections as files (§9, §10.4)."))
)]
pub(crate) struct ApiDoc;

/// Whether exports include Adult: the `content` section's own
/// `export_adult` (§20.9: each output has its own adult setting, off by
/// default), followed live. The server layers it over the router as an
/// `Extension`; without one, as now, exports never include Adult.
#[derive(Clone, Debug, Default)]
pub struct ExportAdult(pub AdultVisibility);

/// Hashes one export takes, at most (the bulk selection).
pub const MAX_HASHES: usize = 10_000;

/// How an export reads and paces itself. The defaults are the
/// decisions file's numbers; tests make them small with an
/// `Extension<ExportConfig>` over the router.
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Ids per chunk: one read transaction under the reader lock each.
    pub chunk: usize,
    /// Rows read per second by every running export together, at most
    /// (0: unthrottled); the jobs registry paces them as one.
    pub rows_per_sec: u32,
    /// Rows one export writes, at most.
    pub cap: u64,
    /// Chunks queued for the socket: past them, a client that isn't
    /// reading holds the export where it is.
    pub queue: usize,
    /// A paused export fails after this long (`pause_expired`).
    pub pause_limit: Duration,
    /// A client that reads nothing for this long fails the export
    /// (`export_stalled`).
    pub stall_limit: Duration,
    /// An export runs at most this long in all (`export_expired`): its
    /// searcher lease keeps a retired index open, as a cursor's does for
    /// at most an hour.
    pub max_life: Duration,
}

/// Rows per chunk.
pub const CHUNK: usize = 256;
/// Ids a query's lease is paged for at once: 128 KiB of ids.
pub const ID_PAGE: usize = 16_384;
/// Rows read per second, by all running exports together.
pub const ROWS_PER_SEC: u32 = 2_000;
/// Rows per export.
pub const CAP: u64 = 1_000_000;

impl Default for ExportConfig {
    fn default() -> Self {
        ExportConfig {
            chunk: CHUNK,
            rows_per_sec: ROWS_PER_SEC,
            cap: CAP,
            queue: 4,
            pause_limit: Duration::from_secs(600),
            stall_limit: Duration::from_secs(600),
            max_life: Duration::from_secs(3600),
        }
    }
}

pub fn routes() -> Router<ApiState> {
    Router::new().route("/export", get(export_get).post(export_post))
}

/// Downloads a query's results as a file. To export an AST or a
/// selection of hashes, POST.
#[utoipa::path(
    operation_id = "exportGet",
    get,
    path = "/export",
    tag = "export",
    params(
        ("q" = Option<String>, Query, description = "The query text, as `/search` takes it; empty (or no source at all) exports everything."),
        ("sort" = Option<String>, Query, description = "As `/search`'s."),
        ("hidden" = Option<bool>, Query, description = "Include torrents a filter rule hid (§20.3); refused with `bitmagnet`."),
        ("collection" = Option<i64>, Query, description = "Export a collection instead, in its order."),
        ("format" = Option<ExportFormat>, Query, description = "`csv` (the default), `json` (NDJSON), `magnets` or `bitmagnet` (bitmagnet's `/import` lines)."),
        ("fields" = Option<String>, Query, description = "A comma-separated subset of `ExportField`; every field but `magnet` by default."),
    ),
    responses(
        (
            status = 200,
            description = "The file, streamed. Its job's id is in `X-Export-Job`; poll `/jobs/{id}` for progress, `skipped` and `truncated`.",
            content(
                (String = "text/csv"),
                (String = "application/x-ndjson"),
                (String = "text/plain"),
            ),
            headers(
                ("Content-Disposition" = String, description = "`attachment` with a generated name, `lodestar-export-<UTC time>.<csv|ndjson|txt|bitmagnet.ndjson>`."),
                ("X-Export-Job" = u64, description = "The export's job id."),
                ("Cache-Control" = String, description = "`no-store`."),
            )
        ),
        (status = 400, body = ErrorEnvelope, description = "A bad parameter, as `/search`'s, or `one_source`, `unknown_field`, `fields_unused`, `sort_unused`, `hidden_unused` (`hidden=true` with `bitmagnet`); `ast` or `hashes` in the query string is a `bad_parameter` (POST them)."),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
        (status = 429, body = ErrorEnvelope, description = "`too_many_jobs`: four jobs are running or paused."),
        (status = 503, body = ErrorEnvelope, description = "`search_disabled` (a query while search is off), or `shutting_down`."),
    )
)]
pub(crate) async fn export_get(
    State(s): State<ApiState>,
    Extension(jobs): Extension<Arc<Jobs>>,
    config: Option<Extension<ExportConfig>>,
    adult: Option<Extension<ExportAdult>>,
    Query(p): Query<ExportRequest>,
) -> ApiResult<Response> {
    if p.ast.is_some() {
        return Err(ApiError::bad_request("ast_needs_post", "Send an AST with POST.").field("ast"));
    }
    if p.hashes.is_some() {
        return Err(
            ApiError::bad_request("hashes_need_post", "Send hashes with POST.").field("hashes"),
        );
    }
    let adult = adult.map(|a| a.0.0).unwrap_or_default();
    start(s, jobs, config.map(|c| c.0).unwrap_or_default(), adult, p).await
}

/// Downloads a query's results (text or AST), a selection of up to
/// 10,000 hashes, or a collection as a file.
#[utoipa::path(
    operation_id = "exportPost",
    post,
    path = "/export",
    tag = "export",
    request_body = ExportRequest,
    responses(
        (
            status = 200,
            description = "The file, streamed, as `GET /export`'s.",
            content(
                (String = "text/csv"),
                (String = "application/x-ndjson"),
                (String = "text/plain"),
            ),
            headers(
                ("Content-Disposition" = String, description = "`attachment` with a generated name."),
                ("X-Export-Job" = u64, description = "The export's job id."),
                ("Cache-Control" = String, description = "`no-store`."),
            )
        ),
        (status = 400, body = ErrorEnvelope, description = "As `GET /export`'s, or `too_many_hashes`, `bad_hash`."),
        (status = 403, body = ErrorEnvelope, description = "`cross_origin`."),
        (status = 404, body = ErrorEnvelope, description = "`collection_not_found`."),
        (status = 429, body = ErrorEnvelope, description = "`too_many_jobs`."),
        (status = 503, body = ErrorEnvelope, description = "`search_disabled`, or `shutting_down`."),
    )
)]
pub(crate) async fn export_post(
    State(s): State<ApiState>,
    Extension(jobs): Extension<Arc<Jobs>>,
    config: Option<Extension<ExportConfig>>,
    adult: Option<Extension<ExportAdult>>,
    headers: HeaderMap,
    Json(p): Json<ExportRequest>,
) -> ApiResult<Response> {
    same_origin_json(&headers)?;
    let adult = adult.map(|a| a.0.0).unwrap_or_default();
    start(s, jobs, config.map(|c| c.0).unwrap_or_default(), adult, p).await
}

/// Where an export's ids come from, and how far it has read.
enum Source {
    /// The export's own lease: not in the registry, so its expiry and
    /// eviction can't end an export (`max_life` bounds it instead).
    /// Ids are taken from the lease [`ID_PAGE`] at a time (a page after
    /// a position scans every match, so few large pages cost far less
    /// than many small ones) and read a chunk at a time from `ids`.
    Query {
        lease: Arc<Lease>,
        next: Option<Start>,
        ids: std::collections::VecDeque<i64>,
    },
    Hashes {
        hashes: Vec<InfoHash>,
        at: usize,
    },
    Collection {
        id: i64,
        after: Option<i64>,
        done: bool,
    },
}

impl Source {
    fn exhausted(&self) -> bool {
        match self {
            Source::Query { next, ids, .. } => next.is_none() && ids.is_empty(),
            Source::Hashes { hashes, at } => *at >= hashes.len(),
            Source::Collection { done, .. } => *done,
        }
    }
}

/// What a request asks to be written.
#[derive(Debug, Clone)]
struct Shape {
    format: ExportFormat,
    fields: Vec<ExportField>,
}

fn shape(p: &ExportRequest) -> ApiResult<Shape> {
    let format = p.format.unwrap_or(ExportFormat::Csv);
    let fields = match (&p.fields, format) {
        (Some(_), ExportFormat::Magnets | ExportFormat::Bitmagnet) => {
            return Err(ApiError::bad_request(
                "fields_unused",
                "A magnet list or a bitmagnet import has fixed fields: leave fields out.",
            )
            .field("fields"));
        }
        (None, ExportFormat::Magnets) => vec![ExportField::Magnet],
        // Its own fixed set (`bitmagnet_line`).
        (None, ExportFormat::Bitmagnet) => Vec::new(),
        (None, _) => ExportField::ALL
            .into_iter()
            .filter(|f| *f != ExportField::Magnet)
            .collect(),
        (Some(text), _) => {
            let mut fields = Vec::new();
            for name in text.split(',').map(str::trim) {
                let f = ExportField::ALL
                    .into_iter()
                    .find(|f| f.as_str() == name)
                    .ok_or_else(|| {
                        let shown: String = name.chars().take(64).collect();
                        ApiError::bad_request(
                            "unknown_field",
                            format!("Unknown field {shown:?}: see ExportField for the names."),
                        )
                        .field("fields")
                    })?;
                fields.push(f);
            }
            fields.sort();
            fields.dedup();
            fields
        }
    };
    Ok(Shape { format, fields })
}

async fn start(
    s: ApiState,
    jobs: Arc<Jobs>,
    config: ExportConfig,
    export_adult: AdultVisibility,
    p: ExportRequest,
) -> ApiResult<Response> {
    let shape = shape(&p)?;
    let query = p.q.is_some() || p.ast.is_some();
    let sources =
        usize::from(query) + usize::from(p.hashes.is_some()) + usize::from(p.collection.is_some());
    if sources > 1 {
        return Err(ApiError::bad_request(
            "one_source",
            "Export one thing: a query (q or ast), hashes or a collection.",
        ));
    }
    if p.sort.is_some() && (p.hashes.is_some() || p.collection.is_some()) {
        return Err(ApiError::bad_request(
            "sort_unused",
            "Selections and collections keep their own order: leave sort out.",
        )
        .field("sort"));
    }
    let bitmagnet = shape.format == ExportFormat::Bitmagnet;
    if bitmagnet && p.hidden == Some(true) {
        return Err(ApiError::bad_request(
            "hidden_unused",
            "A bitmagnet export never includes hidden torrents: leave hidden out.",
        )
        .field("hidden"));
    }
    let hidden = p.hidden.unwrap_or(false);
    let adult = export_adult.shown();
    // The slot first, so a request past the limit costs no search.
    let job = jobs.start(BackgroundJobKind::Export)?;
    // Until the worker owns it, a failed setup or a request dropped
    // mid-setup (the client went away) discards the job, freeing its
    // slot.
    let setup = Setup {
        jobs: jobs.clone(),
        id: Some(job.id()),
    };
    let (source, total, approx) = resolve(&s, p, hidden, adult, &config).await?;
    job.set_total(total.min(config.cap), approx);
    job.set_export(ExportJob {
        cap: config.cap,
        format: shape.format,
        truncated: false,
    });
    let (tx, rx) = mpsc::channel(config.queue.max(1));
    let complete = Arc::new(AtomicBool::new(false));
    let id = job.id();
    let worker = Worker {
        job,
        jobs: jobs.clone(),
        reader: s.reader.clone(),
        hot: s.hot.clone(),
        adult: export_adult,
        started_adult: adult,
        hidden,
        shape: shape.clone(),
        config,
        tx,
        complete: complete.clone(),
    };
    tokio::spawn(worker.run(source));
    setup.disarm();
    respond(shape.format, id, rx, complete)
}

/// Discards a job whose setup didn't reach its worker.
struct Setup {
    jobs: Arc<Jobs>,
    id: Option<u64>,
}

impl Setup {
    /// The worker owns the job now (its `Finisher` ends it).
    fn disarm(mut self) {
        self.id = None;
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.jobs.discard(id);
        }
    }
}

/// The export's source, its row count and whether that's an estimate.
async fn resolve(
    s: &ApiState,
    p: ExportRequest,
    hidden: bool,
    adult: bool,
    config: &ExportConfig,
) -> ApiResult<(Source, u64, bool)> {
    let reader = s.reader.clone();
    Ok(if let Some(list) = p.hashes {
        if list.len() > MAX_HASHES {
            return Err(ApiError::bad_request(
                "too_many_hashes",
                format!("Export at most {MAX_HASHES} hashes at a time."),
            )
            .field("hashes"));
        }
        let mut seen = std::collections::HashSet::with_capacity(list.len());
        let mut hashes = Vec::with_capacity(list.len());
        for h in &list {
            let h = torrents::parse_hash(h).map_err(|e| e.field("hashes"))?;
            if seen.insert(h) {
                hashes.push(h);
            }
        }
        let n = hashes.len() as u64;
        (Source::Hashes { hashes, at: 0 }, n, false)
    } else if let Some(id) = p.collection {
        let chunk = config.chunk;
        let n = tokio::task::spawn_blocking(move || count_collection(&reader, id, chunk))
            .await
            .map_err(internal)??;
        (
            Source::Collection {
                id,
                after: None,
                done: false,
            },
            n,
            false,
        )
    } else {
        let Some(index) = s.index.clone() else {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "search_disabled",
                "Search is turned off.",
            ));
        };
        let ast = search::request_ast(p.ast, p.q)?;
        let sort = search::sort_param(p.sort.as_deref())?;
        let shown = search::base_query(&ast, Visibility { hidden, adult }).query();
        let cancel = CancelToken::new();
        let _guard = cancel.cancel_on_drop();
        let lease = tokio::task::spawn_blocking(move || {
            Lease::new(index.index(), &shown, crate::unix_now(), sort, &cancel)
        })
        .await
        .map_err(internal)?
        .map_err(search::search_error)?;
        let total = lease.total();
        (
            Source::Query {
                lease: Arc::new(lease),
                next: Some(Start::Offset(0)),
                ids: std::collections::VecDeque::new(),
            },
            total.value as u64,
            total.approx,
        )
    })
}

/// The streamed response for job `id`.
fn respond(
    format: ExportFormat,
    id: u64,
    rx: mpsc::Receiver<Result<Bytes, io::Error>>,
    complete: Arc<AtomicBool>,
) -> ApiResult<Response> {
    let (content_type, ext) = match format {
        ExportFormat::Csv => ("text/csv; charset=utf-8; header=present", "csv"),
        ExportFormat::Json => ("application/x-ndjson", "ndjson"),
        ExportFormat::Magnets => ("text/plain; charset=utf-8", "txt"),
        ExportFormat::Bitmagnet => ("application/x-ndjson", "bitmagnet.ndjson"),
    };
    let disposition = format!(
        "attachment; filename=\"{}\"",
        file_name(crate::unix_now(), ext)
    );
    let mut resp = Response::new(Body::from_stream(Chunks {
        rx,
        complete,
        ended: false,
    }));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&disposition).map_err(internal)?,
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert("x-export-job", HeaderValue::from(id));
    Ok(resp)
}

/// Counts a collection's entries, a chunk per read (so a large one holds
/// the reader lock no longer than a chunk does); 404 when there's no such
/// collection.
fn count_collection(reader: &Mutex<Store>, id: i64, chunk: usize) -> ApiResult<u64> {
    {
        let store = reader.lock().expect("reader lock");
        if store.collection(id).map_err(internal)?.is_none() {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "collection_not_found",
                "There's no collection with that id.",
            )
            .field("collection"));
        }
    }
    let (mut n, mut after) = (0u64, None);
    let step = chunk.max(1) * 16;
    loop {
        let items = {
            let store = reader.lock().expect("reader lock");
            store.collection_items(id, after, step).map_err(internal)?
        };
        n += items.len() as u64;
        match items.last() {
            Some(last) if items.len() == step => after = Some(last.position),
            _ => return Ok(n),
        }
    }
}

/// `lodestar-export-YYYYMMDD-HHMMSS.<ext>` in UTC: nothing from the
/// torrents, so no name can reach a header or a path.
fn file_name(now: i64, ext: &str) -> String {
    let (y, m, d, secs) = civil(now);
    format!(
        "lodestar-export-{y:04}{m:02}{d:02}-{:02}{:02}{:02}.{ext}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// `YYYY-MM-DDTHH:MM:SSZ`, as Go's `time.Time` reads it.
fn rfc3339(t: i64) -> String {
    let (y, m, d, secs) = civil(t);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// A Unix time as UTC year, month, day and seconds into the day.
fn civil(t: i64) -> (i64, i64, i64, i64) {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs)
}

/// Why an export stopped early.
enum End {
    Cancelled,
    /// The client went away.
    Gone,
    Failed(ApiError),
}

struct Worker {
    job: Arc<Job>,
    jobs: Arc<Jobs>,
    reader: Arc<Mutex<Store>>,
    /// Swarm health's last-seen column, as search reads it (M4).
    hot: Option<Arc<HotColumns>>,
    adult: AdultVisibility,
    started_adult: bool,
    hidden: bool,
    shape: Shape,
    config: ExportConfig,
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
    /// Set once the last bytes are queued: the body then ends cleanly.
    complete: Arc<AtomicBool>,
}

/// One chunk, read and formatted.
struct Chunk {
    bytes: Vec<u8>,
    rows: u64,
    skipped: u64,
}

impl Worker {
    async fn run(self, source: Source) {
        let _finisher = Finisher(self.job.clone());
        match self.export(source).await {
            Ok(()) => {
                self.complete.store(true, Ordering::Release);
                self.job.finish_done();
            }
            Err(End::Cancelled | End::Gone) => self.job.finish_cancelled(),
            Err(End::Failed(e)) => self.job.finish_failed(e),
        }
        // Dropping the sender ends the body: cleanly once complete, with
        // an error otherwise.
    }

    async fn export(&self, mut source: Source) -> Result<(), End> {
        let born = Instant::now();
        let (mut rows, mut skipped) = (0u64, 0u64);
        if self.shape.format == ExportFormat::Csv {
            self.send(csv_header(&self.shape.fields)).await?;
        }
        let cancel = CancelToken::new();
        let _guard = cancel.cancel_on_drop();
        while !source.exhausted() {
            if rows >= self.config.cap {
                self.job.mark_truncated();
                break;
            }
            // While paused, a client that goes away ends it too.
            let stopped = tokio::select! {
                biased;
                () = self.tx.closed() => return Err(End::Gone),
                stopped = self.job.checkpoint(self.config.pause_limit) => stopped,
            };
            stopped.map_err(|stop| match stop {
                Stop::Cancelled => End::Cancelled,
                Stop::PauseExpired => End::Failed(ApiError::new(
                    StatusCode::REQUEST_TIMEOUT,
                    "pause_expired",
                    "The export was paused for too long; start it again.",
                )),
            })?;
            if born.elapsed() >= self.config.max_life {
                return Err(End::Failed(ApiError::new(
                    StatusCode::REQUEST_TIMEOUT,
                    "export_expired",
                    "The export ran for too long; export less at a time.",
                )));
            }
            let want = (self.config.chunk.max(1) as u64).min(self.config.cap - rows) as usize;
            let rule = Rule {
                hidden: self.hidden,
                // Off now, or when it started: withheld.
                adult: self.started_adult && self.adult.shown(),
                privacy: self.shape.format == ExportFormat::Bitmagnet,
            };
            let reader = self.reader.clone();
            let hot = self.hot.clone();
            let shape = self.shape.clone();
            let cancel = cancel.clone();
            let (back, chunk) = tokio::task::spawn_blocking(move || {
                let chunk = read_chunk(
                    &mut source,
                    &reader,
                    hot.as_deref(),
                    want,
                    rule,
                    &shape,
                    &cancel,
                );
                (source, chunk)
            })
            .await
            .map_err(|e| End::Failed(internal(e)))?;
            source = back;
            let chunk = chunk.map_err(End::Failed)?;
            rows += chunk.rows;
            skipped += chunk.skipped;
            if !chunk.bytes.is_empty() {
                self.send(chunk.bytes).await?;
            }
            self.job.add(chunk.rows, chunk.skipped);
            // One budget for every running export (§1.6.4).
            self.jobs
                .pacer()
                .wait(chunk.rows + chunk.skipped, self.config.rows_per_sec)
                .await;
        }
        if self.shape.format == ExportFormat::Json {
            let trailer = serde_json::json!({
                "trailer": {
                    "cap": self.config.cap,
                    "rows": rows,
                    "skipped": skipped,
                    "truncated": rows >= self.config.cap && !source.exhausted(),
                }
            });
            let mut line = serde_json::to_vec(&trailer).map_err(|e| End::Failed(internal(e)))?;
            line.push(b'\n');
            self.send(line).await?;
        }
        Ok(())
    }

    /// Queues bytes for the client: waits while it isn't reading, for at
    /// most `stall_limit`, and stops if the job is cancelled meanwhile.
    async fn send(&self, bytes: Vec<u8>) -> Result<(), End> {
        let send = tokio::time::timeout(self.config.stall_limit, self.tx.send(Ok(bytes.into())));
        tokio::select! {
            biased;
            () = self.job.cancelled() => Err(End::Cancelled),
            sent = send => match sent {
                Err(_) => Err(End::Failed(ApiError::new(
                    StatusCode::REQUEST_TIMEOUT,
                    "export_stalled",
                    "The download stopped reading; start the export again.",
                ))),
                Ok(Err(_)) => Err(End::Gone),
                Ok(Ok(())) => Ok(()),
            },
        }
    }
}

/// What may be exported of a row, besides never `private_source`.
#[derive(Debug, Clone, Copy)]
struct Rule {
    hidden: bool,
    adult: bool,
    /// Leave out everything a §18.7 privacy gate refuses
    /// ([`flags::PRIVACY`]), not only `private_source`: for an output
    /// that hands hashes to a DHT crawler (bitmagnet).
    privacy: bool,
}

/// Whether a stored row is left out: what search wouldn't show, and any
/// private-source torrent (§18.7), or any private one at all under
/// `rule.privacy`.
fn excluded(row: &TorrentRow, e: Option<&Enriched>, rule: Rule) -> bool {
    let private = if rule.privacy {
        flags::PRIVACY
    } else {
        flags::PRIVATE_SOURCE
    };
    discover::withheld(row, e, rule.hidden, rule.adult) || row.flags & private != 0
}

/// Reads up to `want` ids' rows from `source` in one read transaction
/// under the reader lock, then formats them with the lock released.
fn read_chunk(
    source: &mut Source,
    reader: &Mutex<Store>,
    hot: Option<&HotColumns>,
    want: usize,
    rule: Rule,
    shape: &Shape,
    cancel: &CancelToken,
) -> ApiResult<Chunk> {
    // A query's ids come from its lease, outside the lock.
    let ids: Option<Vec<i64>> = match source {
        Source::Query { lease, next, ids } => {
            if ids.len() < want
                && let Some(start) = next.take()
            {
                let page = lease
                    .page(start, ID_PAGE, cancel)
                    .map_err(search::search_error)?;
                *next = page.next.map(Start::After);
                ids.extend(page.hits.iter().map(|h| h.id as i64));
            }
            let n = want.min(ids.len());
            Some(ids.drain(..n).collect())
        }
        _ => None,
    };
    let mut kept: Vec<(TorrentRow, Option<Enriched>, Summary)> = Vec::with_capacity(want);
    let mut skipped = 0u64;
    {
        let store = reader.lock().expect("reader lock");
        let tx = store
            .connection()
            .unchecked_transaction()
            .map_err(internal)?;
        let ids: Vec<Option<i64>> = match source {
            Source::Query { .. } => ids.unwrap_or_default().into_iter().map(Some).collect(),
            Source::Hashes { hashes, at } => {
                let end = (*at + want).min(hashes.len());
                let found = hashes[*at..end]
                    .iter()
                    .map(|h| store.find(h).map_err(internal))
                    .collect::<ApiResult<_>>()?;
                *at = end;
                found
            }
            Source::Collection { id, after, done } => {
                let items = store
                    .collection_items(*id, *after, want)
                    .map_err(internal)?;
                *done = items.len() < want;
                *after = items.last().map(|i| i.position).or(*after);
                items.into_iter().map(|i| Some(i.torrent_id)).collect()
            }
        };
        for id in ids {
            // An unknown hash, or a torrent deleted since: skipped.
            let Some(row) = id
                .map(|id| store.torrent(id))
                .transpose()
                .map_err(internal)?
                .flatten()
            else {
                skipped += 1;
                continue;
            };
            let e = store.enrichment(row.id).map_err(internal)?;
            if excluded(&row, e.as_ref(), rule) {
                skipped += 1;
            } else {
                let swarm = swarm::summary(&store, hot, &row).map_err(internal)?;
                kept.push((row, e, swarm));
            }
        }
        // Read only: ending it is all there is to do.
        drop(tx);
    }
    let mut bytes = Vec::with_capacity(kept.len() * 256);
    let rows = kept.len() as u64;
    for (row, e, swarm) in kept {
        write_row(&mut bytes, &row, e, swarm, shape)?;
    }
    Ok(Chunk {
        bytes,
        rows,
        skipped,
    })
}

/// The detail view's magnet, byte for byte.
fn magnet(row: &TorrentRow) -> String {
    format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        torrents::hex(&row.info_hash.0),
        torrents::percent_encode(&row.name)
    )
}

/// A row as the summary fields (`docs/metrics.md` §7.1), from the same
/// code search hits come from, so the null rules match.
fn record(row: &TorrentRow, e: Option<Enriched>, swarm: Summary) -> ApiResult<Map<String, Value>> {
    let mut hit = torrents::hit(row, 0.0, None, swarm);
    search::enrich(&mut hit, e);
    let Value::Object(mut m) = serde_json::to_value(&hit).map_err(internal)? else {
        return Err(internal("a hit isn't an object"));
    };
    m.insert("magnet".to_string(), Value::String(magnet(row)));
    Ok(m)
}

fn write_row(
    out: &mut Vec<u8>,
    row: &TorrentRow,
    e: Option<Enriched>,
    swarm: Summary,
    shape: &Shape,
) -> ApiResult<()> {
    match shape.format {
        ExportFormat::Bitmagnet => bitmagnet_line(out, row, e.as_ref())?,
        ExportFormat::Magnets => {
            out.extend_from_slice(magnet(row).as_bytes());
            out.push(b'\n');
        }
        ExportFormat::Json => {
            let mut all = record(row, e, swarm)?;
            let picked: Map<String, Value> = shape
                .fields
                .iter()
                .map(|f| {
                    let k = f.as_str();
                    (k.to_string(), all.remove(k).unwrap_or(Value::Null))
                })
                .collect();
            serde_json::to_writer(&mut *out, &picked).map_err(internal)?;
            out.push(b'\n');
        }
        ExportFormat::Csv => {
            let all = record(row, e, swarm)?;
            for (i, f) in shape.fields.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                let text = csv_text(*f, all.get(f.as_str()).unwrap_or(&Value::Null));
                out.extend_from_slice(csv_cell(&text).as_bytes());
            }
            out.extend_from_slice(b"\r\n");
        }
    }
    Ok(())
}

/// The source bitmagnet files every exported torrent under.
pub const BITMAGNET_SOURCE: &str = "lodestar";

/// One line of bitmagnet's import (`POST /import`: JSON lines its Go
/// `importer.Item` decodes, keys matched without case). Keys in a fixed
/// order; `null`s left out, as bitmagnet's own guide does.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BitmagnetItem<'a> {
    info_hash: String,
    name: std::borrow::Cow<'a, str>,
    size: u64,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<&'static str>,
}

fn bitmagnet_line(out: &mut Vec<u8>, row: &TorrentRow, e: Option<&Enriched>) -> ApiResult<()> {
    // A NUL survives JSON, but PostgreSQL's text refuses it, which would
    // fail bitmagnet's whole import batch: one hostile name mustn't.
    let name = if row.name.contains('\0') {
        std::borrow::Cow::Owned(row.name.replace('\0', "\u{fffd}"))
    } else {
        std::borrow::Cow::Borrowed(row.name.as_str())
    };
    let item = BitmagnetItem {
        info_hash: torrents::hex(&row.info_hash.0),
        name,
        size: row.size,
        source: BITMAGNET_SOURCE,
        published_at: (row.discovered_at > 0).then(|| rfc3339(row.discovered_at)),
        content_type: e
            .and_then(|e| lodestar_classify::ContentType::from_code(e.content_type))
            .and_then(bitmagnet_type),
    };
    let json = serde_json::to_string(&item).map_err(internal)?;
    // Lossless for any JSON reader: the characters [`escaped`] names, and
    // the line and paragraph separators, written `\uXXXX` (all are in
    // the BMP, and only a string can hold them), so no viewer of the
    // file or of bitmagnet's UI is shown them raw.
    for c in json.chars() {
        if escaped(c) || matches!(c, '\u{2028}' | '\u{2029}') {
            out.extend_from_slice(format!("\\u{:04x}", u32::from(c)).as_bytes());
        } else {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    out.push(b'\n');
    Ok(())
}

/// bitmagnet's `ContentType` for Lodestar's, where one means the same:
/// `None` leaves the hint out and bitmagnet classifies the torrent itself
/// (a hint it's given, it keeps).
fn bitmagnet_type(t: lodestar_classify::ContentType) -> Option<&'static str> {
    use lodestar_classify::ContentType as C;
    Some(match t {
        C::Movie => "movie",
        C::Tv => "tv_show",
        C::Music => "music",
        C::Audiobook => "audiobook",
        C::Ebook => "ebook",
        C::Comic => "comic",
        C::Software => "software",
        C::Game => "game",
        C::Adult => "xxx",
        C::Images | C::Video | C::Other => return None,
    })
}

fn csv_header(fields: &[ExportField]) -> Vec<u8> {
    let mut out = fields
        .iter()
        .map(|f| f.as_str())
        .collect::<Vec<_>>()
        .join(",")
        .into_bytes();
    out.extend_from_slice(b"\r\n");
    out
}

/// A value as CSV text: null is empty; lists are joined with `;`;
/// `composition` is `group:pct_bytes` pairs.
fn csv_text(field: ExportField, v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(items) if field == ExportField::Composition => items
            .iter()
            .map(|c| format!("{}:{}", c["group"].as_str().unwrap_or(""), c["pct_bytes"]))
            .collect::<Vec<_>>()
            .join(";"),
        Value::Array(items) => items
            .iter()
            .map(|i| match i {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(";"),
        Value::Object(_) => v.to_string(),
    }
}

/// Characters a cell never carries as they are: controls (C0, DEL, C1)
/// and bidirectional formatting, which could hide or reorder what a
/// spreadsheet shows.
fn escaped(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// One CSV cell (RFC 4180): `'` before a leading `=`, `+`, `-`, `@`, tab
/// or CR, so a spreadsheet takes it as text rather than a formula; the
/// characters [`escaped`] names written `\u{…}`; quoted when it holds a
/// comma or a quote, with quotes doubled.
pub(crate) fn csv_cell(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    if text.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        out.push('\'');
    }
    for c in text.chars() {
        if escaped(c) {
            out.push_str(&format!("\\u{{{:x}}}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    if out.contains([',', '"']) {
        format!("\"{}\"", out.replace('"', "\"\""))
    } else {
        out
    }
}

/// The response body: the worker's chunks, then a clean end only if the
/// worker finished; otherwise an error, so the download fails rather
/// than looking complete.
struct Chunks {
    rx: mpsc::Receiver<Result<Bytes, io::Error>>,
    complete: Arc<AtomicBool>,
    ended: bool,
}

impl futures_core::Stream for Chunks {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(None) => {
                self.ended = true;
                if self.complete.load(Ordering::Acquire) {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(io::Error::other("the export stopped early"))))
                }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_quoted_and_defused() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell(""), "");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        for lead in ["=", "+", "-", "@"] {
            assert_eq!(csv_cell(&format!("{lead}1+1")), format!("'{lead}1+1"));
        }
        assert_eq!(
            csv_cell("=HYPERLINK(\"x\",\"y\")"),
            "\"'=HYPERLINK(\"\"x\"\",\"\"y\"\")\""
        );
        assert_eq!(csv_cell("\tcmd"), "'\\u{9}cmd");
        assert_eq!(csv_cell("\r=x"), "'\\u{d}=x");
        assert_eq!(csv_cell("a\nb\r\nc"), "a\\u{a}b\\u{d}\\u{a}c");
        assert_eq!(csv_cell("x\u{1b}[31m"), "x\\u{1b}[31m");
        assert_eq!(csv_cell("abc\u{202E}gpj.exe"), "abc\\u{202e}gpj.exe");
        assert_eq!(csv_cell("a\u{85}b\u{7f}"), "a\\u{85}b\\u{7f}");
        // Only a leading sign is a formula; inside, it's text.
        assert_eq!(csv_cell("x-264"), "x-264");
        assert_eq!(csv_cell("日本語"), "日本語");
    }

    #[test]
    fn values_become_csv_text() {
        assert_eq!(csv_text(ExportField::Seeders, &Value::Null), "");
        assert_eq!(csv_text(ExportField::Size, &Value::from(42)), "42");
        assert_eq!(
            csv_text(ExportField::Flags, &serde_json::json!(["v2", "private"])),
            "v2;private"
        );
        assert_eq!(csv_text(ExportField::Flags, &serde_json::json!([])), "");
        assert_eq!(
            csv_text(
                ExportField::Composition,
                &serde_json::json!([
                    {"group": "video", "pct_bytes": 95, "files": 1, "bytes": 9, "pct_files": 50},
                    {"group": "other", "pct_bytes": 5, "files": 1, "bytes": 1, "pct_files": 50},
                ])
            ),
            "video:95;other:5"
        );
    }

    #[test]
    fn file_names_are_utc_times() {
        assert_eq!(file_name(0, "csv"), "lodestar-export-19700101-000000.csv");
        assert_eq!(
            file_name(1_700_000_000, "ndjson"),
            "lodestar-export-20231114-221320.ndjson"
        );
        assert_eq!(
            file_name(951_782_400, "txt"),
            "lodestar-export-20000229-000000.txt"
        );
    }

    #[test]
    fn bitmagnet_times_are_rfc3339_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(253_402_300_799), "9999-12-31T23:59:59Z");
    }

    #[test]
    fn every_bitmagnet_type_is_one_of_its_enum() {
        // bitmagnet's `ContentType` ENUM (internal/model/content_type.go).
        const THEIRS: [&str; 9] = [
            "movie",
            "tv_show",
            "music",
            "ebook",
            "comic",
            "audiobook",
            "game",
            "software",
            "xxx",
        ];
        let mut mapped = Vec::new();
        for t in lodestar_classify::ContentType::ALL {
            if let Some(b) = bitmagnet_type(t) {
                assert!(THEIRS.contains(&b), "{t:?} -> {b}");
                mapped.push(b);
            }
        }
        mapped.sort_unstable();
        let mut theirs = THEIRS.to_vec();
        theirs.sort_unstable();
        assert_eq!(mapped, theirs, "one Lodestar type for each of theirs");
        assert_eq!(
            bitmagnet_type(lodestar_classify::ContentType::Adult),
            Some("xxx")
        );
        assert_eq!(bitmagnet_type(lodestar_classify::ContentType::Video), None);
    }

    fn row(name: &str, discovered_at: i64) -> TorrentRow {
        TorrentRow {
            id: 1,
            info_hash: InfoHash([0xab; 20]),
            info_hash_v2: None,
            name: name.to_string(),
            name_raw: None,
            size: 1 << 20,
            piece_len: 1 << 16,
            piece_count: 16,
            file_count: 1,
            info_size: 100,
            flags: 0,
            discovered_at,
            metadata_at: discovered_at,
            last_seen_at: discovered_at,
            sources: 0,
            first_source: None,
        }
    }

    fn line(name: &str, discovered_at: i64) -> String {
        let mut out = Vec::new();
        bitmagnet_line(&mut out, &row(name, discovered_at), None).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_bitmagnet_line_is_one_object_with_its_keys_in_order() {
        assert_eq!(
            line("Plain", 1_700_000_000),
            format!(
                "{{\"infoHash\":\"{}\",\"name\":\"Plain\",\"size\":1048576,\"source\":\"lodestar\",\"publishedAt\":\"2023-11-14T22:13:20Z\"}}\n",
                "ab".repeat(20)
            )
        );
        // No plausible time: left out, so bitmagnet stores none.
        assert!(!line("Plain", 0).contains("publishedAt"));
    }

    #[test]
    fn hostile_names_stay_inside_one_json_string() {
        for name in [
            "a\nb\r\nc",
            "\"},{\"infoHash\":\"0000000000000000000000000000000000000000\"",
            "back\\slash\u{0}nul",
            "x\u{1b}[31m\u{202e}gpj.exe\u{2028}\u{2029}",
            "\u{feff}日本語\u{7f}\u{85}",
        ] {
            let l = line(name, 1);
            // One line: bitmagnet splits on '\n' alone.
            assert_eq!(l.matches('\n').count(), 1, "{l}");
            assert!(l.ends_with('\n'));
            let v: Value = serde_json::from_str(l.trim_end()).unwrap();
            let obj = v.as_object().unwrap();
            assert_eq!(obj.len(), 5, "{l}");
            assert_eq!(obj["infoHash"], "ab".repeat(20));
            assert!(
                !l.contains(['\u{202e}', '\u{2028}', '\u{2029}', '\u{7f}', '\u{85}']),
                "{l}"
            );
            let back = obj["name"].as_str().unwrap();
            assert!(!back.contains('\0'), "a NUL reached PostgreSQL text");
            assert_eq!(back, name.replace('\0', "\u{fffd}"));
        }
    }

    #[test]
    fn fields_are_checked_and_ordered() {
        let p = |fields: Option<&str>, format| ExportRequest {
            fields: fields.map(str::to_string),
            format,
            ..ExportRequest::default()
        };
        let s = shape(&p(Some("size, name,size"), None)).unwrap();
        assert_eq!(s.fields, vec![ExportField::Name, ExportField::Size]);
        assert_eq!(s.format, ExportFormat::Csv);
        let all = shape(&p(None, Some(ExportFormat::Json))).unwrap();
        assert_eq!(all.fields.len(), ExportField::ALL.len() - 1);
        assert!(!all.fields.contains(&ExportField::Magnet));
        assert_eq!(
            shape(&p(Some("name,score"), None)).unwrap_err().code(),
            "unknown_field"
        );
        assert_eq!(
            shape(&p(Some(""), None)).unwrap_err().code(),
            "unknown_field"
        );
        assert_eq!(
            shape(&p(Some("name"), Some(ExportFormat::Magnets)))
                .unwrap_err()
                .code(),
            "fields_unused"
        );
        assert_eq!(
            shape(&p(Some("name"), Some(ExportFormat::Bitmagnet)))
                .unwrap_err()
                .code(),
            "fields_unused"
        );
        assert!(
            shape(&p(None, Some(ExportFormat::Bitmagnet)))
                .unwrap()
                .fields
                .is_empty()
        );
    }
}
