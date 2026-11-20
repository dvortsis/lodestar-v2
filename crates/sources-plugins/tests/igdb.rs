//! Contract tests for the IGDB plugin (ENR-igdb, §18.9), on the fixtures
//! in `fixtures/igdb/` (see its `README.md`), with no network: a match, no
//! match, a tie, an adult game, the token exchange and its reuse, the
//! requests it sends, the credentials, errors (401, 429, a timeout, a
//! redirect, a broken answer, a refused or bad token answer), and its
//! manifest. No fixed sleeps: paused time.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lodestar_integrations::secret::Secret;
use lodestar_sources::consent::Need;
use lodestar_sources::identify::{Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Distribution, Endpoints, Fetch, HttpError, Kind, Kit, MemoryState, Request,
    Response, UserLimits, floors,
};
use lodestar_sources_feeds::igdb::{LIMITS, TOKEN_LIMITS, parse_games, parse_token};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::igdb::{self, Igdb};

/// Made up, the same as in every fixture.
const CLIENT: &str = "testclientid0";
const SECRET: &str = "testclientsecret0";
const TOKEN: &str = "faketoken0000igdb";
const QUEST: &str = "Synthetic Quest";

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/igdb")
        .join(case)
}

fn plugin() -> Igdb {
    Igdb::new(Arc::new(StaticCredentials::new([
        (igdb::CLIENT_ID, CLIENT),
        (igdb::CLIENT_SECRET, SECRET),
    ])))
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        plugin().policy(),
        &UserLimits::default(),
    )
}

fn game(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Game).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = plugin().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

fn clean(s: &str) -> bool {
    !s.contains("SENTINEL") && !s.contains(TOKEN) && !s.contains(SECRET)
}

/// A [`Fetch`] that counts requests and never answers.
#[derive(Default)]
struct Silent(AtomicUsize);

impl Fetch for Silent {
    fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_match_maps_to_an_identification() {
    let (got, replayer) = lookup("igdb_match", &game(QUEST, Some(2015))).await;
    replayer.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("igdb", "900001")]);
    assert_eq!(i.canonical_title.as_deref(), Some(QUEST));
    // Genres by IGDB's fixed ids; the unknown one is dropped.
    assert_eq!(i.genres, ["Role-playing (RPG)", "Adventure"]);
    assert_eq!(
        (i.year, i.confidence),
        (Some(2015), igdb::CONFIDENCE_WITH_YEAR)
    );
    // No Erotic theme: unmarked, which IGDB's terms accept.
    assert_eq!(i.adult, None);
    assert!(igdb::TERMS.accepts(i.adult));
    let claims = i.clone().into_claims();
    assert_eq!(claims.ids.len(), 1, "the igdb scheme survives");
    assert!(clean(&format!("{claims:?}")));
}

#[tokio::test(start_paused = true)]
async fn a_tie_is_no_match_and_the_year_picks_one() {
    let hits = parse_games(
        &std::fs::read(fixture("igdb_match").join("002.body")).unwrap(),
        &LIMITS,
    )
    .hits;
    // Two games called Synthetic Quest, and no year: it doesn't guess.
    assert_eq!(igdb::choose(&game(QUEST, None), &hits), Lookup::NoMatch);
    let Lookup::Match(i) = igdb::choose(&game("synthetic quest!", Some(2001)), &hits) else {
        panic!("a match")
    };
    assert_eq!(i.ids[0].value, "900003");
    assert_eq!(
        igdb::choose(&game(QUEST, Some(1999)), &hits),
        Lookup::NoMatch
    );
    // A longer title is never the title.
    assert_eq!(
        igdb::choose(&game("Synthetic", None), &hits),
        Lookup::NoMatch
    );
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match() {
    let (got, replayer) = lookup("igdb_no_match", &game("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

/// A game with IGDB's Erotic theme is marked adult, and the terms refuse
/// it: the runtime stores nothing from it.
#[tokio::test(start_paused = true)]
async fn an_erotic_game_is_marked_adult() {
    let (got, r) = lookup("igdb_adult", &game("Example Adult Game", None)).await;
    r.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    assert_eq!(i.adult, Some(true));
    assert!(!igdb::TERMS.accepts(i.adult));
}

// ---- Requests and credentials ----

#[tokio::test(start_paused = true)]
async fn the_token_is_asked_for_once_and_requests_are_paced() {
    let replayer = Arc::new(Replayer::load(&fixture("igdb_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let p = plugin();
    let first = p.lookup(&k, &game(QUEST, Some(2015))).await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = p.lookup(&k, &game("Nothing Like This", None)).await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    let hosts: Vec<String> = seen.iter().map(|(_, r)| r.host().unwrap()).collect();
    assert_eq!(hosts, [igdb::TOKEN_HOST, igdb::HOST, igdb::HOST]);
    // The two searches, on one host, a second apart at least.
    assert!(seen[2].0 - seen[1].0 >= Duration::from_secs(1));
    for (_, req) in &seen {
        assert!(!req.url.contains(TOKEN) && !req.url.contains(SECRET) && !req.url.contains('?'));
        assert!(!format!("{req:?}").contains(TOKEN), "not in Debug");
    }
    // The search's headers and body: the fixed template and the title.
    let names: Vec<&str> = seen[1].1.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["Accept", "Content-Type", "Client-ID", "Authorization"]
    );
    assert!(!format!("{p:?}").contains(TOKEN));
}

#[test]
fn a_hostile_title_is_one_literal() {
    let title = "x\"; fields *; where id > 0; search \"y\\";
    let req = igdb::search_request(
        &game(title, None),
        &Secret::new(CLIENT),
        &Secret::new(TOKEN),
    );
    let body = String::from_utf8(req.body).unwrap();
    assert_eq!(
        body,
        "search \"x; fields *; where id > 0; search y\"; \
         fields name,first_release_date,themes,genres; limit 20;"
    );
    let req = igdb::search_request(
        &game(&"a".repeat(2000), None),
        &Secret::new(CLIENT),
        &Secret::new(TOKEN),
    );
    assert!(req.body.len() < igdb::MAX_QUERY_TITLE + 80);
}

/// No credentials stored, or ones that aren't: nothing is sent, and the
/// error names neither.
#[tokio::test(start_paused = true)]
async fn without_valid_credentials_nothing_is_sent() {
    for (creds, want) in [
        (StaticCredentials::default(), igdb::NO_CREDENTIALS),
        (
            StaticCredentials::new([(igdb::CLIENT_ID, CLIENT)]),
            igdb::NO_CREDENTIALS,
        ),
        (
            StaticCredentials::new([
                (igdb::CLIENT_ID, CLIENT),
                (igdb::CLIENT_SECRET, "bad\r\nX-Evil: 1"),
            ]),
            "the stored Twitch Client ID or secret isn't valid",
        ),
    ] {
        let wire = Arc::new(Silent::default());
        let e = Igdb::new(Arc::new(creds))
            .lookup(&kit(wire.clone()), &game(QUEST, None))
            .await
            .unwrap_err()
            .0;
        assert_eq!(e, want);
        assert!(!e.contains("Evil") && !e.contains(CLIENT));
        assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn nothing_is_sent_for_what_it_cant_search() {
    let wire = Arc::new(Silent::default());
    for q in [
        game("...", None),
        Query::new("Some Film", Some(2001), MediaKind::Movie).unwrap(),
    ] {
        let got = plugin().lookup(&kit(wire.clone()), &q).await;
        assert_eq!(got.unwrap(), Lookup::NoMatch);
    }
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}

// ---- Errors ----

/// A 401 drops the token; while the host is held the next lookup sends
/// nothing, not even a token request; after the hold it asks Twitch once
/// for a new token.
#[tokio::test(start_paused = true)]
async fn a_refusal_drops_the_token_and_never_loops() {
    for (case, first) in [
        ("igdb_refused", igdb::REFUSED),
        ("igdb_rate_limited", igdb::RATE_LIMITED),
    ] {
        let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
        let k = kit(replayer.clone());
        let p = plugin();
        let q = game(QUEST, None);
        let e = p.lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(e, first, "{case}");
        let again = p.lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(again, igdb::HELD, "{case}");
        assert_eq!(replayer.requests().len(), 2, "{case}");
        if case == "igdb_refused" {
            // After the 6 h hold: a new token (the old one would still be
            // fresh, so this proves it was dropped), then the search.
            tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
            let after = p.lookup(&k, &game(QUEST, Some(2015))).await;
            assert!(matches!(after, Ok(Lookup::Match(_))), "{after:?}");
            let hosts: Vec<String> = replayer
                .requests()
                .iter()
                .map(|(_, r)| r.host().unwrap())
                .collect();
            assert_eq!(
                hosts,
                [igdb::TOKEN_HOST, igdb::HOST, igdb::TOKEN_HOST, igdb::HOST]
            );
        }
        replayer.finish().unwrap();
    }
}

/// Two lookups at once with no token held: only one asks Twitch, and the
/// other waits for and reuses that token.
#[tokio::test(start_paused = true)]
async fn concurrent_lookups_ask_for_one_token() {
    let dir = fixture("igdb_two_lookups");
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    let text = |f: &str| String::from_utf8(read(f)).unwrap();
    let search =
        || parse_exchange(&text("002.http"), read("002.body"), read("002.req.body")).unwrap();
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&text("001.http"), read("001.body"), read("001.req.body")).unwrap(),
        search(),
        search(),
    ]));
    let (k, p) = (kit(r.clone()), plugin());
    let q = game(QUEST, Some(2015));
    let (a, b) = tokio::join!(p.lookup(&k, &q), p.lookup(&k, &q));
    assert!(matches!(a, Ok(Lookup::Match(_))), "{a:?}");
    assert!(matches!(b, Ok(Lookup::Match(_))), "{b:?}");
    r.finish().unwrap();
    let hosts: Vec<String> = r
        .requests()
        .iter()
        .map(|(_, q)| q.host().unwrap())
        .collect();
    assert_eq!(hosts, [igdb::TOKEN_HOST, igdb::HOST, igdb::HOST]);
}

/// A 400 from Twitch (a wrong Client ID) holds Twitch's host like a 401
/// or 403, for the 6 h ceiling each time: until it ends, a lookup sends
/// nothing, not even a token request.
#[tokio::test(start_paused = true)]
async fn a_refused_client_id_holds_twitch() {
    let ceiling = Duration::from_secs(6 * 3600);
    for case in ["igdb_token_bad_client", "igdb_token_refused"] {
        let dir = fixture(case);
        let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
        let http = String::from_utf8(read("001.http")).unwrap();
        let refusal = || parse_exchange(&http, read("001.body"), read("001.req.body")).unwrap();
        let replayer = Arc::new(Replayer::new(vec![refusal(), refusal()]));
        let k = kit(replayer.clone());
        let p = plugin();
        let q = game(QUEST, None);
        // Refused twice, each time held for the 6 h ceiling at once: a 400
        // isn't reset by the limiter, and a 401 isn't counted twice.
        for round in 1..=2 {
            let e = p.lookup(&k, &q).await.unwrap_err().0;
            assert_eq!(e, igdb::CREDENTIALS_REFUSED, "{case} {round}");
            assert_eq!(replayer.requests().len(), round, "{case} {round}");
            let again = p.lookup(&k, &q).await.unwrap_err().0;
            assert_eq!(again, igdb::HELD, "{case} {round}");
            tokio::time::advance(ceiling - Duration::from_secs(1)).await;
            let held = p.lookup(&k, &q).await.unwrap_err().0;
            assert_eq!(held, igdb::HELD, "{case} {round}");
            assert_eq!(replayer.requests().len(), round, "{case} {round}");
            tokio::time::advance(Duration::from_secs(2)).await;
        }
        replayer.finish().unwrap();
    }
}

/// A short-lived token (60 s) is still reused: renewal waits at least
/// half its life.
#[tokio::test(start_paused = true)]
async fn a_short_lived_token_is_reused() {
    let dir = fixture("igdb_two_lookups");
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    let text = |f: &str| String::from_utf8(read(f)).unwrap();
    let token = text("001.body").replace("5000000", "60").into_bytes();
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&text("001.http"), token, read("001.req.body")).unwrap(),
        parse_exchange(&text("002.http"), read("002.body"), read("002.req.body")).unwrap(),
        parse_exchange(&text("003.http"), read("003.body"), read("003.req.body")).unwrap(),
    ]));
    let (k, p) = (kit(r.clone()), plugin());
    assert!(matches!(
        p.lookup(&k, &game(QUEST, Some(2015))).await,
        Ok(Lookup::Match(_))
    ));
    let second = p.lookup(&k, &game("Nothing Like This", None)).await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    r.finish().unwrap();
    let hosts: Vec<String> = r
        .requests()
        .iter()
        .map(|(_, q)| q.host().unwrap())
        .collect();
    assert_eq!(hosts, [igdb::TOKEN_HOST, igdb::HOST, igdb::HOST]);
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_secret_or_service_text() {
    let q = game(QUEST, None);
    for (case, want) in [
        ("igdb_error_200", "IGDB's answer wasn't understood"),
        ("igdb_bad_body", "IGDB's answer wasn't understood"),
        ("igdb_server_error", "IGDB's request failed"),
        ("igdb_timeout", "IGDB's request failed"),
        ("igdb_redirect", "IGDB's request failed"),
        ("igdb_token_refused", igdb::CREDENTIALS_REFUSED),
        ("igdb_token_bad_client", igdb::CREDENTIALS_REFUSED),
        ("igdb_token_bad", "Twitch's token answer wasn't understood"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(clean(&e), "{case}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_oversize_answer_is_an_error() {
    let dir = fixture("igdb_match");
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    let text = |f: &str| String::from_utf8(read(f)).unwrap();
    let mut big = read("002.body");
    while big.last().is_some_and(u8::is_ascii_whitespace) {
        big.pop();
    }
    let close = big.pop().unwrap();
    big.resize(LIMITS.max_bytes + 1, b' ');
    big.push(close);
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&text("001.http"), read("001.body"), read("001.req.body")).unwrap(),
        parse_exchange(&text("002.http"), big, read("002.req.body")).unwrap(),
    ]));
    let e = plugin()
        .lookup(&kit(r.clone()), &game(QUEST, Some(2015)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "IGDB's answer wasn't understood");
    r.finish().unwrap();
}

#[test]
fn token_answers_are_strict() {
    let ok = br#"{"access_token":"abc123","expires_in":5000000,"token_type":"bearer"}"#;
    let t = parse_token(ok, &TOKEN_LIMITS).unwrap();
    assert_eq!(
        (t.access_token.as_str(), t.expires_in),
        ("abc123", 5_000_000)
    );
    for bad in [
        &br#"{"access_token":"abc123","expires_in":0}"#[..],
        br#"{"access_token":"abc123"}"#,
        br#"{"expires_in":5}"#,
        br#"{"access_token":"abc123","expires_in":5,"token_type":"mac"}"#,
        br#"{"access_token":"abc123","access_token":"x","expires_in":5}"#,
        br#"{"access_token":"abc 123","expires_in":5}"#,
        br#"{"access_token":"abc123","expires_in":-5}"#,
        br#"{"access_token":"abc123","expires_in":5} x"#,
        br#"[]"#,
    ] {
        assert_eq!(
            parse_token(bad, &TOKEN_LIMITS),
            None,
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
    let long = br#"{"access_token":"abc","expires_in":99999999999}"#;
    assert_eq!(
        parse_token(long, &TOKEN_LIMITS).unwrap().expires_in,
        lodestar_sources_feeds::igdb::MAX_EXPIRES_IN
    );
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let p = plugin();
    let m = p.manifest();
    assert_eq!(m.id, "igdb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(
        hosts,
        [("https", "api.igdb.com"), ("https", "id.twitch.tv")]
    );
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(p.kinds(), [MediaKind::Game]);

    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    let names: Vec<&str> = policy.credentials.iter().map(|c| c.name).collect();
    assert_eq!(names, [igdb::CLIENT_ID, igdb::CLIENT_SECRET]);
    assert!(matches!(Igdb::need(&[]), Some(Need::Credentials)));
    assert!(Igdb::need(&[igdb::CLIENT_ID.to_owned()]).is_some());
    assert_eq!(
        Igdb::need(&[igdb::CLIENT_ID.to_owned(), igdb::CLIENT_SECRET.to_owned()]),
        None
    );
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_secs(1));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    assert!(p.min_interval() >= Duration::from_secs(1));
}

#[test]
fn what_is_sent_and_the_attribution() {
    for word in ["title", "Client ID", "token", igdb::HOST, igdb::TOKEN_HOST] {
        assert!(igdb::SENDS.contains(word), "{word}");
    }
    assert!(igdb::SENDS.contains("Never the info hash"));
    let a = igdb::TERMS.attribution.unwrap();
    assert!(a.text.contains("IGDB") && a.link == "https://www.igdb.com");
}
