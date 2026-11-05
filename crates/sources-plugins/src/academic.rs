//! Academic Torrents' RSS, source plugin (architecture §18.2 rank 19).
//!
//! Academic Torrents' public feed of recent uploads
//! (`https://academictorrents.com/rss.xml`, about 30 items), polled at
//! most once an hour (plan Q4). It is a fixed endpoint with no account,
//! so there is no address to enter and no credential. Each poll:
//!
//! 1. fetches the feed through `ctx.kit.http` (`Client::poll`, so the
//!    poll gate spaces it and a 429 or 5xx backs the host off);
//! 2. parses it with SRC-8's bounded RSS parser, which refuses DTDs,
//!    caps size, depth and item count, and reads the bare `<infohash>`
//!    element (SRC-8b);
//! 3. maps each item to a key with claims ([`to_found`], a pure function).
//!
//! **Nothing else is fetched.** The item's `/details/<hash>` page is kept
//! as the external reference but never requested, and there is no
//! `.torrent` link to follow. Metadata comes from the DHT, as for any
//! unverified key.
//!
//! **Every field is a claim** at Low confidence: the title and the size.
//! The feed's `<category>` ("Dataset") is not read by the parser's `Item`
//! (SRC-8b), so no content type is claimed; the classifier decides.
//! Seeders and leechers aren't in the feed.
//!
//! Only Academic Torrents is covered. Official distribution lists are a
//! follow-up (decisions file).

use std::collections::HashSet;
use std::time::Duration;

use lodestar_sources::claims::{Claims, Confidence, Provenance};
use lodestar_sources::{
    Backoff, BoxFuture, Caps, Client, Consent, Distribution, Emitter, Endpoint, Endpoints, Found,
    HostRate, Kind, Manifest, Network, PluginError, Policy, Priority, Request, Risk, Schedule,
    SourceCtx, SourcePlugin,
};
use lodestar_sources_feeds::rss::{self, Item, LIMITS, Limits};

/// The plugin's id in the store. Never rename.
pub const ID: &str = "academic_torrents";
/// The strategy code on every key this plugin emits: the RSS feed.
pub const STRATEGY_RSS: u8 = 1;
/// The feed. Fixed: no user-entered address, no query.
pub const FEED_URL: &str = "https://academictorrents.com/rss.xml";
/// The host the manifest declares and egress is narrowed to.
pub const HOST: &str = "academictorrents.com";

/// The largest size claim believed: 1 PiB.
pub const MAX_SIZE: u64 = 1 << 50;
/// The earliest publish date believed: 2001-07-02, BitTorrent's release.
pub const MIN_PUBLISHED: i64 = 994_032_000;
/// How far past now a publish date may be (clock skew, time zones).
pub const MAX_FUTURE: i64 = 24 * 3600;

/// The caps for one feed page. Academic Torrents sends about 30 items; a feed with more than
/// four times that is cut off by the parser and the rest is ignored.
pub const PAGE_LIMITS: Limits = Limits {
    max_items: 150,
    max_bytes: 1024 * 1024,
    ..LIMITS
};

pub(crate) const MANIFEST: Manifest = Manifest {
    id: ID,
    name: "Academic Torrents",
    version: "1",
    kinds: &[Kind::Discovery],
    networks: &[Network::Bt],
    endpoints: &[Endpoint {
        protocol: "https",
        host: HOST,
    }],
    risk: Risk::Low,
    // §18.7: Academic/official lists are consent class None (a toggle).
    consent: Consent::None,
    default_priority: Priority::Normal,
    enabled_by_default: false,
    always_adult: false,
};

/// No settings: the endpoint is fixed.
pub const CONFIG_SCHEMA: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{},"additionalProperties":false}"#;

/// Plan Q4: Academic lists hourly. One feed, one host, so
/// the per-host rate only matters after a back-off.
const POLICY: Policy = Policy {
    caps: Caps {
        min_poll_interval: Duration::from_secs(3600),
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
    // One agent: the setting can't be changed (SRC-14a).
    max_agents: 1,
};

/// The Academic Torrents plugin.
#[derive(Debug, Default)]
pub struct Academic;

impl Academic {
    pub fn new() -> Academic {
        Academic
    }

    /// Where the plugin's egress may send requests (SRC-5): the manifest's
    /// host only, public addresses only, with DNS through the proxy when
    /// one is set.
    pub fn endpoints(&self) -> Endpoints {
        Endpoints::manifest(MANIFEST.endpoints)
    }
}

impl SourcePlugin for Academic {
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

/// One feed item as a discovery. Every field stays a claim at Low
/// confidence; the key is the parser's, from the bare `<infohash>`.
/// `None` for an item with no usable key.
///
/// - The reference is the item's `guid` (the details page). It is never
///   requested.
/// - A size outside 1 B to 1 PiB and a date before BitTorrent or more
///   than a day ahead are dropped.
/// - The category is not claimed (`Item` doesn't carry the feed's
///   free-text one). Nobody is named: there is no uploader (§18.7).
pub fn to_found(item: Item, now: i64) -> Option<Found> {
    let key = item.hashes.key()?;
    let claims = Claims {
        name: item.title,
        size: item.size.filter(|s| (1..=MAX_SIZE).contains(s)),
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
