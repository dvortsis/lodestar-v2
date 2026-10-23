//! Saved-search alerts (§8 "Saved searches → alerts", §9, §10.8): the
//! list, the read marker, the nav badge, the `alerts` SSE event and the
//! RSS feed per saved search.
//!
//! **Visibility (content safety):** the percolator stores an alert for
//! every match (B8). Every way out of here shows only alerts on torrents
//! search would show: none a filter rule hid ([`flags::HIDDEN`]), and no
//! Adult while "Show adult content" is off (§20.9: left out entirely).
//! The list, the event and the feed check each row with
//! [`shown_row`]; the badge is the same rule in SQL
//! ([`Store::unread_shown`]), and a test holds the two together. No
//! response carries a count of what was left out.
//!
//! **Feeds** (`GET /feeds/{id}`) are the one route here a feed token or
//! an API key with the `feeds` scope reaches ([`auth::feed_token`]). A
//! token for one search, a search without `rss`, and an unknown id all
//! answer the same 404, so a token can't tell which other searches
//! exist. Torrent names are attacker-controlled, so the XML comes only
//! from the escaping renderer ([`rss::render`]), and private-source
//! torrents never appear (§18.7). The token, and any query string, never
//! appear in a link, a body, a header, an error or a log.

use axum::extract::{Extension, State};
use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use lodestar_classify::ContentType;
use lodestar_integrations::notify::rss::{self, RssChannel, RssItem, RssMagnet};
use lodestar_store::{Alert, Store, flags};
use sha2::{Digest, Sha256};
use utoipa::OpenApi;

use crate::auth::{self, Access, SessionAuth};
use crate::dto::alerts::{AlertRow, AlertsPage, AlertsQuery, AlertsRead, FeedQuery, UnreadAlerts};
use crate::dto::auth::OkResponse;
use crate::dto::error::ErrorEnvelope;
use crate::dto::events::AlertsEvent;
use crate::error::{ApiError, ApiResult, internal};
use crate::events::EventHub;
use crate::extract::{Json as JsonBody, Path, Query, peer_ip, same_origin_json};
use crate::search::AdultVisibility;
use crate::user_items::{hit_row, shown_row, user_data};
use crate::{ApiState, torrents, unix_now};

/// This module's half of `/openapi.json`.
#[derive(OpenApi)]
#[openapi(paths(list, mark_read, unread, feed))]
pub(crate) struct ApiDoc;

/// Rows per page, at most.
const MAX_LIMIT: usize = 200;
const DEFAULT_LIMIT: usize = 50;
/// Alerts read from the store at a time while skipping withheld ones.
const BATCH: usize = 200;
/// Items in a feed: the newest shown alerts.
pub const FEED_ITEMS: usize = 50;
/// Where a feed's links point when the request's `Host` isn't usable.
const FALLBACK_ORIGIN: &str = "http://localhost";

/// Notification channels besides the app and RSS, as bits of a saved
/// search's `channels` (the store persists them; B8). Append only: a bit
/// once given never changes meaning. Nothing sends on them yet; sending
/// waits for the notifications settings section, and anything that sends
/// must check the fixture gate (`fixture::unless_offline`).
pub mod channel {
    pub const WEBHOOK: u32 = 1 << 0;
    pub const NTFY: u32 = 1 << 1;
    pub const DISCORD: u32 = 1 << 2;
    pub const EMAIL: u32 = 1 << 3;
    /// Every defined bit.
    pub const ALL: u32 = WEBHOOK | NTFY | DISCORD | EMAIL;
}

pub fn routes() -> Router<ApiState> {
    auth::protected(
        Router::new()
            .route("/saved-searches/{id}/alerts", get(list))
            .route("/alerts/read", post(mark_read))
            .route("/alerts/unread", get(unread)),
    )
    .merge(auth::feed_token(
        Router::new().route("/feeds/{id}", get(feed)),
    ))
}

fn search_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "saved_search_not_found",
        "There's no saved search with that id.",
    )
}

/// The one answer for every feed a caller can't have.
fn feed_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "feed_not_found",
        "There's no feed here.",
    )
}

fn bad_cursor() -> ApiError {
    ApiError::bad_request(
        "bad_cursor",
        "That cursor isn't one this server gave out; start again.",
    )
    .field("cursor")
}

/// The content type the badge leaves out: Adult's, unless shown.
fn hidden_type(adult: bool) -> Option<u8> {
    (!adult).then(|| ContentType::Adult.code())
}

// ---- SSE -------------------------------------------------------------------

/// Announces a percolation run's new alerts (`created`, from
/// [`Store::record_percolation`]) as one `alerts` event per saved search,
/// counting only alerts on torrents shown now. A search whose new alerts
/// are all withheld sends nothing. Returns how many events it published.
///
/// The events are tagged with the visibility they were counted under, so
/// one counted while Adult was shown is withheld once it's hidden.
pub fn announce(
    hub: &EventHub,
    store: &Store,
    created: &[Alert],
    adult: &AdultVisibility,
) -> ApiResult<usize> {
    let shown = adult.shown();
    let mut by_search: std::collections::BTreeMap<i64, Vec<&Alert>> = Default::default();
    for a in created {
        by_search.entry(a.saved_search_id).or_default().push(a);
    }
    let mut sent = 0;
    for (id, alerts) in by_search {
        // Deleted meanwhile: nobody to tell.
        let Some(search) = store.saved_search(id).map_err(internal)? else {
            continue;
        };
        let mut event: Option<AlertsEvent> = None;
        for a in alerts {
            if shown_row(store, a.torrent_id, false, shown)?.is_none() {
                continue;
            }
            let e = event.get_or_insert(AlertsEvent {
                alert: a.id,
                matched: 0,
                max_id: a.torrent_id,
                notify: search.alert,
                saved_search: id,
            });
            e.matched += 1;
            e.alert = e.alert.max(a.id);
            e.max_id = e.max_id.max(a.torrent_id);
        }
        if let Some(e) = event {
            hub.alerts(e, shown);
            sent += 1;
        }
    }
    Ok(sent)
}

// ---- handlers ----------------------------------------------------------------

/// A saved search's alerts, newest first. Only alerts on torrents search
/// would show: none a filter rule hid, and Adult only while "Show adult
/// content" is on. A page is full unless the list ends: withheld rows are
/// skipped, never counted.
#[utoipa::path(
    operation_id = "listAlerts",
    get,
    path = "/saved-searches/{id}/alerts",
    tag = "library",
    params(("id" = i64, Path, description = "The saved search's id."), AlertsQuery),
    responses(
        (status = 200, body = AlertsPage),
        (status = 400, body = ErrorEnvelope, description = "`bad_cursor`: a cursor this server didn't give out."),
        (status = 404, body = ErrorEnvelope, description = "`saved_search_not_found`."),
    )
)]
pub(crate) async fn list(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
    Query(p): Query<AlertsQuery>,
) -> ApiResult<Json<AlertsPage>> {
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut before = match p.cursor.as_deref() {
        None => None,
        Some(c) => Some(
            c.parse::<i64>()
                .ok()
                .filter(|&t| t > 0)
                .ok_or_else(bad_cursor)?,
        ),
    };
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        store
            .saved_search(id)
            .map_err(internal)?
            .ok_or_else(search_not_found)?;
        let mut items = Vec::new();
        let mut more = false;
        // The last row returned: a cursor never names a withheld one.
        let mut last = None;
        'pages: loop {
            let page = store.alerts(id, before, BATCH).map_err(internal)?;
            let end = page.len() < BATCH;
            for a in page {
                before = Some(a.torrent_id);
                let Some((row, e)) = shown_row(&store, a.torrent_id, false, adult)? else {
                    continue;
                };
                if items.len() == limit {
                    more = true;
                    break 'pages;
                }
                items.push(AlertRow {
                    created_at: a.created_at,
                    id: a.id,
                    read: a.read_at.is_some(),
                    torrent: hit_row(&row, e),
                });
                last = Some(a.torrent_id);
            }
            if end {
                break;
            }
        }
        Ok(Json(AlertsPage {
            items,
            next_cursor: more.then(|| last.map(|t| t.to_string())).flatten(),
        }))
    })
    .await
    .map_err(internal)?
}

/// Marks alerts read up to `through` (the highest alert id you saw), for
/// one saved search or all of them. Alerts that arrived since stay unread.
/// It returns no count: one would reveal alerts that are withheld.
#[utoipa::path(
    operation_id = "markAlertsRead",
    post,
    path = "/alerts/read",
    tag = "library",
    request_body = AlertsRead,
    responses(
        (status = 200, body = OkResponse),
        (status = 404, body = ErrorEnvelope, description = "`saved_search_not_found`."),
    )
)]
pub(crate) async fn mark_read(
    State(s): State<ApiState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<AlertsRead>,
) -> ApiResult<Json<OkResponse>> {
    same_origin_json(&headers)?;
    let writer = user_data(&s)?;
    tokio::task::spawn_blocking(move || {
        let mut store = writer.lock().expect("user data lock");
        if let Some(id) = body.saved_search {
            store
                .saved_search(id)
                .map_err(internal)?
                .ok_or_else(search_not_found)?;
        }
        store
            .mark_alerts_read(body.saved_search, body.through, unix_now())
            .map_err(internal)?;
        Ok(Json(OkResponse::OK))
    })
    .await
    .map_err(internal)?
}

/// Unread alerts across every saved search, on torrents search would show:
/// the nav badge, equal to the sum of the saved searches' `unread`.
#[utoipa::path(
    operation_id = "getUnreadAlerts",
    get,
    path = "/alerts/unread",
    tag = "library",
    responses((status = 200, body = UnreadAlerts))
)]
pub(crate) async fn unread(State(s): State<ApiState>) -> ApiResult<Json<UnreadAlerts>> {
    let adult = s.adult.shown();
    tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        let unread = store.unread_shown(hidden_type(adult)).map_err(internal)?;
        Ok(Json(UnreadAlerts { unread }))
    })
    .await
    .map_err(internal)?
}

/// The links' origin: the request's `Host` (a host and port only), over
/// `https` only when a trusted proxy (proxy-header auth on, the TCP peer
/// trusted, B12) says `X-Forwarded-Proto: https`.
fn origin(headers: &HeaderMap, auth: Option<&SessionAuth>, peer: std::net::IpAddr) -> String {
    let https = auth.is_some_and(|a| {
        let proxy = a.auth.proxy();
        if !proxy.enabled || a.auth.proxy_forced_off() {
            return false;
        }
        let trusted = auth::proxy::Proxy::compile(proxy).is_ok_and(|p| p.trusts(peer));
        let mut proto = headers.get_all("x-forwarded-proto").iter();
        let https = match (proto.next(), proto.next()) {
            (Some(v), None) => v
                .to_str()
                .is_ok_and(|v| v.trim().eq_ignore_ascii_case("https")),
            _ => false,
        };
        trusted && https
    });
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = match (hosts.next(), hosts.next()) {
        (Some(h), None) => h.to_str().ok(),
        _ => None,
    };
    host.and_then(|h| rss::feed_origin(https, h))
        .unwrap_or_else(|| FALLBACK_ORIGIN.to_string())
}

/// The feed's document: the newest shown alerts, at most [`FEED_ITEMS`],
/// leaving out private-source torrents. `None` for a search that doesn't
/// exist or doesn't serve RSS.
fn feed_xml(store: &Store, id: i64, adult: bool, origin: &str) -> ApiResult<Option<String>> {
    let Some(search) = store.saved_search(id).map_err(internal)? else {
        return Ok(None);
    };
    if !search.rss {
        return Ok(None);
    }
    let mut items = Vec::new();
    let mut newest = None;
    let mut before = None;
    'pages: loop {
        let page = store.alerts(id, before, BATCH).map_err(internal)?;
        let end = page.len() < BATCH;
        for a in page {
            before = Some(a.torrent_id);
            let Some((row, _)) = shown_row(store, a.torrent_id, false, adult)? else {
                continue;
            };
            let private = row.flags & flags::PRIVATE_SOURCE != 0;
            if private {
                continue;
            }
            let hash = torrents::hex(&row.info_hash.0);
            newest = newest.max(Some(a.created_at));
            items.push(RssItem {
                title: row.name.clone(),
                link: Some(format!("{origin}/t/{hash}")),
                guid: hash.clone(),
                published: u64::try_from(a.created_at).unwrap_or(0),
                description: format!("New for the saved search \u{201c}{}\u{201d}.", search.name),
                private_source: private,
                magnet: Some(RssMagnet {
                    info_hash: hash,
                    name: row.name,
                    size: row.size,
                }),
            });
            if items.len() == FEED_ITEMS {
                break 'pages;
            }
        }
        if end {
            break;
        }
    }
    let channel = RssChannel {
        title: format!("Lodestar: {}", search.name),
        link: format!("{origin}/alerts/{id}"),
        description: format!(
            "New torrents for the saved search \u{201c}{}\u{201d}.",
            search.name
        ),
        // The newest item's time, so the body changes only with the
        // items: an unchanged feed keeps its ETag.
        built_at: u64::try_from(newest.unwrap_or(search.created_at)).unwrap_or(0),
    };
    rss::render(&channel, &items).map(Some).map_err(internal)
}

/// A strong ETag over the body.
fn etag(body: &str) -> String {
    let digest = Sha256::digest(body.as_bytes());
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("\"{hex}\"")
}

/// Whether `If-None-Match` names `tag` (or `*`). It compares weakly
/// (RFC 9110 §13.1.2), so `W/` before a tag doesn't matter.
fn not_modified(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim())
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == tag)
}

/// A saved search's alerts as RSS 2.0: the 50 newest on torrents search
/// would show, each with a magnet enclosure. Feed readers that can't sign
/// in pass a feed token as `?token=`; an API key with the `feeds` scope
/// works too. Private-source torrents never appear. Anything the caller
/// can't have (another search's token, a search without RSS, an unknown
/// id) is the same `404 feed_not_found`.
#[utoipa::path(
    operation_id = "getFeed",
    get,
    path = "/feeds/{id}",
    tag = "library",
    params(("id" = i64, Path, description = "The saved search's id."), FeedQuery),
    responses(
        (status = 200, description = "The feed.", content_type = "application/rss+xml", body = String),
        (status = 304, description = "Unchanged since the `ETag` sent in `If-None-Match`."),
        (status = 401, body = ErrorEnvelope, description = "`login_required` or `bad_api_key`."),
        (status = 403, body = ErrorEnvelope, description = "`key_scope`: an API key without the `feeds` scope."),
        (status = 404, body = ErrorEnvelope, description = "`feed_not_found`."),
    )
)]
pub(crate) async fn feed(
    State(s): State<ApiState>,
    Extension(access): Extension<Access>,
    extensions: Extensions,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    // Any id that isn't a saved search's is the one 404, whatever its shape.
    let id = id.parse::<i64>().map_err(|_| feed_not_found())?;
    if !access.reaches_feed(id) {
        return Err(feed_not_found());
    }
    // "Show adult content" (§20.9): the feed follows the same setting as
    // everything else, whoever reads it.
    let adult = s.adult.shown();
    let origin = origin(
        &headers,
        extensions.get::<SessionAuth>(),
        peer_ip(&extensions),
    );
    let xml = tokio::task::spawn_blocking(move || {
        let store = s.reader.lock().expect("reader lock");
        feed_xml(&store, id, adult, &origin)
    })
    .await
    .map_err(internal)??
    .ok_or_else(feed_not_found)?;
    let tag = etag(&xml);
    let mut resp = if not_modified(&headers, &tag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/rss+xml; charset=utf-8"),
            )],
            xml,
        )
            .into_response()
    };
    let h = resp.headers_mut();
    h.insert(header::ETAG, HeaderValue::from_str(&tag).map_err(internal)?);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use lodestar_classify::Correction;
    use lodestar_store::{NewTorrent, SavedSearchInput, SearchHits, SourceId, StoreConfig};

    use super::*;
    use crate::discover::withheld;

    /// One torrent of each kind the rules tell apart: (Adult correction,
    /// classified at all, extra flags).
    const KINDS: [(bool, bool, i64); 8] = [
        (false, true, 0),
        (true, true, 0),
        (false, true, flags::HIDDEN),
        (true, true, flags::HIDDEN),
        (false, false, 0),
        (false, false, flags::HIDDEN),
        (false, true, flags::PRIVATE_SOURCE),
        (true, true, flags::DEMOTED | flags::PRIVATE_SOURCE),
    ];

    fn info(name: &str) -> Vec<u8> {
        use lodestar_bencode::{OwnedValue, to_vec};
        let b = |s: &str| OwnedValue::Bytes(s.as_bytes().to_vec());
        to_vec(&OwnedValue::Dict(
            [
                ("length", OwnedValue::Int(1 << 20)),
                ("name", b(name)),
                ("piece length", OwnedValue::Int(1 << 16)),
                ("pieces", OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect(),
        ))
    }

    /// The badge's SQL and the per-row rule agree for every kind of
    /// torrent, either way "Show adult content" is set: one store per
    /// torrent, holding one unread alert on it.
    #[test]
    fn the_badge_count_is_exactly_withheld_in_sql() {
        for (i, (is_adult, classified, extra)) in KINDS.into_iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
            let raw = info(&format!("Kind.{i}.1080p"));
            let parsed = lodestar_metainfo::Info::parse(&raw).unwrap();
            let id = store
                .insert(&[NewTorrent {
                    info: &parsed,
                    raw_info: Some(&raw),
                    discovered_at: 0,
                    metadata_at: 0,
                    extra_flags: extra,
                    source: SourceId::new(0).unwrap(),
                }])
                .unwrap()[0]
                .id();
            if classified {
                let row = store.torrent(id).unwrap().unwrap();
                let tree = store.files(id).unwrap().unwrap();
                let files = lodestar_classify::file_infos(&tree.files, tree.encoding);
                let correction = Correction {
                    adult: is_adult.then_some(true),
                    not_junk: false,
                };
                let (e, _) =
                    lodestar_filter::reclassify::enrich_corrected(&row.name, &files, correction);
                store.set_enrichments(&[(id, e)]).unwrap();
            }
            let search = store
                .create_saved_search(
                    &SavedSearchInput {
                        name: "s",
                        query: "",
                        ast: "{}",
                        sort: None,
                        alert: true,
                        rss: true,
                        channels: 0,
                        paused: false,
                    },
                    0,
                    0,
                )
                .unwrap();
            store
                .record_percolation(
                    id,
                    &[SearchHits {
                        saved_search_id: search,
                        torrent_ids: &[id],
                    }],
                    0,
                )
                .unwrap();
            let row = store.torrent(id).unwrap().unwrap();
            let e = store.enrichment(id).unwrap();
            assert_eq!(e.is_some(), classified);
            for adult in [false, true] {
                let shown = !withheld(&row, e.as_ref(), false, adult);
                assert_eq!(
                    store.unread_shown(hidden_type(adult)).unwrap(),
                    u64::from(shown),
                    "kind {i}, adult shown: {adult}"
                );
                assert_eq!(
                    shown_row(&store, id, false, adult).unwrap().is_some(),
                    shown
                );
            }
        }
    }

    #[test]
    fn channel_bits_are_distinct_and_stable() {
        // Persisted: never renumber.
        assert_eq!(
            [
                channel::WEBHOOK,
                channel::NTFY,
                channel::DISCORD,
                channel::EMAIL
            ],
            [1, 2, 4, 8]
        );
        assert_eq!(channel::ALL, 15);
    }

    #[test]
    fn if_none_match_lists_and_wildcards() {
        let tag = etag("<rss/>");
        assert!(tag.starts_with('"') && tag.ends_with('"') && tag.len() == 34);
        let h = |v: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::IF_NONE_MATCH, v.parse().unwrap());
            m
        };
        assert!(not_modified(&h(&tag), &tag));
        assert!(not_modified(&h(&format!("\"a\", {tag}")), &tag));
        assert!(not_modified(&h("*"), &tag));
        assert!(!not_modified(&h("\"a\""), &tag));
        assert!(not_modified(&h(&format!("W/{tag}")), &tag));
        assert!(!not_modified(&HeaderMap::new(), &tag));
    }
}
