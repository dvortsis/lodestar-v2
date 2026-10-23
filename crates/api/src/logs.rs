//! `/logs` (architecture §12.1, §10.10.12): admin session only. API keys
//! and feed tokens can't read logs, in any form.
//!
//! - **`GET /logs/catalog`:** the events and data kinds, from
//!   `lodestar_log` (the events the binary registered and the state its
//!   runtime last applied). It needs no [`LogSource`].
//! - **`GET /logs` and `GET /logs/stream`** read through [`LogSource`],
//!   which the binary implements over its ring buffer and log folder and
//!   layers as an `Extension`. Without one, they're `503 logs_unavailable`.
//!   The file list, downloads, the archive and deletion are B-log2's
//!   second part.
//! - **What a browser can read back** is decided by the source: the live
//!   mask now in force, for the ring, the stream and history.
//! - **Bounded work:** the stream's own caps (2 per session, 8 in all) and
//!   queue (256 matching records, then one `resync`); at most 2 file reads
//!   at once.

mod params;
mod stream;

use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::extract::{Extension, RawQuery};
use axum::http::{HeaderValue, StatusCode, header};
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
    LogCatalog, LogComponent, LogDataKind, LogEvent, LogField, LogFieldKind, LogFrame, LogHistory,
    LogKind, LogKindOutputs, LogLevel, LogLine, LogOutput, LogOutputName, LogPresets, LogPurge,
    LogRepeated, LogResync, LogResyncReason, LogTemporary, LogValue,
};
use crate::error::{ApiError, ApiResult, internal};
pub use params::{LineFilter, parse_rfc3339};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(catalog, history, stream::stream), components(schemas(LogFrame)))]
pub(crate) struct ApiDoc;

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/logs", get(history))
            .route("/logs/stream", get(stream::stream))
            .route("/logs/catalog", get(catalog)),
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

/// Why a source refused.
#[derive(Debug)]
pub enum LogsError {
    BadCursor,
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
    /// A page of history, newest first: the ring, then the files.
    fn history(&self, query: &HistoryQuery) -> Result<LogHistory, LogsError>;
    /// Ends every stream and refuses new ones.
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
