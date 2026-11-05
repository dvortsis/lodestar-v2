//! Contract tests for the StashDB plugin (ENR-5, §18.9), on hand-written
//! fixtures in `fixtures/stashdb/`, with no network. `README.md` is the
//! pattern they follow (SRC-7's).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_sources::claims::{AdultKind, Claims, ReleaseDate};
use lodestar_sources::consent::Need;
use lodestar_sources::enrich::{AdultEnricher, Lookup, Query, QueryForm};
use lodestar_sources::http::Method;
use lodestar_sources::testing::Replayer;
use lodestar_sources::{
    BoxFuture, Consent, Distribution, Endpoints, Fetch, HttpError, Kind, Kit, MemoryState,
    NoEgress, Request, Response, UserLimits, floors,
};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::stashdb::{self, StashDb};
use proptest::prelude::*;

const NAME: &str = "Example.Studio.24.03.15.Synthetic.Scene.Alpha.1080p";
const KEY: &str = "REDACTED";

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/stashdb")
        .join(case)
}

fn plugin_with(key: &'static str) -> StashDb {
    StashDb::new(Arc::new(StaticCredentials::new([("api_key", key)])))
}

fn plugin() -> StashDb {
    plugin_with(KEY)
}

fn kit(p: &StashDb, fetch: Arc<dyn Fetch>) -> Kit {
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
    let (got, replayer) = lookup("stashdb_match", &name()).await;
    replayer.finish().unwrap();
    let Lookup::Match(s) = got.unwrap() else {
        panic!("a match")
    };
    // The top result, not the second.
    assert_eq!(s.scene_id, "00000000-0000-4000-8000-0000000000a1");
    assert_eq!(s.kind, Some(AdultKind::Scene));
    assert_eq!(s.title.as_deref(), Some("Synthetic Scene Alpha"));
    assert_eq!(s.studio.as_deref(), Some("Example Studio One"));
    assert_eq!(s.released, ReleaseDate::parse("2024-03-15"));
    // Names only (Q4), in the service's order.
    assert_eq!(
        s.performers,
        ["Example Performer One", "Example Performer Two"]
    );
    assert!(!s.performers_truncated);
    // No personal field, performer id or service text of any kind (Q5).
    let all = format!("{s:?}");
    assert!(!all.contains("SENTINEL"), "{all}");
    assert!(!all.contains("0000000000d1"), "{all}");
}

#[tokio::test(start_paused = true)]
async fn an_empty_answer_is_no_match() {
    let (got, replayer) = lookup("stashdb_miss", &name()).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn a_request_carries_the_name_and_the_key_header_only() {
    let (_, replayer) = lookup("stashdb_match", &name()).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let (_, req) = &seen[0];
    assert_eq!(req.method, Method::Post);
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(stashdb::HOST));
    assert_eq!(url.path(), "/graphql");
    assert_eq!(url.query(), None, "nothing in the URL");
    assert!(
        !req.url.contains(KEY) && !req.url.contains("Secret"),
        "{}",
        req.url
    );
    // The body is ENR-3's fixed document and two variables, the name and
    // the limit of one, and nothing else.
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    let obj = body.as_object().unwrap();
    assert_eq!(obj.len(), 2, "{body}");
    assert_eq!(obj["query"], lodestar_sources_feeds::stashdb::SEARCH_SCENE);
    let vars = obj["variables"].as_object().unwrap();
    assert_eq!(vars.len(), 2, "{body}");
    assert_eq!(vars["term"], NAME);
    assert_eq!(vars["limit"], 1);
    let text = String::from_utf8(req.body.clone()).unwrap();
    assert!(!text.contains(KEY), "the key is in the header only");
    // The exact header list, key last.
    let headers: Vec<(&str, &str)> = req
        .headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        headers,
        [
            ("Accept", "application/json"),
            ("Content-Type", "application/json"),
            ("ApiKey", "REDACTED"),
        ],
        "no other header"
    );
    assert!(!format!("{req:?}").contains(NAME), "Debug shows no name");
    assert!(!format!("{req:?}").contains(KEY), "Debug shows no key");
}

#[tokio::test(start_paused = true)]
async fn the_request_never_names_a_hash_a_file_a_tracker_or_a_performer() {
    let p = plugin();
    for n in [
        "Some Name With Spaces & Symbols=?#",
        "quote\" \\ {braces} $term",
        NAME,
    ] {
        let req =
            stashdb::search_request(n, &lodestar_integrations::secret::Secret::new(KEY)).unwrap();
        assert_eq!(req.url, "https://stashdb.org/graphql");
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        // The name is a variable, never spliced into the document.
        assert_eq!(body["query"], lodestar_sources_feeds::stashdb::SEARCH_SCENE);
        assert_eq!(body["variables"]["term"], n);
        let doc = body["query"].as_str().unwrap().to_lowercase();
        for word in [
            "hash",
            "fingerprint",
            "file",
            "tracker",
            "announce",
            "magnet",
        ] {
            assert!(!doc.contains(word), "{word} in the document");
        }
        // Nothing of the install: the body holds the name and no more.
        assert_eq!(body["variables"].as_object().unwrap().len(), 2);
        assert!(!String::from_utf8_lossy(&req.body).contains(KEY));
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
    for case in ["stashdb_unauthorized", "stashdb_forbidden"] {
        let e = error_of(case).await;
        assert_eq!(e, stashdb::REFUSED, "{case}");
        assert!(!e.contains(KEY) && !e.contains("SENTINEL"), "{e}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("stashdb_rate_limited")).unwrap());
    let p = plugin();
    let k = kit(&p, replayer.clone());
    let e = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_eq!(e, stashdb::RATE_LIMITED);
    // Within the service's Retry-After (120 s) the host is held: the
    // retry fails at once and sends nothing.
    let again = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_eq!(again, stashdb::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    // After the hold the host is tried again (the replayer has no more
    // exchanges, so it fails as a transport error, not as a hold).
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_ne!(later, stashdb::RATE_LIMITED, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

#[tokio::test(start_paused = true)]
async fn a_redirect_is_not_followed() {
    let (got, replayer) = lookup("stashdb_redirect", &name()).await;
    assert_eq!(got.unwrap_err().0, "StashDB's request failed");
    assert_eq!(replayer.requests().len(), 1, "no request to the Location");
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_401_hold_does_not_read_as_a_rate_limit() {
    let replayer = Arc::new(Replayer::load(&fixture("stashdb_unauthorized")).unwrap());
    let p = plugin();
    let k = kit(&p, replayer.clone());
    assert_eq!(p.lookup(&k, &name()).await.unwrap_err().0, stashdb::REFUSED);
    let held = p.lookup(&k, &name()).await.unwrap_err().0;
    assert_eq!(held, stashdb::HELD);
    assert_eq!(replayer.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_server_error_or_a_bad_body_is_an_error_never_a_miss() {
    assert!(error_of("stashdb_server_error").await.contains("failed"));
    for case in [
        "stashdb_bad_body",
        "stashdb_html_body",
        "stashdb_graphql_errors",
    ] {
        let e = error_of(case).await;
        assert_eq!(e, "StashDB's answer wasn't understood", "{case}");
        assert!(!e.contains("SENTINEL"));
    }
}

#[test]
fn answers_that_are_not_understood_are_errors() {
    let id1 = "00000000-0000-4000-8000-0000000000a1";
    let id2 = "00000000-0000-4000-8000-0000000000a2";
    let owned: Vec<String> = vec![
        // Only the top result is used: an unreadable one is an error, not a
        // reason to take the second-best.
        format!(
            r#"{{"data":{{"searchScene":[{{"id":"bad id!","title":"A"}},{{"id":"{id2}","title":"B"}}]}}}}"#
        ),
        format!(
            r#"{{"data":{{"searchScene":[{{"id":"{id1}","title":"A","release_date":"15/03/2024"}},{{"id":"{id2}","title":"B"}}]}}}}"#
        ),
        // GraphQL errors beside an otherwise empty answer.
        r#"{"errors":[{"message":"x"}],"data":{"searchScene":[]}}"#.to_owned(),
        // An error beside a readable top scene: part of it may be missing.
        format!(
            r#"{{"errors":[{{"message":"x"}}],"data":{{"searchScene":[{{"id":"{id1}","title":"T","performers":null}}]}}}}"#
        ),
        // The early stop at a second item must not hide a later `errors`.
        format!(
            r#"{{"data":{{"searchScene":[{{"id":"{id1}","title":"A"}},{{"id":"{id2}","title":"B"}}]}},"errors":[{{"message":"x"}}]}}"#
        ),
        // A body cut right after the top scene, and trailing garbage.
        format!(r#"{{"data":{{"searchScene":[{{"id":"{id1}","title":"A"}}]"#),
        format!(r#"{{"data":{{"searchScene":[{{"id":"{id1}","title":"A"}}]}}}} x"#),
        // `findScene` is another query's answer, alone or beside this one's.
        format!(r#"{{"data":{{"findScene":{{"id":"{id1}","title":"A"}}}}}}"#),
        format!(r#"{{"data":{{"searchScene":[],"findScene":{{"id":"{id1}","title":"A"}}}}}}"#),
        format!(
            r#"{{"data":{{"findScene":{{"id":"{id1}","title":"A"}},"searchScene":[{{"id":"{id2}","title":"B"}}]}}}}"#
        ),
        r#"{"data":{"searchScene":[],"findScene":null}}"#.to_owned(),
        // Repeated keys must not read as a miss.
        r#"{"data":{"searchScene":{"id":"x"}},"data":{"searchScene":[]}}"#.to_owned(),
        r#"{"data":null,"data":{"searchScene":[]}}"#.to_owned(),
        r#"{"data":{"searchScene":null,"searchScene":[]}}"#.to_owned(),
        r#"{"data":{"searchScene":[]},"data":{"searchScene":[]}}"#.to_owned(),
        // Cut off.
        format!(r#"{{"data":{{"searchScene":[{{"id":"{id1}","title":"#),
        // Every item skipped.
        r#"{"data":{"searchScene":[{"title":"no id"},{"id":"not valid!"}]}}"#.to_owned(),
    ];
    let mut bodies: Vec<&[u8]> = vec![
        b"",
        b"[]",
        b"\"data\"",
        b"null",
        b"{}",
        b"{\"data\":null}",
        b"{\"data\":{}}",
        b"{\"data\":[]}",
        b"{\"data\":{\"searchScene\":null}}",
        b"{\"data\":{\"searchScene\":{}}}",
        b"{\"data\":{\"searchScene\":\"x\"}}",
        b"{\"data\":{\"findScene\":null}}",
        // A `searchScene` that is not under `data`.
        b"{\"searchScene\":[]}",
        b"{\"message\":\"searchScene\"}",
        b"{\"errors\":{\"data\":{\"searchScene\":[]}}}",
    ];
    bodies.extend(owned.iter().map(|s| s.as_bytes()));
    for body in bodies {
        assert!(
            stashdb::parse_answer(body).is_err(),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
    for body in [
        &b"{\"data\":{\"searchScene\":[]}}"[..],
        b" {\"extensions\":{\"data\":5},\"data\" : {\"searchScene\" : [ ]} }",
        b"\xEF\xBB\xBF{\"data\":{\"searchScene\":[]}}",
    ] {
        assert_eq!(
            stashdb::parse_answer(body).unwrap(),
            Lookup::NoMatch,
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn only_the_first_result_is_used() {
    let good = br#"{"data":{"searchScene":[
        {"id":"00000000-0000-4000-8000-0000000000a1","title":"First"},
        {"id":"00000000-0000-4000-8000-0000000000a2","title":"Second"}]}}"#;
    let Lookup::Match(s) = stashdb::parse_answer(good).unwrap() else {
        panic!("a match")
    };
    assert_eq!(
        (s.scene_id.as_str(), s.title.as_deref()),
        ("00000000-0000-4000-8000-0000000000a1", Some("First"))
    );
}

#[test]
fn a_match_is_bounded_and_survives_the_claims_limits_unchanged() {
    let body = std::fs::read(fixture("stashdb_match/001.body")).unwrap();
    let Lookup::Match(s) = stashdb::parse_answer(&body).unwrap() else {
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

    // A scene id the parser refuses is an error, not a match.
    let bad = br#"{"data":{"searchScene":[{"id":"a/b","title":"T"}]}}"#;
    assert!(stashdb::parse_answer(bad).is_err(), "skipped by the parser");
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic_and_a_match_stays_within_the_section(
        body in proptest::collection::vec(any::<u8>(), 0..2048)
    ) {
        if let Ok(Lookup::Match(s)) = stashdb::parse_answer(&body) {
            let mut c = Claims { adult: Some(s.clone()), ..Claims::default() };
            c.clamp();
            prop_assert_eq!(c.adult, Some(s));
        }
    }

    #[test]
    fn mutated_recorded_answers_never_panic(
        cut in 0usize..4096, flip in 0usize..4096, byte in any::<u8>()
    ) {
        let mut body = std::fs::read(fixture("stashdb_match/001.body")).unwrap();
        let at = flip % body.len();
        body[at] = byte;
        body.truncate(cut.max(1).min(body.len()));
        let _ = stashdb::parse_answer(&body);
    }
}

// ---- Setup, the key and the manifest ----

#[tokio::test(start_paused = true)]
async fn a_missing_or_invalid_key_fails_before_any_request() {
    let none = StashDb::new(Arc::new(StaticCredentials::default()));
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
    assert_eq!(StashDb::need(&[]), Some(Need::Credentials));
    assert_eq!(StashDb::need(&["api_key".to_owned()]), None);
}

#[test]
fn manifest_and_policy() {
    let p = plugin();
    let m = p.manifest();
    assert_eq!(m.id, "stashdb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult);
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "stashdb.org")]);
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
    let req = stashdb::search_request(
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
        "stashdb_unauthorized",
        "stashdb_forbidden",
        "stashdb_rate_limited",
        "stashdb_server_error",
        "stashdb_bad_body",
        "stashdb_html_body",
        "stashdb_graphql_errors",
        "stashdb_redirect",
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
    let replayer = Arc::new(Replayer::load(&fixture("stashdb_two_lookups")).unwrap());
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

// ---- This plugin's side of the manifest rule (runner, ENR-1c) ----

/// The crate-wide scan that no manifest is both Enrichment and always-adult
/// lives in `tests/tpdb.rs` (ENR-4) and reads every `Manifest` literal in
/// `src/`, this plugin's included. Here, only this plugin's own assertion.
#[test]
fn this_manifest_is_an_enricher_and_not_always_adult() {
    let m = plugin().manifest().clone();
    assert!(m.is(Kind::Enrichment) && !m.always_adult);
}

// ---- The key never leaks, whatever the service does ----

/// Answers every request with one canned response that echoes the request's
/// `ApiKey` back in its body and headers, as a hostile or sloppy service
/// might.
struct Echo {
    status: u16,
    body: &'static str,
    location: bool,
}

impl Fetch for Echo {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        let key = req
            .headers
            .iter()
            .find(|(n, _)| n == "ApiKey")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let mut headers = vec![("X-Echo".to_owned(), key.clone())];
        if self.location {
            headers.push((
                "Location".to_owned(),
                format!("https://elsewhere.example/{key}"),
            ));
        }
        Box::pin(std::future::ready(Ok(Response {
            status: self.status,
            headers,
            body: self.body.replace("KEY", &key).into_bytes(),
        })))
    }
}

#[tokio::test(start_paused = true)]
async fn a_distinctive_key_reaches_no_error_debug_or_log_in_any_outcome() {
    const DISTINCT: &str = "zq9-DISTINCTIVE-key.7f3a|c1";
    let outcomes = [
        (401, r#"{"errors":[{"message":"bad key KEY"}]}"#, false),
        (403, r#"{"errors":[{"message":"forbidden KEY"}]}"#, false),
        (429, r#"{"errors":[{"message":"slow down KEY"}]}"#, false),
        (500, "KEY", false),
        (200, r#"{"data":{"searchScene":[{"id":"KEY""#, false),
        (200, "<html>KEY</html>", false),
        (200, r#"{"errors":[{"message":"KEY"}],"data":null}"#, false),
        (
            200,
            r#"{"data":{"searchScene":[{"id":"KEY","title":"KEY"}]}}"#,
            false,
        ),
        (302, "KEY", true),
    ];
    for (status, body, location) in outcomes {
        let p = plugin_with(DISTINCT);
        let k = kit(
            &p,
            Arc::new(Echo {
                status,
                body,
                location,
            }),
        );
        let got = p.lookup(&k, &name()).await;
        let shown = match got {
            Ok(l) => format!("{l:?}"),
            Err(e) => format!("{} {e:?}", e.0),
        };
        assert!(
            !shown.contains(DISTINCT) && !shown.contains("DISTINCTIVE"),
            "{status} {body}: {shown}"
        );
        assert!(!format!("{p:?}{k:?}").contains("DISTINCTIVE"));
    }
}
