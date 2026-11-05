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
//! under [`CURSOR_KEY`] in the plugin's [`PluginState`]. A poll asks for
//! items from [`LOOKBACK`] before it (inclusive, so a second shared by
//! several items is never split) and reads pages `1..` of that one query,
//! so a page boundary can't lose an item. The cursor is saved after a
//! page's finds are sent, so a restart resumes close to where it stopped
//! (the pipeline already drops keys it has). A first run, or an
//! unreadable cursor, starts [`FIRST_WINDOW`] back: the plugin follows
//! what is new and never backfills the Archive. A full page is followed at
//! once by the next, at most [`MAX_PAGES`] a poll. A poll that ends on the
//! page cap saves a resume point ([`RESUME_KEY`]) so the next carries on
//! past its rows instead of re-reading them. When a resume run catches up,
//! the next lookback starts no earlier than the run's end ([`FLOOR_KEY`]).
//! A skipped row (no or a bad hash) still moves the window by its date.
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
/// The plugin-state key holding the resume point (decimal Unix seconds):
/// set when a poll reads [`MAX_PAGES`] full pages and so may have left rows
/// behind. The next poll starts there, inclusive and without the lookback,
/// until one ends on a short page. Without it, a window of 10,000 or more
/// rows (a bulk ingest) would be read from its start at every poll and the
/// cursor would never get past it.
pub const RESUME_KEY: &str = "resume";
/// The plugin-state key holding the floor (decimal Unix seconds): where the
/// last resume run ended. The lookback that follows a run's catch-up never
/// starts before it, so it can't re-read the bulk the run just worked off
/// and set the resume point back inside it. Once the cursor's own lookback
/// has passed it, it has no effect. It is never removed, on purpose: a stale
/// floor is a no-op under the `max`, and the next catch-up overwrites it.
pub const FLOOR_KEY: &str = "floor";
/// Rows asked for per page. Under the parser's item cap.
pub const ROWS: u32 = 500;
/// Pages followed in one poll: at most 10,000 items.
pub const MAX_PAGES: u32 = 20;
/// How far before the cursor each poll's query starts. An item is public
/// from its `publicdate`, but its Archive torrent (and so its `btih` and
/// the format the query asks for) appears only after the Archive's derive
/// step, which can take hours for large items. Asking again for the last
/// [`LOOKBACK`] picks those up. The cost is that the window is fetched
/// again each poll and the pipeline drops the keys it has. An assumption,
/// to be checked against the live Archive before SRC-G.
pub const LOOKBACK: i64 = 6 * 3600;
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

pub(crate) const MANIFEST: Manifest = Manifest {
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
    always_adult: false,
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
    let saved = load_time(state, CURSOR_KEY, now()).await;
    let resume = load_time(state, RESUME_KEY, now()).await;
    let floor = load_time(state, FLOOR_KEY, now()).await;
    // The query starts at the resume point if the last poll ran out of
    // pages (see `RESUME_KEY`); else a lookback before the cursor (see
    // `LOOKBACK`); else, on a first run, a day back. It stays put while
    // its pages are read.
    let from = match (resume, saved) {
        (Some(r), _) => r,
        (None, Some(c)) => (c - LOOKBACK).max(floor.unwrap_or(0)).max(MIN_PUBLISHED),
        (None, None) => (now() - FIRST_WINDOW).max(MIN_PUBLISHED),
    };
    let mut cursor = saved.unwrap_or(0);
    // The newest date this poll read: rows come oldest first, so it is the
    // last row's.
    let mut tail: Option<i64> = None;
    let mut seen = HashSet::new();
    let mut ran_out = false;
    for page in 1..=MAX_PAGES {
        let req = Request::get(search_url(from, page)).header("Accept", "application/json");
        let sent = if page == 1 {
            http.poll("search", &req).await
        } else {
            http.send(&req).await
        };
        let body = match sent {
            Ok(res) if res.status == 200 => res.body,
            _ => return Poll::Done,
        };
        let (at, mut found, mut newest) = (now(), Vec::new(), cursor);
        let info = archive_org::parse_each(&body, &PAGE_LIMITS, |item| {
            // A date up to a day ahead is believed, but never moves the
            // cursor past the page's own time.
            if let Some(t) = item.published.filter(|t| plausible(*t, at)) {
                let t = t.min(at);
                newest = newest.max(t);
                tail = Some(tail.map_or(t, |x| x.max(t)));
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
        // Skipped rows (no or a bad hash) still moved the window: their
        // newest date counts like a kept row's.
        if let Some(t) = info.skipped_newest.filter(|t| plausible(*t, at)) {
            let t = t.min(at);
            newest = newest.max(t);
            tail = Some(tail.map_or(t, |x| x.max(t)));
        }
        // Saved after the finds are sent: a restart repeats a page at
        // worst, and never skips one.
        if newest > cursor {
            cursor = newest;
            save_time(state, CURSOR_KEY, cursor).await;
        }
        if info.summary.kept + info.summary.skipped.total() < ROWS {
            break;
        }
        ran_out = page == MAX_PAGES;
    }
    if ran_out {
        // Every page was full and more may follow. The cursor minus the
        // lookback could lie before this poll's rows again, so the next
        // poll carries on from the last row instead. It moves at least a
        // second, so it can't repeat a poll that read only one second.
        if let Some(t) = tail {
            save_time(state, RESUME_KEY, t.max(from + 1)).await;
        }
    } else if resume.is_some() {
        // Caught up: back to the cursor and its lookback, which starts no
        // earlier than where this run ended (see `FLOOR_KEY`).
        save_time(state, FLOOR_KEY, tail.map_or(from, |t| t.max(from))).await;
        let _ = state.remove(RESUME_KEY).await;
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

/// A saved time under `key`, or `None` when there is none or it is
/// unreadable or implausible.
async fn load_time(state: &dyn PluginState, key: &str, now: i64) -> Option<i64> {
    match state.get(key).await {
        Ok(Some(v)) => parse_cursor(&v, now),
        _ => None,
    }
}

/// Digits only, a believable date.
fn parse_cursor(v: &[u8], now: i64) -> Option<i64> {
    if v.is_empty() || v.len() > 12 || !v.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let t: i64 = std::str::from_utf8(v).ok()?.parse().ok()?;
    plausible(t, now).then_some(t)
}

async fn save_time(state: &dyn PluginState, key: &str, t: i64) {
    // A failed save only means the next poll repeats a window.
    let _ = state.set(key, t.to_string().into_bytes()).await;
}

/// The search request for items published from `since` (Unix seconds, inclusive), page `page` (from 1):
/// items with an Archive torrent, oldest first. Built from fixed text and
/// the cursor's digits, so nothing in it comes from an answer.
pub fn search_url(since: i64, page: u32) -> String {
    let (y, mo, d, h, mi, s) = civil(since);
    format!(
        "https://{HOST}/advancedsearch.php\
         ?q=format%3A%22Archive%20BitTorrent%22%20AND%20publicdate%3A%5B\
         {y:04}-{mo:02}-{d:02}T{h:02}%3A{mi:02}%3A{s:02}Z%20TO%20%2A%5D\
         &fl%5B%5D=identifier&fl%5B%5D=btih&fl%5B%5D=title&fl%5B%5D=item_size\
         &fl%5B%5D=mediatype&fl%5B%5D=collection&fl%5B%5D=publicdate\
         &sort%5B%5D=publicdate%20asc&rows={ROWS}&page={page}&output=json"
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
/// - Files, where an item lists them, become file claims. A list the
///   parser marked truncated is still true, only partial.
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
