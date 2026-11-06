//! Contract tests for the Prowlarr/Jackett plugin (§18.9), on recorded
//! fixtures in `fixtures/prowlarr/`, with no network. `README.md` is the
//! pattern they set for every plugin in this crate.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, Replayer, wait_for};
use lodestar_sources::{
    Consent, Discovery, Distribution, ExternalId, SourcePlugin, UserLimits, floors,
};
use lodestar_sources_feeds::torznab::{Hashes, Item, Kind};
use lodestar_sources_plugins::prowlarr::{
    self, Config, ConfigError, ListError, MAX_INDEXERS, MAX_SIZE, Manager, Prowlarr,
    is_manager_download, parse_indexers, to_found,
};
use lodestar_sources_plugins::{Credentials, StaticCredentials};
use lodestar_store::SourceId;
use proptest::prelude::*;

const POLL: Duration = Duration::from_secs(15 * 60);

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/prowlarr")
        .join(case)
}

fn key() -> Arc<dyn Credentials> {
    Arc::new(StaticCredentials::new([("api_key", "REDACTED")]))
}

fn plugin(url: &str, manager: Manager) -> Arc<Prowlarr> {
    Arc::new(Prowlarr::new(
        Config::new(url, manager, Some(2)).unwrap(),
        key(),
    ))
}

fn hash(hex: &str) -> InfoHash {
    hex.parse().unwrap()
}

/// Runs `p` on `case` until the replayer has seen `requests` requests and
/// the plugin has sent `finds` discoveries; returns them and the replayer.
async fn replay(
    p: &Arc<Prowlarr>,
    case: &str,
    requests: usize,
    finds: usize,
) -> (Vec<Discovery>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load_unordered(&fixture(case)).unwrap());
    let mut ctx = FakeCtx::with_fetch(&**p, SourceId::new(5).unwrap(), replayer.clone());
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

/// Each unit's polls are at least the policy's interval apart.
fn assert_spacing(replayer: &Replayer, min: Duration) {
    let mut last: HashMap<String, tokio::time::Instant> = HashMap::new();
    for (at, req) in replayer.requests() {
        if let Some(prev) = last.insert(req.url.clone(), at) {
            assert!(at - prev >= min, "{} polled after {:?}", req.url, at - prev);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn prowlarr_polls_each_enabled_torrent_indexer() {
    let p = plugin("http://prowlarr.example:9696/", Manager::Prowlarr);
    // Two cycles: the list, indexers 1 (public) and 2 (private); never 3
    // (disabled), 4 (Usenet) or 5 (no RSS), and no download link.
    let (got, replayer) = replay(&p, "prowlarr_rss", 6, 5).await;
    assert_spacing(&replayer, POLL);
    let by_key = |h: &'static str| got.iter().filter(move |d| d.found.key == hash(h));

    let bunny: Vec<_> = by_key("1f2e3d4c5b6a79880123456789abcdeffedcba98").collect();
    assert_eq!(bunny.len(), 2, "polled twice, found twice");
    let f = &bunny[0].found;
    assert!(!f.private_source && f.peers.is_empty());
    assert_eq!(f.strategy, prowlarr::STRATEGY_RSS);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        c.name.as_deref(),
        Some("Big.Buck.Bunny.2008.1080p.BluRay.x264-EXAMPLE")
    );
    assert_eq!(c.size, Some(928_670_754));
    assert!(c.files.is_empty() && c.trackers.is_empty() && c.publisher.is_none());
    assert!(c.ids.contains(&ExternalId {
        scheme: "imdb".into(),
        value: "tt1254207".into()
    }));
    let prov = f.provenance.as_deref().unwrap();
    assert_eq!(
        prov.external_ref.as_deref(),
        Some("https://public.example/torrent/1234")
    );
    assert_eq!(prov.published_at, Some(1_793_470_925));

    // A magnet-only item: its GUID is the manager's own download URL, so
    // there's no reference; size 0 and a 1990 date aren't believed.
    let dream = got
        .iter()
        .find(|d| {
            d.found.claims.as_deref().and_then(|c| c.name.as_deref())
                == Some("Elephants Dream (2006) [720p]")
        })
        .unwrap();
    let c = dream.found.claims.as_deref().unwrap();
    assert_eq!(c.size, None);
    let prov = dream.found.provenance.as_deref().unwrap();
    assert_eq!(prov.external_ref, None);
    assert_eq!(prov.published_at, None);

    // The private indexer's find: claims only, no reference, and flagged
    // so it never reaches public peer finding, Torznab, Stremio or exports.
    let sintel: Vec<_> = by_key("00112233445566778899aabbccddeeff00112233").collect();
    assert_eq!(sintel.len(), 1);
    let f = &sintel[0].found;
    assert!(f.private_source);
    assert!(f.peers.is_empty() && !f.interest);
    assert_eq!(f.provenance.as_deref().unwrap().external_ref, None);
    assert_eq!(
        f.claims.as_deref().unwrap().name.as_deref(),
        Some("Sintel.2010.2160p.WEB-DL-EXAMPLE")
    );

    // No key, passkey or manager address reaches a discovery.
    let all = format!("{got:?}");
    for secret in ["REDACTED", "PASSKEY", "prowlarr.example", "download"] {
        assert!(!all.contains(secret), "{secret} in {all}");
    }
    // The key goes in a header, never in a URL.
    for (_, req) in replayer.requests() {
        assert!(!req.url.contains("REDACTED"), "{}", req.url);
    }
}

#[tokio::test(start_paused = true)]
async fn jackett_polls_configured_indexers_but_not_all() {
    let p = plugin("http://jackett.example:9117", Manager::Jackett);
    let (got, _) = replay(&p, "jackett_rss", 3, 2).await;
    let public = got
        .iter()
        .find(|d| d.found.key == hash("aabbccddeeff00112233445566778899aabbccdd"))
        .unwrap();
    assert!(!public.found.private_source);
    // Jackett's `jackett_apikey` is taken out of the reference.
    assert_eq!(
        public
            .found
            .provenance
            .as_deref()
            .unwrap()
            .external_ref
            .as_deref(),
        Some("https://public.example/details/55")
    );
    let semi = got
        .iter()
        .find(|d| d.found.key == hash("99887766554433221100ffeeddccbbaa99887766"))
        .unwrap();
    assert!(semi.found.private_source, "semi-private is private");
    assert_eq!(semi.found.provenance.as_deref().unwrap().external_ref, None);
}

#[tokio::test(start_paused = true)]
async fn an_indexer_error_is_skipped_and_polling_goes_on() {
    let p = plugin("http://prowlarr.example:9696", Manager::Prowlarr);
    // Indexer 7 answers 500; indexer 8 is still polled and its find sent.
    let (got, _) = replay(&p, "prowlarr_indexer_error", 3, 1).await;
    assert_eq!(
        got[0].found.key,
        hash("0a0b0c0d0e0f101112131415161718191a1b1c1d")
    );
}

async fn ends_with_error(p: Arc<Prowlarr>, case: Option<&str>) -> String {
    let fetch: Arc<dyn lodestar_sources::Fetch> = match case {
        Some(c) => Arc::new(Replayer::load(&fixture(c)).unwrap()),
        None => Arc::new(lodestar_sources::NoEgress),
    };
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(5).unwrap(), fetch);
    let running = ctx.start(p);
    wait_for("the plugin to end", || running.is_finished()).await;
    let stopped = running.stop().await.unwrap();
    stopped.result.unwrap_err().0
}

#[tokio::test(start_paused = true)]
async fn a_refused_key_stops_the_plugin() {
    for (url, manager, case) in [
        (
            "http://prowlarr.example:9696",
            Manager::Prowlarr,
            "prowlarr_refused",
        ),
        (
            "http://jackett.example:9117",
            Manager::Jackett,
            "jackett_refused",
        ),
        // On an indexer page too: Torznab error 101, and a plain 403.
        (
            "http://prowlarr.example:9696",
            Manager::Prowlarr,
            "prowlarr_page_refused",
        ),
        (
            "http://jackett.example:9117",
            Manager::Jackett,
            "jackett_page_403",
        ),
    ] {
        let e = ends_with_error(plugin(url, manager), Some(case)).await;
        assert!(e.contains("refused the API key"), "{e}");
        assert!(!e.contains("REDACTED"));
    }
}

#[tokio::test(start_paused = true)]
async fn a_wrong_address_or_manager_asks_for_setup() {
    // A redirect, a 404, and an HTML page where the list should be (a
    // Jackett address with Prowlarr chosen) each stop the plugin at once.
    for case in [
        "prowlarr_redirect",
        "prowlarr_not_found",
        "prowlarr_not_a_list",
    ] {
        let p = plugin("http://prowlarr.example:9696", Manager::Prowlarr);
        let e = ends_with_error(p, Some(case)).await;
        assert!(
            e.starts_with("needs setup") && !e.contains("https://") && !e.contains("html"),
            "{case}: {e}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn missing_setup_fails_at_once_without_a_request() {
    let unset = Arc::new(Prowlarr::unconfigured(key()));
    assert!(ends_with_error(unset, None).await.contains("needs setup"));

    let config = Config::new("http://prowlarr.example:9696", Manager::Prowlarr, None).unwrap();
    let nokey = Arc::new(Prowlarr::new(
        config.clone(),
        Arc::new(StaticCredentials::default()),
    ));
    assert!(ends_with_error(nokey, None).await.contains("no API key"));

    let bad = Arc::new(Prowlarr::new(
        config,
        Arc::new(StaticCredentials::new([("api_key", "abc\r\nX-Evil: 1")])),
    ));
    let e = ends_with_error(bad, None).await;
    assert!(
        e.contains("isn't a valid key") && !e.contains("Evil"),
        "{e}"
    );
}

#[test]
fn manifest_and_policy() {
    let p = Prowlarr::unconfigured(key());
    let m = p.manifest();
    assert_eq!(m.id, "prowlarr");
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default);
    assert!(m.finds_private, "never named to a public viewer (§18.7)");
    assert!(
        m.endpoints.is_empty(),
        "the user's address is the only host"
    );
    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::ShardedUnit);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.poll_interval >= POLL);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    let schema: serde_json::Value = serde_json::from_str(policy.config_schema).unwrap();
    assert_eq!(schema["properties"]["api_key"]["writeOnly"], true);
    assert!(p.endpoints().is_none(), "nothing reachable before setup");
}

#[test]
fn config_is_checked() {
    let ok = |u: &str| Config::new(u, Manager::Prowlarr, None);
    assert!(ok("http://prowlarr:9696").is_ok());
    assert!(ok("https://home.example/prowlarr/").is_ok());
    assert!(ok("http://127.0.0.1:9696").is_ok());
    assert!(ok("http://192.168.1.5:9696").is_ok());
    for bad in [
        "ftp://prowlarr.example/",
        "http://user:pass@prowlarr.example/",
        "http://prowlarr.example/?apikey=x",
        "http://prowlarr.example/#x",
        "http://169.254.169.254/",
        "not a url",
    ] {
        assert!(ok(bad).is_err(), "{bad}");
    }
    assert!(Config::new("http://p.example", Manager::Jackett, Some(0)).is_err());
    assert!(Config::new("http://p.example", Manager::Jackett, Some(9)).is_err());
    let c = Config::from_json(r#"{"url":"http://p.example:9117/","manager":"jackett","agents":3}"#)
        .unwrap();
    assert_eq!((c.manager(), c.agents()), (Manager::Jackett, 3));
    // A key in the plain settings is refused: it's a credential.
    let e = Config::from_json(r#"{"url":"http://p.example","manager":"prowlarr","api_key":"k"}"#)
        .unwrap_err();
    assert_eq!(e, ConfigError("the settings don't match the form"));
    assert!(Config::from_json(r#"{"url":"http://p.example","manager":"sonarr"}"#).is_err());
}

#[test]
fn endpoints_are_the_configured_host_only() {
    let p = plugin("http://prowlarr.example:9696/sub", Manager::Prowlarr);
    let e = p.endpoints().unwrap();
    assert_eq!(
        e,
        lodestar_sources::Endpoints::user_entered(
            lodestar_integrations::net::NetPolicy::download_client()
        )
        .only_origins(&["http://prowlarr.example:9696"])
    );
}

#[test]
fn nothing_prints_the_key() {
    let p = Prowlarr::new(
        Config::new("http://p.example", Manager::Prowlarr, None).unwrap(),
        Arc::new(StaticCredentials::new([("api_key", "topsecretkey")])),
    );
    assert!(!format!("{p:?}").contains("topsecretkey"));
}

// ---- Indexer lists ----

#[test]
fn prowlarr_list_keeps_enabled_torrent_indexers() {
    let body = std::fs::read(fixture("prowlarr_rss/001.body")).unwrap();
    let list = parse_indexers(Manager::Prowlarr, &body).unwrap();
    let got: Vec<_> = list.iter().map(|i| (i.id.as_str(), i.private)).collect();
    assert_eq!(got, [("1", false), ("2", true)]);
    for bad in [
        &b"{}"[..],
        b"[1]",
        b"[{\"id\":-1}]",
        b"",
        b"[",
        b"[{\"id\":1,\"id\":2}]",
    ] {
        assert_eq!(
            parse_indexers(Manager::Prowlarr, bad),
            Err(ListError::Malformed),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
    // Unknown privacy is private; duplicates are polled once.
    let odd = br#"[{"id":5,"enable":true,"protocol":"torrent","privacy":"Public"},
                   {"id":5,"enable":true,"protocol":"torrent","privacy":"public"},
                   {"id":6,"enable":true,"protocol":"torrent"}]"#;
    let got = parse_indexers(Manager::Prowlarr, odd).unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|i| i.private));
}

#[test]
fn jackett_list_reads_type_and_configured() {
    let body = std::fs::read(fixture("jackett_rss/001.body")).unwrap();
    let list = parse_indexers(Manager::Jackett, &body).unwrap();
    let got: Vec<_> = list.iter().map(|i| (i.id.as_str(), i.private)).collect();
    assert_eq!(got, [("examplepublic", false), ("example-semi", true)]);

    let one = |inner: &str| {
        parse_indexers(
            Manager::Jackett,
            format!("<indexers>{inner}</indexers>").as_bytes(),
        )
    };
    let private = |inner: &str| one(inner).unwrap()[0].private;
    assert!(!private(
        r#"<indexer id="a" configured="true"><type>public</type></indexer>"#
    ));
    assert!(private(r#"<indexer id="a" configured="true"></indexer>"#));
    assert!(private(r#"<indexer id="a" configured="true"/>"#));
    assert!(private(
        r#"<indexer id="a" configured="true"><type>pub&#108;ic</type></indexer>"#
    ));
    assert!(private(
        r#"<indexer id="a" configured="true"><type><![CDATA[public]]></type></indexer>"#
    ));
    assert!(private(
        r#"<indexer id="a" configured="true"><type>private</type><type>public</type></indexer>"#
    ));
    for id in ["", "../x", "a/b", "a b", "a&amp;b", "ALL", &"x".repeat(65)] {
        let xml = format!(r#"<indexer id="{id}" configured="true"><type>public</type></indexer>"#);
        assert_eq!(one(&xml), Ok(vec![]), "{id}");
    }
    assert_eq!(one(""), Ok(vec![]));
    assert_eq!(parse_indexers(Manager::Jackett, b"<indexers/>"), Ok(vec![]));
}

#[test]
fn jackett_list_refuses_hostile_documents() {
    let p = |b: &[u8]| parse_indexers(Manager::Jackett, b);
    assert_eq!(
        p(b"<!DOCTYPE x [<!ENTITY e \"public\">]><indexers/>"),
        Err(ListError::Doctype)
    );
    let deep = format!(
        "<indexers>{}{}</indexers>",
        "<a>".repeat(40),
        "</a>".repeat(40)
    );
    assert_eq!(p(deep.as_bytes()), Err(ListError::Depth));
    assert_eq!(p(b"<indexers>"), Err(ListError::Malformed));
    assert_eq!(p(b"<indexers></indexer>"), Err(ListError::Malformed));
    assert_eq!(p(b"<indexers/><indexers/>"), Err(ListError::Malformed));
    assert_eq!(p(b"<rss/>"), Err(ListError::Malformed));
    assert_eq!(p(b"\xff<indexers/>"), Err(ListError::Malformed));
    assert_eq!(
        p(br#"<error code="101" description="x"/>"#),
        Err(ListError::Refused)
    );
    assert_eq!(p(br#"<error code="300"/>"#), Err(ListError::Malformed));
    let many: String = (0..MAX_INDEXERS + 10)
        .map(|i| format!(r#"<indexer id="i{i}" configured="true"/>"#))
        .collect();
    let list = p(format!("<indexers>{many}</indexers>").as_bytes()).unwrap();
    assert_eq!(list.len(), MAX_INDEXERS);
    let huge = vec![b' '; prowlarr::MAX_LIST_BYTES + 1];
    assert_eq!(p(&huge), Err(ListError::TooBig));
    assert_eq!(
        parse_indexers(Manager::Prowlarr, &huge),
        Err(ListError::TooBig)
    );
}

#[test]
fn prowlarr_list_caps_count_and_nesting() {
    let many: Vec<String> = (0..MAX_INDEXERS + 10)
        .map(|i| format!(r#"{{"id":{i},"enable":true,"protocol":"torrent","privacy":"public"}}"#))
        .collect();
    let body = format!("[{}]", many.join(","));
    let list = parse_indexers(Manager::Prowlarr, body.as_bytes()).unwrap();
    assert_eq!(list.len(), MAX_INDEXERS);
    // Nesting inside a skipped field is walked without recursion, and
    // nesting where an indexer belongs is refused, so neither can
    // overflow the stack.
    let deep = format!(
        r#"[{{"id":1,"enable":true,"protocol":"torrent","x":{}{}}}]"#,
        "[".repeat(100_000),
        "]".repeat(100_000)
    );
    assert_eq!(
        parse_indexers(Manager::Prowlarr, deep.as_bytes()).map(|l| l.len()),
        Ok(1)
    );
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(
        parse_indexers(Manager::Prowlarr, deep.as_bytes()),
        Err(ListError::Malformed)
    );
}

// ---- Items to discoveries ----

fn item(kind: Kind) -> Item {
    Item {
        kind,
        title: Some("T".into()),
        size: Some(1),
        categories: vec![],
        published: Some(1_800_000_000),
        seeders: None,
        leechers: None,
        peers: None,
        files: None,
        download_factor: None,
        upload_factor: None,
        tags: vec![],
        ids: vec![],
        guid: Some("https://t.example/1".into()),
        link: Some("https://t.example/1.torrent".into()),
    }
}

const NOW: i64 = 1_800_000_000;

#[test]
fn manager_download_links_are_never_references() {
    // The manager's links can name another host than the configured one
    // (a reverse proxy's address, a Docker service name).
    for guid in [
        "https://prowlarr.mydomain.net/1/download?link=YmFyYmF6&file=X",
        "http://prowlarr:9696/prowlarr/12/download?link=x",
        "http://jackett.mydomain.net/dl/example-semi/?path=abc&file=Tears",
        "http://10.0.0.2:9117/jackett/DL/x/",
    ] {
        assert!(is_manager_download(guid), "{guid}");
        let mut it = item(Kind::Torrent(Hashes {
            v1: Some(InfoHash([7; 20])),
            v2: None,
        }));
        it.guid = Some(guid.into());
        let f = to_found(it, false, "192.168.1.5", NOW).unwrap();
        assert_eq!(
            f.provenance.as_deref().unwrap().external_ref,
            None,
            "{guid}"
        );
    }
    for keep in [
        "https://public.example/torrent/1234",
        "https://public.example/download.php?id=3",
        "https://public.example/downloads/abc",
        "urn:example:1",
    ] {
        assert!(!is_manager_download(keep), "{keep}");
    }
}

#[test]
fn duplicates_never_crowd_out_the_cap() {
    let dupes = r#"<indexer id="d" configured="true"/>"#.repeat(MAX_INDEXERS + 5);
    let body = format!(r#"<indexers>{dupes}<indexer id="late" configured="true"/></indexers>"#);
    let ids: Vec<_> = parse_indexers(Manager::Jackett, body.as_bytes())
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(ids, ["d", "late"]);
    let dupes = vec![r#"{"id":1,"enable":true,"protocol":"torrent"}"#; MAX_INDEXERS + 5].join(",");
    let body = format!(r#"[{dupes},{{"id":2,"enable":true,"protocol":"torrent"}}]"#);
    let ids: Vec<_> = parse_indexers(Manager::Prowlarr, body.as_bytes())
        .unwrap()
        .into_iter()
        .map(|i| i.id)
        .collect();
    assert_eq!(ids, ["1", "2"]);
}

#[test]
fn usenet_items_are_not_torrents() {
    assert!(to_found(item(Kind::Usenet), false, "m.example", NOW).is_none());
}

prop_compose! {
    fn any_item()(
        v1 in proptest::option::of(any::<[u8; 20]>()),
        size in any::<Option<u64>>(),
        published in any::<Option<i64>>(),
        guid in proptest::option::of(
            "(https?://(m|t|proxy)\\.example/[a-z0-9.]{0,8}|https?://[a-z]{1,6}\\.example(/[a-z]{1,4})?/([0-9]{1,3}/download|dl/[a-z]{1,6}/)(\\?link=[a-z]{1,6})?|urn:x:[a-z]{0,4})"
        ),
        same in any::<bool>(),
    ) -> Item {
        let mut it = item(Kind::Torrent(Hashes { v1: v1.map(InfoHash), v2: None }));
        it.size = size;
        it.published = published;
        if same {
            it.link = guid.clone();
        }
        it.guid = guid;
        it
    }
}

proptest! {
    #[test]
    fn found_holds_its_invariants(it in any_item(), private in any::<bool>()) {
        let had_key = matches!(it.kind, Kind::Torrent(h) if h.key().is_some());
        let link = it.link.clone();
        match to_found(it, private, "m.example", NOW) {
            None => prop_assert!(!had_key),
            Some(f) => {
                prop_assert_eq!(f.private_source, private);
                prop_assert!(f.peers.is_empty() && !f.interest);
                let c = f.claims.as_deref().unwrap();
                prop_assert!(c.size.is_none_or(|s| (1..=MAX_SIZE).contains(&s)));
                let p = f.provenance.as_deref().unwrap();
                let dates = prowlarr::MIN_PUBLISHED..=NOW + prowlarr::MAX_FUTURE;
                prop_assert!(p.published_at.is_none_or(|t| dates.contains(&t)));
                if let Some(r) = &p.external_ref {
                    prop_assert!(!private);
                    prop_assert!(!is_manager_download(r), "{r}");
                    prop_assert!(!r.contains("m.example"));
                    prop_assert!(Some(r) != link.as_ref());
                }
            }
        }
    }

    #[test]
    fn lists_never_panic(body in proptest::collection::vec(any::<u8>(), 0..512)) {
        for m in [Manager::Prowlarr, Manager::Jackett] {
            if let Ok(list) = parse_indexers(m, &body) {
                prop_assert!(list.len() <= MAX_INDEXERS);
                for i in list {
                    prop_assert!(!i.id.is_empty() && i.id.len() <= prowlarr::MAX_ID);
                    prop_assert!(i.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
                }
            }
        }
    }

    #[test]
    fn jackett_lists_from_fragments_never_panic(
        parts in proptest::collection::vec(prop_oneof![
            Just("<indexers>"), Just("</indexers>"), Just("<indexer id=\"a\" configured=\"true\">"),
            Just("</indexer>"), Just("<type>"), Just("</type>"), Just("public"), Just("&amp;"),
            Just("<indexer id=\"b\" configured=\"true\"/>"), Just("<caps>"), Just("</caps>"),
            Just("<![CDATA[x]]>"), Just("<!-- c -->"), Just("<error code=\"100\"/>"),
        ], 0..40)
    ) {
        let body = parts.concat();
        if let Ok(list) = parse_indexers(Manager::Jackett, body.as_bytes()) {
            prop_assert!(list.len() <= 2, "deduplicated: {list:?}");
        }
    }
}
