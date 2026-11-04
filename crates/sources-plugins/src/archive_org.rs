//! Internet Archive, source plugin #4 (architecture §18.2 rank 6).
//!
//! Every item on archive.org has a torrent, and the Archive documents its
//! search API for this kind of use. Each poll asks `advancedsearch.php`
//! for the items published after a `since` cursor, oldest first, and maps
//! them to keys with claims. The endpoint is fixed: no address to enter,
//! no account, no credential.
//!
//! 1. The request is [`search_url`]: items that have an Archive torrent,
//!    published after the cursor, 500 rows, JSON.
//! 2. The answer goes to SRC-9's bounded parser
//!    (`lodestar_sources_feeds::archive_org`), which caps size, depth,
//!    values, items and text, and takes the info hash only from `btih`.
//!    This module parses no text of its own.
//! 3. [`to_found`], a pure function, maps each typed item to a `Found`.
//!
//! **Nothing else is fetched.** Never a `.torrent`, a details page or a
//! file. Metadata comes from the DHT, as for any unverified key.
//!
//! **The cursor** is the newest `publicdate` seen, in Unix seconds, kept
//! under [`CURSOR_KEY`] in the plugin's [`PluginState`]. It is saved after
//! a page's finds are sent, so a restart resumes where it stopped and
//! repeats at most one page (the pipeline already drops keys it has). A
//! first run, or an unreadable cursor, starts [`FIRST_WINDOW`] back: the
//! plugin follows what is new and never backfills the Archive. A full page
//! is followed at once by the next, at most [`MAX_PAGES`] a poll.
//!
//! **Pace.** One poll an hour ([`POLICY`]); the follow-up pages of one
//! poll go through `Client::send`, so the per-host rate (one request a
//! second) spaces them. A 429 or 5xx backs the poll off through the
//! limiter and is tried again at the next one.
//!
//! **Every field is a claim** at Low confidence: title, size (`item_size`),
//! media type and collections (as ids), and a file list where an item
//! gives one. A search answer gives none; the mapping handles metadata
//! items for when a later slice fetches them. Uploaders are never kept
//! (§18.7).

use std::collections::HashSet;
use std::time::Duration;

use lodestar_sources::claims::{ClaimedFile, Claims, Confidence, ExternalId, Provenance};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, Consent, Distribution, Emitter, Endpoint, Endpoints, Found,
    HostRate, Kind, Manifest, Network, PluginError, PluginState, Policy, Priority, Request, Risk,
    Schedule, SourceCtx, SourcePlugin,
};
use lodestar_sources_feeds::archive_org::{self, Item, LIMITS, Limits};

/// The plugin's id in the store. Never rename.
pub const ID: &str = "archive_org";
/// The strategy code on every key this plugin emits: the search API.
pub const STRATEGY_SEARCH: u8 = 1;
/// The host the manifest declares and egress is narrowed to.
pub const HOST: &str = "archive.org";
/// The plugin-state key holding the cursor (decimal Unix seconds).
pub const CURSOR_KEY: &str = "since";
/// Rows asked for per page. Under the parser's item cap.
pub const ROWS: u32 = 500;
/// Pages followed in one poll: at most 5,000 items.
pub const MAX_PAGES: u32 = 10;
/// How far back a first run starts: one day.
pub const FIRST_WINDOW: i64 = 24 * 3600;

/// The largest size claim believed: 1 PiB.
pub const MAX_SIZE: u64 = 1 << 50;
/// The earliest publish date believed: 2001-07-02, BitTorrent's release.
pub const MIN_PUBLISHED: i64 = 994_032_000;
/// How far past now a publish date may be (clock skew, time zones).
pub const MAX_FUTURE: i64 = 24 * 3600;

/// The parser's caps for one page: its defaults, with the items cut to
/// what was asked for.
pub const PAGE_LIMITS: Limits = Limits {
    max_items: ROWS as usize * 2,
    ..LIMITS
};

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Internet Archive",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Low,
    // §18.7: Internet Archive is consent class None (a toggle).
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: false,
};

/// No settings: the endpoint is fixed.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// Q4: at most 1 request/s to the Archive, with `since` cursors. Polls an
/// hour apart; a poll that catches up sends a few requests, a second apart.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(3600),
        per_host: HostRate {
            interval: Duration::from_secs(1),
            burst: 1,
        },
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
    },
    config_schema: CONFIG_SCHEMA,
    // One instance follows the Archive's new items.
    distribution: Distribution::Singleton,
    schedule: Schedule::ALWAYS,
    // One agent: the setting can't be changed (SRC-14a).
    max_agents: 1,
};

/// The Internet Archive plugin.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveOrg {
    now: fn() -> i64,
}

impl Default for ArchiveOrg {
    fn default() -> ArchiveOrg {
        ArchiveOrg::new()
    }
}

impl ArchiveOrg {
    pub fn new() -> ArchiveOrg {
        ArchiveOrg { now: unix_now }
    }

    /// A plugin that reads the time from `now` (Unix seconds), so a test
    /// can fix the first-run cursor and the request URLs.
    #[doc(hidden)]
    pub fn with_clock(now: fn() -> i64) -> ArchiveOrg {
        ArchiveOrg { now }
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, public addresses only, with DNS through the proxy when
    /// one is set.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

impl SourcePlugin for ArchiveOrg {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(run(ctx, self.now))
    }
}

async fn run(ctx: SourceCtx, now: fn() -> i64) -> Result<(), PluginError> {
    let SourceCtx {
        emitter,
        mut shutdown,
        kit,
        ..
    } = ctx;
    loop {
        let polled = tokio::select! {
            _ = shutdown.requested() => return Ok(()),
            r = poll(&kit.http, &*kit.state, &emitter, now) => r,
        };
        // Any failure is tried again at the next poll; the limiter's
        // back-off covers 429 and 5xx. Only a closed pipeline ends the run.
        if polled == Poll::Closed {
            return Ok(());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Poll {
    Done,
    Closed,
}

async fn poll(http: &Client, state: &dyn PluginState, emitter: &Emitter, now: fn() -> i64) -> Poll {
    let mut since = load_cursor(state, now()).await;
    let mut seen = HashSet::new();
    for page in 0..MAX_PAGES {
        let req = Request::get(search_url(since)).header("Accept", "application/json");
        let sent = if page == 0 {
            http.poll("search", &req).await
        } else {
            http.send(&req).await
        };
        let body = match sent {
            Ok(res) if res.status == 200 => res.body,
            _ => return Poll::Done,
        };
        let (at, mut found, mut newest) = (now(), Vec::new(), since);
        let page_info = archive_org::parse_each(&body, &PAGE_LIMITS, |item| {
            if let Some(t) = item.published.filter(|t| plausible(*t, at)) {
                newest = newest.max(t);
            }
            // A repeated item is one find.
            if let Some(f) = to_found(item, at)
                && seen.insert(f.key)
            {
                found.push(f);
            }
        });
        for f in found {
            if emitter.emit(f).await.is_err() {
                return Poll::Closed;
            }
        }
        // Saved after the finds are sent: a restart repeats a page at
        // worst, and never skips one.
        let advanced = newest > since;
        if advanced {
            since = newest;
            save_cursor(state, since).await;
        }
        let docs = page_info.summary.kept + page_info.summary.skipped.total();
        if docs < ROWS || !advanced {
            break;
        }
    }
    Poll::Done
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn plausible(t: i64, now: i64) -> bool {
    (MIN_PUBLISHED..=now.saturating_add(MAX_FUTURE)).contains(&t)
}

/// The saved cursor, or a day back from `now` when there is none or it is
/// unreadable or implausible.
async fn load_cursor(state: &dyn PluginState, now: i64) -> i64 {
    let saved = match state.get(CURSOR_KEY).await {
        Ok(Some(v)) => parse_cursor(&v, now),
        _ => None,
    };
    saved.unwrap_or_else(|| (now - FIRST_WINDOW).max(MIN_PUBLISHED))
}

/// Digits only, a believable date.
fn parse_cursor(v: &[u8], now: i64) -> Option<i64> {
    if v.is_empty() || v.len() > 12 || !v.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let t: i64 = std::str::from_utf8(v).ok()?.parse().ok()?;
    plausible(t, now).then_some(t)
}

async fn save_cursor(state: &dyn PluginState, since: i64) {
    // A failed save only means the next poll repeats a page.
    let _ = state.set(CURSOR_KEY, since.to_string().into_bytes()).await;
}

/// The search request for items published after `since` (Unix seconds):
/// items with an Archive torrent, oldest first. Built from fixed text and
/// the cursor's digits, so nothing in it comes from an answer.
pub fn search_url(since: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(since);
    format!(
        "https://{HOST}/advancedsearch.php\
         ?q=format%3A%22Archive%20BitTorrent%22%20AND%20publicdate%3A%7B\
         {y:04}-{mo:02}-{d:02}T{h:02}%3A{mi:02}%3A{s:02}Z%20TO%20%2A%5D\
         &fl%5B%5D=identifier&fl%5B%5D=btih&fl%5B%5D=title&fl%5B%5D=item_size\
         &fl%5B%5D=mediatype&fl%5B%5D=collection&fl%5B%5D=publicdate\
         &sort%5B%5D=publicdate%20asc&rows={ROWS}&page=1&output=json"
    )
}

/// Unix seconds as UTC (year, month, day, hour, minute, second), by
/// Howard Hinnant's `civil_from_days`.
fn civil(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let (days, rem) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// What an Internet Archive media type means for the content type claim,
/// as a `ContentType::as_str` name (`lodestar-classify`). `None` for one
/// that says nothing (`data`, `web`, `collection`, …). The claim is Low
/// confidence and the classifier has the last word.
pub fn content_type(mediatype: &str) -> Option<&'static str> {
    match mediatype {
        "movies" => Some("video"),
        "audio" | "etree" => Some("music"),
        "texts" => Some("ebook"),
        "image" => Some("images"),
        "software" => Some("software"),
        _ => None,
    }
}

/// One item as a discovery. Every field stays a claim at Low confidence;
/// the key is the parser's, from `btih`. `None` for an item with no key.
///
/// - The reference is the item's details link, which the parser builds
///   from a validated identifier. It is never fetched.
/// - A size outside 1 B to 1 PiB and a date before BitTorrent or more
///   than a day ahead are dropped.
/// - The identifier becomes `archive-org:<identifier>`, the media type
///   `archive-mediatype:<type>` and, when known, `content-type:<type>`
///   ([`content_type`]), and each collection `archive-collection:<id>`.
/// - Files, where an item lists them, become file claims.
pub fn to_found(item: Item, now: i64) -> Option<Found> {
    let key = item.hashes.key()?;
    let mut ids = Vec::new();
    let mut id = |scheme: &str, value: &str| {
        ids.push(ExternalId {
            scheme: scheme.to_owned(),
            value: value.to_owned(),
        });
    };
    if let Some(i) = item.identifier.as_deref() {
        id("archive-org", i);
    }
    if let Some(m) = item.mediatype.as_deref() {
        id("archive-mediatype", m);
        if let Some(t) = content_type(m) {
            id("content-type", t);
        }
    }
    for c in &item.collections {
        id("archive-collection", c);
    }
    let claims = Claims {
        name: item.title,
        size: item.size.filter(|s| (1..=MAX_SIZE).contains(s)),
        files: item
            .files
            .into_iter()
            .map(|f| ClaimedFile {
                path: f.path,
                size: f.size,
            })
            .collect(),
        ids,
        ..Claims::default()
    };
    let published_at = item.published.filter(|t| plausible(*t, now));
    Some(
        Found::key(key, STRATEGY_SEARCH)
            .with_claims(claims)
            .with_provenance(Provenance {
                external_ref: item.link,
                published_at,
                confidence: Confidence::Low,
            }),
    )
}
