//! Contract tests for the Internet Archive plugin (§18.9), on fixtures in
//! `fixtures/archive_org/`, with no network. `README.md` is the pattern.
//!
//! The fixtures are hand-written in the shape of `advancedsearch.php`
//! answers (key order, string-or-array fields, string sizes, `btih`,
//! RFC 3339 `publicdate`) with made-up identifiers and hashes and
//! open-licence titles. No recording was taken; the sandbox that wrote
//! them had no route to archive.org. Replace them with scrubbed
//! recordings when one is available; the tests don't change.
//!
//! The plugin's clock is fixed at [`NOW`], so request URLs are stable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, Replayer, wait_for};
use lodestar_sources::{
    ClaimedFile, Consent, Discovery, Distribution, Endpoints, ExternalId, PluginState,
    SourcePlugin, UserLimits, floors,
};
use lodestar_sources_feeds::archive_org::{File, Hashes, Item};
use lodestar_sources_plugins::archive_org::{
    self, ArchiveOrg, CURSOR_KEY, FIRST_WINDOW, MAX_SIZE, ROWS, content_type, search_url, to_found,
};
use lodestar_store::SourceId;
use proptest::prelude::*;

const POLL: Duration = Duration::from_secs(3600);
/// 2026-11-03T12:00:00Z.
const NOW: i64 = 1_793_707_200;
/// The newest `publicdate` in `archive_normal` (2026-11-03T09:15:00Z).
const NEWEST: i64 = 1_793_697_300;

fn clock() -> i64 {
    NOW
}

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/archive_org")
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

/// Runs the plugin on `dir`, with `seed` as its saved cursor, until the
/// replayer has seen `requests` requests and the plugin has sent `finds`
/// discoveries. Returns the finds, the replayer and the cursor left in
/// the plugin's state.
async fn replay_with(
    dir: &Path,
    seed: Option<&[u8]>,
    requests: usize,
    finds: usize,
) -> (Vec<Discovery>, Arc<Replayer>, Option<Vec<u8>>) {
    let p = Arc::new(ArchiveOrg::with_clock(clock));
    let replayer = Arc::new(Replayer::load(dir).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(7).unwrap(), replayer.clone());
    if let Some(v) = seed {
        ctx.state.set(CURSOR_KEY, v.to_vec()).await.unwrap();
    }
    let state = ctx.state.clone();
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
    let cursor = state.get(CURSOR_KEY).await.unwrap();
    (got, replayer, cursor)
}

async fn replay(case: &str, requests: usize, finds: usize) -> (Vec<Discovery>, Arc<Replayer>) {
    let (got, replayer, _) = replay_with(&fixture(case), None, requests, finds).await;
    (got, replayer)
}

/// A scratch fixture directory.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("archive_org_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes exchange `n` of a scratch fixture: a 200 answer to the search
/// after `since`.
fn write_exchange(dir: &Path, n: usize, since: i64, body: &str) {
    std::fs::write(
        dir.join(format!("{n:03}.http")),
        format!(
            "GET {}\nAccept: application/json\n\nHTTP 200\nContent-Type: application/json\n",
            search_url(since)
        ),
    )
    .unwrap();
    std::fs::write(dir.join(format!("{n:03}.body")), body).unwrap();
}

/// `n` made-up docs whose `publicdate` runs from `start` up a second each.
fn docs(first: usize, n: usize, start: i64) -> String {
    let docs: Vec<String> = (0..n)
        .map(|i| {
            let t = start + i as i64;
            format!(
                r#"{{"identifier":"item-{k}","title":"Item {k}","btih":"{:040x}","publicdate":"{}"}}"#,
                first + i + 1,
                rfc3339(t),
                k = first + i
            )
        })
        .collect();
    format!(
        r#"{{"response":{{"numFound":{n},"docs":[{}]}}}}"#,
        docs.join(",")
    )
}

fn rfc3339(t: i64) -> String {
    // Within one day of 2026-11-03T00:00:00Z, which is all the tests need.
    let day = 1_793_664_000;
    assert!((day..day + 86_400).contains(&t));
    let s = t - day;
    format!(
        "2026-11-03T{:02}:{:02}:{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
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
async fn a_normal_page_is_found_and_the_cursor_moves_to_the_newest() {
    // Poll 1 asks for the last day and gets three items; poll 2, an hour
    // later, asks only for what is newer than the newest and gets none.
    let (got, replayer, cursor) = replay_with(&fixture("archive_normal"), None, 2, 3).await;
    let urls: Vec<_> = replayer
        .requests()
        .into_iter()
        .map(|(_, r)| r.url)
        .collect();
    assert_eq!(urls[0], search_url(NOW - FIRST_WINDOW));
    assert_eq!(urls[1], search_url(NEWEST));
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(times[1] - times[0] >= POLL);
    assert_eq!(cursor.as_deref(), Some(NEWEST.to_string().as_bytes()));

    let f = &got[0].found;
    assert_eq!(f.key, hash("1f2e3d4c5b6a79880123456789abcdeffedcba98"));
    assert!(!f.private_source && !f.interest && f.peers.is_empty());
    assert_eq!(f.strategy, archive_org::STRATEGY_SEARCH);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(c.name.as_deref(), Some("Big Buck Bunny (2008)"));
    assert_eq!(c.size, Some(276_134_947));
    assert!(c.files.is_empty() && c.trackers.is_empty() && c.publisher.is_none());
    assert!(c.description.is_none());
    assert_eq!(
        c.ids,
        [
            id("archive-org", "big-buck-bunny-example"),
            id("archive-mediatype", "movies"),
            id("content-type", "video"),
            id("archive-collection", "opensource_movies"),
            id("archive-collection", "community"),
        ]
    );
    let prov = f.provenance.as_deref().unwrap();
    assert_eq!(
        prov.external_ref.as_deref(),
        Some("https://archive.org/details/big-buck-bunny-example")
    );
    assert_eq!(prov.published_at, Some(1_793_624_400));
    assert_eq!(prov.confidence, lodestar_sources::Confidence::Low);

    // A base32 hash, a string size and an array title (first one wins).
    let sintel = &got[1].found;
    assert_eq!(
        sintel.claims.as_deref().unwrap().name.as_deref(),
        Some("Sintel Soundtrack")
    );
    assert_eq!(sintel.claims.as_deref().unwrap().size, Some(52_428_800));

    // A size of 0 isn't believed.
    assert_eq!(got[2].found.claims.as_deref().unwrap().size, None);

    // Only the search is requested: never a `.torrent`, a details page or
    // metadata (the replayer fails on any other request). Nothing the
    // Archive says about uploaders, downloads or descriptions is kept.
    let all = format!("{got:?}");
    for leak in [".torrent", "download", "uploader"] {
        assert!(!all.contains(leak), "{leak} in {all}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_empty_answer_finds_nothing_and_the_cursor_stays() {
    let (_, replayer, cursor) = replay_with(&fixture("archive_empty"), None, 2, 0).await;
    assert_spacing(&replayer, POLL);
    assert_eq!(cursor, None, "nothing new, nothing saved");
    for (_, req) in replayer.requests() {
        assert_eq!(req.url, search_url(NOW - FIRST_WINDOW));
    }
}

#[tokio::test(start_paused = true)]
async fn a_saved_cursor_resumes_without_refetching() {
    // After a restart the first request is the one after the saved cursor.
    let dir = scratch("resume");
    write_exchange(&dir, 1, NEWEST, r#"{"response":{"numFound":0,"docs":[]}}"#);
    write_exchange(&dir, 2, NEWEST, r#"{"response":{"numFound":0,"docs":[]}}"#);
    let (_, replayer, cursor) = replay_with(&dir, Some(NEWEST.to_string().as_bytes()), 2, 0).await;
    assert_eq!(cursor.as_deref(), Some(NEWEST.to_string().as_bytes()));
    assert_eq!(replayer.requests()[0].1.url, search_url(NEWEST));
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_or_implausible_cursor_starts_a_day_back() {
    for bad in [
        b"abc".as_slice(),
        b"",
        b"-5",
        b"12.5",
        b"0",
        b"99999999999",
        b"1793707200 ",
        &[0xff, 0xfe],
    ] {
        let (_, replayer, _) = replay_with(&fixture("archive_empty"), Some(bad), 2, 0).await;
        assert_eq!(
            replayer.requests()[0].1.url,
            search_url(NOW - FIRST_WINDOW),
            "{bad:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn an_item_without_a_usable_hash_is_skipped() {
    // One missing `btih`, one that isn't a hash; the cursor follows the
    // item that was kept.
    let (got, _, cursor) = replay_with(&fixture("archive_no_hash"), None, 2, 1).await;
    assert_eq!(
        got[0].found.key,
        hash("1f2e3d4c5b6a79880123456789abcdeffedcba98")
    );
    assert_eq!(cursor.as_deref(), Some(b"1793624400".as_slice()));
}

#[tokio::test(start_paused = true)]
async fn a_repeated_item_in_one_page_is_one_find() {
    let (got, _) = replay("archive_duplicate", 2, 2).await;
    // The first one wins: the original, not the repost.
    let c = got[0].found.claims.as_deref().unwrap();
    assert_eq!(c.name.as_deref(), Some("Big Buck Bunny (2008)"));
}

#[tokio::test(start_paused = true)]
async fn a_cut_off_answer_keeps_the_items_before_the_cut() {
    let (got, _) = replay("archive_malformed", 2, 1).await;
    assert_eq!(
        got[0].found.key,
        hash("1f2e3d4c5b6a79880123456789abcdeffedcba98")
    );
}

#[tokio::test(start_paused = true)]
async fn a_page_that_is_not_json_finds_nothing_and_polling_goes_on() {
    replay("archive_html", 2, 0).await;
}

#[tokio::test(start_paused = true)]
async fn an_answer_over_the_item_cap_is_cut_off_and_followed_up() {
    // More docs than the parser takes: it keeps its cap, and the full
    // page is followed by a next one at once, which is empty.
    let cap = archive_org::PAGE_LIMITS.max_items;
    let dir = scratch("oversized");
    let start = 1_793_664_000;
    write_exchange(&dir, 1, NOW - FIRST_WINDOW, &docs(0, cap + 100, start));
    write_exchange(
        &dir,
        2,
        start + cap as i64 - 1,
        r#"{"response":{"numFound":0,"docs":[]}}"#,
    );
    let (got, _, _) = replay_with(&dir, None, 2, cap).await;
    assert_eq!(got.len(), cap);
}

#[tokio::test(start_paused = true)]
async fn a_full_page_is_followed_by_the_next_a_second_later() {
    let dir = scratch("paging");
    let start = 1_793_664_000;
    let n = ROWS as usize;
    write_exchange(&dir, 1, NOW - FIRST_WINDOW, &docs(0, n, start));
    let last = start + n as i64 - 1;
    write_exchange(&dir, 2, last, &docs(n, 2, last + 1));
    let (got, replayer, cursor) = replay_with(&dir, None, 2, n + 2).await;
    assert_eq!(got.len(), n + 2);
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(
        times[1] - times[0] >= Duration::from_secs(1),
        "Q4: at most one request a second"
    );
    assert!(times[1] - times[0] < POLL, "a follow-up isn't a new poll");
    assert_eq!(cursor.as_deref(), Some((last + 2).to_string().as_bytes()));
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_for_its_retry_after_then_polling_resumes() {
    let (got, replayer) = replay("archive_429", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(
        times[1] - times[0] >= Duration::from_secs(1800),
        "retried after {:?}",
        times[1] - times[0]
    );
    assert_eq!(got.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_5xx_is_retried_later_and_is_not_an_error() {
    let (got, replayer) = replay("archive_5xx", 2, 1).await;
    let times: Vec<_> = replayer.requests().into_iter().map(|(at, _)| at).collect();
    assert!(times[1] - times[0] >= POLL);
    assert_eq!(got.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_plugin_stops_within_a_second_of_the_request() {
    let p = Arc::new(ArchiveOrg::with_clock(clock));
    let replayer = Arc::new(Replayer::load(&fixture("archive_normal")).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(7).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the first request", || !replayer.requests().is_empty()).await;
    running.stop_within().await.unwrap();
}

#[test]
fn manifest_and_policy() {
    let p = ArchiveOrg::new();
    let m = p.manifest();
    assert_eq!(m.id, "archive_org");
    assert_eq!(m.consent, Consent::None, "§18.7: class None");
    assert!(!m.enabled_by_default);
    assert_eq!(m.endpoints.len(), 1);
    assert_eq!(
        (m.endpoints[0].protocol, m.endpoints[0].host),
        ("https", "archive.org")
    );
    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Singleton);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.poll_interval >= POLL);
    assert!(
        limits.per_host.interval >= Duration::from_secs(1),
        "Q4: at most 1 request/s"
    );
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
}

#[test]
fn the_request_is_fixed_text_and_the_cursor() {
    let url = search_url(NOW - FIRST_WINDOW);
    assert!(url.starts_with("https://archive.org/advancedsearch.php?q="));
    assert!(url.contains("publicdate%3A%7B2026-11-02T12%3A00%3A00Z%20TO"));
    assert!(url.contains("rows=500") && url.contains("output=json"));
    assert!(!url.contains(' ') && !url.contains('"'));
    // The date arithmetic across a leap day and the epoch.
    assert!(search_url(0).contains("1970-01-01T00%3A00%3A00Z"));
    assert!(search_url(1_709_164_800).contains("2024-02-29T00%3A00%3A00Z"));
    assert!(search_url(1_709_251_199).contains("2024-02-29T23%3A59%3A59Z"));
}

#[test]
fn content_types_follow_the_media_type() {
    for (m, want) in [
        ("movies", Some("video")),
        ("audio", Some("music")),
        ("etree", Some("music")),
        ("texts", Some("ebook")),
        ("image", Some("images")),
        ("software", Some("software")),
        ("data", None),
        ("web", None),
        ("collection", None),
        ("Movies", None),
        ("", None),
    ] {
        assert_eq!(content_type(m), want, "{m}");
    }
}

fn item(hashes: Hashes) -> Item {
    Item {
        hashes,
        identifier: Some("item-1".into()),
        title: Some("T".into()),
        size: Some(1),
        published: Some(NOW),
        mediatype: Some("movies".into()),
        collections: vec!["c1".into()],
        link: Some("https://archive.org/details/item-1".into()),
        files: vec![File {
            path: "a/b.mkv".into(),
            size: Some(5),
        }],
        files_truncated: false,
    }
}

fn key_hashes() -> Hashes {
    Hashes {
        v1: Some(hash("0123456789abcdef0123456789abcdef01234567")),
        v2: None,
    }
}

#[test]
fn an_item_without_a_key_is_not_a_find() {
    let none = Hashes { v1: None, v2: None };
    assert!(to_found(item(none), NOW).is_none());
}

#[test]
fn files_are_claims_and_nothing_else_is_kept() {
    let f = to_found(item(key_hashes()), NOW).unwrap();
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        c.files,
        [ClaimedFile {
            path: "a/b.mkv".into(),
            size: Some(5)
        }]
    );
    assert!(c.publisher.is_none() && c.description.is_none() && c.trackers.is_empty());
    assert_eq!(
        f.provenance.as_deref().unwrap().confidence,
        lodestar_sources::Confidence::Low
    );
}

#[test]
fn sizes_and_dates_outside_sane_ranges_are_dropped() {
    for size in [0, MAX_SIZE + 1, u64::MAX] {
        let mut i = item(key_hashes());
        i.size = Some(size);
        assert_eq!(to_found(i, NOW).unwrap().claims.unwrap().size, None);
    }
    let mut i = item(key_hashes());
    i.size = Some(MAX_SIZE);
    assert_eq!(
        to_found(i, NOW).unwrap().claims.unwrap().size,
        Some(MAX_SIZE)
    );
    for t in [0, 994_031_999, NOW + 24 * 3600 + 1, i64::MAX, i64::MIN] {
        let mut i = item(key_hashes());
        i.published = Some(t);
        let f = to_found(i, NOW).unwrap();
        assert_eq!(f.provenance.unwrap().published_at, None, "{t}");
    }
}

proptest! {
    #[test]
    fn the_mapping_never_panics(
        size in any::<Option<u64>>(),
        published in any::<Option<i64>>(),
        title in any::<Option<String>>(),
        media in any::<Option<String>>(),
    ) {
        let mut i = item(key_hashes());
        i.size = size;
        i.published = published;
        i.title = title;
        i.mediatype = media;
        let f = to_found(i, NOW).unwrap();
        prop_assert!(f.claims.unwrap().size.is_none_or(|s| (1..=MAX_SIZE).contains(&s)));
    }

    #[test]
    fn the_cursor_dates_never_panic(t in any::<i64>()) {
        let url = search_url(t);
        prop_assert!(url.is_ascii() && !url.contains(' '));
    }
}
