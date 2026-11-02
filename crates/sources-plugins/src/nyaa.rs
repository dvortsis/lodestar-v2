//! Nyaa RSS, source plugin #2 (architecture §18.2 rank 8).
//!
//! Nyaa's public RSS feed (`https://nyaa.si/?page=rss`), the newest
//! uploads, polled at most every 5 minutes (§18.1). It is a fixed
//! endpoint with no account, so there is no address to enter and no
//! credential. Each poll:
//!
//! 1. fetches the feed through `ctx.kit.http` (`Client::poll`, so the
//!    poll gate spaces it and a 429 or 5xx backs the host off);
//! 2. parses it with SRC-8's bounded RSS parser, which refuses DTDs,
//!    caps size, depth and item count, and takes the info hash only from
//!    `nyaa:infoHash` or a magnet;
//! 3. maps each item to a key with claims ([`to_found`], a pure function).
//!
//! **Nothing else is fetched.** The `.torrent` link and the view page are
//! kept (the view page as the external reference) but never requested.
//! Metadata comes from the DHT, as for any unverified key.
//!
//! **Every field is a claim** at Low confidence: the title, the size, the
//! category (as its Nyaa id and the content type it maps to) and Nyaa's
//! `trusted` and `remake` flags as Nyaa states them. Seeders, leechers
//! and downloads are not kept: [`Claims`] has no field for them, and a
//! swarm count from a feed is stale by the time it's read (the crawler
//! measures swarms itself).

use std::collections::HashSet;
use std::time::Duration;

use lodestar_sources::claims::{Claims, Confidence, ExternalId, Provenance};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, Consent, Distribution, Emitter, Endpoint, Endpoints, Found,
    HostRate, Kind, Manifest, Network, PluginError, Policy, Priority, Request, Risk, Schedule,
    SourceCtx, SourcePlugin,
};
use lodestar_sources_feeds::rss::{self, Item, LIMITS, Limits};

/// The plugin's id in the store. Never rename.
pub const ID: &str = "nyaa";
/// The strategy code on every key this plugin emits: the home RSS feed.
pub const STRATEGY_RSS: u8 = 1;
/// The feed. Fixed: no user-entered address, no query, no `&magnets`.
pub const FEED_URL: &str = "https://nyaa.si/?page=rss";
/// The host the manifest declares and egress is narrowed to.
pub const HOST: &str = "nyaa.si";

/// The largest size claim believed: 1 PiB.
pub const MAX_SIZE: u64 = 1 << 50;
/// The earliest publish date believed: 2001-07-02, BitTorrent's release.
pub const MIN_PUBLISHED: i64 = 994_032_000;
/// How far past now a publish date may be (clock skew, time zones).
pub const MAX_FUTURE: i64 = 24 * 3600;

/// The caps for one feed page. Nyaa sends 75 items; a feed with more than
/// four times that is cut off by the parser and the rest is ignored.
pub const PAGE_LIMITS: Limits = Limits {
    max_items: 300,
    max_bytes: 4 * 1024 * 1024,
    ..LIMITS
};

const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Nyaa",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Medium,
    consent: Consent::External,
    default_priority: Priority::Normal,
    enabled_by_default: false,
};

/// No settings: the endpoint is fixed.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// §18.1: Nyaa at least 5 minutes between polls. One feed, one host, so
/// the per-host rate only matters after a back-off.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(5 * 60),
        per_host: HostRate {
            interval: Duration::from_secs(5),
            burst: 1,
        },
        max_full_scrapes_per_day: 0,
        backoff: Backoff {
            base: Duration::from_secs(60),
            max: Duration::from_secs(6 * 3600),
        },
    },
    config_schema: CONFIG_SCHEMA,
    // §18.2/§19.5: one instance polls the feed.
    distribution: Distribution::Singleton,
    schedule: Schedule::ALWAYS,
};

/// The Nyaa plugin.
#[derive(Debug, Default)]
pub struct Nyaa;

impl Nyaa {
    pub fn new() -> Nyaa {
        Nyaa
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, public addresses only, with DNS through the proxy when
    /// one is set.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

impl SourcePlugin for Nyaa {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn policy(&self) -> &Policy {
        &POLICY
    }

    fn run(&self, ctx: SourceCtx) -> BoxFuture<'_, Result<(), PluginError>> {
        Box::pin(run(ctx))
    }
}

async fn run(ctx: SourceCtx) -> Result<(), PluginError> {
    let SourceCtx {
        emitter,
        mut shutdown,
        kit,
        ..
    } = ctx;
    let http = kit.http;
    loop {
        let polled = tokio::select! {
            _ = shutdown.requested() => return Ok(()),
            r = poll(&http, &emitter) => r,
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

async fn poll(http: &Client, emitter: &Emitter) -> Poll {
    let req = Request::get(FEED_URL).header("Accept", "application/rss+xml, application/xml;q=0.9");
    let body = match http.poll("feed", &req).await {
        Ok(res) if res.status == 200 => res.body,
        _ => return Poll::Done,
    };
    let (now, mut seen) = (unix_now(), HashSet::new());
    let mut found = Vec::new();
    rss::parse_each(&body, &PAGE_LIMITS, |item| {
        // A repeated item in one page is one find.
        if let Some(f) = to_found(item, now)
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
    Poll::Done
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// What a Nyaa category id (`1_2`) means for the content type claim, as a
/// `ContentType::as_str` name (`lodestar-classify`). Ids are
/// `<main>_<sub>`; the main category decides, except AMV (`1_1`, video
/// rather than tv) and Software, where Games are told apart. Anime is
/// `tv` as the best guess for most of it; the claim is Low confidence and
/// the classifier has the last word. `None` for an id that isn't known.
pub fn content_type(category: &str) -> Option<&'static str> {
    let (main, sub) = category.split_once('_')?;
    match (main, sub) {
        ("1", "1") | ("4", _) => Some("video"),
        ("1", _) => Some("tv"),
        ("2", _) => Some("music"),
        ("3", _) => Some("ebook"),
        ("5", _) => Some("images"),
        ("6", "2") => Some("game"),
        ("6", _) => Some("software"),
        _ => None,
    }
}

/// One feed item as a discovery. Every field stays a claim at Low
/// confidence; the key is the parser's, from `nyaa:infoHash` or a magnet.
/// `None` for an item with no usable key.
///
/// - The reference is the item's `guid` (the view page). The `.torrent`
///   link is never kept: nothing here fetches it.
/// - A size outside 1 B to 1 PiB and a date before BitTorrent or more
///   than a day ahead are dropped.
/// - The category becomes `nyaa-category:<id>` and, when it's a known
///   one, `content-type:<type>` ([`content_type`]). `trusted` and `remake`
///   become `nyaa-flag:trusted` and `nyaa-flag:remake` when Nyaa says
///   `Yes`. Nobody is named: there is no uploader (§18.7).
pub fn to_found(item: Item, now: i64) -> Option<Found> {
    let key = item.hashes.key()?;
    let mut ids = Vec::new();
    let mut id = |scheme: &str, value: &str| {
        ids.push(ExternalId {
            scheme: scheme.to_owned(),
            value: value.to_owned(),
        });
    };
    if let Some(cat) = item.category.as_deref() {
        id("nyaa-category", cat);
        if let Some(t) = content_type(cat) {
            id("content-type", t);
        }
    }
    if item.trusted == Some(true) {
        id("nyaa-flag", "trusted");
    }
    if item.remake == Some(true) {
        id("nyaa-flag", "remake");
    }
    let claims = Claims {
        name: item.title,
        size: item.size.filter(|s| (1..=MAX_SIZE).contains(s)),
        ids,
        ..Claims::default()
    };
    let published_at = item
        .published
        .filter(|t| (MIN_PUBLISHED..=now.saturating_add(MAX_FUTURE)).contains(t));
    Some(
        Found::key(key, STRATEGY_RSS)
            .with_claims(claims)
            .with_provenance(Provenance {
                external_ref: item.guid,
                published_at,
                confidence: Confidence::Low,
            }),
    )
}
