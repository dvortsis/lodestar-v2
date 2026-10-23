//! `/logs` (architecture §12.1, §10.10.12): admin session only. API keys
//! and feed tokens can't read logs, in any form.
//!
//! - **`GET /logs/catalog`:** the events and data kinds, from
//!   `lodestar_log` (the events the binary registered and the state its
//!   runtime last applied). It needs no [`LogSource`].
//! - **`GET /logs`, `GET /logs/stream`, the files and the archive** read
//!   through [`LogSource`], which the binary implements over its ring
//!   buffer and log folder and layers as an `Extension`. Without one,
//!   they're `503 logs_unavailable`.
//! - **What a browser can read back** is decided by the source: the live
//!   mask now in force for the ring, the stream and history; at most the
//!   Private kinds (less client addresses) for a redacted download.
//! - **Bounded work:** the stream's own caps (2 per session, 8 in all) and
//!   queue (256 matching records, then one `resync`); at most 2 file reads
//!   at once across history, downloads and the archive.

mod params;
mod stream;

use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::rejection::PathRejection;
use axum::extract::{Extension, Path, RawQuery};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use lodestar_log::{Component, Kind, Level, Output};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use utoipa::OpenApi;

use crate::ApiState;
use crate::auth;
use crate::dto::error::ErrorEnvelope;
pub use crate::dto::logs::{
    DeleteLogFiles, LogCatalog, LogComponent, LogDataKind, LogEvent, LogField, LogFieldKind,
    LogFileInfo, LogFileSkipped, LogFileState, LogFiles, LogFilesDeleted, LogFilesWhich, LogFormat,
    LogFrame, LogHistory, LogKind, LogKindOutputs, LogLevel, LogLine, LogOutput, LogOutputName,
    LogPresets, LogPurge, LogRepeated, LogResync, LogResyncReason, LogTemporary, LogValue,
};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::same_origin_json;
pub use params::{LineFilter, parse_rfc3339};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(
    paths(
        catalog,
        history,
        stream::stream,
        files,
        download,
        archive,
        delete_files
    ),
    components(schemas(LogFrame, LogFormat))
)]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/logs", get(history))
            .route("/logs/stream", get(stream::stream))
            .route("/logs/catalog", get(catalog))
            .route("/logs/files", get(files).delete(delete_files))
            .route("/logs/files/{name}", get(download))
            .route("/logs/archive.zip", get(archive)),
    )
}

/// What the ring buffer holds after a cursor, through a filter.
#[derive(Debug, Default)]
pub struct RingRead {
    /// The first matching lines, at most the `max` asked for, each with
    /// its sequence.
    pub lines: Vec<(u64, LogLine)>,
    /// Every matching line after the cursor, `lines` included.
    pub matching: u64,
    /// Lines after the cursor that left the ring unseen.
    pub evicted: u64,
    /// The newest sequence looked at: the next cursor.
    pub head: u64,
}

/// A history request, checked.
#[derive(Debug, Clone)]
pub struct HistoryQuery {
    pub filter: LineFilter,
    /// Unix millis, inclusive.
    pub since: Option<i64>,
    /// Unix millis, inclusive.
    pub until: Option<i64>,
    pub limit: usize,
    pub cursor: Option<String>,
}

/// A file served: its headers and its bytes, read on the blocking pool.
pub struct Download {
    /// ASCII only, built from the typed name.
    pub file_name: String,
    pub content_type: &'static str,
    pub chunks: Box<dyn Iterator<Item = io::Result<Vec<u8>>> + Send>,
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Download")
            .field("file_name", &self.file_name)
            .finish_non_exhaustive()
    }
}

/// Why a source refused.
#[derive(Debug)]
pub enum LogsError {
    BadCursor,
    /// No such file, or one refused: never echoes the name.
    NotFound,
    /// Shut down, or nothing to read.
    Unavailable,
    /// Logged by its type only.
    Internal(io::Error),
}

impl From<LogsError> for ApiError {
    fn from(e: LogsError) -> ApiError {
        match e {
            LogsError::BadCursor => ApiError::bad_request(
                "bad_cursor",
                "That cursor isn't valid for these filters; load the newest lines again.",
            )
            .field("cursor"),
            LogsError::NotFound => ApiError::new(
                StatusCode::NOT_FOUND,
                "log_file_not_found",
                "There's no such log file.",
            )
            .field("name"),
            LogsError::Unavailable => unavailable(),
            LogsError::Internal(e) => internal(e),
        }
    }
}

/// The live log's sizes and intervals.
#[derive(Debug, Clone)]
pub struct LogStreamConfig {
    /// An SSE comment this often; the session is checked again each time.
    pub heartbeat: Duration,
    /// The `retry:` hint.
    pub retry: Duration,
    /// Log streams in all...
    pub max_streams: usize,
    /// ...and per session, separate from `/events`' caps.
    pub max_per_session: usize,
    /// Frames queued to one client's socket.
    pub queue: usize,
    /// Matching records one client may have waiting before it gets a
    /// `lagged` resync.
    pub pending: usize,
    /// File reads at once, across history, downloads and archives.
    pub reads: usize,
}

impl Default for LogStreamConfig {
    fn default() -> Self {
        LogStreamConfig {
            heartbeat: Duration::from_secs(15),
            retry: Duration::from_secs(5),
            max_streams: 8,
            max_per_session: 2,
            queue: 16,
            pending: 256,
            reads: 2,
        }
    }
}

#[derive(Debug, Default)]
struct Slots {
    total: usize,
    per_session: std::collections::HashMap<u64, usize>,
}

/// What the routes keep per source: stream slots, the read limit and
/// the shutdown switch. A [`LogSource`] embeds one.
#[derive(Debug)]
pub struct Transport {
    config: LogStreamConfig,
    slots: Arc<Mutex<Slots>>,
    reads: Arc<Semaphore>,
    closed: watch::Sender<bool>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A held stream slot, released on drop.
#[derive(Debug)]
pub struct Slot {
    slots: Arc<Mutex<Slots>>,
    session: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut slots = lock(&self.slots);
        slots.total -= 1;
        if let Some(n) = slots.per_session.get_mut(&self.session) {
            *n -= 1;
            if *n == 0 {
                slots.per_session.remove(&self.session);
            }
        }
    }
}

/// Why a stream or a read wasn't started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Past a cap, or the reads are busy.
    Busy,
    /// Shutting down.
    Closed,
}

impl Transport {
    pub fn new(config: LogStreamConfig) -> Transport {
        Transport {
            reads: Arc::new(Semaphore::new(config.reads.max(1))),
            config,
            slots: Arc::default(),
            closed: watch::channel(false).0,
        }
    }

    pub fn config(&self) -> &LogStreamConfig {
        &self.config
    }

    /// Ends every stream and download, and refuses new ones.
    pub fn close(&self) {
        self.closed.send_replace(true);
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub(crate) fn closed(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }

    /// Open streams.
    pub fn streams(&self) -> usize {
        lock(&self.slots).total
    }

    fn acquire(&self, session: u64) -> Result<Slot, Refused> {
        if self.is_closed() {
            return Err(Refused::Closed);
        }
        let mut slots = lock(&self.slots);
        if slots.total >= self.config.max_streams {
            return Err(Refused::Busy);
        }
        let mine = slots.per_session.entry(session).or_insert(0);
        if *mine >= self.config.max_per_session {
            return Err(Refused::Busy);
        }
        *mine += 1;
        slots.total += 1;
        Ok(Slot {
            slots: self.slots.clone(),
            session,
        })
    }

    /// One of the file reads, or `Busy`.
    fn read(&self) -> Result<OwnedSemaphorePermit, Refused> {
        if self.is_closed() {
            return Err(Refused::Closed);
        }
        self.reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| Refused::Busy)
    }
}

/// What `/logs` reads through: the binary's ring buffer and log folder.
/// The blocking methods run on the blocking pool.
pub trait LogSource: Send + Sync + fmt::Debug {
    /// The routes' state.
    fn transport(&self) -> &Transport;
    /// This process's epoch, the first half of every line's id.
    fn epoch(&self) -> &str;
    /// The newest sequence given out, and the oldest still in the ring.
    fn bounds(&self) -> (u64, u64);
    /// Registers a stream: until the guard is dropped, new records change
    /// the receiver (to the newest sequence).
    fn listen(&self) -> (Box<dyn Send + Sync>, watch::Receiver<u64>);
    /// The ring's lines after sequence `cursor` that `filter` matches
    /// under the mask in force now: the first `max`, and how many in all.
    fn after(&self, cursor: u64, filter: &LineFilter, max: usize) -> RingRead;
    /// The newest purge generation (a cheap check).
    fn generation(&self) -> u64;
    /// The purges after `generation`, and the newest generation.
    fn purges_since(&self, generation: u64) -> (u64, Vec<LogPurge>);
    /// The purges that reach lines up to `seq` or later (`through >=
    /// seq`): what a stream resuming after `seq` may have missed.
    fn purges_covering(&self, seq: u64) -> Vec<LogPurge>;
    /// A page of history, newest first: the ring, then the files.
    fn history(&self, query: &HistoryQuery) -> Result<LogHistory, LogsError>;
    /// The log files.
    fn files(&self) -> LogFiles;
    /// One file; `redact` `None` takes the file's default.
    fn download(
        &self,
        name: &str,
        format: LogFormat,
        redact: Option<bool>,
    ) -> Result<Download, LogsError>;
    /// Every listed file, as a zip.
    fn archive(&self, format: LogFormat, redact: Option<bool>) -> Result<Download, LogsError>;
    /// Deletes files, all of them or those with details.
    fn delete(&self, which: LogFilesWhich) -> Result<LogFilesDeleted, LogsError>;
    /// Ends every stream and in-flight download and refuses new ones.
    fn close(&self) {
        self.transport().close();
    }
}

type Handle = Option<Extension<Arc<dyn LogSource>>>;

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "logs_unavailable",
        "Logs aren't available in this process right now.",
    )
}

fn source(handle: Handle) -> ApiResult<Arc<dyn LogSource>> {
    match handle {
        Some(Extension(s)) if !s.transport().is_closed() => Ok(s),
        _ => Err(unavailable()),
    }
}

fn busy() -> Response {
    let mut res = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "logs_busy",
        "Log files are being read already; try again in a moment.",
    )
    .into_response();
    res.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("2"));
    res
}

/// Runs `f` on the blocking pool with one of the file reads held. `Err`
/// is the response to send instead (`logs_busy` with its `Retry-After`,
/// or the error).
async fn with_read<T: Send + 'static>(
    s: &Arc<dyn LogSource>,
    f: impl FnOnce(&dyn LogSource) -> Result<T, LogsError> + Send + 'static,
) -> Result<T, Box<Response>> {
    let permit = match s.transport().read() {
        Ok(p) => p,
        Err(Refused::Busy) => return Err(Box::new(busy())),
        Err(Refused::Closed) => return Err(Box::new(unavailable().into_response())),
    };
    let s = s.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f(&*s)
    })
    .await;
    match joined {
        Ok(r) => r.map_err(|e| Box::new(ApiError::from(e).into_response())),
        Err(e) => Err(Box::new(internal(e).into_response())),
    }
}

fn level(l: Level) -> LogLevel {
    match l {
        Level::Error => LogLevel::Error,
        Level::Warn => LogLevel::Warn,
        Level::Info => LogLevel::Info,
        Level::Debug => LogLevel::Debug,
        Level::Trace => LogLevel::Trace,
    }
}

fn component(c: Component) -> LogComponent {
    match c {
        Component::System => LogComponent::System,
        Component::Dht => LogComponent::Dht,
        Component::Fetch => LogComponent::Fetch,
        Component::Pipeline => LogComponent::Pipeline,
        Component::Search => LogComponent::Search,
        Component::Api => LogComponent::Api,
        Component::Sources => LogComponent::Sources,
        Component::Filters => LogComponent::Filters,
        Component::Integrations => LogComponent::Integrations,
        Component::Workers => LogComponent::Workers,
        Component::Companions => LogComponent::Companions,
        Component::Auth => LogComponent::Auth,
        Component::Libraries => LogComponent::Libraries,
    }
}

fn kind(k: Kind) -> LogDataKind {
    match k {
        Kind::Hash => LogDataKind::Hash,
        Kind::Name => LogDataKind::Name,
        Kind::Path => LogDataKind::Path,
        Kind::Url => LogDataKind::Url,
        Kind::Query => LogDataKind::Query,
        Kind::Client => LogDataKind::Client,
        Kind::Peer => LogDataKind::Peer,
        Kind::Size => LogDataKind::Size,
        Kind::PeerActivity => LogDataKind::PeerActivity,
        Kind::FilterChange => LogDataKind::FilterChanges,
    }
}

fn output(o: Output) -> LogOutputName {
    match o {
        Output::Console => LogOutputName::Console,
        Output::File => LogOutputName::File,
        Output::Live => LogOutputName::Live,
        Output::EventLog => LogOutputName::EventLog,
    }
}

/// The catalogue as `lodestar_log` has it now.
pub fn current(now: i64) -> LogCatalog {
    let status = lodestar_log::status();
    let events = lodestar_log::events()
        .map(|e| LogEvent {
            component: component(e.component),
            current: e.is_on(),
            default_on: e.default_on,
            help_key: e.help_key(),
            id: e.id.to_string(),
            kinds: e.kinds().into_iter().map(kind).collect(),
            label_key: e.label_key(),
            level: level(e.level),
            opt_in: e.opt_in.map(kind),
            rate_per_hour: lodestar_log::rate_per_hour(e),
            required: e.required,
        })
        .collect();
    let kinds = Kind::ALL
        .into_iter()
        .map(|k| {
            let (outputs, until) = match status
                .as_ref()
                .and_then(|s| s.kinds.iter().find(|x| x.kind == k))
            {
                Some(s) => (s.outputs, s.until),
                None => {
                    let on = k.default_on();
                    ([on; 4], if on { -1 } else { 0 })
                }
            };
            LogKind {
                default_on: k.default_on(),
                identifying: k.identifying(),
                kind: kind(k),
                opt_in: k.opt_in(),
                outputs: LogKindOutputs {
                    console: outputs[0],
                    file: outputs[1],
                    live: outputs[2],
                    event_log: outputs[3],
                },
                until,
            }
        })
        .collect();
    let (presets, outputs) = match status {
        Some(s) => (
            LogPresets {
                data: s.data.to_string(),
                temporary: s.temporary.map(|(v, until)| LogTemporary {
                    until,
                    verbosity: v.to_string(),
                }),
                verbosity: s.verbosity.to_string(),
            },
            s.outputs
                .iter()
                .map(|o| LogOutput {
                    detail: o.detail.map(str::to_string),
                    enabled: o.enabled,
                    format: o.format.map(str::to_string),
                    level: level(o.level),
                    output: output(o.output),
                })
                .collect(),
        ),
        None => (
            LogPresets {
                data: "private".into(),
                temporary: None,
                verbosity: "normal".into(),
            },
            Vec::new(),
        ),
    };
    LogCatalog {
        events,
        kinds,
        now,
        outputs,
        presets,
    }
}

/// Every log event (id, component, level, label and help keys, default,
/// current, data kinds, required, rate per hour), every data kind with its
/// switch per output and its deadline, the presets and the outputs. Admin
/// session only.
#[utoipa::path(
    operation_id = "getLogCatalog",
    get,
    path = "/logs/catalog",
    tag = "logs",
    responses((status = 200, body = LogCatalog))
)]
pub(crate) async fn catalog() -> Json<LogCatalog> {
    Json(current(crate::unix_now()))
}

/// History, newest first: the ring buffer, then the log files (the
/// current one read backwards, then the rotated ones by their time).
/// Lines are shown through the data kinds in force now: a field whose kind
/// is off reads `[hidden]`, and lines of an opt-in that's off are left out.
/// A page cut short by the read cap says `more` with a cursor.
#[utoipa::path(
    operation_id = "getLogHistory",
    get,
    path = "/logs",
    tag = "logs",
    params(
        ("level" = Option<String>, Query, description = "The least severe level shown (default trace)."),
        ("components" = Option<String>, Query, description = "A comma-separated list of components; all when absent."),
        ("text" = Option<String>, Query, description = "A case-insensitive substring of the line as shown, at most 200 characters."),
        ("since" = Option<String>, Query, description = "RFC 3339 UTC: lines at or after it."),
        ("until" = Option<String>, Query, description = "RFC 3339 UTC: lines at or before it."),
        ("limit" = Option<u32>, Query, description = "Lines per page: 1 to 1,000, default 200."),
        ("cursor" = Option<String>, Query, description = "The previous page's `next_cursor`, with the same filters."),
    ),
    responses(
        (status = 200, body = LogHistory),
        (status = 400, description = "A bad parameter (`bad_parameter`, `unknown_component`) or a cursor from other filters (`bad_cursor`), with `field`.", body = ErrorEnvelope),
        (status = 401, description = "No session (`login_required`).", body = ErrorEnvelope),
        (
            status = 429,
            description = "Log files are being read already (`logs_busy`).",
            body = ErrorEnvelope,
            headers(("Retry-After" = u32, description = "Seconds to wait."))
        ),
        (status = 503, description = "Not served here (`logs_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn history(handle: Handle, RawQuery(query): RawQuery) -> ApiResult<Response> {
    let p = params::parse(query.as_deref())?;
    let filter = params::filter(&p)?;
    let since = params::time(&p, "since")?;
    let until = params::time(&p, "until")?;
    let limit = params::limit(&p)?;
    let cursor = p.get("cursor").cloned();
    if cursor
        .as_ref()
        .is_some_and(|c| c.len() > params::MAX_CURSOR)
    {
        return Err(LogsError::BadCursor.into());
    }
    let s = source(handle)?;
    let q = HistoryQuery {
        filter,
        since,
        until,
        limit,
        cursor,
    };
    match with_read(&s, move |s| s.history(&q)).await {
        Ok(page) => Ok(([(header::CACHE_CONTROL, "no-store")], Json(page)).into_response()),
        Err(res) => Ok(*res),
    }
}

/// The log files, newest first, with whether each contains torrent
/// details. An unreadable details index marks every file as having them.
#[utoipa::path(
    operation_id = "getLogFiles",
    get,
    path = "/logs/files",
    tag = "logs",
    responses(
        (status = 200, body = LogFiles),
        (status = 401, description = "No session (`login_required`).", body = ErrorEnvelope),
        (status = 503, description = "Not served here (`logs_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn files(handle: Handle) -> ApiResult<Response> {
    let s = source(handle)?;
    let listed = tokio::task::spawn_blocking(move || s.files())
        .await
        .map_err(internal)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(listed)).into_response())
}

/// The response for a download: its headers, and its bytes read on the
/// blocking pool until it ends, fails or the source closes.
fn served(s: &Arc<dyn LogSource>, d: Download, permit: OwnedSemaphorePermit) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<io::Result<Bytes>>(4);
    let closed = s.transport().closed();
    let mut chunks = d.chunks;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        for chunk in chunks.by_ref() {
            if *closed.borrow() {
                let _ = tx.blocking_send(Err(io::Error::other("shutting down")));
                return;
            }
            let failed = chunk.is_err();
            if tx.blocking_send(chunk.map(Bytes::from)).is_err() || failed {
                return;
            }
        }
    });
    let mut res = Response::new(Body::from_stream(Chunks { rx }));
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(d.content_type),
    );
    // The name is built from a validated, typed name: ASCII only.
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", d.file_name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

struct Chunks {
    rx: tokio::sync::mpsc::Receiver<io::Result<Bytes>>,
}

impl futures_core::Stream for Chunks {
    type Item = io::Result<Bytes>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Opens a download on the blocking pool with a read held for its whole
/// length.
async fn download_with(
    s: Arc<dyn LogSource>,
    open: impl FnOnce(&dyn LogSource) -> Result<Download, LogsError> + Send + 'static,
) -> ApiResult<Response> {
    let permit = match s.transport().read() {
        Ok(p) => p,
        Err(Refused::Busy) => return Ok(busy()),
        Err(Refused::Closed) => return Err(unavailable()),
    };
    let src = s.clone();
    let opened = tokio::task::spawn_blocking(move || open(&*src))
        .await
        .map_err(internal)??;
    Ok(served(&s, opened, permit))
}

/// One log file, always decompressed. `redact` defaults to true when the
/// file contains details: fields of the kinds that identify torrents or
/// people, client addresses and the opt-ins read `[hidden]`, and lines of
/// an opt-in event are left out. `redact=false` serves the file as it is.
#[utoipa::path(
    operation_id = "getLogFile",
    get,
    path = "/logs/files/{name}",
    tag = "logs",
    params(
        ("name" = String, Path, description = "`lodestar.jsonl` or a rotated file's name, as `GET /logs/files` lists it."),
        ("format" = Option<LogFormat>, Query, description = "`jsonl` (default) or `text`."),
        ("redact" = Option<bool>, Query, description = "Remove torrent details; the default is true when the file has them."),
    ),
    responses(
        (
            status = 200,
            description = "The file, as an attachment.",
            content_type = "application/x-ndjson",
            body = String,
            headers(
                ("Content-Disposition" = String, description = "`attachment; filename=\"<stem>[-redacted].<jsonl|txt>\"`."),
                ("Cache-Control" = String, description = "`no-store`."),
            )
        ),
        (status = 400, description = "A bad parameter (`bad_parameter`).", body = ErrorEnvelope),
        (status = 401, description = "No session (`login_required`).", body = ErrorEnvelope),
        (status = 404, description = "No such file (`log_file_not_found`, `field: \"name\"`).", body = ErrorEnvelope),
        (status = 429, description = "Log files are being read already (`logs_busy`).", body = ErrorEnvelope),
        (status = 503, description = "Not served here (`logs_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn download(
    handle: Handle,
    name: Result<Path<String>, PathRejection>,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let p = params::parse(query.as_deref())?;
    let format = params::format(&p)?;
    let redact = params::flag(&p, "redact")?;
    let s = source(handle)?;
    let Ok(Path(name)) = name else {
        return Err(LogsError::NotFound.into());
    };
    download_with(s, move |s| s.download(&name, format, redact)).await
}

/// Every listed log file in one zip, each converted as a single download
/// is. `redact` defaults to true when any file contains details.
#[utoipa::path(
    operation_id = "getLogArchive",
    get,
    path = "/logs/archive.zip",
    tag = "logs",
    params(
        ("format" = Option<LogFormat>, Query, description = "`jsonl` (default) or `text`."),
        ("redact" = Option<bool>, Query, description = "Remove torrent details; the default is true when any file has them."),
    ),
    responses(
        (
            status = 200,
            description = "A zip of the log files.",
            content_type = "application/zip",
            body = String,
        ),
        (status = 400, description = "A bad parameter (`bad_parameter`).", body = ErrorEnvelope),
        (status = 401, description = "No session (`login_required`).", body = ErrorEnvelope),
        (status = 429, description = "Log files are being read already (`logs_busy`).", body = ErrorEnvelope),
        (status = 503, description = "Not served here (`logs_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn archive(handle: Handle, RawQuery(query): RawQuery) -> ApiResult<Response> {
    let p = params::parse(query.as_deref())?;
    let format = params::format(&p)?;
    let redact = params::flag(&p, "redact")?;
    let s = source(handle)?;
    download_with(s, move |s| s.archive(format, redact)).await
}

/// Deletes log files: all of them, or only those containing torrent
/// details (or written under an opt-in). The file being written is
/// rotated first, so no new line is lost. A file that isn't a regular file
/// is skipped and reported. Needs no password (Destructive, §10.10.7).
#[utoipa::path(
    operation_id = "deleteLogFiles",
    delete,
    path = "/logs/files",
    tag = "logs",
    request_body = DeleteLogFiles,
    responses(
        (status = 200, body = LogFilesDeleted),
        (status = 401, description = "No session (`login_required`).", body = ErrorEnvelope),
        (status = 403, description = "A cross-site request (`cross_origin`).", body = ErrorEnvelope),
        (status = 415, description = "Not JSON (`unsupported_media_type`).", body = ErrorEnvelope),
        (status = 422, description = "A bad body (`bad_body`).", body = ErrorEnvelope),
        (status = 503, description = "Not served here (`logs_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn delete_files(
    handle: Handle,
    headers: HeaderMap,
    crate::extract::Json(body): crate::extract::Json<DeleteLogFiles>,
) -> ApiResult<Json<LogFilesDeleted>> {
    same_origin_json(&headers)?;
    let s = source(handle)?;
    let which = body.which;
    let done = tokio::task::spawn_blocking(move || s.delete(which))
        .await
        .map_err(internal)??;
    Ok(Json(done))
}
