//! `POST /send` (§9, §10.10.8): torrents to a saved download client, one
//! or many, with a result for each.
//!
//! - **Only what you can see is sent.** A hash this install doesn't have,
//!   one a filter rule hides, and an Adult one while "Show adult content"
//!   is off are all `not_found`, alike: a withheld torrent's presence
//!   isn't revealed, as in detail, Compare and export (§20.9). A
//!   quarantined or tombstoned torrent isn't stored at all.
//! - **Never a private torrent** (BEP 27, or found only through a private
//!   source, §18.7): `private`. A magnet with no trackers would make the
//!   client look the hash up on the public DHT, leaking it.
//! - **The magnet is built from the stored torrent,** never from the
//!   request: `btih` with its v1 hash, or `btmh` for a v2-only torrent,
//!   and its name cut to [`MAX_NAME`] bytes so the link always fits. A
//!   hash naming a torrent already in the send (a repeat, or a v1 hash and
//!   the same torrent's truncated v2 hash) is `same_torrent`.
//! - **Duplicates and partial sends** follow the adapters (§9): when the
//!   client already has every torrent of the send, each is `duplicate`
//!   (counted in `sent`: they're where you want them); when it has only
//!   some, the adapter can't say which, so all are `sent`. A bulk send
//!   that fails part-way reports the first `sent` as sent and the rest as
//!   failed, so nothing is retried twice. qBittorrent answers a duplicate
//!   and an invalid link alike, so a lone duplicate there is `failed`.
//! - **A send that runs out of time** ([`SEND_TIMEOUT`]) can't say how far
//!   it got: every torrent is `failed` with `timeout_unknown`, telling you
//!   to check the client before sending again.
//! - **Rate-limited:** two sends at a time and [`MAX_SENDS`] a minute;
//!   past either, `429` and nothing is sent.
//! - Results never repeat the client's address, a credential or its raw
//!   answer.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use lodestar_integrations::{AddOptions, ClientError, Magnet};
use lodestar_store::flags;

use super::{
    DownloadClientDraft, DownloadClientKind, SavedClient, blocking, client_config, settings,
    test_failure,
};
use crate::ApiState;
use crate::dto::error::ErrorEnvelope;
use crate::dto::settings::{ClientSend, ClientSendItem, ClientSendOutcome, ClientSendResult};
use crate::error::{ApiError, ApiResult, internal};
use crate::extract::{Json as JsonBody, same_origin_json};
use crate::torrents::percent_encode;

/// The most torrents one send carries.
pub const MAX_HASHES: usize = 100;
/// Sends allowed in [`WINDOW`].
pub const MAX_SENDS: u32 = 30;
const WINDOW: Duration = Duration::from_secs(60);
/// The most bytes of a torrent's name a magnet's `dn` carries. Encoded,
/// that's at most three times as long, well inside [`Magnet::MAX_LEN`].
pub const MAX_NAME: usize = 1024;
/// The longest a send may take, every request to the client included.
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

/// Sends at once, past which a send is refused with `sends_busy`.
const AT_ONCE: usize = 2;

/// The limits on sends to one install's download clients: [`AT_ONCE`] at
/// a time and [`MAX_SENDS`] a minute. The registry owns one
/// ([`super::SettingsApi::send_limits`]), so each server, and each test's router,
/// has its own.
#[derive(Debug)]
pub struct SendLimits {
    running: tokio::sync::Semaphore,
    rate: Mutex<Option<Window>>,
}

impl SendLimits {
    pub const fn new() -> SendLimits {
        SendLimits {
            running: tokio::sync::Semaphore::const_new(AT_ONCE),
            rate: Mutex::new(None),
        }
    }
}

impl Default for SendLimits {
    fn default() -> Self {
        SendLimits::new()
    }
}

/// The limits of a [`SettingsApi`] that keeps none of its own.
pub(crate) static SHARED_LIMITS: SendLimits = SendLimits::new();

/// A fixed window: at most `max` sends from `start` for [`WINDOW`].
#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

/// Counts a send at `now`; false when the window is full.
fn allow(slot: &mut Option<Window>, now: Instant, max: u32) -> bool {
    let w = match slot {
        Some(w) if now.duration_since(w.start) < WINDOW => w,
        _ => slot.insert(Window {
            start: now,
            count: 0,
        }),
    };
    if w.count >= max {
        return false;
    }
    w.count += 1;
    true
}

fn busy(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, code, message)
}

/// Sends torrents to a saved download client (the default one unless
/// `client` names another), with its saved category, save path and
/// paused setting. Each hash gets a result; hidden ones are refused, never
/// sent.
#[utoipa::path(
    operation_id = "sendToClient",
    post,
    path = "/send",
    tag = "settings",
    request_body = ClientSend,
    responses(
        (status = 200, body = ClientSendResult, description = "The send ran; each hash has its result."),
        (status = 403, body = ErrorEnvelope, description = "`cross_origin`."),
        (status = 404, body = ErrorEnvelope, description = "`no_client`: no download client is saved, or none with that id."),
        (status = 422, body = ErrorEnvelope, description = "`invalid_hashes`: none, more than 100, or one isn't an info hash."),
        (status = 429, body = ErrorEnvelope, description = "`sends_busy` or `send_rate_limited`: try again shortly."),
    )
)]
pub(crate) async fn send(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<ClientSend>,
) -> ApiResult<Json<ClientSendResult>> {
    same_origin_json(&headers)?;
    let hashes = parse_hashes(&body.hashes)?;
    let api = settings(&s)?;
    let api_limits = api.clone();
    let id = body.client.clone();
    let client = blocking(move || api.download_client(id.as_deref()))
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "no_client",
                "No download client with that id is saved; add one in Settings → Integrations.",
            )
        })?;
    let limits = api_limits.send_limits();
    let _permit = limits.running.try_acquire().map_err(|_| {
        busy(
            "sends_busy",
            "Other sends are still running; try again in a few seconds.",
        )
    })?;
    if !allow(
        &mut limits.rate.lock().expect("rate lock"),
        Instant::now(),
        MAX_SENDS,
    ) {
        return Err(busy(
            "send_rate_limited",
            "Too many sends this minute; try again shortly.",
        ));
    }
    let reader = s.reader.clone();
    let adult_shown = s.adult.shown();
    let checked = blocking(move || {
        let store = reader.lock().expect("reader lock");
        let mut ids = Vec::new();
        hashes
            .into_iter()
            .map(|h| {
                let v = visible(&store, &h, adult_shown)?;
                // A v1 hash and the same torrent's v2 hash: sent once.
                let v = match v {
                    Ok(t) if ids.contains(&t.id) => Err("same_torrent"),
                    Ok(t) => {
                        ids.push(t.id);
                        Ok(t)
                    }
                    Err(code) => Err(code),
                };
                Ok((h, v))
            })
            .collect::<ApiResult<Vec<_>>>()
    })
    .await??;
    Ok(Json(deliver(client, checked).await))
}

/// The request's hashes, lowercased, in order (a repeat is
/// `same_torrent` later).
fn parse_hashes(raw: &[String]) -> ApiResult<Vec<String>> {
    let bad =
        |m: &'static str| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_hashes", m);
    if raw.is_empty() || raw.len() > MAX_HASHES {
        return Err(bad("Send 1 to 100 info hashes."));
    }
    raw.iter()
        .map(|h| {
            let h = h.to_ascii_lowercase();
            if h.len() != 40 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad("Each hash must be 40 hexadecimal characters."));
            }
            Ok(h)
        })
        .collect()
}

/// Whether a hash may be sent: `Ok(name)` when it may, or the refusal's
/// code.
fn visible(
    store: &lodestar_store::Store,
    hash: &str,
    adult_shown: bool,
) -> ApiResult<Result<Sendable, &'static str>> {
    let h = crate::user_items::hash_of(hash)?;
    let Some(id) = store.find(&h).map_err(internal)? else {
        return Ok(Err("not_found"));
    };
    let Some(row) = store.torrent(id).map_err(internal)? else {
        return Ok(Err("not_found"));
    };
    // Withheld looks exactly like unknown.
    let e = store.enrichment(id).map_err(internal)?;
    if crate::discover::withheld(&row, e.as_ref(), false, adult_shown) {
        return Ok(Err("not_found"));
    }
    if row.flags & (flags::PRIVATE | flags::PRIVATE_SOURCE) != 0 {
        return Ok(Err("private"));
    }
    let Some(xt) = topic(row.flags, &row.info_hash.0, row.info_hash_v2.map(|h| h.0)) else {
        return Ok(Err("invalid"));
    };
    Ok(Ok(Sendable {
        id,
        xt,
        name: row.name,
    }))
}

/// A magnet's exact topic for a stored torrent: `btih:<v1 hash>`, or
/// `btmh:1220<v2 hash>` for a v2-only torrent (its stored hash is the
/// truncated v2 hash, which no client can use as a v1 hash). `None` for a
/// v2-only torrent with no v2 hash stored: no link would resolve.
fn topic(flags: i64, v1: &[u8; 20], v2: Option<[u8; 32]>) -> Option<String> {
    let v2_only = flags & flags::V2 != 0 && flags & flags::HYBRID == 0;
    match (v2_only, v2) {
        (true, Some(v2)) => Some(format!("btmh:1220{}", crate::torrents::hex(&v2))),
        (true, None) => None,
        (false, _) => Some(format!("btih:{}", crate::torrents::hex(v1))),
    }
}

/// A torrent that may be sent, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sendable {
    id: i64,
    /// The magnet's exact topic: `btih:<v1 hex>`, or `btmh:1220<v2 hex>`
    /// for a v2-only torrent.
    xt: String,
    name: String,
}

impl Sendable {
    /// Its magnet: the stored topic and the name, cut to [`MAX_NAME`]
    /// bytes on a character boundary.
    fn magnet(&self) -> Option<Magnet> {
        let mut end = self.name.len().min(MAX_NAME);
        while !self.name.is_char_boundary(end) {
            end -= 1;
        }
        let uri = format!(
            "magnet:?xt=urn:{}&dn={}",
            self.xt,
            percent_encode(&self.name[..end])
        );
        Magnet::parse(&uri).ok()
    }
}

fn item(
    hash: String,
    outcome: ClientSendOutcome,
    code: Option<&str>,
    message: Option<String>,
) -> ClientSendItem {
    ClientSendItem {
        code: code.map(str::to_string),
        hash,
        message,
        outcome,
    }
}

fn refusal(code: &str) -> String {
    match code {
        "private" => {
            "This torrent is private: it's sent only by its own tracker, never as a magnet link."
        }
        "same_torrent" => "Another hash in this send names the same torrent.",
        "invalid" => "Lodestar couldn't make a magnet link for this torrent.",
        _ => "There's no torrent with that hash here.",
    }
    .to_string()
}

/// Why a send failed, in a send's words: the connection test's codes, but
/// a refusal is the torrent's, and a timeout can't say how far it got.
fn send_failure(e: &ClientError, kind: DownloadClientKind) -> (String, String) {
    let f = test_failure(e).error.expect("a failure");
    match e {
        // An HTTP status isn't the client refusing the torrent: the
        // address isn't its API, and the message says so.
        ClientError::Rejected(why) if super::http_status(why).is_some() => (f.code, f.message),
        // qBittorrent answers a duplicate and an invalid link alike.
        ClientError::Rejected(_) if kind == DownloadClientKind::Qbittorrent => (
            f.code,
            "The client refused the torrent; it may already be in qBittorrent.".into(),
        ),
        ClientError::Timeout => (
            "timeout_unknown".into(),
            "The client didn't answer in time, so some torrents may have gone in. Check the client before sending again.".into(),
        ),
        ClientError::Rejected(_) => (f.code, "The client refused the torrent.".into()),
        _ => (f.code, f.message),
    }
}

/// Sends the visible torrents and puts every hash's result in order.
async fn deliver(
    client: SavedClient,
    checked: Vec<(String, Result<Sendable, &'static str>)>,
) -> ClientSendResult {
    let id = client.id.clone();
    let kind = client.kind;
    let opts = AddOptions {
        save_path: (!client.save_path.is_empty()).then(|| client.save_path.clone()),
        category: (!client.category.is_empty()).then(|| client.category.clone()),
        paused: client.paused,
        selected_files: None,
    };
    // Each sendable torrent's magnet, or `invalid` in its place, so the
    // results stay aligned with what the client was sent.
    let checked: Vec<(String, Result<Magnet, &'static str>)> = checked
        .into_iter()
        .map(|(h, v)| (h, v.and_then(|t| t.magnet().ok_or("invalid"))))
        .collect();
    let magnets: Vec<Magnet> = checked
        .iter()
        .filter_map(|(_, v)| v.as_ref().ok().cloned())
        .collect();
    let outcome = if magnets.is_empty() {
        None
    } else {
        let draft = DownloadClientDraft {
            client: client.kind,
            id: None,
            password: client.password.clone(),
            url: client.url.clone(),
            username: Some(client.username.clone()).filter(|u| !u.is_empty()),
        };
        let run = async {
            let c = lodestar_integrations::clients::connect(client_config(draft))?;
            c.add_magnets(&magnets, &opts).await
        };
        Some(
            tokio::time::timeout(SEND_TIMEOUT, run)
                .await
                .unwrap_or(Err(ClientError::Timeout)),
        )
    };
    let (results, sent) = results(checked, outcome, kind);
    ClientSendResult {
        client: id,
        results,
        sent,
    }
}

/// Every hash's result, in order, from the send's outcome (`None` when
/// nothing was sendable), and how many went in. A bulk send that fails
/// part-way took the first `sent` of the sendable ones, in order; one
/// that found every torrent already there took them all as duplicates.
fn results<T>(
    checked: Vec<(String, Result<T, &'static str>)>,
    outcome: Option<Result<lodestar_integrations::AddOutcome, ClientError>>,
    kind: DownloadClientKind,
) -> (Vec<ClientSendItem>, usize) {
    let sendable = checked.iter().filter(|(_, v)| v.is_ok()).count();
    let (went_in, duplicate, cause) = match outcome {
        None => (0, false, None),
        Some(Ok(_)) => (sendable, false, None),
        Some(Err(ClientError::Partial { sent, cause })) => {
            (sent.min(sendable), false, Some(*cause))
        }
        Some(Err(e)) if lodestar_integrations::clients::is_already_there(&e) => {
            (sendable, true, None)
        }
        Some(Err(e)) => (0, false, Some(e)),
    };
    let failure = cause.map(|e| send_failure(&e, kind));
    let mut out = Vec::with_capacity(checked.len());
    let mut n = 0;
    for (hash, v) in checked {
        let r = match v {
            Err(code) => item(
                hash,
                ClientSendOutcome::Refused,
                Some(code),
                Some(refusal(code)),
            ),
            Ok(_) => {
                n += 1;
                if n <= went_in {
                    let outcome = if duplicate {
                        ClientSendOutcome::Duplicate
                    } else {
                        ClientSendOutcome::Sent
                    };
                    item(hash, outcome, None, None)
                } else {
                    item(
                        hash,
                        ClientSendOutcome::Failed,
                        failure.as_ref().map(|f| f.0.as_str()),
                        failure.as_ref().map(|f| f.1.clone()),
                    )
                }
            }
        };
        out.push(r);
    }
    (out, went_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_allows_a_fixed_number_a_minute() {
        let mut slot = None;
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(allow(&mut slot, t0, 3));
        }
        assert!(!allow(&mut slot, t0 + Duration::from_secs(59), 3));
        assert!(allow(&mut slot, t0 + WINDOW, 3), "a new window");
    }

    const TR: DownloadClientKind = DownloadClientKind::Transmission;

    fn checked() -> Vec<(String, Result<String, &'static str>)> {
        vec![
            ("a".into(), Ok("A".into())),
            ("h".into(), Err("not_found")),
            ("b".into(), Ok("B".into())),
            ("c".into(), Ok("C".into())),
        ]
    }

    fn outcomes(r: &[ClientSendItem]) -> Vec<(ClientSendOutcome, Option<&str>)> {
        r.iter().map(|i| (i.outcome, i.code.as_deref())).collect()
    }

    #[test]
    fn a_partial_send_counts_the_first_ones_and_fails_the_rest() {
        let partial = ClientError::Partial {
            sent: 1,
            cause: Box::new(ClientError::Unreachable),
        };
        let (r, sent) = results(checked(), Some(Err(partial)), TR);
        assert_eq!(sent, 1);
        use ClientSendOutcome::*;
        assert_eq!(
            outcomes(&r),
            [
                (Sent, None),
                (Refused, Some("not_found")),
                (Failed, Some("unreachable")),
                (Failed, Some("unreachable"))
            ]
        );
    }

    /// An HTTP status keeps its own message for every client, qBittorrent
    /// included; only a refusal without one may be a duplicate.
    #[test]
    fn an_http_status_isnt_read_as_a_refused_torrent() {
        const QB: DownloadClientKind = DownloadClientKind::Qbittorrent;
        let http = ClientError::Rejected("HTTP 500: Internal Server Error".into());
        for kind in [QB, TR] {
            let (code, message) = send_failure(&http, kind);
            assert_eq!(code, "rejected");
            assert_eq!(
                message, "The address answered HTTP 500: it isn't this client's API.",
                "{kind:?}"
            );
        }
        let refused = ClientError::Rejected("Fails.".into());
        assert!(
            send_failure(&refused, QB)
                .1
                .contains("may already be in qBittorrent")
        );
        assert_eq!(
            send_failure(&refused, TR).1,
            "The client refused the torrent."
        );
    }

    #[test]
    fn a_send_that_runs_out_of_time_says_some_may_have_gone_in() {
        let (r, sent) = results(checked(), Some(Err(ClientError::Timeout)), TR);
        assert_eq!(sent, 0);
        assert_eq!(r[0].code.as_deref(), Some("timeout_unknown"));
        assert!(r[0].message.as_ref().unwrap().contains("Check the client"));
    }

    #[test]
    fn a_topic_is_v1_v2_or_none() {
        let (v1, v2) = ([0xaa; 20], [0xbb; 32]);
        assert_eq!(
            topic(0, &v1, None),
            Some(format!("btih:{}", "aa".repeat(20)))
        );
        let hybrid = flags::V2 | flags::HYBRID;
        assert_eq!(
            topic(hybrid, &v1, Some(v2)),
            Some(format!("btih:{}", "aa".repeat(20)))
        );
        assert_eq!(
            topic(flags::V2, &v1, Some(v2)),
            Some(format!("btmh:1220{}", "bb".repeat(32)))
        );
        // v2-only with no v2 hash stored: refused as `invalid`, never a
        // `btih` link made of the truncated hash.
        assert_eq!(topic(flags::V2, &v1, None), None);
    }

    #[test]
    fn magnets_fit_whatever_the_name_and_name_v2_only_torrents_by_btmh() {
        // A long name of characters that encode to three bytes each, then
        // three again when percent-encoded: cut, never dropped.
        let t = Sendable {
            id: 1,
            xt: format!("btih:{}", "a".repeat(40)),
            name: "é".repeat(4096),
        };
        let m = t.magnet().expect("it fits");
        assert!(m.as_str().len() <= Magnet::MAX_LEN);
        let v2 = Sendable {
            id: 2,
            xt: format!("btmh:1220{}", "b".repeat(64)),
            name: "x".into(),
        };
        assert!(v2.magnet().unwrap().as_str().contains("urn:btmh:1220"));
    }

    #[test]
    fn duplicates_count_as_sent_and_failures_say_why() {
        use ClientSendOutcome::*;
        let (r, sent) = results(checked(), Some(Err(ClientError::AlreadyThere)), TR);
        assert_eq!(sent, 3);
        assert_eq!(outcomes(&r)[0], (Duplicate, None));
        // A refusal that only reads like one is a refusal.
        let lookalike = ClientError::Rejected("the torrent is already in the client".into());
        let (r, sent) = results(checked(), Some(Err(lookalike.clone())), TR);
        assert_eq!((sent, outcomes(&r)[0]), (0, (Failed, Some("rejected"))));
        // qBittorrent can't tell a duplicate from a bad link: it says so.
        let (r, _) = results(
            checked(),
            Some(Err(lookalike)),
            DownloadClientKind::Qbittorrent,
        );
        assert!(
            r[0].message
                .as_ref()
                .unwrap()
                .contains("may already be in qBittorrent")
        );
        let (r, sent) = results(
            checked(),
            Some(Err(ClientError::Auth(
                lodestar_integrations::AuthFailure::BadCredentials,
            ))),
            TR,
        );
        assert_eq!(sent, 0);
        assert_eq!(outcomes(&r)[2], (Failed, Some("bad_credentials")));
        assert!(r[2].message.as_ref().unwrap().ends_with('.'));
        // A hidden one is never in the send.
        let (r, _) = results::<()>(vec![("h".into(), Err("not_found"))], None, TR);
        assert_eq!(outcomes(&r), [(Refused, Some("not_found"))]);
    }

    #[test]
    fn hashes_are_checked_and_kept_in_order() {
        let a = "A".repeat(40);
        let out = parse_hashes(&[a.clone(), a.to_lowercase()]).unwrap();
        assert_eq!(out, vec!["a".repeat(40); 2], "a repeat gets its own result");
        assert!(parse_hashes(&[]).is_err());
        assert!(parse_hashes(&["xyz".into()]).is_err());
        assert!(parse_hashes(&vec!["a".repeat(40); MAX_HASHES + 1]).is_err());
    }
}
