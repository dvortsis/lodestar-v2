//! The Torznab endpoint, `GET /api/v1/torznab/api`, for Prowlarr, Sonarr,
//! Radarr and other *arr apps (architecture §9, §18.2 Outputs; plan
//! §4.3 TZ-0; the decisions are in `docs/decisions/2026-11-11-TZ-0.md`).
//!
//! **Functions:** `t=caps`, and the searches `search`, `tvsearch`,
//! `movie`, `music` and `book` with `q`, `cat`, `season`, `ep`, `limit`
//! (≤ 100) and `offset`. Every parameter is untrusted and parsed in
//! [`query`]. Results are magnet links only, as RSS 2.0 with Torznab's
//! attributes, written by [`xml`]. Categories follow [`categories`].
//!
//! **Auth:**
//! - An API key with the `torznab` scope (`?apikey=`, `X-Api-Key` or
//!   `Authorization: Bearer`), or a session.
//! - Checked by the API's own key guard ([`auth::key_or_session`]): the
//!   stored hash, compared in constant time, its per-IP failure bucket
//!   and its per-key request bucket.
//! - A refusal (no key, a wrong or revoked one, the wrong scope, a limit)
//!   is answered as a Torznab error document with the guard's status
//!   (and `Retry-After`), not the JSON envelope.
//! - The key is never written back: the documents carry no URL but
//!   magnet links, and the request log drops the query string.
//!
//! **What's never served:**
//! - `private_source` and BEP 27 private torrents, and remote rows whose
//!   private bit isn't verified yet ([`flags::PRIVACY`], §18.7): left out
//!   by the index query, then checked again on the stored row.
//! - Unverified items, whatever the query says (plan Q17): the AST is
//!   built from the parameters, never parsed, so `q` can't name
//!   `verified:`.
//! - Torrents a filter rule hid (your filters apply).
//! - Adult unless `torznab_adult` is on ([`TorznabAdult`], §20.9), read
//!   once per request; the stored class is checked again, as search does
//!   ([`discover::withheld`]).
//! - Held and blocklisted items are never stored, so nothing finds them.
//!
//! **The search** is search's own: [`search::base_query`] over the AST,
//! a [`Lease`] of the index serving now, its ranking and its counts. With
//! no words it lists the newest first, as an RSS feed would.

pub mod categories;
pub mod query;
pub mod xml;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use lodestar_classify::Record;
use lodestar_search::{CancelToken, Lease, Sort, Start};
use lodestar_store::{Enriched, TorrentRow, flags};

use self::query::{Function, Mode, Refused};
use self::xml::Xml;
use crate::auth;
use crate::keys::scope;
use crate::search::{self, Visibility};
use crate::{AdultVisibility, ApiState, MAX_WINDOW, discover, swarm};

/// Whether Torznab serves Adult: the `content` section's own
/// `torznab_adult` (§20.9: each output has its own adult setting, off by
/// default), followed live. The server layers it over the router as an
/// `Extension`; without one, Torznab never serves Adult.
#[derive(Clone, Debug, Default)]
pub struct TorznabAdult(pub AdultVisibility);

/// The server's name in caps and feeds.
const TITLE: &str = "Lodestar";
/// The feed's `<channel><link>`: the web UI, relative to the server.
const CHANNEL_LINK: &str = "/";
/// Further lease reads a page may take to fill itself, past rows the
/// stored check left out: bounds the work one request does.
const MAX_REFILLS: usize = 4;
/// Bytes of a torrent's name a magnet link's `dn` carries, at most.
const MAX_DN_BYTES: usize = 256;

/// Newznab's error codes this endpoint answers with, besides
/// [`query`]'s.
mod code {
    /// "Incorrect user credentials".
    pub const CREDENTIALS: u16 = 100;
    /// "Insufficient privileges/not authorized".
    pub const PRIVILEGES: u16 = 102;
    /// "Request limit reached".
    pub const LIMIT: u16 = 500;
    /// "Unknown error".
    pub const UNKNOWN: u16 = 900;
    /// "API disabled".
    pub const DISABLED: u16 = 910;
}

pub fn routes() -> Router<ApiState> {
    auth::key_or_session(
        Router::new().route("/torznab/api", get(api)),
        scope::TORZNAB,
    )
    // Outside the guard, so its refusals come out as Torznab documents.
    .layer(middleware::from_fn(as_torznab_error))
}

/// The guard's (and the router's) JSON refusals, as Torznab error
/// documents with the same status. The JSON message isn't carried over:
/// each status has a fixed description.
async fn as_torznab_error(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let status = resp.status();
    let json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if status.is_success() || !json {
        return resp;
    }
    let (code, description) = match status {
        StatusCode::UNAUTHORIZED => (code::CREDENTIALS, "Incorrect user credentials"),
        StatusCode::FORBIDDEN => (
            code::PRIVILEGES,
            "Insufficient privileges: use an API key with the Torznab scope",
        ),
        // Only a signed-in browser meets the notice gate: keys skip it.
        StatusCode::CONFLICT => (
            code::PRIVILEGES,
            "Insufficient privileges: accept the Legal Notice in the Lodestar web UI first",
        ),
        StatusCode::TOO_MANY_REQUESTS => (code::LIMIT, "Request limit reached"),
        StatusCode::METHOD_NOT_ALLOWED => (query::NO_FUNCTION, "No such function"),
        StatusCode::SERVICE_UNAVAILABLE => (code::DISABLED, "API disabled"),
        _ => (code::UNKNOWN, "Unknown error"),
    };
    let mut out = error_document(status, code, description);
    if let Some(v) = resp.headers().get(header::RETRY_AFTER) {
        out.headers_mut().insert(header::RETRY_AFTER, v.clone());
    }
    out
}

/// `<error code="…" description="…"/>`.
fn error_document(status: StatusCode, code: u16, description: &str) -> Response {
    let mut x = Xml::new();
    x.empty(
        "error",
        &[("code", &code.to_string()), ("description", description)],
    );
    document(status, "application/xml; charset=utf-8", x.finish())
}

fn refusal(r: Refused) -> Response {
    error_document(StatusCode::BAD_REQUEST, r.code, r.description)
}

fn document(status: StatusCode, content_type: &'static str, body: String) -> Response {
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp
}

/// The Torznab API: `t=caps` or a search.
async fn api(
    State(s): State<ApiState>,
    adult: Option<Extension<TorznabAdult>>,
    uri: Uri,
) -> Response {
    let req = match query::parse(uri.query()) {
        Ok(r) => r,
        Err(r) => return refusal(r),
    };
    let mode = match req.function {
        Function::Caps => {
            return document(StatusCode::OK, "application/xml; charset=utf-8", caps());
        }
        Function::Search(m) => m,
    };
    let adult = adult.is_some_and(|a| a.0.0.shown());
    run(s, &req, mode, adult).await
}

/// The caps document: what this server searches and its categories.
/// `imdbid`, `tmdbid` and `tvdbid` join `supportedParams` with ENR-11.
pub fn caps() -> String {
    let mut x = Xml::new();
    x.open("caps", &[]);
    x.empty("server", &[("title", TITLE)]);
    let max = query::MAX_LIMIT.to_string();
    x.empty("limits", &[("default", &max), ("max", &max)]);
    x.open("searching", &[]);
    for (name, params) in [
        ("search", "q"),
        ("tv-search", "q,season,ep"),
        ("movie-search", "q"),
        ("music-search", "q"),
        ("audio-search", "q"),
        ("book-search", "q"),
    ] {
        x.empty(name, &[("available", "yes"), ("supportedParams", params)]);
    }
    x.close();
    x.open("categories", &[]);
    for c in categories::CATEGORIES {
        x.open("category", &[("id", &c.id.to_string()), ("name", c.name)]);
        for s in c.subs {
            x.empty("subcat", &[("id", &s.id.to_string()), ("name", s.name)]);
        }
        x.close();
    }
    x.close();
    x.finish()
}

/// One result, read under the reader lock and written after.
#[derive(Debug)]
struct Item {
    name: String,
    hash: String,
    /// The v2 hash of a v2 or hybrid torrent.
    hash_v2: Option<String>,
    /// A pure v2 torrent: its magnet names only the v2 hash.
    v2_only: bool,
    size: u64,
    files: u64,
    discovered_at: i64,
    cats: (u32, Option<u32>),
    seeders: Option<u32>,
    leechers: Option<u32>,
    completed: Option<u32>,
}

/// Whether a stored row is left out of every Torznab answer.
fn excluded(row: &TorrentRow, e: Option<&Enriched>, adult: bool) -> bool {
    discover::withheld(row, e, false, adult) || row.flags & flags::PRIVACY != 0
}

async fn run(s: ApiState, req: &query::Request, mode: Mode, adult: bool) -> Response {
    let Some(search) = s.index.clone() else {
        return error_document(
            StatusCode::SERVICE_UNAVAILABLE,
            code::DISABLED,
            "Search is turned off",
        );
    };
    let (offset, limit) = (req.offset, req.limit);
    let Some(ast) = req.ast(mode) else {
        // Only categories this server doesn't list: nothing matches.
        return results(feed(offset, 0, &[]));
    };
    let sort = if req.words.is_empty() {
        Sort::Discovered { desc: true }
    } else {
        Sort::Relevance
    };
    let visibility = Visibility {
        hidden: false,
        adult,
    };
    let shown = search::base_query(&ast, visibility).query();
    let (reader, hot) = (s.reader.clone(), s.hot.clone());
    // Axum drops this future when the client goes away; the guard then
    // cancels the search on the blocking pool.
    let cancel = CancelToken::new();
    let _guard = cancel.cancel_on_drop();
    let answer = tokio::task::spawn_blocking(move || {
        let lease = Lease::new_at(search.index(), &shown, crate::unix_now(), 0, sort, &cancel)
            .map_err(search::search_error)?;
        let total = lease.total().value;
        // Past the search's window, an empty page (§8's offset bound).
        let limit = limit.min(MAX_WINDOW.saturating_sub(offset));
        if limit == 0 {
            return Ok((total, Vec::new()));
        }
        // A row the stored check leaves out isn't counted against the
        // page: the lease is read on until the page is full, so a client
        // that stops at a short page (Sonarr, Radarr) doesn't stop early.
        let mut items = Vec::with_capacity(limit);
        let mut start = Start::Offset(offset);
        for _ in 0..=MAX_REFILLS {
            let page = lease
                .page(start, limit - items.len(), &cancel)
                .map_err(search::search_error)?;
            let store = reader.lock().expect("reader lock");
            for h in &page.hits {
                // Deleted since indexing: it drops out of the page.
                let Some(row) = store.torrent(h.id as i64).map_err(crate::error::internal)? else {
                    continue;
                };
                let e = store.enrichment(row.id).map_err(crate::error::internal)?;
                if excluded(&row, e.as_ref(), adult) {
                    continue;
                }
                let summary =
                    swarm::summary(&store, hot.as_deref(), &row).map_err(crate::error::internal)?;
                let record = e.as_ref().and_then(|e| Record::decode(&e.data).ok());
                items.push(Item {
                    hash: hex(&row.info_hash.0),
                    hash_v2: row.info_hash_v2.map(|h| hex(&h.0)),
                    v2_only: row.flags & flags::V2 != 0 && row.flags & flags::HYBRID == 0,
                    size: row.size,
                    files: row.file_count,
                    discovered_at: row.discovered_at,
                    cats: categories::of(record.as_ref()),
                    seeders: summary.seeders,
                    leechers: summary.leechers,
                    completed: summary.completed,
                    name: row.name,
                });
            }
            drop(store);
            match page.next {
                Some(next) if items.len() < limit => start = Start::After(next),
                _ => break,
            }
        }
        Ok::<_, crate::ApiError>((total, items))
    })
    .await;
    match answer {
        Ok(Ok((total, items))) => results(feed(offset, total as u64, &items)),
        Ok(Err(e)) => failed(e.into_response().status()),
        Err(e) => failed(crate::error::internal(e).into_response().status()),
    }
}

/// A page of results, as RSS.
fn results(body: String) -> Response {
    document(StatusCode::OK, "application/rss+xml; charset=utf-8", body)
}

/// A search that failed while running (already logged, if internal).
fn failed(status: StatusCode) -> Response {
    error_document(status, code::UNKNOWN, "Unknown error")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A magnet link: the hash (or a pure v2 torrent's multihash), a hybrid's
/// v2 hash too, and the name, percent-encoded and cut to
/// [`MAX_DN_BYTES`]. No trackers: none are stored for DHT finds.
fn magnet(item: &Item) -> String {
    use std::fmt::Write;
    let mut out = String::from("magnet:?");
    if !item.v2_only || item.hash_v2.is_none() {
        out.push_str("xt=urn:btih:");
        out.push_str(&item.hash);
    }
    if let Some(v2) = &item.hash_v2 {
        if !out.ends_with('?') {
            out.push('&');
        }
        out.push_str("xt=urn:btmh:1220");
        out.push_str(v2);
    }
    out.push_str("&dn=");
    let mut name = item.name.as_str();
    if name.len() > MAX_DN_BYTES {
        let mut end = MAX_DN_BYTES;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name = &name[..end];
    }
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// The RSS feed of one page of results.
fn feed(offset: usize, total: u64, items: &[Item]) -> String {
    let mut x = Xml::new();
    x.open(
        "rss",
        &[
            ("version", "2.0"),
            ("xmlns:torznab", "http://torznab.com/schemas/2015/feed"),
        ],
    );
    x.open("channel", &[]);
    x.text("title", &[], TITLE);
    // RSS 2.0 requires a channel link: the instance's own web UI, as a
    // constant relative URL, never built from the request's Host.
    x.text("link", &[], CHANNEL_LINK);
    x.text("description", &[], "Lodestar search results");
    x.empty(
        "torznab:response",
        &[
            ("offset", &offset.to_string()),
            ("total", &total.to_string()),
        ],
    );
    for item in items {
        write_item(&mut x, item);
    }
    x.finish()
}

fn write_item(x: &mut Xml, item: &Item) {
    let link = magnet(item);
    let size = item.size.to_string();
    let (parent, sub) = item.cats;
    let cats: Vec<String> = std::iter::once(parent)
        .chain(sub)
        .map(|c| c.to_string())
        .collect();
    x.open("item", &[]);
    x.text("title", &[], &item.name);
    x.text("guid", &[("isPermaLink", "false")], &item.hash);
    x.text("pubDate", &[], &xml::rfc822(item.discovered_at));
    x.text("size", &[], &size);
    x.text("link", &[], &link);
    for c in &cats {
        x.text("category", &[], c);
    }
    x.empty(
        "enclosure",
        &[
            ("url", &link),
            ("length", &size),
            ("type", "application/x-bittorrent"),
        ],
    );
    let mut attr = |name: &'static str, value: &str| {
        x.empty("torznab:attr", &[("name", name), ("value", value)]);
    };
    for c in &cats {
        attr("category", c);
    }
    attr("size", &size);
    attr("files", &item.files.to_string());
    attr("infohash", &item.hash);
    attr("magneturl", &link);
    if let Some(s) = item.seeders {
        attr("seeders", &s.to_string());
        let peers = u64::from(s) + u64::from(item.leechers.unwrap_or(0));
        attr("peers", &peers.to_string());
    }
    if let Some(c) = item.completed {
        attr("grabs", &c.to_string());
    }
    // Public swarms: nothing is counted against a ratio.
    attr("downloadvolumefactor", "0");
    attr("uploadvolumefactor", "1");
    x.close();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> Item {
        Item {
            name: name.into(),
            hash: "ab".repeat(20),
            hash_v2: None,
            v2_only: false,
            size: 1 << 20,
            files: 1,
            discovered_at: 1_700_000_000,
            cats: (categories::TV, Some(5040)),
            seeders: None,
            leechers: None,
            completed: None,
        }
    }

    #[test]
    fn magnets_encode_the_name_and_carry_no_trackers() {
        let m = magnet(&item("A&B <x> é/\"?\u{0}"));
        assert_eq!(
            m,
            format!(
                "magnet:?xt=urn:btih:{}&dn=A%26B%20%3Cx%3E%20%C3%A9%2F%22%3F%00",
                "ab".repeat(20)
            )
        );
        assert!(!m.contains("tr="));
    }

    #[test]
    fn magnets_name_v2_hashes() {
        let mut i = item("n");
        i.hash_v2 = Some("cd".repeat(32));
        let hybrid = magnet(&i);
        assert!(hybrid.starts_with("magnet:?xt=urn:btih:"));
        assert!(hybrid.contains(&format!("&xt=urn:btmh:1220{}", "cd".repeat(32))));
        i.v2_only = true;
        assert_eq!(
            magnet(&i),
            format!("magnet:?xt=urn:btmh:1220{}&dn=n", "cd".repeat(32))
        );
    }

    #[test]
    fn long_names_are_cut_on_a_character() {
        let m = magnet(&item(&"é".repeat(200)));
        let dn = m.split("&dn=").nth(1).unwrap();
        // 128 two-byte characters, six bytes encoded each.
        assert_eq!(dn.len(), 128 * 6);
    }

    #[test]
    fn swarm_attributes_only_when_scraped() {
        let mut x = Xml::new();
        write_item(&mut x, &item("n"));
        let unscraped = x.finish();
        assert!(!unscraped.contains("\"seeders\"") && !unscraped.contains("\"peers\""));
        let mut i = item("n");
        (i.seeders, i.leechers, i.completed) = (Some(3), Some(4), Some(5));
        let mut x = Xml::new();
        write_item(&mut x, &i);
        let scraped = x.finish();
        for want in [
            "name=\"seeders\" value=\"3\"",
            "name=\"peers\" value=\"7\"",
            "name=\"grabs\" value=\"5\"",
        ] {
            assert!(scraped.contains(want), "{want}");
        }
    }
}
