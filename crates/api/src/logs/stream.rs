//! `GET /logs/stream`: the live log (architecture §12.1), separate from
//! the `/events` hub (B4), over its own cursor on the 5,000-record ring.
//!
//! - **Cursor:** `Last-Event-ID` (what `EventSource` sends on reconnect),
//!   else `?after=`, else the newest record. An id from another epoch is a
//!   `resync {reason: restarted}`, one older than the ring's oldest
//!   `expired`, a malformed or future one `malformed`; each continues live
//!   from the newest. Never an error.
//! - **Filters** (level, components, text) are applied by the source
//!   before anything is queued, so filtered-out lines never travel.
//! - **Backpressure:** a 16-frame queue to the socket and at most 256
//!   matching records waiting. Past that the client gets one
//!   `resync {reason: lagged, skipped, evicted}` and carries on from the
//!   newest; the skipped lines stay readable through `GET /logs`.
//! - **Purges:** when a data kind turns off, an `event: purge` comes
//!   before any line read after it.
//! - **It ends with its session:** at each heartbeat it re-checks what
//!   let it in, the cookie or the trusted proxy's header from the same
//!   TCP peer.

use std::convert::Infallible;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};

use super::{
    Handle, LineFilter, LogResync, LogResyncReason, LogSource, Refused, Slot, params, source,
    unavailable,
};
use crate::dto::error::ErrorEnvelope;
use crate::error::{ApiError, ApiResult};
use crate::{ApiState, Auth, auth};

/// What a cursor names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cursor {
    Ours(u64),
    OtherEpoch,
    Malformed,
}

/// `<epoch>-<seq>`, against this process's epoch.
pub(crate) fn parse_id(epoch: &str, id: &str) -> Cursor {
    let Some((e, seq)) = id.split_once('-') else {
        return Cursor::Malformed;
    };
    let wellformed = e.len() == 16
        && e.bytes().all(|b| b.is_ascii_hexdigit())
        && !seq.is_empty()
        && seq.len() <= 20
        && seq.bytes().all(|b| b.is_ascii_digit());
    if !wellformed {
        return Cursor::Malformed;
    }
    if e != epoch {
        return Cursor::OtherEpoch;
    }
    seq.parse().map_or(Cursor::Malformed, Cursor::Ours)
}

/// The session's key for the per-session cap: a hash of the session
/// cookie, or of the proxy user for a trusted-proxy session. Never logged.
fn session_of(auth: &Auth, headers: &HeaderMap) -> u64 {
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| auth.verify(c))
        .and_then(|c| auth::cookie_value(c, auth::COOKIE));
    let key = match cookie {
        Some(c) => format!("cookie:{c}"),
        None => {
            let name = auth.proxy().header;
            let user = headers
                .get(name.as_str())
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            format!("proxy:{user}")
        }
    };
    let d = Sha256::digest(key.as_bytes());
    u64::from_le_bytes(d[..8].try_into().expect("8 bytes"))
}

/// Whether what let the stream in still does: the cookie, or the proxy
/// header from the same TCP peer.
fn still_signed_in(auth: &Auth, headers: &HeaderMap, peer: IpAddr) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|c| auth.verify(c))
        || auth.proxy_signs_in(peer, headers)
}

fn frame(id: Option<&str>, event: &str, data: &str) -> Bytes {
    let mut s = String::with_capacity(data.len() + 64);
    if let Some(id) = id {
        s.push_str("id: ");
        s.push_str(id);
        s.push('\n');
    }
    s.push_str("event: ");
    s.push_str(event);
    // Compact JSON escapes newlines, so it's one `data:` line.
    s.push_str("\ndata: ");
    s.push_str(data);
    s.push_str("\n\n");
    Bytes::from(s)
}

fn resync(reason: LogResyncReason, skipped: Option<u64>, evicted: Option<u64>) -> Bytes {
    let data = serde_json::to_string(&LogResync {
        evicted,
        reason,
        skipped,
    })
    .expect("serializes");
    frame(None, "resync", &data)
}

/// The response body: frames from the client's task, holding its slot.
struct Frames {
    rx: mpsc::Receiver<Bytes>,
    _slot: Slot,
}

impl futures_core::Stream for Frames {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|f| f.map(Ok))
    }
}

/// Sends `frame`, giving up when the source closes or the client went.
async fn send(out: &mpsc::Sender<Bytes>, closed: &watch::Receiver<bool>, frame: Bytes) -> bool {
    let mut closed = closed.clone();
    tokio::select! {
        biased;
        _ = closed.wait_for(|c| *c) => false,
        sent = out.send(frame) => sent.is_ok(),
    }
}

/// The client's task: purges, then the lines after its cursor, until it
/// goes away, the source closes or its session ends.
async fn run(
    source: Arc<dyn LogSource>,
    filter: LineFilter,
    start: Option<String>,
    out: mpsc::Sender<Bytes>,
    signed_in: impl Fn() -> bool + Send + 'static,
) {
    let t = source.transport();
    let config = t.config().clone();
    let closed = t.closed();
    let retry = Bytes::from(format!("retry: {}\n\n", config.retry.as_millis()));
    if !send(&out, &closed, retry).await {
        return;
    }
    let (_guard, mut wake) = source.listen();
    let mut generation = source.generation();
    let epoch = source.epoch().to_string();
    let (head, oldest) = source.bounds();
    let mut cursor = head;
    let first = match start.as_deref().map(|s| parse_id(&epoch, s)) {
        None => None,
        Some(Cursor::Ours(seq)) if seq > head => Some(LogResyncReason::Malformed),
        Some(Cursor::Ours(seq)) if seq.saturating_add(1) < oldest => Some(LogResyncReason::Expired),
        Some(Cursor::Ours(seq)) => {
            cursor = seq;
            None
        }
        Some(Cursor::OtherEpoch) => Some(LogResyncReason::Restarted),
        Some(Cursor::Malformed) => Some(LogResyncReason::Malformed),
    };
    if let Some(reason) = first
        && !send(&out, &closed, resync(reason, None, None)).await
    {
        return;
    }
    let start = tokio::time::Instant::now() + config.heartbeat;
    let mut beat = tokio::time::interval_at(start, config.heartbeat);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut closed_wait = closed.clone();
    'outer: loop {
        wake.borrow_and_update();
        let (g, purges) = source.purges_since(generation);
        generation = g;
        for p in purges {
            let data = serde_json::to_string(&p).expect("serializes");
            if !send(&out, &closed, frame(None, "purge", &data)).await {
                return;
            }
        }
        let read = source.after(cursor, &filter, config.pending);
        if read.evicted > 0 || read.matching > config.pending as u64 {
            let r = resync(
                LogResyncReason::Lagged,
                Some(read.matching),
                Some(read.evicted),
            );
            if !send(&out, &closed, r).await {
                return;
            }
            cursor = read.head;
            continue;
        }
        for (seq, line) in read.lines {
            // A purge since the read: its frame first, and these lines
            // again as the ring has them now.
            if source.generation() != generation {
                continue 'outer;
            }
            let id = format!("{epoch}-{seq}");
            let data = serde_json::to_string(&line).expect("serializes");
            if !send(&out, &closed, frame(Some(&id), "line", &data)).await {
                return;
            }
            cursor = seq;
        }
        cursor = cursor.max(read.head);
        let beat_now = tokio::select! {
            biased;
            _ = closed_wait.wait_for(|c| *c) => return,
            () = out.closed() => return,
            changed = wake.changed() => match changed {
                Ok(()) => false,
                Err(_) => return,
            },
            _ = beat.tick() => true,
        };
        if beat_now {
            // Expired, signed out, or the password changed: the stream
            // ends with the session.
            if !signed_in() {
                return;
            }
            if !send(&out, &closed, Bytes::from_static(b": keep-alive\n\n")).await {
                return;
            }
        }
    }
}

/// The live log.
///
/// Each frame is `id: <epoch>-<seq>` (none on `resync` and `purge`),
/// `event: line | resync | purge` and `data:` its payload (`LogFrame`).
/// Send the last id back as `Last-Event-ID` (`EventSource` does) or
/// `after` to resume without a gap while it's still in the ring. A comment
/// every 15 s keeps the connection open, and the session is checked again
/// each time; `retry:` is 5 s.
#[utoipa::path(
    operation_id = "getLogStream",
    get,
    path = "/logs/stream",
    tag = "logs",
    params(
        ("level" = Option<String>, Query, description = "The least severe level shown: error, warn, info, debug or trace (default)."),
        ("components" = Option<String>, Query, description = "A comma-separated list of components (`LogComponent`); all when absent."),
        ("text" = Option<String>, Query, description = "A case-insensitive substring of the line as shown, at most 200 characters."),
        ("after" = Option<String>, Query, description = "The id to resume after, when there's no `Last-Event-ID`."),
        ("Last-Event-ID" = Option<String>, Header, description = "The last `id:` received; wins over `after`."),
    ),
    responses(
        (
            status = 200,
            description = "The stream: one `LogFrame` per frame (`event:` names the variant, `data:` holds its payload).",
            content_type = "text/event-stream",
            body = super::LogFrame,
            headers(
                ("Cache-Control" = String, description = "`no-store`."),
                ("X-Accel-Buffering" = String, description = "`no`, so a proxy doesn't buffer it."),
            )
        ),
        (status = 400, description = "A bad filter (`bad_parameter`, `unknown_component`), with `field`.", body = ErrorEnvelope),
        (status = 401, description = "No session (`login_required`); keys and feed tokens can't read logs.", body = ErrorEnvelope),
        (
            status = 429,
            description = "Too many log streams, in all or for this session (`too_many_streams`).",
            body = ErrorEnvelope,
            headers(("Retry-After" = u32, description = "Seconds to wait."))
        ),
        (status = 503, description = "Not served here, or shutting down (`logs_unavailable`).", body = ErrorEnvelope),
    )
)]
pub(crate) async fn stream(
    State(s): State<ApiState>,
    handle: Handle,
    headers: HeaderMap,
    extensions: axum::http::Extensions,
    RawQuery(query): RawQuery,
) -> ApiResult<Response> {
    let p = params::parse(query.as_deref())?;
    let filter = params::filter(&p)?;
    let source = source(handle)?;
    let auth = s.auth.clone();
    let slot = match source.transport().acquire(session_of(&auth, &headers)) {
        Ok(slot) => slot,
        Err(Refused::Closed) => return Err(unavailable()),
        Err(Refused::Busy) => {
            let mut res = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_streams",
                "Too many live logs are open; close one or try again shortly.",
            )
            .into_response();
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            return Ok(res);
        }
    };
    let start = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| p.get("after").cloned());
    let peer = crate::extract::peer_ip(&extensions);
    let (out, rx) = mpsc::channel(source.transport().config().queue.max(1));
    let signed_in = move || still_signed_in(&auth, &headers, peer);
    tokio::spawn(run(source, filter, start, out, signed_in));
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-store"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(Frames { rx, _slot: slot }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let e = "00112233aabbccdd";
        assert_eq!(parse_id(e, "00112233aabbccdd-7"), Cursor::Ours(7));
        assert_eq!(parse_id(e, "ffffffffffffffff-7"), Cursor::OtherEpoch);
        for bad in [
            "",
            "7",
            "00112233aabbccdd-",
            "00112233aabbccdd-x",
            "00112233aabbccd-7",
            "00112233aabbccdd-99999999999999999999999",
            "00112233aabbccdd--7",
            "00112233aabbccdd-+7",
        ] {
            assert_eq!(parse_id(e, bad), Cursor::Malformed, "{bad}");
        }
    }
}
