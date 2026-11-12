//! `POST /torrents/upload` (UP-0): the one route that takes a file, the
//! `.torrent` upload exception the owner approved (plan
//! `docs/plan/2026-11-05-v07-m7.md` §5, owner §2.37,
//! `docs/decisions/2026-11-12-UP-0.md`).
//!
//! - **Raw body, not multipart.** `Content-Type: application/x-bittorrent`
//!   exactly, or `415`: no multipart parser, no file names, no form
//!   fields. One request carries one file, and the body is the file.
//! - **The same guard as JSON writes:** [`auth::protected`] (a session,
//!   or trusted-proxy auth where it's on, and the accepted notice). No API
//!   key reaches it: the route isn't under `key_or_session`, so no scope
//!   grants it. Then the same-origin rule JSON writes rely on
//!   ([`same_origin`], `same_origin_json`'s Origin–Host comparison, with
//!   `Sec-Fetch-Site: cross-site` refused too). `extract.rs` isn't edited,
//!   so the comparison is repeated here; a test holds the two to the same
//!   answers.
//! - **Bounded:** its own rate-limit bucket per client address
//!   ([`RATE_BURST`], [`RATE_PER_SEC`], [`RATE_MAX_IPS`]), spent before
//!   the body is read; a `Content-Length` over
//!   [`MAX_BYTES`] is refused unread, and any other body is read through a
//!   limit within [`READ_DEADLINE`]; at most [`IN_FLIGHT`] requests read
//!   or wait on the pipeline at once.
//! - **Judged like any metadata,** by the pipeline's
//!   `local_torrent` through the [`Sink`] the host installs.
//!
//! Errors name the cause and never echo a name, path, tracker, comment or
//! anything else from the file.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router};
use serde::Serialize;
use tokio::sync::Semaphore;
use utoipa::{OpenApi, ToSchema};

use crate::dto::error::ErrorEnvelope;
use crate::error::ApiError;
use crate::extract::ClientIp;
use crate::ops::BoxFuture;
use crate::{ApiState, auth};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(upload), components(schemas(UploadAccepted, TorrentBody)))]
pub(crate) struct ApiDoc;

/// The one content type taken.
pub const CONTENT_TYPE: &str = "application/x-bittorrent";

/// The largest body taken: 8 MiB (plan §5, a §2.26 number). The same as
/// the pipeline's `local_torrent::MAX_BYTES`, which a test checks.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

/// Uploads per client address: a burst of 30, then one a second (3,600
/// an hour). A §2.26 number (runner, 2026-11-12). A folder dropped on the
/// page sends one request per file.
pub const RATE_BURST: f64 = 30.0;
pub const RATE_PER_SEC: f64 = 1.0;
/// Addresses tracked at most. When it's full, an address with no bucket
/// is refused: nothing is evicted but a bucket that has fully refilled,
/// which is the same as a new one.
pub const RATE_MAX_IPS: usize = 10_000;

/// Requests reading a body or waiting on the pipeline at once. A request
/// past this waits, unread, for a turn. With the pipeline's own queue
/// (4) and its judge (1), and requests whose client left after sending,
/// at most about 9 bodies (72 MiB) are held at once.
pub const IN_FLIGHT: usize = 4;

/// How long a body may take to arrive once it's this request's turn, so
/// a stalled client can't keep a turn: `408 body_timeout` after it.
pub const READ_DEADLINE: Duration = Duration::from_secs(60);

/// What the pipeline made of an upload ([`Sink::submit`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Handed to the store: `info_hash` is the info dict's primary hash.
    Accepted { info_hash: [u8; 20], private: bool },
    /// Not a `.torrent` within the limits.
    Malformed,
    /// On the floor's hash list.
    Refused,
    /// The filter holds it: never stored.
    Held,
}

impl fmt::Debug for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Accepted { private, .. } => f
                .debug_struct("Accepted")
                .field("private", private)
                .finish_non_exhaustive(),
            Outcome::Malformed => f.write_str("Malformed"),
            Outcome::Refused => f.write_str("Refused"),
            Outcome::Held => f.write_str("Held"),
        }
    }
}

/// Uploads aren't being taken (the pipeline has stopped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable;

/// Where an upload goes: the pipeline's `local_torrent`, through the
/// host.
pub trait Sink: Send + Sync + fmt::Debug {
    /// Reads, judges and (when it may) stores `body`.
    fn submit(&self, body: Vec<u8>) -> BoxFuture<'_, Result<Outcome, Unavailable>>;
}

/// What the route needs, installed by the host as an [`Extension`] on the
/// router. One per process: it holds the rate buckets.
#[derive(Clone)]
pub struct Uploader {
    sink: Arc<dyn Sink>,
    limiter: Arc<Limiter>,
    in_flight: Arc<Semaphore>,
}

impl fmt::Debug for Uploader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Uploader").finish_non_exhaustive()
    }
}

impl Uploader {
    pub fn new(sink: Arc<dyn Sink>) -> Uploader {
        Uploader::with_rate(sink, RATE_BURST, RATE_PER_SEC, RATE_MAX_IPS)
    }

    /// [`Uploader::new`] with other rate numbers (tests set `per_sec` to
    /// 0, so a slow runner can't refill a bucket).
    pub fn with_rate(sink: Arc<dyn Sink>, burst: f64, per_sec: f64, max_ips: usize) -> Uploader {
        Uploader {
            sink,
            limiter: Arc::new(Limiter::new(burst, per_sec, max_ips)),
            in_flight: Arc::new(Semaphore::new(IN_FLIGHT)),
        }
    }
}

/// The request body: the `.torrent` file's bytes (the schema only; the
/// handler reads the body itself).
#[derive(Debug, ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct TorrentBody(pub Vec<u8>);

/// The body of a `202`.
#[derive(Debug, Serialize, ToSchema)]
pub struct UploadAccepted {
    /// The info dict's primary hash, as 40 hex characters.
    pub info_hash: String,
    /// The info dict says `private=1` (BEP 27): stored as private, so it
    /// never reaches the DHT, a tracker or a cache.
    pub private: bool,
}

pub fn routes() -> Router<ApiState> {
    auth::protected(Router::new().route("/torrents/upload", post(upload)))
}

/// Adds one `.torrent` file, sent as the raw request body.
#[utoipa::path(
    operation_id = "uploadTorrent",
    post,
    path = "/torrents/upload",
    tag = "torrents",
    request_body(content = TorrentBody, content_type = "application/x-bittorrent", description = "The `.torrent` file itself, at most 8 MiB. Nothing else: no multipart, no file name."),
    responses(
        (status = 202, body = UploadAccepted, description = "Judged and handed to the store. The store judges it again when it commits, should the rules have changed meanwhile, and may then still hold it."),
        (status = 400, body = ErrorEnvelope, description = "`malformed_torrent`: not a `.torrent` within the limits; or `bad_body`: the body couldn't be read."),
        (status = 408, body = ErrorEnvelope, description = "`body_timeout`: the body didn't arrive within 60 s."),
        (status = 403, body = ErrorEnvelope, description = "`cross_origin`: sent from another site."),
        (status = 413, body = ErrorEnvelope, description = "`body_too_large`: over 8 MiB."),
        (status = 415, body = ErrorEnvelope, description = "`unsupported_media_type`: the body isn't sent as `application/x-bittorrent`."),
        (status = 422, body = ErrorEnvelope, description = "`refused`: on the safety floor's list; or `held`: the filter holds it. Neither is stored."),
        (status = 429, body = ErrorEnvelope, description = "`rate_limited`: too many uploads; wait for `Retry-After`."),
        (status = 503, body = ErrorEnvelope, description = "`uploads_unavailable`: uploads aren't being taken right now."),
    )
)]
pub(crate) async fn upload(
    ClientIp(ip): ClientIp,
    uploader: Option<Extension<Uploader>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match take(ip, uploader, &headers, body).await {
        Ok(r) => r,
        Err(r) => *r,
    }
}

/// A refusal, boxed: a `Response` is too large for an `Err`.
type Refusal = Box<Response>;

fn refuse(r: impl IntoResponse) -> Refusal {
    Box::new(r.into_response())
}

async fn take(
    ip: IpAddr,
    uploader: Option<Extension<Uploader>>,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, Refusal> {
    if !bittorrent_type(headers) {
        return Err(refuse(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Send the file itself as the body, as application/x-bittorrent.",
        )));
    }
    same_origin(headers).map_err(refuse)?;
    let Some(Extension(up)) = uploader else {
        return Err(refuse(unavailable()));
    };
    up.limiter
        .take(ip, Instant::now())
        .map_err(|wait| refuse(limited_response(wait)))?;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BYTES as u64) {
        return Err(refuse(too_large()));
    }
    // Unread until it's this request's turn: waiting holds no body.
    let Ok(_permit) = up.in_flight.acquire().await else {
        return Err(refuse(unavailable()));
    };
    let bytes = tokio::time::timeout(READ_DEADLINE, read_limited(body, declared))
        .await
        .map_err(|_| {
            refuse(ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "body_timeout",
                "The file took too long to arrive; try again.",
            ))
        })??;
    let outcome = up
        .sink
        .submit(bytes)
        .await
        .map_err(|Unavailable| refuse(unavailable()))?;
    Ok(match outcome {
        Outcome::Accepted { info_hash, private } => (
            StatusCode::ACCEPTED,
            axum::Json(UploadAccepted {
                info_hash: crate::torrents::hex(&info_hash),
                private,
            }),
        )
            .into_response(),
        Outcome::Malformed => ApiError::bad_request(
            "malformed_torrent",
            "That file isn't a .torrent Lodestar can read.",
        )
        .into_response(),
        Outcome::Refused => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "refused",
            "Lodestar's safety floor refuses this torrent, so it wasn't added.",
        )
        .into_response(),
        Outcome::Held => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "held",
            "Your filter holds this torrent, so it wasn't added.",
        )
        .into_response(),
    })
}

/// `application/x-bittorrent` and nothing else: no parameters.
fn bittorrent_type(headers: &HeaderMap) -> bool {
    let mut all = headers.get_all(header::CONTENT_TYPE).iter();
    match (all.next(), all.next()) {
        (Some(v), None) => v
            .to_str()
            .is_ok_and(|t| t.trim().eq_ignore_ascii_case(CONTENT_TYPE)),
        _ => false,
    }
}

/// The same-origin rule JSON writes rely on: an `Origin` whose host isn't
/// `Host` is refused, exactly as `extract::same_origin_json` compares
/// them; and, stricter, so is `Sec-Fetch-Site: cross-site`. A request
/// with neither is let through, as a JSON write is:
/// `application/x-bittorrent` isn't a CORS-safelisted type, so another
/// site's page can't send it without a preflight the server never
/// answers, and an HTML form can't send it at all.
pub(crate) fn same_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let cross_site = headers.get_all("sec-fetch-site").iter().any(|v| {
        v.to_str()
            .map_or(true, |v| v.trim().eq_ignore_ascii_case("cross-site"))
    });
    if cross_site || !origin_matches_host(headers) {
        return Err(cross_origin());
    }
    Ok(())
}

/// `same_origin_json`'s Origin–Host comparison, as it is in `extract.rs`.
fn origin_matches_host(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let origin_host = origin.split_once("://").map_or(origin, |(_, h)| h);
    origin_host == host
}

fn cross_origin() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "cross_origin",
        "Cross-origin requests aren't allowed.",
    )
}

fn too_large() -> Response {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "body_too_large",
        "That file is over 8 MiB, the most Lodestar takes.",
    )
    .into_response()
}

fn unavailable() -> Response {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "uploads_unavailable",
        "Lodestar isn't taking uploads right now. Try again once it's running.",
    )
    .into_response()
}

fn limited_response(wait: Duration) -> Response {
    let mut resp = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "Too many uploads; wait and try again.",
    )
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

/// Reads the body, refusing it as soon as it passes [`MAX_BYTES`]
/// (whatever its `Content-Length` said).
async fn read_limited(mut body: Body, declared: Option<u64>) -> Result<Vec<u8>, Refusal> {
    use axum::body::HttpBody;
    // `declared` is at most the cap here, so this reserves no more.
    let mut buf = Vec::with_capacity(declared.map_or(0, |n| n as usize).min(MAX_BYTES));
    loop {
        let frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await;
        match frame {
            None => return Ok(buf),
            Some(Err(_)) => {
                return Err(refuse(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad_body",
                    "The request body couldn't be read.",
                )));
            }
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    if buf.len() + data.len() > MAX_BYTES {
                        return Err(refuse(too_large()));
                    }
                    buf.extend_from_slice(&data);
                }
            }
        }
    }
}

/// Token buckets per client address: `burst` tokens, regaining
/// `per_sec` a second, at most `max` addresses. When the map is full, a
/// bucket that has fully refilled is forgotten (it's the same as a new
/// one, so nothing is reset); if it's still full, an address with no
/// bucket is refused until one refills. Existing buckets are never
/// evicted to make room.
#[derive(Debug)]
pub(crate) struct Limiter {
    map: Mutex<HashMap<IpAddr, Bucket>>,
    burst: f64,
    per_sec: f64,
    max: usize,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Limiter {
    pub(crate) fn new(burst: f64, per_sec: f64, max: usize) -> Limiter {
        Limiter {
            map: Mutex::new(HashMap::new()),
            burst,
            per_sec,
            max,
        }
    }

    fn refilled(&self, b: &Bucket, now: Instant) -> f64 {
        (b.tokens + now.saturating_duration_since(b.at).as_secs_f64() * self.per_sec)
            .min(self.burst)
    }

    /// Until `tokens` is back to one, in whole seconds (at least one).
    fn wait(&self, tokens: f64) -> Duration {
        if self.per_sec <= 0.0 {
            return Duration::from_secs(3600);
        }
        Duration::from_secs(((1.0 - tokens) / self.per_sec).ceil().clamp(1.0, 3600.0) as u64)
    }

    /// Takes a token for `ip`, or says how long until one is back.
    pub(crate) fn take(&self, ip: IpAddr, now: Instant) -> Result<(), Duration> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if !map.contains_key(&ip) && map.len() >= self.max {
            map.retain(|_, b| self.refilled(b, now) < self.burst);
            if map.len() >= self.max {
                // The soonest any tracked address refills completely.
                let soonest = map
                    .values()
                    .map(|b| self.refilled(b, now))
                    .fold(0.0_f64, f64::max);
                let wait = if self.per_sec <= 0.0 {
                    Duration::from_secs(3600)
                } else {
                    let secs = ((self.burst - soonest) / self.per_sec).ceil();
                    Duration::from_secs(secs.clamp(1.0, 3600.0) as u64)
                };
                return Err(wait);
            }
        }
        let b = map.entry(ip).or_insert(Bucket {
            tokens: self.burst,
            at: now,
        });
        b.tokens = self.refilled(b, now);
        b.at = now;
        if b.tokens < 1.0 {
            return Err(self.wait(b.tokens));
        }
        b.tokens -= 1.0;
        Ok(())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u32) -> IpAddr {
        IpAddr::from(n.to_be_bytes())
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn only_the_bittorrent_type_is_taken() {
        let ok = |t: &[&str]| {
            let pairs: Vec<_> = t.iter().map(|t| ("content-type", *t)).collect();
            bittorrent_type(&headers(&pairs))
        };
        assert!(ok(&["application/x-bittorrent"]));
        assert!(ok(&["Application/X-BitTorrent"]));
        assert!(ok(&[" application/x-bittorrent "]));
        for bad in [
            &[][..],
            &["application/x-bittorrent; charset=utf-8"],
            &["application/x-bittorrent;"],
            &["multipart/form-data; boundary=x"],
            &["application/octet-stream"],
            &["application/json"],
            &["text/plain"],
            &["application/x-www-form-urlencoded"],
            &["application/x-bittorrentx"],
            &["application/x-bittorrent", "application/x-bittorrent"],
        ] {
            assert!(!ok(bad), "{bad:?}");
        }
    }

    /// The Origin–Host comparison is repeated from `extract.rs`: both are
    /// run over the same cases and must agree, apart from the stricter
    /// `Sec-Fetch-Site: cross-site` refusal (runner, 2026-11-12).
    #[test]
    fn the_origin_rule_agrees_with_json_writes() {
        let cases: &[&[(&str, &str)]] = &[
            &[],
            &[("host", "lodestar.lan:8090")],
            &[
                ("origin", "http://lodestar.lan:8090"),
                ("host", "lodestar.lan:8090"),
            ],
            &[
                ("origin", "https://lodestar.lan:8090"),
                ("host", "lodestar.lan:8090"),
            ],
            &[
                ("origin", "http://evil.example"),
                ("host", "lodestar.lan:8090"),
            ],
            &[
                ("origin", "http://lodestar.lan"),
                ("host", "lodestar.lan:8090"),
            ],
            &[("origin", "null"), ("host", "lodestar.lan:8090")],
            &[("origin", "http://lodestar.lan:8090")],
            &[
                ("origin", "lodestar.lan:8090"),
                ("host", "lodestar.lan:8090"),
            ],
            &[
                ("origin", "http://LODESTAR.lan:8090"),
                ("host", "lodestar.lan:8090"),
            ],
            &[
                ("origin", "http://lodestar.lan:8090/"),
                ("host", "lodestar.lan:8090"),
            ],
            &[("origin", ""), ("host", "")],
        ];
        for fetch_site in [
            None,
            Some("same-origin"),
            Some("none"),
            Some("same-site"),
            Some("cross-site"),
            Some("CROSS-SITE"),
        ] {
            for case in cases {
                let mut pairs: Vec<(&str, &str)> = case.to_vec();
                if let Some(s) = fetch_site {
                    pairs.push(("sec-fetch-site", s));
                }
                let mine = same_origin(&headers(&pairs)).is_ok();
                pairs.push(("content-type", "application/json"));
                let json = crate::extract::same_origin_json(&headers(&pairs)).is_ok();
                let stricter = fetch_site.is_some_and(|s| s.eq_ignore_ascii_case("cross-site"));
                assert_eq!(mine, json && !stricter, "{pairs:?}");
            }
        }
    }

    #[test]
    fn a_bucket_empties_and_refills() {
        let l = Limiter::new(3.0, 1.0, 10);
        let t = Instant::now();
        for _ in 0..3 {
            assert!(l.take(ip(1), t).is_ok());
        }
        assert_eq!(l.take(ip(1), t), Err(Duration::from_secs(1)));
        assert!(l.take(ip(2), t).is_ok(), "addresses are separate");
        assert!(l.take(ip(1), t + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn a_full_table_refuses_new_addresses_and_resets_none() {
        let l = Limiter::new(2.0, 0.5, 3);
        let t = Instant::now();
        for n in 1..=3 {
            l.take(ip(n), t).unwrap();
            l.take(ip(n), t).unwrap();
        }
        let t2 = t + Duration::from_secs(2);
        l.take(ip(1), t2).unwrap();
        // Full, no bucket refilled: a newcomer is refused, nobody evicted.
        assert!(l.take(ip(9), t2).is_err());
        assert_eq!(l.len(), 3);
        // ip 1 is still empty: its bucket wasn't reset to make room.
        assert!(l.take(ip(1), t2).is_err());
        // Buckets that have fully refilled are forgotten, which resets
        // nothing; ip 1's, not yet full, is kept.
        let t4 = t + Duration::from_secs(4);
        assert!(l.take(ip(9), t4).is_ok());
        assert_eq!(l.len(), 2);
        assert!(l.take(ip(1), t4).is_ok());
        assert!(l.take(ip(1), t4).is_err(), "ip 1 kept its own count");
    }

    #[test]
    fn a_frozen_bucket_waits_an_hour() {
        let l = Limiter::new(1.0, 0.0, 10);
        let t = Instant::now();
        l.take(ip(1), t).unwrap();
        assert_eq!(l.take(ip(1), t), Err(Duration::from_secs(3600)));
    }
}
