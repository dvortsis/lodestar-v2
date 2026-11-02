//! Contract tests for the Nyaa RSS plugin (§18.9), on fixtures in
//! `fixtures/nyaa/`, with no network. `README.md` is the pattern.
//!
//! The fixtures are hand-written in the shape of Nyaa's RSS (element
//! order, the `nyaa:` set, `-0000` dates, CDATA descriptions) with made-up
//! titles and hashes. No recording was taken; see the README's note on
//! Prowlarr's fixtures. Replace them with scrubbed recordings when one is
//! available; the tests don't change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, Replayer, wait_for};
use lodestar_sources::{
    Consent, Discovery, Distribution, Endpoints, ExternalId, SourcePlugin, UserLimits, floors,
};
use lodestar_sources_feeds::rss::{Hashes, Item};
use lodestar_sources_plugins::nyaa::{self, MAX_SIZE, Nyaa, content_type, to_found};
use lodestar_store::SourceId;
use proptest::prelude::*;

const POLL: Duration = Duration::from_secs(5 * 60);
const NOW: i64 = 1_800_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/nyaa")
        .join(case)
}

fn hash(hex: &str) -> InfoHash {
    hex.parse().unwrap()
}

fn id(scheme: &str, value: &str) -> ExternalId {
    ExternalId {
        scheme: scheme.into(),
        value: value.into(),
    }
}

/// Runs the plugin on `dir` until the replayer has seen `requests`
/// requests and the plugin has sent `finds` discoveries.
async fn replay_dir(dir: &Path, requests: usize, finds: usize) -> (Vec<Discovery>, Arc<Replayer>) {
    let p = Arc::new(Nyaa::new());
    let replayer = Arc::new(Replayer::load(dir).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(6).unwrap(), replayer.clone());
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
    // Two cycles of three items. Only the feed is requested: never a
    // `.torrent` or a view page (the replayer fails on any other request).
    let (got, replayer) = replay("nyaa_rss", 2, 6).await;
    assert_spacing(&replayer, POLL);
    for (_, req) in replayer.requests() {
        assert_eq!(req.url, nyaa::FEED_URL);
    }

    let bunny: Vec<_> = got
        .iter()
        .filter(|d| d.found.key == hash("1f2e3d4c5b6a79880123456789abcdeffedcba98"))
        .collect();
    assert_eq!(bunny.len(), 2);
    let f = &bunny[0].found;
    assert!(!f.private_source && !f.interest && f.peers.is_empty());
    assert_eq!(f.strategy, nyaa::STRATEGY_RSS);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        c.name.as_deref(),
        Some("Big Buck Bunny (2008) [1080p] [ABCD1234].mkv")
    );
    assert_eq!(c.size, Some(1_503_238_554));
    assert!(c.files.is_empty() && c.trackers.is_empty() && c.publisher.is_none());
    assert!(c.description.is_none(), "the description isn't kept");
    assert_eq!(
        c.ids,
        [
            id("nyaa-category", "1_2"),
            id("content-type", "video"),
            id("nyaa-flag", "trusted")
        ]
    );
    let prov = f.provenance.as_deref().unwrap();
    assert_eq!(
        prov.external_ref.as_deref(),
        Some("https://nyaa.si/view/2000001")
    );
    assert_eq!(prov.published_at, Some(1_793_613_600));
    assert_eq!(prov.confidence, lodestar_sources::Confidence::Low);

    let album = got
        .iter()
        .find(|d| d.found.key == hash("00112233445566778899aabbccddeeff00112233"))
        .unwrap();
    let c = album.found.claims.as_deref().unwrap();
    assert_eq!(c.size, Some(512 << 20));
    assert_eq!(
        c.ids,
        [
            id("nyaa-category", "2_1"),
            id("content-type", "audio"),
            id("nyaa-flag", "remake")
        ]
    );

    // An unknown category keeps its id but no content type; a size of 0
    // and a 1997 date aren't believed.
    let odd = got
        .iter()
        .find(|d| d.found.key == hash("aabbccddeeff00112233445566778899aabbccdd"))
        .unwrap();
    let c = odd.found.claims.as_deref().unwrap();
    assert_eq!(c.size, None);
    assert_eq!(c.ids, [id("nyaa-category", "9_9")]);
    assert_eq!(odd.found.provenance.as_deref().unwrap().published_at, None);

    // Seeders, leechers, downloads and the .torrent link aren't carried.
    let all = format!("{got:?}");
    for leak in [".torrent", "download", "Category"] {
        assert!(!all.contains(leak), "{leak} in {all}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_empty_feed_finds_nothing_and_polling_goes_on() {
    replay("nyaa_empty", 2, 0).await;
}

#[tokio::test(start_paused = true)]
async fn an_item_without_a_hash_is_skipped() {
    let (got, _) = replay("nyaa_no_hash", 2, 2).await;
    assert!(
        got.iter()
            .all(|d| d.found.key == hash("99887766554433221100ffeeddccbbaa99887766"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_repeated_item_in_one_page_is_one_find() {
    let (got, _) = replay("nyaa_duplicate", 2, 2).await;
    // The first one wins: the trusted original, not the repost.
    let c = got[0].found.claims.as_deref().unwrap();
    assert_eq!(c.name.as_deref(), Some("Big Buck Bunny (2008) [1080p]"));
    assert!(c.ids.contains(&id("nyaa-flag", "trusted")));
}

#[tokio::test(start_paused = true)]
async fn a_cut_off_feed_keeps_the_items_before_the_cut() {
    let (got, _) = replay("nyaa_malformed", 2, 2).await;
    assert_eq!(
        got[0].found.key,
        hash("00112233445566778899aabbccddeeff00112233")
    );
}

#[tokio::test(start_paused = true)]
async fn a_dtd_or_a_page_that_is_not_a_feed_finds_nothing() {
    replay("nyaa_doctype", 2, 0).await;
    replay("nyaa_html", 2, 0).await;
}

#[tokio::test(start_paused = true)]
async fn a_feed_over_the_item_cap_is_cut_off() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("nyaa_oversized");
    std::fs::create_dir_all(&dir).unwrap();
    let mut body =
        String::from(r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa" version="2.0"><channel>"#);
    for i in 0..(nyaa::PAGE_LIMITS.max_items + 100) {
        body.push_str(&format!(
            "<item><title>T{i}</title><nyaa:infoHash>{:040x}</nyaa:infoHash></item>",
            i + 1
        ));
    }
    body.push_str("</channel></rss>");
    std::fs::write(dir.join("001.body"), body).unwrap();
    std::fs::copy(fixture("nyaa_empty/001.http"), dir.join("001.http")).unwrap();
    let (got, _) = replay_dir(&dir, 1, nyaa::PAGE_LIMITS.max_items).await;
    assert_eq!(got.len(), nyaa::PAGE_LIMITS.max_items);
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_for_its_retry_after_then_polling_resumes() {
    let (got, replayer) = replay("nyaa_429", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(
        times[1] - times[0] >= Duration::from_secs(1800),
        "retried after {:?}",
        times[1] - times[0]
    );
    assert_eq!(
        got[0].found.key,
        hash("1f2e3d4c5b6a79880123456789abcdeffedcba98")
    );
}

#[tokio::test(start_paused = true)]
async fn a_5xx_is_retried_later_and_is_not_an_error() {
    let (got, replayer) = replay("nyaa_5xx", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(times[1] - times[0] >= POLL);
    assert_eq!(got.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_plugin_stops_within_a_second_of_the_request() {
    let p = Arc::new(Nyaa::new());
    let replayer = Arc::new(Replayer::load(&fixture("nyaa_rss")).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(6).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the first request", || !replayer.requests().is_empty()).await;
    running.stop_within().await.unwrap();
}

#[test]
fn manifest_and_policy() {
    let p = Nyaa::new();
    let m = p.manifest();
    assert_eq!(m.id, "nyaa");
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default);
    assert_eq!(m.endpoints.len(), 1);
    assert_eq!(
        (m.endpoints[0].protocol, m.endpoints[0].host),
        ("https", "nyaa.si")
    );
    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Singleton);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(
        limits.poll_interval >= POLL,
        "§18.1: Nyaa at least 5 minutes"
    );
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    // A user can slow it, never speed it up.
    let fast = policy.limits(&UserLimits {
        poll_interval: Some(Duration::from_secs(1)),
        ..UserLimits::default()
    });
    assert_eq!(fast.poll_interval, POLL);
    // No settings and no credentials.
    let schema: serde_json::Value = serde_json::from_str(policy.config_schema).unwrap();
    assert!(schema["properties"].as_object().unwrap().is_empty());
    assert_eq!(schema["additionalProperties"], false);
    assert!(!policy.config_schema.contains("writeOnly"));
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(nyaa::FEED_URL, "https://nyaa.si/?page=rss");
}

#[test]
fn content_types_follow_the_main_category() {
    for (cat, want) in [
        ("1_1", Some("video")),
        ("1_4", Some("video")),
        ("2_2", Some("audio")),
        ("3_3", Some("book")),
        ("4_2", Some("video")),
        ("5_2", Some("image")),
        ("6_1", Some("software")),
        ("6_2", Some("game")),
        ("0_0", None),
        ("7_1", None),
        ("1", None),
        ("", None),
        ("_", None),
    ] {
        assert_eq!(content_type(cat), want, "{cat}");
    }
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
        trusted: Some(false),
        remake: Some(false),
        guid: Some("https://nyaa.si/view/1".into()),
        link: Some("https://nyaa.si/download/1.torrent".into()),
    }
}

#[test]
fn an_item_without_a_key_is_not_a_find() {
    let none = Hashes { v1: None, v2: None };
    assert!(to_found(item(none), NOW).is_none());
}

#[test]
fn flags_are_claims_only_when_yes() {
    let h = Hashes {
        v1: Some(InfoHash([3; 20])),
        v2: None,
    };
    let c = |t, r| {
        let mut it = item(h);
        (it.trusted, it.remake) = (t, r);
        to_found(it, NOW).unwrap().claims.unwrap().ids
    };
    assert_eq!(c(Some(false), Some(false)).len(), 2);
    assert_eq!(c(None, None).len(), 2);
    assert!(c(Some(true), None).contains(&id("nyaa-flag", "trusted")));
    assert!(c(None, Some(true)).contains(&id("nyaa-flag", "remake")));
}

proptest! {
    #[test]
    fn found_holds_its_invariants(
        v1 in proptest::option::of(any::<[u8; 20]>()),
        size in any::<Option<u64>>(),
        published in any::<Option<i64>>(),
        category in proptest::option::of("[0-9_]{0,16}"),
        guid in proptest::option::of("[ -~]{0,40}"),
    ) {
        let mut it = item(Hashes { v1: v1.map(InfoHash), v2: None });
        it.size = size;
        it.published = published;
        it.category = category.clone();
        it.guid = guid.clone();
        match to_found(it, NOW) {
            None => prop_assert!(v1.is_none()),
            Some(f) => {
                prop_assert!(!f.private_source && !f.interest && f.peers.is_empty());
                prop_assert_eq!(f.key, InfoHash(v1.unwrap()));
                let c = f.claims.as_deref().unwrap();
                prop_assert!(c.size.is_none_or(|s| (1..=MAX_SIZE).contains(&s)));
                prop_assert!(c.files.is_empty() && c.trackers.is_empty());
                prop_assert!(c.ids.len() <= 2);
                let p = f.provenance.as_deref().unwrap();
                let dates = nyaa::MIN_PUBLISHED..=NOW + nyaa::MAX_FUTURE;
                prop_assert!(p.published_at.is_none_or(|t| dates.contains(&t)));
                prop_assert_eq!(&p.external_ref, &guid);
                prop_assert_eq!(p.confidence, lodestar_sources::Confidence::Low);
            }
        }
    }
}
