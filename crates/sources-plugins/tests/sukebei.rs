//! Contract tests for the Sukebei variant of the Nyaa plugin (SRC-10e), on
//! fixtures in `fixtures/sukebei/`, with no network. `README.md` is the
//! pattern; `tests/nyaa.rs` covers the parser paths the two share.
//!
//! The fixtures are hand-written and synthetic (clean, made-up titles and
//! hashes; no real adult titles) in the shape of Nyaa's RSS. No recording
//! was taken: the sandbox cannot reach the site.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, Replayer, wait_for};
use lodestar_sources::{
    Consent, Discovery, Distribution, Endpoints, ExternalId, SourcePlugin, UserLimits, floors,
};
use lodestar_sources_feeds::rss::{Hashes, Item};
use lodestar_sources_plugins::nyaa::{self, Nyaa, Sukebei, to_found, to_found_adult};
use lodestar_store::SourceId;
use proptest::prelude::*;

const POLL: Duration = Duration::from_secs(5 * 60);
const NOW: i64 = 1_800_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/sukebei")
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

async fn replay(case: &str, requests: usize, finds: usize) -> (Vec<Discovery>, Arc<Replayer>) {
    let p = Arc::new(Sukebei::new());
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(7).unwrap(), replayer.clone());
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

fn content_types(d: &Discovery) -> Vec<String> {
    d.found
        .claims
        .as_deref()
        .unwrap()
        .ids
        .iter()
        .filter(|i| i.scheme == "content-type")
        .map(|i| i.value.clone())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn every_item_is_found_as_adult_whatever_its_feed_category() {
    // Categories 1_2 (tv under Nyaa), 2_2 (video) and 9_9 (unknown) all
    // end up as Adult. Only Sukebei's feed is requested, never a view
    // page or a `.torrent`.
    let (got, replayer) = replay("sukebei_rss", 2, 6).await;
    let mut last = None;
    for (at, req) in replayer.requests() {
        assert_eq!(req.url, nyaa::SUKEBEI_FEED_URL);
        if let Some(prev) = last.replace(at) {
            assert!(at - prev >= POLL, "polled after {:?}", at - prev);
        }
    }
    for d in &got {
        assert_eq!(content_types(d), ["adult"], "{d:#?}");
        let f = &d.found;
        assert!(!f.private_source && !f.interest && f.peers.is_empty());
        let c = f.claims.as_deref().unwrap();
        assert!(c.files.is_empty() && c.trackers.is_empty() && c.publisher.is_none());
        assert_eq!(
            f.provenance.as_deref().unwrap().confidence,
            lodestar_sources::Confidence::Low
        );
        assert!(
            f.provenance
                .as_deref()
                .unwrap()
                .external_ref
                .as_deref()
                .is_some_and(|r| r.starts_with("https://sukebei.nyaa.si/view/"))
        );
    }
    let first = got
        .iter()
        .find(|d| d.found.key == hash("1f2e3d4c5b6a79880123456789abcdeffedcba98"))
        .unwrap();
    // The feed's own category is still a claim; the content type is forced.
    assert_eq!(
        first.found.claims.as_deref().unwrap().ids,
        [
            id("nyaa-category", "1_2"),
            id("nyaa-flag", "trusted"),
            id("content-type", "adult"),
        ]
    );
    let odd = got
        .iter()
        .find(|d| d.found.key == hash("aabbccddeeff00112233445566778899aabbccdd"))
        .unwrap();
    assert_eq!(
        odd.found.claims.as_deref().unwrap().ids,
        [id("nyaa-category", "9_9"), id("content-type", "adult")]
    );
}

#[tokio::test(start_paused = true)]
async fn a_blocklisted_hash_is_an_ordinary_find_for_the_filter_to_judge() {
    // The plugin never filters or skips on its own and has no way round
    // the filter: a hash on a blocklist arrives like any other find, with
    // no flag that would let it past the child-safety core or the floor,
    // and the pipeline holds it before storage (`crates/pipeline/tests/
    // unverified.rs`; SRC-G runs the cross-plugin injection test).
    let (got, _) = replay("sukebei_blocked", 2, 4).await;
    let blocked = hash("b10c4ed0b10c4ed0b10c4ed0b10c4ed0b10c4ed0");
    let hits: Vec<_> = got.iter().filter(|d| d.found.key == blocked).collect();
    assert_eq!(hits.len(), 2);
    for d in got {
        let f = d.found;
        assert!(!f.private_source && !f.interest && f.peers.is_empty());
        assert_eq!(f.strategy, nyaa::STRATEGY_RSS);
    }
}

#[tokio::test(start_paused = true)]
async fn an_empty_feed_finds_nothing_and_polling_goes_on() {
    replay("sukebei_empty", 2, 0).await;
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_for_its_retry_after_then_polling_resumes() {
    let (got, replayer) = replay("sukebei_429", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(times[1] - times[0] >= Duration::from_secs(1800));
    assert_eq!(content_types(&got[0]), ["adult"]);
}

#[tokio::test(start_paused = true)]
async fn a_5xx_is_retried_later_and_is_not_an_error() {
    let (got, replayer) = replay("sukebei_5xx", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(times[1] - times[0] >= POLL);
    assert_eq!(got.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_plugin_stops_within_a_second_of_the_request() {
    let p = Arc::new(Sukebei::new());
    let replayer = Arc::new(Replayer::load(&fixture("sukebei_rss")).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(7).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the first request", || !replayer.requests().is_empty()).await;
    running.stop_within().await.unwrap();
}

#[test]
fn manifest_and_policy() {
    let p = Sukebei::new();
    let m = p.manifest();
    assert_eq!(m.id, "sukebei");
    assert_ne!(m.id, Nyaa::new().manifest().id);
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default, "off by default");
    assert_eq!(m.endpoints.len(), 1);
    assert_eq!(
        (m.endpoints[0].protocol, m.endpoints[0].host),
        ("https", "sukebei.nyaa.si")
    );
    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Singleton);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.poll_interval >= POLL, "§18.1: at least 5 minutes");
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
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
    assert_eq!(nyaa::SUKEBEI_FEED_URL, "https://sukebei.nyaa.si/?page=rss");
    // Nyaa itself is unchanged.
    assert_eq!(Nyaa::new().manifest().endpoints[0].host, "nyaa.si");
}

fn item(category: Option<&str>) -> Item {
    Item {
        hashes: Hashes {
            v1: Some(InfoHash([3; 20])),
            v2: None,
        },
        title: Some("T".into()),
        size: Some(1),
        published: Some(NOW),
        seeders: None,
        leechers: None,
        downloads: None,
        category: category.map(Into::into),
        trusted: Some(false),
        remake: Some(false),
        guid: Some("https://sukebei.nyaa.si/view/1".into()),
        link: None,
    }
}

#[test]
fn adult_is_forced_for_every_category_and_for_none() {
    for cat in [
        None,
        Some("1_1"),
        Some("1_2"),
        Some("2_2"),
        Some("6_2"),
        Some("9_9"),
        Some(""),
        Some("garbage"),
    ] {
        let f = to_found_adult(item(cat), NOW).unwrap();
        let ids = f.claims.unwrap().ids;
        let types: Vec<_> = ids.iter().filter(|i| i.scheme == "content-type").collect();
        assert_eq!(types.len(), 1, "{cat:?}: {ids:?}");
        assert_eq!(types[0].value, "adult", "{cat:?}");
    }
    // Plain Nyaa mapping is untouched.
    let f = to_found(item(Some("2_1")), NOW).unwrap();
    assert!(f.claims.unwrap().ids.contains(&id("content-type", "music")));
}

#[test]
fn an_item_without_a_key_is_not_a_find() {
    let mut it = item(Some("1_2"));
    it.hashes = Hashes { v1: None, v2: None };
    assert!(to_found_adult(it, NOW).is_none());
}

proptest! {
    #[test]
    fn adult_found_always_has_exactly_one_adult_content_type(
        v1 in proptest::option::of(any::<[u8; 20]>()),
        category in proptest::option::of("[0-9_a-z]{0,16}"),
    ) {
        let mut it = item(category.as_deref());
        it.hashes = Hashes { v1: v1.map(InfoHash), v2: None };
        match to_found_adult(it, NOW) {
            None => prop_assert!(v1.is_none()),
            Some(f) => {
                let ids = f.claims.as_deref().unwrap().ids.clone();
                let types: Vec<_> = ids.iter().filter(|i| i.scheme == "content-type").collect();
                prop_assert_eq!(types.len(), 1);
                prop_assert_eq!(types[0].value.as_str(), "adult");
                prop_assert!(!f.private_source && !f.interest);
            }
        }
    }
}
