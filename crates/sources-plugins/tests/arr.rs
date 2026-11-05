//! Contract tests for the Sonarr and Radarr webhooks plugin (§18.9), on
//! hand-written bodies in `fixtures/arr/` in the apps' documented shapes,
//! with no network and no fixed sleeps.

use std::path::PathBuf;
use std::sync::Arc;

use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, wait_for};
use lodestar_sources::{Consent, Discovery, Distribution, Found, SourcePlugin};
use lodestar_sources_plugins::arr::{
    self, App, ArrWebhooks, Delivery, EventType, LIMITS, MAX_SIZE, MAX_TITLE, Malformed,
    STRATEGY_DOWNLOAD, STRATEGY_GRAB, parse, to_found,
};
use lodestar_store::SourceId;
use proptest::prelude::*;

const BUNNY: &str = "fb9ff1ef824d61bd83b24fb91811804db71c0c53";
const SINTEL: &str = "6cd8bae816112560c859c22a04238d8f22c552df";

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/arr")
            .join(name),
    )
    .unwrap()
}

fn hash(hex: &str) -> InfoHash {
    hex.parse().unwrap()
}

/// §18.7: every find is private, with nothing a public lookup, a feed or
/// an export could use.
fn assert_private(f: &Found) {
    assert!(f.private_source, "{f:?}");
    assert!(f.peers.is_empty() && !f.interest);
    let p = f.provenance.as_deref().unwrap();
    assert!(p.external_ref.is_none() && p.published_at.is_none());
    if let Some(c) = f.claims.as_deref() {
        assert!(c.trackers.is_empty() && c.publisher.is_none() && c.description.is_none());
        assert!(c.files.is_empty() && c.adult.is_none());
    }
}

fn ids(f: &Found) -> Vec<(String, String)> {
    f.claims
        .as_deref()
        .map(|c| {
            c.ids
                .iter()
                .map(|i| (i.scheme.clone(), i.value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn sonarr_grab_maps_to_a_private_key_with_title_size_and_ids() {
    let e = parse(App::Sonarr, &fixture("sonarr_grab.json")).unwrap();
    assert_eq!(e.event_type, EventType::Grab);
    let f = to_found(&e).unwrap();
    assert_eq!(f.key, hash(BUNNY));
    assert_eq!(f.strategy, STRATEGY_GRAB);
    assert_private(&f);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        c.name.as_deref(),
        Some("Example.Open.Series.S01E01.1080p.WEB-DL-OPEN")
    );
    assert_eq!(c.size, Some(1_503_238_553));
    assert_eq!(
        ids(&f),
        [
            ("tvdb".into(), "81189".into()),
            ("tmdb".into(), "1396".into()),
            ("imdb".into(), "tt0903747".into())
        ]
    );
    // The indexer's name and the client are never kept.
    let shown = format!("{f:?}");
    assert!(!shown.contains("Private Tracker") && !shown.contains("qBittorrent"));
}

#[test]
fn sonarr_download_takes_the_scene_name_and_no_file_size() {
    let e = parse(App::Sonarr, &fixture("sonarr_download.json")).unwrap();
    assert_eq!(e.event_type, EventType::Download);
    let f = to_found(&e).unwrap();
    assert_eq!((f.key, f.strategy), (hash(BUNNY), STRATEGY_DOWNLOAD));
    assert_private(&f);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        c.name.as_deref(),
        Some("Example.Open.Series.S01E01.1080p.WEB-DL-OPEN")
    );
    // One imported file's size isn't the release's.
    assert_eq!(c.size, None);
    assert!(!format!("{f:?}").contains("/tv/"));
}

#[test]
fn radarr_grab_and_download_read_the_movie() {
    let g = to_found(&parse(App::Radarr, &fixture("radarr_grab.json")).unwrap()).unwrap();
    assert_eq!((g.key, g.strategy), (hash(SINTEL), STRATEGY_GRAB));
    assert_private(&g);
    assert_eq!(g.claims.as_deref().unwrap().size, Some(1_288_490_188));
    assert_eq!(
        ids(&g),
        [
            ("tmdb".into(), "45745".into()),
            ("imdb".into(), "tt1727587".into())
        ]
    );
    let d = to_found(&parse(App::Radarr, &fixture("radarr_download.json")).unwrap()).unwrap();
    assert_eq!((d.key, d.strategy), (hash(SINTEL), STRATEGY_DOWNLOAD));
    assert_eq!(
        d.claims.as_deref().unwrap().name.as_deref(),
        Some("Sintel.2010.1080p.BluRay.x264-BLENDER")
    );
    // Read as Sonarr, a Radarr body has no series: no ids, still the key.
    let s = to_found(&parse(App::Sonarr, &fixture("radarr_grab.json")).unwrap()).unwrap();
    assert!(ids(&s).is_empty());
    assert_eq!(s.key, hash(SINTEL));
}

#[test]
fn tests_other_events_and_usenet_find_nothing() {
    let t = parse(App::Sonarr, &fixture("sonarr_test.json")).unwrap();
    assert_eq!(t.event_type, EventType::Test);
    assert!(to_found(&t).is_none());
    let u = parse(App::Sonarr, &fixture("sonarr_grab_usenet.json")).unwrap();
    assert_eq!((u.event_type, u.key), (EventType::Grab, None));
    assert!(to_found(&u).is_none());
    for body in [
        r#"{"eventType":"Rename","downloadId":"fb9ff1ef824d61bd83b24fb91811804db71c0c53"}"#,
        r#"{"downloadId":"fb9ff1ef824d61bd83b24fb91811804db71c0c53"}"#,
        r#"{"eventType":"Grab","downloadId":"fb9ff1ef824d61bd83b24fb91811804db71c0c5"}"#,
        r#"{"eventType":"Grab","downloadId":"zb9ff1ef824d61bd83b24fb91811804db71c0c53"}"#,
        r#"{"eventType":"Grab","downloadId":12}"#,
        r#"{}"#,
    ] {
        let e = parse(App::Sonarr, body.as_bytes()).unwrap();
        assert!(to_found(&e).is_none(), "{body}");
    }
}

#[test]
fn malformed_bodies_are_refused_whole() {
    let deep = format!("{}{}", "{\"a\":".repeat(40), "1");
    let many = format!(
        "{{\"x\":[{}],\"eventType\":\"Grab\"}}",
        vec!["0"; LIMITS.max_values + 1].join(",")
    );
    let big = format!(
        "{{\"eventType\":\"Grab\",\"pad\":\"{}\"}}",
        "a".repeat(LIMITS.max_bytes)
    );
    let cases: Vec<Vec<u8>> = vec![
        b"".to_vec(),
        b"[]".to_vec(),
        b"null".to_vec(),
        b"\"Grab\"".to_vec(),
        b"{\"eventType\":\"Grab\",}".to_vec(),
        b"{\"eventType\":'Grab'}".to_vec(),
        b"{\"eventType\":\"Grab\"} {}".to_vec(),
        b"{\"eventType\":\"Gr\x01ab\"}".to_vec(),
        b"{\"eventType\":\"\xff\"}".to_vec(),
        b"{\"release\":{\"size\":01}}".to_vec(),
        deep.into_bytes(),
        many.into_bytes(),
        big.into_bytes(),
    ];
    for body in cases {
        assert_eq!(
            parse(App::Sonarr, &body),
            Err(Malformed),
            "{:?}",
            String::from_utf8_lossy(&body[..body.len().min(80)])
        );
    }
    // An array past `max_array` refuses the body, even well under the
    // value cap; one at the cap is fine.
    let array = |n: usize| {
        format!(
            "{{\"eventType\":\"Test\",\"episodes\":[{}]}}",
            vec!["0"; n].join(",")
        )
    };
    const { assert!(LIMITS.max_array < LIMITS.max_values / 2) };
    assert_eq!(
        parse(App::Sonarr, array(LIMITS.max_array + 1).as_bytes()),
        Err(Malformed)
    );
    assert!(parse(App::Sonarr, array(LIMITS.max_array).as_bytes()).is_ok());
    // The cap holds at its edge: a body of exactly `max_bytes` parses.
    let pad = LIMITS.max_bytes - r#"{"eventType":"Test","p":""}"#.len();
    let edge = format!("{{\"eventType\":\"Test\",\"p\":\"{}\"}}", "a".repeat(pad));
    assert_eq!(edge.len(), LIMITS.max_bytes);
    assert!(parse(App::Sonarr, edge.as_bytes()).is_ok());
}

#[test]
fn fields_are_capped_and_cleaned() {
    let long = "T".repeat(LIMITS.max_text + 1);
    let body = format!(
        r#"{{"eventType":"Grab","downloadId":"{BUNNY}","release":{{"releaseTitle":"{long}","size":{}}},"episodeFile":{{"sceneName":"Fallback\u202e Name"}}}}"#,
        MAX_SIZE + 1
    );
    let f = to_found(&parse(App::Sonarr, body.as_bytes()).unwrap()).unwrap();
    let c = f.claims.as_deref().unwrap();
    // The over-long title is dropped, not cut, and the scene name stands
    // in, with its bidi override removed; the size is out of range.
    assert_eq!(c.name.as_deref(), Some("Fallback Name"));
    assert_eq!(c.size, None);
    // A repeated field keeps its first value; a wrong type is skipped.
    let body = format!(
        r#"{{"eventType":"Grab","downloadId":"{BUNNY}","eventType":"Test","release":{{"releaseTitle":["x"],"size":"12"}},"series":{{"tvdbId":"0","imdbId":42}}}}"#
    );
    let e = parse(App::Sonarr, body.as_bytes()).unwrap();
    assert_eq!(e.event_type, EventType::Grab);
    // The first usable size wins: a repeat or a later non-number doesn't
    // change it.
    let body = format!(
        r#"{{"eventType":"Grab","downloadId":"{BUNNY}","release":{{"size":500,"size":900,"size":"x"}}}}"#
    );
    assert_eq!(parse(App::Sonarr, body.as_bytes()).unwrap().size, Some(500));
    assert_eq!((e.title, e.size), (None, None));
    assert_eq!(e.ids, [("imdb", "tt0000042".to_owned())]);
    const { assert!(MAX_TITLE <= lodestar_sources::claims::MAX_NAME) };
}

#[test]
fn caps_consent_and_endpoints() {
    let p = ArrWebhooks::new();
    let m = p.manifest();
    assert_eq!(m.id, arr::ID);
    assert_eq!(m.consent, Consent::None);
    assert!(m.endpoints.is_empty(), "it contacts nothing");
    assert!(m.finds_private, "never named to a public viewer (§18.7)");
    assert!(!m.enabled_by_default);
    assert_eq!(p.policy().distribution, Distribution::Coordinator);
    assert_eq!(p.policy().max_agents, 1);
    assert_eq!(arr::LIMITS.max_bytes, 256 * 1024);
}

fn find(d: &Discovery) -> &Found {
    &d.found
}

/// The inbox takes deliveries only while the plugin runs, and hands each
/// find to the pipeline at once; the plugin stops within a second and
/// takes nothing after.
#[tokio::test]
async fn the_inbox_delivers_only_while_the_plugin_runs() {
    let p = Arc::new(ArrWebhooks::new());
    let inbox = p.inbox();
    let grab = fixture("sonarr_grab.json");
    assert_eq!(
        inbox.deliver(App::Sonarr, &grab),
        Delivery::Off { test: false }
    );
    // The app's Test button is told, so it shows the source is off.
    let test = fixture("sonarr_test.json");
    assert_eq!(
        inbox.deliver(App::Sonarr, &test),
        Delivery::Off { test: true }
    );
    let mut ctx = FakeCtx::new(&*p, SourceId::new(9).unwrap());
    let running = ctx.start(p.clone());
    wait_for("the plugin to start", || inbox.is_running()).await;
    assert_eq!(inbox.deliver(App::Sonarr, &grab), Delivery::Found);
    assert_eq!(
        inbox.deliver(App::Radarr, &fixture("radarr_download.json")),
        Delivery::Found
    );
    assert_eq!(
        inbox.deliver(App::Sonarr, &fixture("sonarr_test.json")),
        Delivery::Ignored
    );
    assert_eq!(inbox.deliver(App::Sonarr, b"{nope"), Delivery::Malformed);
    assert_eq!(inbox.deliver(App::Sonarr, b"[1]"), Delivery::Malformed);
    assert_eq!(inbox.malformed(), 2);
    let mut got = Vec::new();
    wait_for("the finds", || {
        got.extend(ctx.drain());
        got.len() >= 2
    })
    .await;
    assert_eq!(got.len(), 2);
    assert_eq!(find(&got[0]).key, hash(BUNNY));
    assert_eq!(find(&got[1]).key, hash(SINTEL));
    for d in &got {
        assert_private(find(d));
        assert_eq!(d.source, SourceId::new(9).unwrap());
    }
    running.stop_within().await.unwrap();
    assert!(!inbox.is_running());
    assert_eq!(
        inbox.deliver(App::Sonarr, &grab),
        Delivery::Off { test: false }
    );
    assert!(ctx.drain().is_empty());
}

/// An old run ending after a newer one started leaves the newer one's
/// emitter in place.
#[tokio::test]
async fn an_old_run_ending_late_keeps_the_new_one_connected() {
    let p = Arc::new(ArrWebhooks::new());
    let inbox = p.inbox();
    let mut old = FakeCtx::new(&*p, SourceId::new(9).unwrap());
    let first = old.start(p.clone());
    wait_for("the first run", || inbox.is_running()).await;
    let mut new = FakeCtx::new(&*p, SourceId::new(9).unwrap());
    let second = new.start(p.clone());
    // The second run lends its emitter; finds then go to it.
    let grab = fixture("sonarr_grab.json");
    let mut got = Vec::new();
    wait_for("the second run's emitter", || {
        assert_eq!(inbox.deliver(App::Sonarr, &grab), Delivery::Found);
        got.extend(new.drain());
        !got.is_empty()
    })
    .await;
    first.stop_within().await.unwrap();
    assert!(inbox.is_running());
    assert_eq!(inbox.deliver(App::Sonarr, &grab), Delivery::Found);
    second.stop_within().await.unwrap();
    assert!(!inbox.is_running());
}

/// Sonarr 4 and Radarr 5 send the release with a Download too; its size is
/// the release's, so it's claimed.
#[test]
fn a_download_with_its_release_claims_the_release_size() {
    let body = format!(
        r#"{{"eventType":"Download","downloadId":"{BUNNY}","release":{{"releaseTitle":"Release.Title","size":1000}},"episodeFile":{{"sceneName":"Scene.Name","size":10}}}}"#
    );
    let f = to_found(&parse(App::Sonarr, body.as_bytes()).unwrap()).unwrap();
    let c = f.claims.as_deref().unwrap();
    assert_eq!(
        (c.name.as_deref(), c.size),
        (Some("Release.Title"), Some(1000))
    );
}

/// A pipeline that's full drops the find rather than waiting: the API's
/// handler never blocks on it.
#[tokio::test]
async fn a_full_pipeline_is_busy_not_blocked() {
    let p = Arc::new(ArrWebhooks::new());
    let inbox = p.inbox();
    let mut ctx = FakeCtx::new(&*p, SourceId::new(9).unwrap());
    let running = ctx.start(p.clone());
    wait_for("the plugin to start", || inbox.is_running()).await;
    let grab = fixture("sonarr_grab.json");
    let mut found = 0;
    let busy = loop {
        match inbox.deliver(App::Sonarr, &grab) {
            Delivery::Found => found += 1,
            other => break other,
        }
        assert!(found < 1_000_000, "the channel never filled");
    };
    assert_eq!(busy, Delivery::Busy);
    assert!(found > 0);
    running.stop_within().await.unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Any bytes: no panic, and any find is private with claims within
    /// their limits.
    #[test]
    fn any_body_parses_or_is_refused_and_finds_stay_private(
        body in proptest::collection::vec(any::<u8>(), 0..512),
        radarr in any::<bool>(),
    ) {
        let app = if radarr { App::Radarr } else { App::Sonarr };
        if let Ok(e) = parse(app, &body)
            && let Some(f) = to_found(&e)
        {
            assert_private(&f);
        }
    }

    /// Bodies shaped like events, with arbitrary values in the kept
    /// fields.
    #[test]
    fn event_shaped_bodies_map_safely(
        event in prop_oneof![Just("Grab".to_owned()), Just("Download".to_owned()), Just("Test".to_owned()), ".{0,8}"],
        id in "[0-9a-fA-F]{38,42}",
        title in ".{0,64}",
        size in any::<i64>(),
        tvdb in any::<i64>(),
        imdb in ".{0,40}",
    ) {
        let body = serde_json::json!({
            "eventType": event,
            "downloadId": id,
            "release": {"releaseTitle": title, "size": size},
            "series": {"tvdbId": tvdb, "imdbId": imdb},
        })
        .to_string();
        let e = parse(App::Sonarr, body.as_bytes()).unwrap();
        if let Some(t) = &e.title {
            prop_assert!(!t.is_empty() && t.len() <= MAX_TITLE);
            prop_assert!(!t.chars().any(char::is_control));
        }
        if let Some(s) = e.size {
            prop_assert!((1..=MAX_SIZE).contains(&s));
        }
        for (_, v) in &e.ids {
            prop_assert!(v.bytes().all(|b| b.is_ascii_alphanumeric()));
        }
        match to_found(&e) {
            Some(f) => {
                assert_private(&f);
                prop_assert!(matches!(e.event_type, EventType::Grab | EventType::Download));
                prop_assert_eq!(f.key.to_string(), id.to_ascii_lowercase());
            }
            None => prop_assert!(
                !matches!(e.event_type, EventType::Grab | EventType::Download)
                    || id.len() != 40
            ),
        }
    }
}
