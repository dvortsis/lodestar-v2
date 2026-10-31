//! `POST /send` (§9, §10.10.8): torrents to a saved download client, one
//! or many, with a result for each.
//!
//! - **Only what you can see is sent.** A hash this install doesn't have
//!   is `not_found`; one a filter rule hides is `hidden`; an Adult
//!   torrent while "Show adult content" is off is `adult_hidden`, so a
//!   pasted hash doesn't get round the preference (§20.9). A quarantined
//!   or tombstoned torrent isn't stored, so it's `not_found`.
//! - **Duplicates and partial sends** follow the adapters (§9): a torrent
//!   already in the client counts as sent in a bulk send, and a lone one
//!   is `duplicate`; a bulk send that fails part-way reports the first
//!   `sent` as sent and the rest as failed, so nothing is retried twice.
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

use super::{DownloadClientDraft, SavedClient, blocking, client_config, settings, test_failure};
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
/// The longest a send may take, every request to the client included.
const SEND_TIMEOUT: Duration = Duration::from_secs(120);

static RUNNING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static RATE: Mutex<Option<Window>> = Mutex::new(None);

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
    let _permit = RUNNING.try_acquire().map_err(|_| {
        busy(
            "sends_busy",
            "Other sends are still running; try again in a few seconds.",
        )
    })?;
    if !allow(
        &mut RATE.lock().expect("rate lock"),
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
        hashes
            .into_iter()
            .map(|h| visible(&store, &h, adult_shown).map(|v| (h, v)))
            .collect::<ApiResult<Vec<_>>>()
    })
    .await??;
    Ok(Json(deliver(client, checked).await))
}

/// The request's hashes, lowercased, once each, in order.
fn parse_hashes(raw: &[String]) -> ApiResult<Vec<String>> {
    let bad =
        |m: &'static str| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_hashes", m);
    if raw.is_empty() || raw.len() > MAX_HASHES {
        return Err(bad("Send 1 to 100 info hashes."));
    }
    let mut out: Vec<String> = Vec::new();
    for h in raw {
        let h = h.to_ascii_lowercase();
        if h.len() != 40 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("Each hash must be 40 hexadecimal characters."));
        }
        if !out.contains(&h) {
            out.push(h);
        }
    }
    Ok(out)
}

/// Whether a hash may be sent: `Ok(name)` when it may, or the refusal's
/// code.
fn visible(
    store: &lodestar_store::Store,
    hash: &str,
    adult_shown: bool,
) -> ApiResult<Result<String, &'static str>> {
    let h = crate::user_items::hash_of(hash)?;
    let Some(id) = store.find(&h).map_err(internal)? else {
        return Ok(Err("not_found"));
    };
    let Some(row) = store.torrent(id).map_err(internal)? else {
        return Ok(Err("not_found"));
    };
    if row.flags & flags::HIDDEN != 0 {
        return Ok(Err("hidden"));
    }
    if !adult_shown
        && store
            .enrichment(id)
            .map_err(internal)?
            .is_some_and(|e| crate::search::is_adult(&e))
    {
        return Ok(Err("adult_hidden"));
    }
    Ok(Ok(row.name))
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
        "hidden" => "A filter rule hides this torrent, so it isn't sent.",
        "adult_hidden" => {
            "This torrent is Adult, which is hidden: turn on Show adult content to send it."
        }
        _ => "There's no torrent with that hash here.",
    }
    .to_string()
}

/// Sends the visible torrents and puts every hash's result in order.
async fn deliver(
    client: SavedClient,
    checked: Vec<(String, Result<String, &'static str>)>,
) -> ClientSendResult {
    let id = client.id.clone();
    let opts = AddOptions {
        save_path: (!client.save_path.is_empty()).then(|| client.save_path.clone()),
        category: (!client.category.is_empty()).then(|| client.category.clone()),
        paused: client.paused,
        selected_files: None,
    };
    let mut magnets = Vec::new();
    for (hash, v) in &checked {
        if let Ok(name) = v {
            let uri = format!("magnet:?xt=urn:btih:{hash}&dn={}", percent_encode(name));
            // Built from a checked hash: it always parses.
            if let Ok(m) = Magnet::parse(&uri) {
                magnets.push((hash.clone(), m));
            }
        }
    }
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
        let list: Vec<Magnet> = magnets.iter().map(|(_, m)| m.clone()).collect();
        let run = async {
            let c = lodestar_integrations::clients::connect(client_config(draft))?;
            c.add_magnets(&list, &opts).await
        };
        Some(
            tokio::time::timeout(SEND_TIMEOUT, run)
                .await
                .unwrap_or(Err(ClientError::Timeout)),
        )
    };
    let (results, sent) = results(checked, outcome);
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
fn results(
    checked: Vec<(String, Result<String, &'static str>)>,
    outcome: Option<Result<lodestar_integrations::AddOutcome, ClientError>>,
) -> (Vec<ClientSendItem>, usize) {
    let sendable = checked.iter().filter(|(_, v)| v.is_ok()).count();
    let (went_in, duplicate, cause) = match outcome {
        None => (0, false, None),
        Some(Ok(_)) => (sendable, false, None),
        Some(Err(ClientError::Partial { sent, cause })) => {
            (sent.min(sendable), false, Some(*cause))
        }
        Some(Err(ClientError::Rejected(why))) if why.contains("already in the client") => {
            (sendable, true, None)
        }
        Some(Err(e)) => (0, false, Some(e)),
    };
    let failure = cause.map(|e| test_failure(&e).error.expect("a failure"));
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
                        failure.as_ref().map(|f| f.code.as_str()),
                        failure.as_ref().map(|f| f.message.clone()),
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

    fn checked() -> Vec<(String, Result<String, &'static str>)> {
        vec![
            ("a".into(), Ok("A".into())),
            ("h".into(), Err("hidden")),
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
            cause: Box::new(ClientError::Timeout),
        };
        let (r, sent) = results(checked(), Some(Err(partial)));
        assert_eq!(sent, 1);
        use ClientSendOutcome::*;
        assert_eq!(
            outcomes(&r),
            [
                (Sent, None),
                (Refused, Some("hidden")),
                (Failed, Some("timeout")),
                (Failed, Some("timeout"))
            ]
        );
    }

    #[test]
    fn duplicates_count_as_sent_and_failures_say_why() {
        use ClientSendOutcome::*;
        let dup = ClientError::Rejected("the torrent is already in the client".into());
        let (r, sent) = results(checked(), Some(Err(dup)));
        assert_eq!(sent, 3);
        assert_eq!(outcomes(&r)[0], (Duplicate, None));
        let (r, sent) = results(
            checked(),
            Some(Err(ClientError::Auth(
                lodestar_integrations::AuthFailure::BadCredentials,
            ))),
        );
        assert_eq!(sent, 0);
        assert_eq!(outcomes(&r)[2], (Failed, Some("bad_credentials")));
        assert!(r[2].message.as_ref().unwrap().ends_with('.'));
        // A hidden one is never in the send.
        let (r, _) = results(vec![("h".into(), Err("adult_hidden"))], None);
        assert_eq!(outcomes(&r), [(Refused, Some("adult_hidden"))]);
    }

    #[test]
    fn hashes_are_checked_and_deduplicated() {
        let a = "A".repeat(40);
        let out = parse_hashes(&[a.clone(), a.to_lowercase()]).unwrap();
        assert_eq!(out, vec!["a".repeat(40)]);
        assert!(parse_hashes(&[]).is_err());
        assert!(parse_hashes(&["xyz".into()]).is_err());
        assert!(parse_hashes(&vec!["a".repeat(40); MAX_HASHES + 1]).is_err());
    }
}
