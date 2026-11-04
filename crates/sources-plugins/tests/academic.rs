//! Contract tests for the Academic Torrents plugin (§18.9), on fixtures in
//! `fixtures/academic/`, with no network. `README.md` is the pattern.
//!
//! `academic_rss` is a scrubbed recording of `https://academictorrents.com/rss.xml` made
//! on 2026-11-04 (five of its 30 items; descriptions replaced by a
//! placeholder). The empty, no-hash, duplicate, cut-off and DTD cases are
//! hand-edited from the same items, and the 429 and 503 responses (not their
//! bodies) and the `academic_html` challenge page are hand-written.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, Replayer, wait_for};
use lodestar_sources::{
    Consent, Discovery, Distribution, Endpoints, SourcePlugin, UserLimits, floors,
};
use lodestar_sources_feeds::rss::{Hashes, Item};
use lodestar_sources_plugins::academic::{self, Academic, MAX_SIZE, to_found};
use lodestar_store::SourceId;
use proptest::prelude::*;

const POLL: Duration = Duration::from_secs(3600);
const NOW: i64 = 1_800_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/academic")
        .join(case)
}

fn hash(hex: &str) -> InfoHash {
    hex.parse().unwrap()
}

/// Runs the plugin on `dir` until the replayer has seen `requests`
/// requests and the plugin has sent `finds` discoveries.
async fn replay_dir(dir: &Path, requests: usize, finds: usize) -> (Vec<Discovery>, Arc<Replayer>) {
    let p = Arc::new(Academic::new());
    let replayer = Arc::new(Replayer::load(dir).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(9).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the recorded requests", || {
        replayer.requests().len() >= requests
    })
    .await;
    let mut got = Vec::new();
    wait_for("the discoveries", || {
        got.extend(ctx.drain());
        got.len() >= finds
    })
    .await;
    assert!(!running.is_finished(), "the plugin ended on its own");
    running.stop_within().await.unwrap();
    got.extend(ctx.drain());
    assert_eq!(got.len(), finds, "{got:#?}");
    assert_eq!(replayer.requests().len(), requests);
    replayer.finish().unwrap();
    (got, replayer)
}

async fn replay(case: &str, requests: usize, finds: usize) -> (Vec<Discovery>, Arc<Replayer>) {
    replay_dir(&fixture(case), requests, finds).await
}

/// Each URL's requests are at least `min` apart.
fn assert_spacing(replayer: &Replayer, min: Duration) {
    let mut last: HashMap<String, tokio::time::Instant> = HashMap::new();
    for (at, req) in replayer.requests() {
        if let Some(prev) = last.insert(req.url.clone(), at) {
            assert!(at - prev >= min, "{} polled after {:?}", req.url, at - prev);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_normal_page_is_found_twice_over_two_polls() {
    let (got, replayer) = replay("academic_rss", 2, 10).await;
    assert_spacing(&replayer, POLL);
    for (_, req) in replayer.requests() {
        assert_eq!(req.url, academic::FEED_URL);
    }
    let plantnet: Vec<_> = got
        .iter()
        .filter(|d| d.found.key == hash("447df5f09ca5c48b0cf2844cfffc1425ae18aaa4"))
        .collect();
    assert_eq!(plantnet.len(), 2);
    let f = &plantnet[0].found;
    assert!(!f.private_source && !f.interest && f.peers.is_empty());
    assert_eq!(f.strategy, academic::STRATEGY_RSS);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        c.name.as_deref(),
        Some(
            "Pl@ntNet-300K: a plant image dataset with high label ambiguity and a long-tailed distribution"
        )
    );
    assert_eq!(c.size, Some(31_670_505_069));
    assert!(c.files.is_empty() && c.trackers.is_empty() && c.publisher.is_none());
    assert!(c.ids.is_empty(), "no category is claimed");
    assert!(c.description.is_none(), "the description isn't kept");
    let prov = f.provenance.as_deref().unwrap();
    assert_eq!(
        prov.external_ref.as_deref(),
        Some("https://academictorrents.com/details/447df5f09ca5c48b0cf2844cfffc1425ae18aaa4")
    );
    assert_eq!(prov.published_at, None);
    assert_eq!(prov.confidence, lodestar_sources::Confidence::Low);
    // A title with an entity is read, not kept raw.
    assert!(got.iter().all(|d| {
        !d.found
            .claims
            .as_deref()
            .unwrap()
            .name
            .as_deref()
            .unwrap()
            .contains("&amp;")
    }));
}

#[tokio::test(start_paused = true)]
async fn an_empty_feed_finds_nothing_and_polling_goes_on() {
    replay("academic_empty", 2, 0).await;
}

#[tokio::test(start_paused = true)]
async fn an_item_without_a_hash_is_skipped() {
    let (got, _) = replay("academic_no_hash", 2, 4).await;
    assert!(
        got.iter()
            .all(|d| d.found.key != hash("470cfb013ee680b099b27a3ac6ae92733bcaf8f1"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_repeated_item_in_one_page_is_one_find() {
    let (got, _) = replay("academic_duplicate", 2, 4).await;
    // The first one wins, not the repost.
    let plantnet: Vec<_> = got
        .iter()
        .filter(|d| d.found.key == hash("447df5f09ca5c48b0cf2844cfffc1425ae18aaa4"))
        .collect();
    assert_eq!(plantnet.len(), 2);
    for d in plantnet {
        let name = d.found.claims.as_deref().unwrap().name.as_deref().unwrap();
        assert!(!name.contains("repost"), "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_cut_off_feed_keeps_the_items_before_the_cut() {
    let (got, _) = replay("academic_malformed", 2, 4).await;
    assert_eq!(got.len(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_dtd_or_a_page_that_is_not_a_feed_finds_nothing() {
    replay("academic_doctype", 2, 0).await;
    replay("academic_html", 2, 0).await;
}

#[tokio::test(start_paused = true)]
async fn a_feed_over_the_byte_cap_is_cut_off() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("academic_bytes_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let item = |n: u8| {
        format!(
            "<item><title>T{n}</title><infohash>{:040x}</infohash></item>",
            n
        )
    };
    let body = format!(
        "<rss version=\"2.0\"><channel>{}<!--{}-->{}</channel></rss>",
        item(1),
        "x".repeat(academic::PAGE_LIMITS.max_bytes + 1024),
        item(2)
    );
    std::fs::write(dir.join("001.body"), body).unwrap();
    std::fs::copy(fixture("academic_rss/001.http"), dir.join("001.http")).unwrap();
    let (got, _) = replay_dir(&dir, 1, 1).await;
    assert_eq!(got[0].found.key, hash(&format!("{:040x}", 1)));
}

#[tokio::test(start_paused = true)]
async fn a_feed_over_the_item_cap_is_cut_off() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("academic_oversized_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = String::from(r#"<rss version="2.0"><channel>"#);
    for i in 0..(academic::PAGE_LIMITS.max_items + 100) {
        body.push_str(&format!(
            "<item><title>T{i}</title><infohash>{:040x}</infohash></item>",
            i + 1
        ));
    }
    body.push_str("</channel></rss>");
    std::fs::write(dir.join("001.body"), body).unwrap();
    std::fs::copy(fixture("academic_rss/001.http"), dir.join("001.http")).unwrap();
    let (got, _) = replay_dir(&dir, 1, academic::PAGE_LIMITS.max_items).await;
    assert_eq!(got.len(), academic::PAGE_LIMITS.max_items);
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_for_its_retry_after_then_polling_resumes() {
    let (got, replayer) = replay("academic_429", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(
        times[1] - times[0] >= Duration::from_secs(7200),
        "retried after {:?}",
        times[1] - times[0]
    );
    assert_eq!(
        got[0].found.key,
        hash("447df5f09ca5c48b0cf2844cfffc1425ae18aaa4")
    );
}

#[tokio::test(start_paused = true)]
async fn a_5xx_is_retried_later_and_is_not_an_error() {
    let (got, replayer) = replay("academic_5xx", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(times[1] - times[0] >= POLL);
    assert_eq!(got.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_plugin_stops_within_a_second_of_the_request() {
    let p = Arc::new(Academic::new());
    let replayer = Arc::new(Replayer::load(&fixture("academic_rss")).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(9).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the first request", || !replayer.requests().is_empty()).await;
    running.stop_within().await.unwrap();
}

#[test]
fn manifest_and_policy() {
    let p = Academic::new();
    let m = p.manifest();
    assert_eq!(m.id, "academic_torrents");
    assert_eq!(m.consent, Consent::None, "§18.7");
    assert!(!m.enabled_by_default);
    assert_eq!(m.endpoints.len(), 1);
    assert_eq!(
        (m.endpoints[0].protocol, m.endpoints[0].host),
        ("https", "academictorrents.com")
    );
    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Singleton);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.poll_interval >= POLL, "Q4: at most hourly");
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    // A user can slow it, never speed it up.
    let fast = policy.limits(&UserLimits {
        poll_interval: Some(Duration::from_secs(1)),
        ..UserLimits::default()
    });
    assert_eq!(fast.poll_interval, POLL);
    let schema: serde_json::Value = serde_json::from_str(policy.config_schema).unwrap();
    assert!(schema["properties"].as_object().unwrap().is_empty());
    assert_eq!(schema["additionalProperties"], false);
    assert!(!policy.config_schema.contains("writeOnly"));
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(academic::FEED_URL, "https://academictorrents.com/rss.xml");
}

fn item(hashes: Hashes) -> Item {
    Item {
        hashes,
        title: Some("T".into()),
        size: Some(1),
        published: Some(NOW),
        seeders: Some(5),
        leechers: Some(1),
        downloads: Some(9),
        category: Some("1_2".into()),
        trusted: Some(true),
        remake: Some(true),
        guid: Some("https://academictorrents.com/details/1".into()),
        link: Some("https://academictorrents.com/details/1".into()),
    }
}

#[test]
fn an_item_without_a_key_is_not_a_find() {
    let none = Hashes { v1: None, v2: None };
    assert!(to_found(item(none), NOW).is_none());
}

#[test]
fn nothing_but_the_title_and_size_is_claimed() {
    let h = Hashes {
        v1: Some(InfoHash([3; 20])),
        v2: None,
    };
    let f = to_found(item(h), NOW).unwrap();
    let c = f.claims.as_deref().unwrap();
    assert!(c.ids.is_empty() && c.files.is_empty() && c.trackers.is_empty());
    assert!(!format!("{f:?}").contains("trusted"));
}

proptest! {
    #[test]
    fn found_holds_its_invariants(
        v1 in proptest::option::of(any::<[u8; 20]>()),
        size in any::<Option<u64>>(),
        published in any::<Option<i64>>(),
        guid in proptest::option::of("[ -~]{0,40}"),
    ) {
        let mut it = item(Hashes { v1: v1.map(InfoHash), v2: None });
        it.size = size;
        it.published = published;
        it.guid = guid.clone();
        match to_found(it, NOW) {
            None => prop_assert!(v1.is_none()),
            Some(f) => {
                prop_assert!(!f.private_source && !f.interest && f.peers.is_empty());
                prop_assert_eq!(f.key, InfoHash(v1.unwrap()));
                let c = f.claims.as_deref().unwrap();
                prop_assert!(c.size.is_none_or(|s| (1..=MAX_SIZE).contains(&s)));
                prop_assert!(c.files.is_empty() && c.trackers.is_empty() && c.ids.is_empty());
                let p = f.provenance.as_deref().unwrap();
                let dates = academic::MIN_PUBLISHED..=NOW + academic::MAX_FUTURE;
                prop_assert!(p.published_at.is_none_or(|t| dates.contains(&t)));
                prop_assert_eq!(&p.external_ref, &guid);
                prop_assert_eq!(p.confidence, lodestar_sources::Confidence::Low);
            }
        }
    }
}
