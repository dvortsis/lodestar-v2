//! Contract tests for the ThePornDB plugin (ENR-4, §18.9), on hand-written
//! fixtures in `fixtures/tpdb/`, with no network. `README.md` is the
//! pattern they follow (SRC-7's).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_sources::claims::{AdultKind, Claims, ReleaseDate};
use lodestar_sources::consent::Need;
use lodestar_sources::enrich::{AdultEnricher, Lookup, Query, QueryForm};
use lodestar_sources::testing::Replayer;
use lodestar_sources::{
    BoxFuture, Consent, Distribution, Endpoints, Fetch, HttpError, Kind, Kit, MemoryState,
    NoEgress, Request, Response, UserLimits, floors,
};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::tpdb::{self, Tpdb};
use proptest::prelude::*;

const NAME: &str = "Example.Studio.24.02.29.Synthetic.Scene.One.1080p";
const KEY: &str = "REDACTED";

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/tpdb")
        .join(case)
}

fn plugin_with(key: &'static str) -> Tpdb {
    Tpdb::new(Arc::new(StaticCredentials::new([("api_key", key)])))
}

fn plugin() -> Tpdb {
    plugin_with(KEY)
}

fn kit(p: &Tpdb, fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        p.policy(),
        &UserLimits::default(),
    )
}

fn name() -> Query {
    Query::name(NAME).unwrap()
}

/// Looks `q` up on `case`'s recorded answers; returns the outcome and the
/// replayer, which has been checked for stray or missing requests.
async fn lookup(
    case: &str,
    q: &Query,
) -> (Result<Lookup, lodestar_sources::PluginError>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let p = plugin();
    let k = kit(&p, replayer.clone());
    let got = p.lookup(&k, q).await;
    (got, replayer)
}

async fn error_of(case: &str) -> String {
    let (got, replayer) = lookup(case, &name()).await;
    replayer.finish().unwrap();
    got.unwrap_err().0
}

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_the_section() {
    let (got, replayer) = lookup("tpdb_match", &name()).await;
    replayer.finish().unwrap();
    let Lookup::Match(s) = got.unwrap() else {
        panic!("a match")
    };
    // The first kept item, not the second (a JAV).
    assert_eq!(s.scene_id, "00000000-0000-4000-8000-000000000001");
    assert_eq!(s.kind, Some(AdultKind::Scene));
    assert_eq!(s.title.as_deref(), Some("Synthetic Scene One"));
    assert_eq!(s.studio.as_deref(), Some("Example Studio"));
    assert_eq!(s.released, ReleaseDate::parse("2024-02-29"));
    // Names only (Q4), in the service's order; the entry without a usable
    // shape would set `performers_truncated`, as the parser decides.
    assert!(
        s.performers
            .iter()
            .all(|p| p.starts_with("Example Performer"))
    );
    assert!(s.performers.contains(&"Example Performer One".to_owned()));
    // No personal field, URL, id or service text of any kind (Q5).
    let all = format!("{s:?}{:?}{:?}", s.performers, s.title);
    assert!(!all.contains("SENTINEL"), "{all}");
    for p in &s.performers {
        assert!(!p.contains("SENTINEL") && !p.contains("0000"));
    }
}

#[tokio::test(start_paused = true)]
async fn an_empty_answer_is_no_match() {
    let (got, replayer) = lookup("tpdb_miss", &name()).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn a_request_carries_the_name_and_the_key_header_only() {
    let (_, replayer) = lookup("tpdb_match", &name()).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let (_, req) = &seen[0];
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(tpdb::HOST));
    assert_eq!(url.path(), "/scenes", "never /scenes/hash/…");
    let params: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        params,
        [
            ("parse".to_owned(), NAME.to_owned()),
            ("per_page".to_owned(), "5".to_owned())
        ],
        "the name and the page size, nothing else"
    );
    assert!(!req.url.contains(KEY), "{}", req.url);
    assert!(req.body.is_empty());
    let auth = req
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str());
    assert_eq!(auth, Some("Bearer REDACTED"));
    let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "Authorization"], "no other header");
    assert!(!format!("{req:?}").contains(NAME), "Debug shows no name");
}

#[tokio::test(start_paused = true)]
async fn the_request_never_names_a_hash_a_file_a_tracker_or_a_performer() {
    // Whatever the name holds, a request is built from the name alone. An
    // info hash in the name would be the user's torrent name, which the
    // runtime may send; the plugin adds nothing of its own.
    let p = plugin();
    for q in [
        Query::name(NAME).unwrap(),
        Query::name("Some Name With Spaces & Symbols=?#").unwrap(),
    ] {
        let Query::Name(n) = &q else { unreachable!() };
        let req =
            tpdb::search_request(n, &lodestar_integrations::secret::Secret::new(KEY)).unwrap();
        let url = url::Url::parse(&req.url).unwrap();
        assert_eq!(url.path(), "/scenes");
        assert_eq!(url.query_pairs().count(), 2, "{}", req.url);
        assert_eq!(url.fragment(), None);
        for word in ["hash", "performer", "file", "tracker", "announce", "magnet"] {
            assert!(
                !req.url.to_lowercase().contains(word),
                "{word} in {}",
                req.url
            );
        }
        assert!(!req.url.contains(KEY));
    }
    assert_eq!(p.query_form(), QueryForm::Name);
}

#[tokio::test(start_paused = true)]
async fn only_the_name_form_is_answered_and_without_a_request() {
    let p = plugin();
    let parsed = Query::parsed(Some("Site"), ReleaseDate::parse("2024-01-02"), "Title").unwrap();
    let k = kit(&p, Arc::new(NoEgress));
    let e = p.lookup(&k, &parsed).await.unwrap_err().0;
    assert!(e.contains("by name only"), "{e}");
}

#[tokio::test(start_paused = true)]
async fn a_refused_key_says_so_without_the_key_or_the_service_text() {
    for case in ["tpdb_unauthorized", "tpdb_forbidden"] {
        let e = error_of(case).await;
        assert_eq!(e, tpdb::REFUSED, "{case}");
        assert!(!e.contains(KEY) && !e.contains("SENTINEL"), "{e}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("tpdb_rate_limited")).unwrap());
    let p = plugin();
    let k = kit(&p, replayer.clone());
    let e = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_eq!(e, tpdb::RATE_LIMITED);
    // Within the service's Retry-After (120 s) the host is held: the
    // retry fails at once and sends nothing.
    let again = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_eq!(again, tpdb::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    // After the hold the host is tried again (the replayer has no more
    // exchanges, so it fails as a transport error, not as a hold).
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_ne!(later, tpdb::RATE_LIMITED, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

#[tokio::test(start_paused = true)]
async fn a_redirect_is_not_followed() {
    let (got, replayer) = lookup("tpdb_redirect", &name()).await;
    assert_eq!(got.unwrap_err().0, "ThePornDB's request failed");
    assert_eq!(replayer.requests().len(), 1, "no request to the Location");
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_401_hold_does_not_read_as_a_rate_limit() {
    let replayer = Arc::new(Replayer::load(&fixture("tpdb_unauthorized")).unwrap());
    let p = plugin();
    let k = kit(&p, replayer.clone());
    assert_eq!(p.lookup(&k, &name()).await.unwrap_err().0, tpdb::REFUSED);
    let held = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_eq!(held, tpdb::HELD);
    assert_eq!(replayer.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_server_error_or_a_bad_body_is_an_error_never_a_miss() {
    assert!(error_of("tpdb_server_error").await.contains("failed"));
    for case in ["tpdb_bad_body", "tpdb_html_body"] {
        let e = error_of(case).await;
        assert_eq!(e, "ThePornDB's answer wasn't understood", "{case}");
        assert!(!e.contains("SENTINEL"));
    }
}

#[test]
fn answers_that_are_not_understood_are_errors() {
    for body in [
        &b""[..],
        b"[]",
        b"\"data\"",
        b"null",
        // The service's own error object on a 200 is not a miss.
        b"{\"message\":\"anything\"}",
        b"{}",
        // `data` must be the root's own key, with a list, object or null.
        br#"{"message":"data"}"#,
        br#"{"errors":{"data":[]}}"#,
        br#"{"data":"x"}"#,
        br#"{"data":5}"#,
        br#"{"data"}"#,
        // Only the top result is used: an unreadable one is an error, not a
        // reason to take the second-best.
        br#"{"data":[{"id":"bad id!","title":"A"},{"id":"good-1","title":"B"}]}"#,
        b"{\"data\":[{\"id\":\"a\",\"title\":",
        // Every item skipped: nothing the parser trusts.
        b"{\"data\":[{\"title\":\"no id\"},{\"id\":\"not valid!\"}]}",
    ] {
        assert!(
            tpdb::parse_answer(body).is_err(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    for body in [
        &b"{\"data\":[]}"[..],
        b"{\"data\":null}",
        b"{ \"meta\": {\"data\": 5}, \"data\" : [] }",
        b"\xEF\xBB\xBF {\"data\":[]}",
    ] {
        assert_eq!(
            tpdb::parse_answer(body).unwrap(),
            Lookup::NoMatch,
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn only_the_first_result_is_used() {
    let good = br#"{"data":[{"id":"good-1","title":"First"},{"id":"good-2","title":"Second"}]}"#;
    let Lookup::Match(s) = tpdb::parse_answer(good).unwrap() else {
        panic!("a match")
    };
    assert_eq!(
        (s.scene_id.as_str(), s.title.as_deref()),
        ("good-1", Some("First"))
    );
}

#[test]
fn a_match_is_bounded_and_survives_the_claims_limits_unchanged() {
    let body = std::fs::read(fixture("tpdb_match/001.body")).unwrap();
    let Lookup::Match(s) = tpdb::parse_answer(&body).unwrap() else {
        panic!("a match")
    };
    let mut claims = Claims {
        adult: Some(s.clone()),
        ..Claims::default()
    };
    claims.clamp();
    assert_eq!(
        claims.adult,
        Some(s),
        "nothing was left for the clamp to cut"
    );

    // A scene id the section refuses is no match.
    let bad = br#"{"data":{"id":"a/b","title":"T"}}"#;
    assert!(tpdb::parse_answer(bad).is_err(), "skipped by the parser");
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic_and_a_match_stays_within_the_section(
        body in proptest::collection::vec(any::<u8>(), 0..2048)
    ) {
        if let Ok(Lookup::Match(s)) = tpdb::parse_answer(&body) {
            let mut c = Claims { adult: Some(s.clone()), ..Claims::default() };
            c.clamp();
            prop_assert_eq!(c.adult, Some(s));
        }
    }

    #[test]
    fn mutated_recorded_answers_never_panic(
        cut in 0usize..4096, flip in 0usize..4096, byte in any::<u8>()
    ) {
        let mut body = std::fs::read(fixture("tpdb_match/001.body")).unwrap();
        let at = flip % body.len();
        body[at] = byte;
        body.truncate(cut.max(1).min(body.len()));
        let _ = tpdb::parse_answer(&body);
    }
}

// ---- Setup, the key and the manifest ----

#[tokio::test(start_paused = true)]
async fn a_missing_or_invalid_key_fails_before_any_request() {
    let none = Tpdb::new(Arc::new(StaticCredentials::default()));
    let k = kit(&none, Arc::new(NoEgress));
    let e = none.lookup(&k, &name()).await.unwrap_err().0;
    assert!(e.starts_with("needs setup") && e.contains("API key"), "{e}");

    let bad = plugin_with("abc\r\nX-Evil: 1");
    let k = kit(&bad, Arc::new(NoEgress));
    let e = bad.lookup(&k, &name()).await.unwrap_err().0;
    assert!(
        e.contains("isn't a valid key") && !e.contains("Evil"),
        "{e}"
    );
}

#[test]
fn enabling_without_a_key_is_refused() {
    // Account class: the consent layer asks for a stored credential.
    assert_eq!(Tpdb::need(&[]), Some(Need::Credentials));
    assert_eq!(Tpdb::need(&["api_key".to_owned()]), None);
}

#[test]
fn manifest_and_policy() {
    let p = plugin();
    let m = p.manifest();
    assert_eq!(m.id, "tpdb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult);
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "api.theporndb.net")]);
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));

    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    // Q15: one request a second to the host, no burst.
    assert!(limits.per_host.interval >= Duration::from_secs(1));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    let schema: serde_json::Value = serde_json::from_str(policy.config_schema).unwrap();
    assert_eq!(schema["properties"]["api_key"]["writeOnly"], true);
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn nothing_prints_the_key_or_the_query() {
    let p = plugin_with("topsecretkey");
    assert!(!format!("{p:?}").contains("topsecretkey"));
    let req = tpdb::search_request(
        "Secret Name",
        &lodestar_integrations::secret::Secret::new("topsecretkey"),
    )
    .unwrap();
    assert!(!format!("{req:?}").contains("topsecretkey"));
    assert!(!format!("{req:?}").contains("Secret Name"));
    let q = Query::name("Secret Name").unwrap();
    assert!(!format!("{q:?}").contains("Secret"));
}

#[tokio::test(start_paused = true)]
async fn no_error_holds_the_key_or_the_service_text() {
    // The real key is in the request (the fixtures match on it), so a leak
    // in any outcome would show.
    let key = KEY;
    let p = plugin();
    for case in [
        "tpdb_unauthorized",
        "tpdb_forbidden",
        "tpdb_rate_limited",
        "tpdb_server_error",
        "tpdb_bad_body",
        "tpdb_html_body",
        "tpdb_redirect",
    ] {
        let r = Arc::new(Replayer::load(&fixture(case)).unwrap());
        let k = kit(&p, r.clone());
        let e = p.lookup(&k, &name()).await.unwrap_err();
        let shown = format!("{} {e:?}", e.0);
        assert!(
            !shown.contains(key) && !shown.contains("SENTINEL"),
            "{case}"
        );
        r.finish().unwrap();
    }
}

/// A transport that never answers, to cancel a lookup mid-request.
struct Hangs;

impl Fetch for Hangs {
    fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn a_lookup_in_flight_can_be_dropped() {
    let p = plugin();
    let k = kit(&p, Arc::new(Hangs));
    // The runtime races a lookup against shutdown and drops it: nothing is
    // left holding the plugin or the host.
    let raced = tokio::time::timeout(Duration::from_secs(30), p.lookup(&k, &name())).await;
    assert!(raced.is_err(), "the lookup should still be waiting");
    let task = tokio::spawn({
        let k = k.clone();
        async move { plugin().lookup(&k, &name()).await.map(drop) }
    });
    tokio::time::advance(Duration::from_secs(5)).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn lookups_are_at_least_a_second_apart() {
    let replayer = Arc::new(Replayer::load(&fixture("tpdb_two_lookups")).unwrap());
    let p = plugin();
    let k = kit(&p, replayer.clone());
    let first = p.lookup(&k, &name()).await.unwrap();
    assert!(matches!(first, Lookup::Match(_)));
    let second = p
        .lookup(&k, &Query::name("Another.Example.Name").unwrap())
        .await
        .unwrap();
    assert_eq!(second, Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    assert_eq!(seen.len(), 2);
    assert!(
        seen[1].0 - seen[0].0 >= Duration::from_secs(1),
        "{:?} apart",
        seen[1].0 - seen[0].0
    );
}

// ---- Every manifest in the crate (runner, ENR-1c) ----

/// The adult enrichers are picked by id (`PipelineConfig::adult_enrichers`)
/// because the torrent cache is `Kind::Enrichment` too. So no shipped
/// manifest may be both an enricher and always-adult: that would mix the
/// two roles. The manifests are private consts, so this reads every
/// `Manifest { … }` literal in `src/` and checks its `kinds` and
/// `always_adult`; the count guards against the scan finding nothing.
#[test]
fn no_manifest_is_both_enrichment_and_always_adult() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut literals = 0;
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut parts = text.split("= Manifest {");
        parts.next(); // before the first literal
        for body in parts {
            // The literal ends at its closing `};`.
            let body = body.split("\n};").next().unwrap_or(body);
            let body = body.split("\n    };").next().unwrap_or(body);
            literals += 1;
            let enrichment = body.contains("Kind::Enrichment");
            let always_adult = body.contains("always_adult: true");
            assert!(
                !(enrichment && always_adult),
                "{}: a manifest is both Enrichment and always_adult",
                path.display()
            );
            assert!(
                body.contains("always_adult:") || body.contains(".."),
                "{}: the scan didn't find `always_adult`",
                path.display()
            );
        }
    }
    assert!(literals >= 10, "only {literals} manifests found");

    // This plugin, through its trait.
    let m = plugin().manifest().clone();
    assert!(m.is(Kind::Enrichment) && !m.always_adult);
}

/// The decisions read the parser's own report of what it saw (X-enr-page-flag):
/// a repeated `data`, a cut or trailing body, and a fault after the top
/// result are errors, never a miss or a match.
#[test]
fn the_parser_s_report_decides_and_only_a_whole_answer_counts() {
    for body in [
        // A repeated `data`, either way round (escaped repeats: below).
        &br#"{"data":[],"data":[{"id":"good-1"}]}"#[..],
        br#"{"data":[{"id":"good-1"}],"data":[]}"#,
        br#"{"data":null,"data":null}"#,
        // Trailing bytes after a match or a miss.
        br#"{"data":[{"id":"good-1"}]} x"#,
        br#"{"data":[{"id":"good-1"}]}{}"#,
        br#"{"data":[]} x"#,
        br#"{"data":null},"#,
        // Cut after the top result, inside or outside the list.
        br#"{"data":[{"id":"good-1"}"#,
        br#"{"data":[{"id":"good-1"},"#,
        br#"{"data":[{"id":"good-1"},{"id":"#,
        br#"{"data":[{"id":"good-1"}]"#,
        br#"{"data":{"id":"good-1"}"#,
        br#"{"data":[{"id":"good-1"}],"meta":{"total":"#,
        // A structural fault after the top result.
        br#"{"data":[{"id":"good-1"},{"id":"b","title":tru}]}"#,
        br#"{"data":[{"id":"good-1"},[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]]}"#,
        br#"{"data":[{"id":"good-1"}],"links":{"next":"x\q"}}"#,
        // An empty object is an item without an id, not a miss.
        br#"{"data":{}}"#,
        br#"{"data":true}"#,
        br#"{"meta":{"total":0}}"#,
    ] {
        assert!(
            tpdb::parse_answer(body).is_err(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    for body in [
        // The real page's other keys don't matter.
        &br#"{"data":[],"links":{"next":null},"meta":{"total":0}}"#[..],
        br#"{"meta":{"total":0},"meta":5,"data":null}"#,
    ] {
        assert_eq!(
            tpdb::parse_answer(body).unwrap(),
            Lookup::NoMatch,
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    // Past the top result, only the document's structure is checked: a
    // second item the parser would skip doesn't spoil the first.
    for body in [
        &br#"{"data":[{"id":"good-1"},{"id":"bad id!"}],"meta":{"total":2}}"#[..],
        br#"{"data":[{"id":"good-1"},"x",{"id":"good-3"}]}"#,
        br#"{"links":{},"data":{"id":"good-1"}}"#,
    ] {
        let Ok(Lookup::Match(s)) = tpdb::parse_answer(body) else {
            panic!("a match: {}", String::from_utf8_lossy(body))
        };
        assert_eq!(s.scene_id, "good-1");
    }
}

/// Root keys are compared unescaped, so an escaped `data` is `data`, and an
/// escaped repeat is still a repeat. `~` stands for a backslash, built at
/// run time.
#[test]
fn an_escaped_root_key_is_its_name() {
    let esc = |s: &str| s.replace('~', &char::from(92).to_string());
    for body in [
        r#"{"data":[],"d~u0061ta":[{"id":"good-1"}]}"#,
        r#"{"d~u0061ta":[{"id":"good-1"}],"data":[]}"#,
        r#"{"~u0064ata":null,"data":null}"#,
    ] {
        let body = esc(body);
        assert!(tpdb::parse_answer(body.as_bytes()).is_err(), "{body}");
    }
    let miss = esc(r#"{"d~u0061ta":[],"m~u0065ta":{"total":0}}"#);
    assert_eq!(
        tpdb::parse_answer(miss.as_bytes()).unwrap(),
        Lookup::NoMatch
    );
    let one = esc(r#"{"d~u0061ta":{"id":"good-1"}}"#);
    let Ok(Lookup::Match(s)) = tpdb::parse_answer(one.as_bytes()) else {
        panic!("a match: {one}")
    };
    assert_eq!(s.scene_id, "good-1");
}

/// Inside the top item, ENR-2b's house rule stands: the first of a
/// repeated field is used and the repeat is read past (StashDB's parser
/// spoils the scene instead; both are one reader, so neither is read two
/// ways).
#[test]
fn a_repeated_field_in_the_top_item_keeps_the_first() {
    let body = br#"{"data":[{"id":"good-1","title":"First","id":"other","title":"Second"}]}"#;
    let Lookup::Match(s) = tpdb::parse_answer(body).unwrap() else {
        panic!("a match")
    };
    assert_eq!(
        (s.scene_id.as_str(), s.title.as_deref()),
        ("good-1", Some("First"))
    );
}
