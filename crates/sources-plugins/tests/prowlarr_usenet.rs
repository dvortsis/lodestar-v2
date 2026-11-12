//! UN-1: Prowlarr's Newznab results as usenet items, on recorded fixtures
//! in `fixtures/prowlarr/usenet/`, with no network. The NZB link (which
//! carries Prowlarr's API key and the indexer's) is never stored, logged
//! or printed; see `docs/decisions/2026-11-12-UN-1.md`.
//!
//! In its own test binary: the end-to-end test installs the process's
//! only log sink and turns every event on.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use lodestar_filter::Action;
use lodestar_pipeline::Pipeline;
use lodestar_sources::testing::{FakeCtx, Replayer, parse_exchange, wait_for};
use lodestar_sources::{Discovery, Found, SourcePlugin};
use lodestar_sources_feeds::torznab::{self, Item, Kind};
use lodestar_sources_plugins::prowlarr::{
    self, Config, Indexer, MAX_INDEXERS, Manager, Prowlarr, USENET_MAX_GUID, USENET_MAX_JUDGED,
    USENET_PAGE_BYTES, USENET_PAGE_ITEMS, USENET_POLL_ITEMS, UsenetPage, parse_indexers,
    parse_indexers_with, to_found, to_usenet, usenet_category, usenet_guid, usenet_page,
};
use lodestar_sources_plugins::{Credentials, StaticCredentials};
use lodestar_store::unverified::{Network, USENET_MAX_PART, UsenetKey};
use lodestar_store::{SourceId, Store, StoreConfig};
use proptest::prelude::*;

#[path = "support/harness.rs"]
mod harness;
#[allow(dead_code)]
#[path = "support/plugins.rs"]
mod plugins;

use harness::{core_with, n, pipeline_config, until, user_rule};
use plugins::relay;

/// This crate's folder, for the shared fixture runs.
const PLUGINS_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// The manager's API key in the fixtures.
const PK: &str = "prowlarrkey0000000000000000000001";
/// The indexer's API key, inside Prowlarr's download links.
const IK: &str = "indexerkey00000000000000000000002";
/// The poster every fixture item names: never stored.
const POSTER: &str = "someone@example.invalid";
/// What a stored or logged byte must never hold: either key, the poster,
/// and the shapes of the links that carry them.
const NEVER: [&str; 7] = [PK, IK, POSTER, "/download", "getnzb", "link=", "t%3Dget"];
const HOST: &str = "prowlarr.example";
const NOW: i64 = 1_794_500_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(PLUGINS_DIR)
        .join("fixtures/prowlarr/usenet")
        .join(case)
}

fn body(case: &str, n: u32) -> Vec<u8> {
    std::fs::read(fixture(case).join(format!("{n:03}.body"))).unwrap()
}

fn creds() -> Arc<dyn Credentials> {
    Arc::new(StaticCredentials::new([("api_key", PK)]))
}

fn plugin(usenet: bool) -> Arc<Prowlarr> {
    let c = Config::new("http://prowlarr.example:9696", Manager::Prowlarr, Some(2))
        .unwrap()
        .with_usenet(usenet)
        .unwrap();
    Arc::new(Prowlarr::new(c, creds()))
}

fn usenet_ix(id: &str, private: bool) -> Indexer {
    Indexer {
        id: id.into(),
        private,
        usenet: true,
    }
}

fn key(indexer: &str, guid: &str) -> [u8; 32] {
    *UsenetKey::new(indexer, guid).unwrap().as_bytes()
}

/// Runs `p` on `case` until it has made `requests` requests and sent
/// `finds` discoveries, then stops it.
async fn replay(p: &Arc<Prowlarr>, case: &str, requests: usize, finds: usize) -> Vec<Discovery> {
    let replayer = Arc::new(Replayer::load_unordered(&fixture(case)).unwrap());
    replay_with(p, replayer, requests, finds).await
}

async fn replay_with(
    p: &Arc<Prowlarr>,
    replayer: Arc<Replayer>,
    requests: usize,
    finds: usize,
) -> Vec<Discovery> {
    let mut ctx = FakeCtx::with_fetch(&**p, SourceId::new(5).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    let mut got = Vec::new();
    wait_for("the recorded requests", || {
        got.extend(ctx.drain());
        replayer.requests().len() >= requests
    })
    .await;
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
    for (_, req) in replayer.requests() {
        assert!(!req.url.contains(PK), "the key goes in a header");
    }
    got
}

fn assert_clean(what: &str, text: &str) {
    for s in NEVER {
        assert!(!text.contains(s), "{what}: {s} in {text}");
    }
}

fn name(d: &Discovery) -> Option<&str> {
    d.found.claims.as_deref()?.name.as_deref()
}

fn by_name<'a>(got: &'a [Discovery], n: &str) -> Vec<&'a Discovery> {
    got.iter().filter(|d| name(d) == Some(n)).collect()
}

// ---- The plugin's finds ----

#[tokio::test(start_paused = true)]
async fn newznab_results_become_usenet_finds_keyed_by_indexer_and_guid() {
    // The list, the torrent indexer 1 and the Usenet indexers 7 (private)
    // and 8 (public); never 9 (disabled) or 10 (no RSS).
    let got = replay(&plugin(true), "usenet_rss", 4, 6).await;
    let usenet: Vec<&Discovery> = got.iter().filter(|d| d.found.is_usenet()).collect();
    assert_eq!(usenet.len(), 4, "{got:#?}");
    for d in &usenet {
        let f = &d.found;
        assert!(f.private_source, "every usenet item is private-source");
        assert!(f.peers.is_empty() && !f.interest);
        assert_eq!(f.strategy, prowlarr::STRATEGY_RSS);
        // The 20-byte key is the usenet key's prefix, never a swarm hash.
        assert_eq!(&f.usenet.unwrap()[..20], &f.key.0[..]);
        let c = f.claims.as_deref().unwrap();
        assert!(c.description.is_none() && c.publisher.is_none());
        assert!(c.files.is_empty() && c.trackers.is_empty());
    }

    let tears = by_name(&got, "Tears.of.Steel.2012.1080p.BluRay.x264-USENET");
    let [tears] = tears[..] else {
        panic!("{tears:#?}")
    };
    let guid = "https://indexer.example/details/9f8e7d6c5b4a";
    assert_eq!(tears.found.usenet, Some(key("7", guid)));
    let c = tears.found.claims.as_deref().unwrap();
    assert_eq!(c.size, Some(4_000_000_000));
    let d = c.details.as_deref().unwrap();
    assert_eq!(d.source_category.as_deref(), Some("2040"));
    assert_eq!(d.posted_at, Some(1_794_428_100));
    assert!(d.newsgroups.is_empty(), "the parser keeps no group");
    assert_eq!(c.ids.len(), 1);
    assert_eq!(
        (c.ids[0].scheme.as_str(), c.ids[0].value.as_str()),
        ("imdb", "tt2285752")
    );
    let p = tears.found.provenance.as_deref().unwrap();
    // No reference: a private find stores none (L3).
    assert_eq!(p.external_ref, None);
    assert_eq!(p.published_at, Some(1_794_428_100));

    // NZBGeek's details page keeps its `guid=` query.
    let geek = by_name(&got, "Caminandes.Llamigos.2016.720p.WEB-USENET");
    let guid = "https://geek.example/geekseek.php?guid=abc123def456";
    assert_eq!(geek[0].found.usenet, Some(key("7", guid)));
    let c = geek[0].found.claims.as_deref().unwrap();
    assert_eq!(
        c.details.as_deref().unwrap().source_category.as_deref(),
        Some("5040")
    );

    // An opaque GUID, on both indexers: two items, since the indexer is
    // part of the key. Size 0 and a 1979 date aren't believed.
    let spring = by_name(&got, "Spring.2019.2160p.WEB-USENET");
    let keys: Vec<_> = spring.iter().map(|d| d.found.usenet.unwrap()).collect();
    assert_eq!(spring.len(), 2);
    assert!(keys.contains(&key("7", "a1b2c3d4e5f6")) && keys.contains(&key("8", "a1b2c3d4e5f6")));
    let c = spring[0].found.claims.as_deref().unwrap();
    assert_eq!(c.size, None);
    assert_eq!(c.details.as_deref().unwrap().posted_at, None);

    // Dropped: a GUID that is the link, an NZB download, a credential
    // word, no GUID, no title; and the torrent item on a Usenet indexer.
    for gone in [
        "Agent.327.2017.1080p.WEB-USENET",
        "Hero.2018.1080p.WEB-USENET",
        "Coffee.Run.2020.1080p.WEB-USENET",
        "Sprite.Fright.2021.1080p.WEB-USENET",
        "Torrent.On.A.Usenet.Indexer",
    ] {
        assert!(by_name(&got, gone).is_empty(), "{gone}");
    }

    // No key, link, poster or manager address in any find.
    assert_clean("the finds", &format!("{got:?}"));
    assert!(!format!("{got:?}").contains(HOST));
}

#[tokio::test(start_paused = true)]
async fn usenet_indexers_are_polled_hourly_and_torrent_ones_as_before() {
    // Two polls 15 minutes apart: the second asks the torrent indexer
    // again and skips both Usenet indexers, which aren't due for an hour.
    let got = replay(&plugin(true), "usenet_hourly", 6, 8).await;
    assert_eq!(got.iter().filter(|d| d.found.is_usenet()).count(), 4);
    assert!(prowlarr::USENET_POLL_EVERY >= std::time::Duration::from_secs(3600));
}

#[tokio::test(start_paused = true)]
async fn a_failed_usenet_page_is_asked_again_on_the_next_poll() {
    // A 500 isn't a poll: 15 minutes on, the indexer is asked again, not
    // an hour later, and its results arrive.
    let got = replay(&plugin(true), "usenet_retry", 4, 3).await;
    assert!(got.iter().all(|d| d.found.is_usenet()));
}

#[tokio::test(start_paused = true)]
async fn torrent_results_from_the_same_source_are_unchanged() {
    let got = replay(&plugin(true), "usenet_rss", 4, 6).await;
    let mut torrents: Vec<Found> = got
        .into_iter()
        .filter(|d| !d.found.is_usenet())
        .map(|d| d.found)
        .collect();
    // What the torrent path made of the same page before UN-1.
    let page = torznab::parse(&body("usenet_rss", 2));
    let mut want: Vec<Found> = page
        .items
        .into_iter()
        .filter_map(|i| to_found(i, false, HOST, NOW))
        .map(|mut f| {
            f.clamp();
            f
        })
        .collect();
    let order = |v: &mut Vec<Found>| v.sort_by_key(|f| f.key.0);
    order(&mut torrents);
    order(&mut want);
    // The dates are checked against the clock, so compare without them.
    for f in torrents.iter_mut().chain(want.iter_mut()) {
        assert!(f.usenet.is_none());
        if let Some(p) = f.provenance.as_deref_mut() {
            p.published_at = None;
        }
    }
    assert_eq!(torrents, want);
    assert_eq!(torrents.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn usenet_results_are_off_by_default() {
    // The same list: only the torrent indexer is polled, and nothing it
    // sends is a usenet item.
    let off = Arc::new(Prowlarr::new(
        Config::new("http://prowlarr.example:9696", Manager::Prowlarr, Some(2)).unwrap(),
        creds(),
    ));
    let got = replay(&off, "usenet_off", 2, 2).await;
    assert!(got.iter().all(|d| !d.found.is_usenet()));
    // Saved settings without the switch read back off.
    let c = Config::from_json(r#"{"url":"http://p.example","manager":"prowlarr"}"#).unwrap();
    assert!(!c.usenet());
    let schema: serde_json::Value = serde_json::from_str(off.policy().config_schema).unwrap();
    // The switch isn't on the settings form until UN-3.
    assert!(schema["properties"].get("usenet").is_none());
}

#[test]
fn the_usenet_switch_is_saved_only_when_on_and_only_for_prowlarr() {
    let on = Config::from_json(r#"{"url":"http://p.example","manager":"prowlarr","usenet":true}"#)
        .unwrap();
    assert!(on.usenet());
    let saved = on.to_json();
    assert_eq!(
        saved,
        r#"{"agents":2,"manager":"prowlarr","url":"http://p.example","usenet":true}"#
    );
    assert_eq!(Config::from_json(&saved).unwrap(), on);
    let off =
        Config::from_json(r#"{"url":"http://p.example","manager":"prowlarr","usenet":false}"#)
            .unwrap();
    assert_eq!(
        off.to_json(),
        r#"{"agents":2,"manager":"prowlarr","url":"http://p.example"}"#
    );
    let jackett = r#"{"url":"http://p.example","manager":"jackett","usenet":true}"#;
    assert_eq!(
        Config::from_json(jackett).unwrap_err().0,
        "Usenet results need Prowlarr"
    );
    for bad in [r#""yes""#, "1", "null"] {
        let j = format!(r#"{{"url":"http://p.example","manager":"prowlarr","usenet":{bad}}}"#);
        // `null` is the default; anything else that isn't a boolean is refused.
        assert_eq!(Config::from_json(&j).is_ok(), bad == "null", "{j}");
    }
}

#[test]
fn the_list_keeps_usenet_indexers_only_when_asked() {
    let list = body("usenet_rss", 1);
    let got = |on| {
        parse_indexers_with(Manager::Prowlarr, &list, on)
            .unwrap()
            .into_iter()
            .map(|i| (i.id, i.private, i.usenet))
            .collect::<Vec<_>>()
    };
    assert_eq!(got(false), [("1".into(), false, false)]);
    assert_eq!(
        got(true),
        [
            ("1".into(), false, false),
            ("7".into(), true, true),
            ("8".into(), false, true)
        ]
    );
    assert_eq!(parse_indexers(Manager::Prowlarr, &list).unwrap().len(), 1);
    // Jackett has no Usenet indexers; a garbage list is still refused.
    assert!(parse_indexers_with(Manager::Prowlarr, b"[", true).is_err());
    // Usenet indexers count toward the same cap.
    let many: Vec<String> = (1..=MAX_INDEXERS + 10)
        .map(|i| format!(r#"{{"id":{i},"enable":true,"protocol":"usenet"}}"#))
        .collect();
    let many = format!("[{}]", many.join(","));
    assert_eq!(
        parse_indexers_with(Manager::Prowlarr, many.as_bytes(), true)
            .unwrap()
            .len(),
        MAX_INDEXERS
    );
}

// ---- The rules: the GUID, the poster, the category ----

#[test]
fn only_a_guid_that_cant_fetch_anything_is_kept() {
    let ok = |g: &str| {
        usenet_guid(
            Some(g),
            Some("http://prowlarr.example:9696/7/download"),
            HOST,
            PK,
        )
    };
    for keep in [
        "a1b2c3d4e5f6",
        "0f8fad5bd9cb469fa16570867728950e",
        "https://indexer.example/details/9f8e7d6c5b4a",
        "https://geek.example/geekseek.php?guid=abc123",
    ] {
        assert_eq!(ok(keep).as_deref(), Some(keep), "{keep}");
    }
    for refuse in [
        "",
        "http://prowlarr.example:9696/7/download",
        "https://prowlarr.example/details/1",
        "https://indexer.example/api?t=get&id=1",
        "https://indexer.example/getnzb/1.nzb",
        "https://indexer.example/details/1.NZB",
        "https://indexer.example/Download/1",
        "https://indexer.example/dl/7/x",
        "https://indexer.example/details/1?id=2",
        "https://indexer.example/details/1?guid=",
        "https://indexer.example/details/1?guid=a&guid=b&x=1",
        "https://indexer.example/details/1?TOKEN=a",
        "https://indexer.example/apikey/1",
        "https://user:pw@indexer.example/details/1",
        "https://indexer.example/details/1#x",
        "ftp://indexer.example/details/1",
        "has space",
        // Made up by the *arr parser for an item with no GUID.
        "0f8fad5b-d9cb-469f-a165-70867728950e",
        "urn:uuid:0F8FAD5B-D9CB-469F-A165-70867728950E",
        &format!("https://indexer.example/details/{}", PK.to_uppercase()),
        &format!("guid-{PK}"),
        &"a".repeat(USENET_MAX_GUID + 1),
    ] {
        assert_eq!(ok(refuse), None, "{refuse}");
    }
    assert_eq!(usenet_guid(None, None, HOST, PK), None);
    assert_eq!(USENET_MAX_GUID, USENET_MAX_PART);
}

#[test]
fn the_poster_and_the_link_are_never_kept() {
    let page = torznab::parse(&body("usenet_rss", 3));
    assert!(page.items.iter().any(|i| i.kind == Kind::Usenet));
    for item in page.items {
        if let Some(u) = to_usenet(item, &usenet_ix("7", true), HOST, PK, NOW) {
            let all = format!("{:?} {:?}", u, u.claims);
            assert_clean("a usenet item", &all);
            // Debug shows lengths, never the title or the GUID.
            let dbg = format!("{u:?}");
            assert!(
                !dbg.contains("Steel") && !dbg.contains("indexer.example"),
                "{dbg}"
            );
        }
    }
}

#[test]
fn a_result_on_a_torrent_indexer_or_with_an_odd_indexer_id_is_skipped() {
    let item = || {
        torznab::parse(&body("usenet_rss", 3))
            .items
            .into_iter()
            .find(|i| i.kind == Kind::Usenet)
            .unwrap()
    };
    assert!(to_usenet(item(), &usenet_ix("7", true), HOST, PK, NOW).is_some());
    let torrent = Indexer {
        usenet: false,
        ..usenet_ix("7", true)
    };
    assert!(to_usenet(item(), &torrent, HOST, PK, NOW).is_none());
    for id in ["07", "x7", "", "-7"] {
        assert!(
            to_usenet(item(), &usenet_ix(id, true), HOST, PK, NOW).is_none(),
            "{id}"
        );
    }
    // A title holding the manager's key is never kept.
    let mut keyed = item();
    keyed.title = Some(format!("Release {PK}"));
    assert!(to_usenet(keyed, &usenet_ix("7", true), HOST, PK, NOW).is_none());
    // A date past tomorrow isn't believed.
    let mut future = item();
    future.published = Some(NOW + 2 * 86_400);
    let u = to_usenet(future, &usenet_ix("7", true), HOST, PK, NOW).unwrap();
    assert_eq!(u.claims.details.as_deref().unwrap().posted_at, None);
    // A private indexer's find is private; so is a public one's, since
    // every usenet item is.
    for private in [true, false] {
        let u = to_usenet(item(), &usenet_ix("8", private), HOST, PK, NOW).unwrap();
        assert_eq!(u.private, private);
        assert!(u.into_found().unwrap().private_source);
    }
}

#[test]
fn the_largest_item_stays_inside_the_judged_bytes_budget() {
    // A 1,024-byte title, a 1,024-byte GUID, the widest category and
    // three ids: what one usenet item can give the filter to judge.
    let item = Item {
        kind: Kind::Usenet,
        title: Some("t".repeat(1024)),
        size: Some(1),
        categories: vec![u32::MAX],
        published: Some(NOW),
        seeders: None,
        leechers: None,
        peers: None,
        files: None,
        download_factor: None,
        upload_factor: None,
        tags: Vec::new(),
        ids: vec![
            ("imdb", "tt0111161".into()),
            ("tmdb", "278".into()),
            ("tvdb", "81189".into()),
        ],
        guid: Some("g".repeat(USENET_MAX_GUID)),
        link: None,
    };
    let f = to_usenet(item, &usenet_ix("4294967295", true), HOST, PK, NOW)
        .unwrap()
        .into_found()
        .unwrap();
    let d = Discovery {
        source: SourceId::new(5).unwrap(),
        found: f,
        at: NOW,
        reply: false,
    };
    let c = lodestar_pipeline::unverified::store_claims(&d).unwrap();
    assert_eq!(c.ids.len(), 3);
    assert_eq!(c.provenance.external_ref, None);
    let judged = lodestar_pipeline::unverified::judged_len(&c, None);
    assert!(judged <= USENET_MAX_JUDGED, "{judged}");
}

#[test]
fn the_category_is_the_most_specific_one() {
    assert_eq!(usenet_category(&[2000, 2040]).as_deref(), Some("2040"));
    assert_eq!(usenet_category(&[5000]).as_deref(), Some("5000"));
    assert_eq!(usenet_category(&[7020, 7000]).as_deref(), Some("7020"));
    assert_eq!(usenet_category(&[]), None);
    assert_eq!(usenet_category(&[u32::MAX]).as_deref(), Some("4294967295"));
}

// ---- Bounds ----

fn page_of(n: usize) -> Vec<u8> {
    let mut s = String::from(r#"<?xml version="1.0"?><rss xmlns:newznab="x"><channel>"#);
    for i in 0..n {
        s.push_str(&format!(
            r#"<item><title>Item {i}</title><guid>g{i}</guid><newznab:attr name="size" value="10"/></item>"#
        ));
    }
    s.push_str("</channel></rss>");
    s.into_bytes()
}

#[test]
fn a_page_is_capped_in_bytes_and_items_and_garbage_gives_nothing() {
    let ix = usenet_ix("7", true);
    let got = usenet_page(&page_of(USENET_PAGE_ITEMS * 3), &ix, HOST, PK, NOW).unwrap();
    assert_eq!(got.len(), USENET_PAGE_ITEMS);
    let mut big = page_of(1);
    big.resize(USENET_PAGE_BYTES + 1, b' ');
    assert_eq!(
        usenet_page(&big, &ix, HOST, PK, NOW),
        Err(UsenetPage::TooBig)
    );
    for junk in [
        &b""[..],
        b"\xff\xfe\x00garbage",
        b"<rss><channel><item><title>x</title>",
        b"<!DOCTYPE rss [<!ENTITY a 'b'>]><rss/>",
        b"{\"json\":true}",
    ] {
        assert_eq!(usenet_page(junk, &ix, HOST, PK, NOW), Ok(Vec::new()));
    }
    let refused =
        br#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#;
    assert_eq!(
        usenet_page(refused, &ix, HOST, PK, NOW),
        Err(UsenetPage::Refused)
    );
    // Sizes past the cap are dropped, never wrapped.
    let huge = br#"<rss xmlns:newznab="x"><channel><item><title>T</title><guid>g</guid>
        <newznab:attr name="size" value="18446744073709551616"/></item></channel></rss>"#;
    let got = usenet_page(huge, &ix, HOST, PK, NOW).unwrap();
    assert_eq!(got[0].claims.size, None);
}

#[tokio::test(start_paused = true)]
async fn a_poll_emits_at_most_its_cap() {
    // 42 Usenet indexers with 100 results each: 4,200 results, 4,096 sent.
    let ixs = 42usize;
    assert!(ixs * USENET_PAGE_ITEMS > USENET_POLL_ITEMS);
    let list: Vec<String> = (1..=ixs)
        .map(|i| format!(r#"{{"id":{i},"enable":true,"protocol":"usenet","privacy":"private"}}"#))
        .collect();
    let head = |path: &str| {
        format!(
            "GET http://prowlarr.example:9696{path}\nX-Api-Key: {PK}\n\nHTTP 200\nContent-Type: application/rss+xml\n"
        )
    };
    let mut ex = vec![
        parse_exchange(
            &format!(
                "GET http://prowlarr.example:9696/api/v1/indexer\nAccept: application/json\nX-Api-Key: {PK}\n\nHTTP 200\nContent-Type: application/json\n"
            ),
            format!("[{}]", list.join(",")).into_bytes(),
            Vec::new(),
        )
        .unwrap(),
    ];
    for i in 1..=ixs {
        ex.push(
            parse_exchange(
                &head(&format!("/{i}/api?t=search&extended=1&limit=100")),
                page_of(USENET_PAGE_ITEMS),
                Vec::new(),
            )
            .unwrap(),
        );
    }
    let replayer = Arc::new(Replayer::unordered(ex));
    let got = replay_with(&plugin(true), replayer, 1 + ixs, USENET_POLL_ITEMS).await;
    assert!(got.iter().all(|d| d.found.is_usenet()));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Whatever a page holds, parsing never panics, keeps at most a page,
    /// and nothing kept holds the manager's key or a query but `guid=`.
    #[test]
    fn a_hostile_page_never_panics(
        raw in proptest::collection::vec(any::<u8>(), 0..2048),
        guid in "[ -~]{0,80}",
        title in "[ -~]{0,80}",
    ) {
        let ix = usenet_ix("7", true);
        let _ = usenet_page(&raw, &ix, HOST, PK, NOW);
        let xml = format!(
            r#"<rss xmlns:newznab="x"><channel><item><title>{}</title><guid>{}</guid><newznab:attr name="size" value="1"/></item></channel></rss>"#,
            escape(&title), escape(&guid)
        );
        let got = usenet_page(xml.as_bytes(), &ix, HOST, PK, NOW).unwrap();
        prop_assert!(got.len() <= 1);
        for u in got {
            prop_assert!(!u.guid.contains(PK));
            if let Some((_, q)) = u.guid.split_once('?') {
                prop_assert!(q.starts_with("guid="));
            }
            prop_assert!(u.guid.len() <= USENET_MAX_GUID);
        }
    }

    /// Any item, any indexer: [`to_usenet`] never panics.
    #[test]
    fn to_usenet_never_panics(
        size in any::<Option<u64>>(),
        published in any::<Option<i64>>(),
        cats in proptest::collection::vec(any::<u32>(), 0..20),
        id in "[0-9a-z]{0,6}",
        now in any::<i64>(),
    ) {
        let item = Item {
            kind: Kind::Usenet,
            title: Some("T".into()),
            size,
            categories: cats,
            published,
            seeders: None,
            leechers: None,
            peers: None,
            files: None,
            download_factor: None,
            upload_factor: None,
            tags: Vec::new(),
            ids: Vec::new(),
            guid: Some("g".into()),
            link: None,
        };
        if let Some(u) = to_usenet(item, &usenet_ix(&id, true), HOST, PK, now) {
            prop_assert!(u.claims.size.is_none_or(|s| (1..=prowlarr::MAX_SIZE).contains(&s)));
            let _ = u.into_found();
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---- Through the real pipeline and store ----

/// Every record the pipeline, filter and integrations log, with every
/// event on.
struct Capture(Mutex<Vec<String>>);

impl lodestar_log::Sink for Capture {
    fn record(&self, r: &lodestar_log::Record<'_>) {
        let all = lodestar_log::Mask(u32::MAX).without_opt_ins();
        self.0.lock().unwrap().push(r.text(all, None));
    }
}

fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    let c = CAPTURE.get_or_init(|| Capture(Mutex::new(Vec::new())));
    static SET: OnceLock<()> = OnceLock::new();
    SET.get_or_init(|| {
        lodestar_log::set_sink(c).expect("this binary's only sink");
        for cat in [
            &lodestar_pipeline::log_events::CATALOGUE,
            &lodestar_filter::log_events::CATALOGUE,
            &lodestar_integrations::log_events::CATALOGUE,
        ] {
            for e in cat.events {
                e.set_on(true);
            }
        }
    });
    c
}

/// Every text a stored usenet item's claims hold.
fn stored_texts(store: &Store) -> Vec<String> {
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = store.unverified_after(after, 100).unwrap();
        let Some(last) = page.last().map(|i| i.id) else {
            return out;
        };
        after = last;
        for i in page.into_iter().filter(|i| i.network == Network::Usenet) {
            let Some(c) = i.claims else { continue };
            out.extend(c.name.clone());
            out.extend(c.description.clone());
            out.extend(c.publisher.clone());
            out.extend(c.provenance.external_ref.clone());
            out.extend(c.trackers.iter().cloned());
            out.extend(c.files.iter().map(|f| f.path.clone()));
            out.extend(c.ids.iter().map(|x| format!("{}:{}", x.scheme, x.value)));
            if let Some(d) = c.details.as_deref() {
                out.extend(d.source_category.clone());
                out.extend(d.newsgroups.iter().cloned());
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usenet_items_are_stored_private_rejudged_by_claims_and_never_hold_a_link() {
    let capture = capture();
    // The plugin's finds, as recorded.
    let finds: Vec<Found> = replay(&plugin(true), "usenet_rss", 4, 6)
        .await
        .into_iter()
        .map(|d| d.found)
        .collect();
    // A usenet find the child-safety core holds: nothing is stored.
    let held_title = Item {
        title: Some("Rolea Lexa Usenet".into()),
        ..torznab::parse(&body("usenet_rss", 3))
            .items
            .into_iter()
            .find(|i| i.kind == Kind::Usenet)
            .unwrap()
    };
    let mut held_title = to_usenet(held_title, &usenet_ix("7", true), HOST, PK, NOW).unwrap();
    held_title.guid = "held-guid".into();
    let held_key = key("7", "held-guid");
    let held = held_title.into_found().unwrap();
    // Each usenet find's judged text is inside the per-item budget.
    for f in finds.iter().filter(|f| f.is_usenet()) {
        let d = Discovery {
            source: SourceId::new(5).unwrap(),
            found: f.clone(),
            at: NOW,
            reply: false,
        };
        let c = lodestar_pipeline::unverified::store_claims(&d).unwrap();
        assert!(lodestar_pipeline::unverified::judged_len(&c, None) <= USENET_MAX_JUDGED);
        assert_eq!(c.provenance.external_ref, None);
    }

    let filter = core_with(&["lexa"], &["rolea"]);
    let (source, tx) = relay(&prowlarr::Prowlarr::unconfigured(creds()).manifest().clone());
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let store = Store::open(&db, StoreConfig::default()).unwrap();
    let pipeline = Pipeline::start(pipeline_config(filter.clone()), store, vec![source])
        .await
        .unwrap();
    let mut round = finds.clone();
    round.push(held);
    tx.send(round).await.unwrap();
    until("the finds stored and the held one dropped", || {
        let u = &pipeline.writer_stats().unverified;
        n(&u.stored) >= 6 && n(&u.held) >= 1
    })
    .await;
    // Every find went to the writer's claims path: none to the
    // coordinator and the fetch pool, none to touches, none refused by
    // the held set or a hash check.
    let i = pipeline.intake_stats();
    assert_eq!(n(&i.claims), 7);
    assert_eq!(n(&i.to_pool), 0);
    assert_eq!(n(&i.known), 0);
    assert_eq!(n(&i.refused), 0);
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    assert_eq!(reader.unverified_count(Network::Usenet).unwrap(), 4);
    assert_eq!(reader.unverified_count(Network::Bt).unwrap(), 2);
    assert!(
        reader
            .unverified(Network::Usenet, &held_key)
            .unwrap()
            .is_none()
    );
    let tears_guid = "https://indexer.example/details/9f8e7d6c5b4a";
    let tears = reader
        .unverified(Network::Usenet, &key("7", tears_guid))
        .unwrap()
        .unwrap();
    assert!(tears.private_source && tears.claims_private);
    let c = tears.claims.as_ref().unwrap();
    assert_eq!(
        c.name.as_deref(),
        Some("Tears.of.Steel.2012.1080p.BluRay.x264-USENET")
    );
    assert_eq!(c.provenance.external_ref, None, "L3: no reference");
    let d = c.details.as_deref().unwrap();
    assert_eq!(d.source_category.as_deref(), Some("2040"));
    // The public indexer's item is private too (§18.7, UN-0).
    let open = reader
        .unverified(Network::Usenet, &key("8", "a1b2c3d4e5f6"))
        .unwrap()
        .unwrap();
    assert!(open.private_source);

    // A rule that matches only a torrent: the sweep re-judges every item,
    // and keeps every usenet one (its key isn't 20 bytes).
    let refiled = n(&pipeline.writer_stats().unverified.refiled);
    user_rule(&filter, "bunny", Action::Drop);
    until("the sweep removes the matching torrent item", || {
        n(&pipeline.writer_stats().unverified.refiled) > refiled
    })
    .await;
    assert_eq!(reader.unverified_count(Network::Usenet).unwrap(), 4);
    // A rule a usenet item's claims match: the sweep removes it by them.
    let refiled = n(&pipeline.writer_stats().unverified.refiled);
    user_rule(&filter, "caminandes", Action::Drop);
    until("the sweep removes the matching usenet item", || {
        n(&pipeline.writer_stats().unverified.refiled) > refiled
            && reader.unverified_count(Network::Usenet).unwrap() == 3
    })
    .await;
    // Only the two torrent finds count as harvested keys: a usenet
    // result has no swarm hash.
    let harvested = |r: &Store| -> i64 {
        r.rollup(lodestar_pipeline::metric::HARVESTED, 0, i64::MAX)
            .unwrap()
            .iter()
            .map(|c| c.value)
            .sum()
    };
    until("the harvested rollup", || harvested(&reader) >= 2).await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(harvested(&store), 2);

    // No stored text, no stored byte and no log line holds a key, the
    // link or the poster.
    for t in stored_texts(&store) {
        assert_clean("a stored claim", &t);
    }
    drop(store);
    drop(reader);
    // Claim blobs are compressed, so this raw scan backs up the decoded
    // check above; it catches anything stored outside a claims blob.
    for f in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(f.unwrap().path()).unwrap();
        for s in NEVER {
            assert!(
                !bytes.windows(s.len()).any(|w| w == s.as_bytes()),
                "{s} in the store's files"
            );
        }
    }
    let logs = capture.0.lock().unwrap();
    assert!(!logs.is_empty(), "the capture saw the pipeline");
    for line in logs.iter() {
        assert_clean("a log line", line);
    }
}

// ---- The cap's cost (UN-0's heads-up, brief requirement 5) ----

/// What the usenet cap's count-and-evict costs per adding write at 1M
/// stored usenet items (beside 1M BitTorrent items), with this plugin's
/// sizes: the writer stores a poll in batches of 256 finds
/// (`PipelineConfig::batch_max`), so a full poll of [`USENET_POLL_ITEMS`]
/// is 16 adding writes. Prints the numbers; run with
/// `cargo test --release -p lodestar-sources-plugins --test prowlarr_usenet -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement: about a minute in release"]
fn usenet_cap_cost_at_one_million() {
    use lodestar_store::claims::{Claims as StoreClaims, Details as StoreDetails};
    use lodestar_store::unverified::{MAX_USENET_BATCH, NewUnverified, USENET_CAP};
    use std::time::Instant;

    const BATCH: usize = 256;
    let claims = |i: u64| StoreClaims {
        name: Some(format!("Some.Release.Name.2026.1080p.WEB-DL.x264-GROUP{i}")),
        size: Some(4_000_000_000),
        details: Some(Box::new(StoreDetails {
            source_category: Some("2040".into()),
            posted_at: Some(NOW),
            ..StoreDetails::default()
        })),
        ..StoreClaims::default()
    };
    for disk in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = if disk {
            Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap()
        } else {
            Store::open_in_memory(StoreConfig::default()).unwrap()
        };
        let src = store.register_source("prowlarr", "1", NOW).unwrap();
        let put = |store: &mut Store, from: u64, n: usize, usenet: bool| {
            let keys: Vec<Vec<u8>> = (from..from + n as u64)
                .map(|i| match usenet {
                    true => key("7", &format!("guid-{i}")).to_vec(),
                    false => {
                        let mut k = [0u8; 20];
                        k[..8].copy_from_slice(&i.to_be_bytes());
                        k[19] = 1;
                        k.to_vec()
                    }
                })
                .collect();
            let cs: Vec<StoreClaims> = (from..from + n as u64).map(claims).collect();
            let items: Vec<NewUnverified<'_>> = keys
                .iter()
                .zip(&cs)
                .map(|(k, c)| NewUnverified {
                    network: if usenet { Network::Usenet } else { Network::Bt },
                    key: k,
                    source: src,
                    private_source: usenet,
                    claims: Some(c),
                    at: NOW,
                })
                .collect();
            store.put_unverified(&items).unwrap();
        };
        let next = std::cell::Cell::new(0u64);
        let fill = |store: &mut Store, to: u64, usenet: bool| {
            while next.get() < to {
                let n = (to - next.get()).min(MAX_USENET_BATCH as u64) as usize;
                put(store, next.get(), n, usenet);
                next.set(next.get() + n as u64);
            }
        };
        fill(&mut store, 1_000_000, false);
        let bt_end = next.get();
        let report = |store: &mut Store, label: &str| {
            let mut times = Vec::new();
            for _ in 0..USENET_POLL_ITEMS / BATCH {
                let t = Instant::now();
                put(store, next.get(), BATCH, true);
                times.push(t.elapsed());
                next.set(next.get() + BATCH as u64);
            }
            times.sort();
            let total: std::time::Duration = times.iter().sum();
            println!(
                "{} {label}: {} usenet items; per {BATCH}-item write median {:?}, max {:?}; a {}-item poll {:?}",
                if disk { "disk" } else { "memory" },
                store.unverified_count(Network::Usenet).unwrap(),
                times[times.len() / 2],
                times[times.len() - 1],
                USENET_POLL_ITEMS,
                total,
            );
        };
        fill(&mut store, bt_end + 100_000, true);
        report(&mut store, "at 100k");
        fill(&mut store, bt_end + USENET_CAP, true);
        report(&mut store, "at the cap");
    }
}
