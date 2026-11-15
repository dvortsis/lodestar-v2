//! Contract tests for the TheTVDB plugin (ENR-tvdb, §18.9), on the
//! fixtures in `fixtures/tvdb/` (see its `README.md`), with no network.
//! Three parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   requests it sends, errors (429 back-off, a timeout, a redirect, a
//!   broken or oversize answer), and its manifest;
//! - the credential: the key and PIN read on each lookup, the token kept
//!   in memory, refreshed once on a 401 and never in a loop, a refused
//!   key held, and none of them in an error or `Debug`;
//! - the plugin under ENR-10's runtime and the writer's ingest: off (no
//!   key) nothing is sent, a private item never sent, a held answer never
//!   stored.
//!
//! The key, PIN and tokens here are obviously fake. No fixed sleeps:
//! paused time for the plugin alone, and condition waits of at least
//! 10 s for the runtime.

#[path = "support/harness.rs"]
mod harness;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_filter::Handle;
use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::enrich::{enrich_with_rule, file_infos};
use lodestar_pipeline::identify::{Config, IdentifySource, IdentifyStats, Switches};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::unverified::{self, Ctx, UnverifiedStats};
use lodestar_sources::consent::{Need, need};
use lodestar_sources::identify::{Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::tvdb::SEARCH_LIMITS;
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::tvdb::{self, Tvdb};
use lodestar_store::{NewTorrent, SourceId, SourceState, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const BB: &str = "Breaking Bad";
const T: i64 = 1_700_000_000;
/// Obviously fake, and the same in the fixtures' login bodies.
const KEY: &str = "00000000-fake-0000-0000-000000000000";
const PIN: &str = "FAKEPIN1";

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/tvdb")
        .join(case)
}

fn with_key() -> Tvdb {
    Tvdb::new(Arc::new(StaticCredentials::new([("api_key", KEY)])))
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        with_key().policy(),
        &UserLimits::default(),
    )
}

fn series(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Tv).unwrap()
}

fn replayer(case: &str) -> Arc<Replayer> {
    Arc::new(Replayer::load(&fixture(case)).unwrap())
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let r = replayer(case);
    let got = with_key().lookup(&kit(r.clone()), q).await;
    (got.map_err(|e| e.0), r)
}

/// No error, request or `Debug` holds the key, the PIN or a token.
fn no_secret(text: &str) {
    for s in [KEY, PIN, "fake.token", "fake-0000"] {
        assert!(!text.contains(s), "{s} in {text}");
    }
    assert!(!text.contains("SENTINEL"), "service text in {text}");
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, r) = lookup("tvdb_match", &series(BB, Some(2008))).await;
    r.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    // The series; not the movie of the same name, nor the longer title.
    assert_eq!(
        ids,
        [("tvdb", "81189"), ("imdb", "tt0903747"), ("tmdb", "1396")]
    );
    assert_eq!(i.canonical_title.as_deref(), Some(BB));
    let alts: Vec<(&str, &str)> = i
        .alt_titles
        .iter()
        .map(|a| (a.language.as_str(), a.title.as_str()))
        .collect();
    assert_eq!(
        alts,
        [
            ("de", "Breaking Bad – Die Serie"),
            ("ja", "ブレイキング・バッド"),
            ("und", "Breaking Bad (2008)"),
            ("und", "Во все тяжкие"),
        ]
    );
    assert_eq!(i.genres, ["Drama", "Crime", "Thriller"]);
    assert_eq!(
        (i.year, i.confidence),
        (Some(2008), tvdb::CONFIDENCE_WITH_YEAR)
    );
    let claims = i.clone().into_claims();
    no_secret(&format!(
        "{claims:?}{:?}{:?}",
        i.canonical_title, i.alt_titles
    ));
    assert_eq!(claims.ids.len(), 3);
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.alt_titles.len(), 4);
    assert_eq!(d.match_confidence, Some(90));
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match() {
    for case in ["tvdb_no_match", "tvdb_null_data"] {
        let (got, r) = lookup(case, &series("Nothing Like This", None)).await;
        r.finish().unwrap();
        assert_eq!(got.unwrap(), Lookup::NoMatch, "{case}");
    }
}

#[tokio::test(start_paused = true)]
async fn the_year_picks_among_candidates_and_a_tie_is_no_match() {
    let (got, r) = lookup("tvdb_candidates", &series("Doctor Who", Some(2005))).await;
    r.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    assert_eq!(i.ids[0].value, "78804");
    // No year: two series agree, so it doesn't guess. A year none has.
    let (got, _) = lookup("tvdb_candidates", &series("Doctor Who", None)).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    let (got, _) = lookup("tvdb_candidates", &series("Doctor Who", Some(1999))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

/// Only series are asked about; a film sends nothing.
#[tokio::test(start_paused = true)]
async fn other_kinds_and_punctuation_send_nothing() {
    let wire = Arc::new(Silent::default());
    let t = with_key();
    let film = Query::new(BB, Some(2008), MediaKind::Movie).unwrap();
    assert_eq!(
        t.lookup(&kit(wire.clone()), &film).await.unwrap(),
        Lookup::NoMatch
    );
    let dots = series("...", None);
    assert_eq!(
        t.lookup(&kit(wire.clone()), &dots).await.unwrap(),
        Lookup::NoMatch
    );
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    assert_eq!(t.kinds(), [MediaKind::Tv]);
}

// ---- What is sent ----

#[tokio::test(start_paused = true)]
async fn requests_carry_the_title_and_fixed_parameters_only() {
    let (_, r) = lookup("tvdb_match", &series(BB, Some(2008))).await;
    let seen = r.requests();
    assert_eq!(seen.len(), 2);
    let login = &seen[0].1;
    assert_eq!(login.url, "https://api4.thetvdb.com/v4/login");
    assert_eq!(login.body, format!(r#"{{"apikey":"{KEY}"}}"#).as_bytes());
    let names: Vec<&str> = login.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "Content-Type"], "no token at login");
    let search = &seen[1].1;
    let url = url::Url::parse(&search.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(tvdb::HOST));
    assert_eq!(url.path(), tvdb::SEARCH);
    let params: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let want: Vec<(String, String)> = [("query", BB), ("type", "series"), ("limit", "10")]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    assert_eq!(params, want, "the title, never the year, a hash or a file");
    assert!(search.body.is_empty());
    let names: Vec<&str> = search.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "Authorization"], "no other header");
    // The token in the header only, and Debug never shows it or the key.
    no_secret(&format!("{login:?}{search:?}"));
}

#[tokio::test(start_paused = true)]
async fn a_pin_goes_to_login_with_the_key() {
    let r = replayer("tvdb_pin");
    let creds = StaticCredentials::new([("api_key", KEY), ("pin", PIN)]);
    let t = Tvdb::new(Arc::new(creds));
    let got = t.lookup(&kit(r.clone()), &series(BB, None)).await;
    assert!(matches!(got, Ok(Lookup::Match(_))));
    r.finish().unwrap();
}

/// Text that would inject parameters stays one encoded value.
#[test]
fn a_hostile_title_is_one_parameter() {
    let title = "x&type=movie&limit=5000&query=y #";
    let req = tvdb::search_request(title).unwrap();
    let url = url::Url::parse(&req.url).unwrap();
    let q: Vec<String> = url
        .query_pairs()
        .filter(|(k, _)| k == "query")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(q, [title]);
    assert_eq!(url.query_pairs().filter(|(k, _)| k == "type").count(), 1);
    assert_eq!(url.query_pairs().filter(|(k, _)| k == "limit").count(), 1);
}

// ---- The credential ----

#[tokio::test(start_paused = true)]
async fn the_token_is_reused_and_lookups_are_paced() {
    let r = replayer("tvdb_two_lookups");
    let k = kit(r.clone());
    let t = with_key();
    assert!(matches!(
        t.lookup(&k, &series(BB, None)).await,
        Ok(Lookup::Match(_))
    ));
    let second = t.lookup(&k, &series("Nothing Like This", None)).await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    r.finish().unwrap();
    let seen = r.requests();
    assert_eq!(seen.len(), 3, "one login for both");
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(1), "one a second");
    }
    assert!(t.min_interval() >= Duration::from_secs(1));
}

/// Past the host limiter's hold on a refusal (its back-off ceiling).
const HOLD: Duration = Duration::from_secs(6 * 3600 + 1);

#[tokio::test(start_paused = true)]
async fn a_401_refreshes_the_token_once() {
    let r = replayer("tvdb_refresh");
    let k = kit(r.clone());
    let t = with_key();
    assert!(matches!(
        t.lookup(&k, &series(BB, None)).await,
        Ok(Lookup::Match(_))
    ));
    // The held token is refused: dropped, and nothing retried at once.
    let e = t.lookup(&k, &series(BB, None)).await.unwrap_err().0;
    assert_eq!(e, tvdb::TOKEN_REFUSED);
    assert_eq!(
        t.lookup(&k, &series(BB, None)).await.unwrap_err().0,
        tvdb::HELD
    );
    assert_eq!(r.requests().len(), 3, "nothing sent while held");
    // After the hold: a new login, and the search with the new token.
    tokio::time::advance(HOLD).await;
    assert!(matches!(
        t.lookup(&k, &series(BB, None)).await,
        Ok(Lookup::Match(_))
    ));
    r.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_second_401_fails_cleanly_not_in_a_loop() {
    let r = replayer("tvdb_refresh_fails");
    let k = kit(r.clone());
    let t = with_key();
    assert!(t.lookup(&k, &series(BB, None)).await.is_ok());
    assert_eq!(
        t.lookup(&k, &series(BB, None)).await.unwrap_err().0,
        tvdb::TOKEN_REFUSED
    );
    tokio::time::advance(HOLD).await;
    // The refreshed token is refused too: the key is, and that's the end.
    let e = t.lookup(&k, &series(BB, None)).await.unwrap_err().0;
    assert_eq!(e, tvdb::REFUSED);
    no_secret(&e);
    assert_eq!(
        t.lookup(&k, &series(BB, None)).await.unwrap_err().0,
        tvdb::HELD
    );
    r.finish().unwrap();
    assert_eq!(r.requests().len(), 5, "one refresh, no more");

    // A token just issued and refused at once isn't refreshed.
    let r = replayer("tvdb_fresh_token_refused");
    let e = with_key()
        .lookup(&kit(r.clone()), &series(BB, None))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, tvdb::REFUSED);
    r.finish().unwrap();
    assert_eq!(r.requests().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_token_is_renewed_when_old() {
    let dir = fixture("tvdb_refresh");
    let ex = |n: &str, body: &str| {
        let head = std::fs::read_to_string(dir.join(format!("{n}.http"))).unwrap();
        let req = std::fs::read(dir.join(format!("{n}.req.body"))).unwrap_or_default();
        parse_exchange(&head, std::fs::read(dir.join(body)).unwrap(), req).unwrap()
    };
    // Login, search; then past the TTL: login again (token two), search.
    let r = Arc::new(Replayer::new(vec![
        ex("001", "001.body"),
        ex("002", "002.body"),
        ex("004", "004.body"),
        ex("005", "005.body"),
    ]));
    let k = kit(r.clone());
    let t = with_key();
    assert!(t.lookup(&k, &series(BB, None)).await.is_ok());
    tokio::time::advance(tvdb::TOKEN_TTL).await;
    assert!(t.lookup(&k, &series(BB, None)).await.is_ok());
    r.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_refused_key_is_held_and_never_retried_in_a_loop() {
    let r = replayer("tvdb_refused");
    let k = kit(r.clone());
    let t = with_key();
    let e = t.lookup(&k, &series(BB, None)).await.unwrap_err().0;
    assert_eq!(e, tvdb::REFUSED);
    no_secret(&e);
    let again = t.lookup(&k, &series(BB, None)).await.unwrap_err().0;
    assert_eq!(again, tvdb::HELD);
    assert_eq!(r.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(HOLD).await;
    assert_eq!(
        t.lookup(&k, &series(BB, None)).await.unwrap_err().0,
        tvdb::REFUSED
    );
    r.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn no_key_or_a_bad_one_sends_nothing() {
    let wire = Arc::new(Silent::default());
    let k = kit(wire.clone());
    let q = series(BB, None);
    let none = Tvdb::new(Arc::new(StaticCredentials::default()));
    assert_eq!(none.lookup(&k, &q).await.unwrap_err().0, tvdb::NO_KEY);
    for (key, pin) in [
        ("a\"b", None),
        ("key with space", None),
        (KEY, Some("PIN-WITH-DASH")),
        (KEY, Some("pin\"x")),
    ] {
        let mut creds: Vec<(&'static str, &'static str)> = vec![("api_key", key)];
        creds.extend(pin.map(|p| ("pin", p)));
        let t = Tvdb::new(Arc::new(StaticCredentials::new(creds)));
        let e = t.lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(e, tvdb::BAD_KEY);
        assert!(!e.contains(key) && pin.is_none_or(|p| !e.contains(p)));
    }
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    // Debug names nothing.
    no_secret(&format!("{:?}", with_key()));
}

/// A token that could end a header is refused, never sent.
#[tokio::test(start_paused = true)]
async fn a_bad_token_is_an_error() {
    let (got, r) = lookup("tvdb_bad_token", &series(BB, None)).await;
    r.finish().unwrap();
    assert_eq!(got.unwrap_err(), tvdb::BAD_ANSWER);
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let r = replayer("tvdb_rate_limited");
    let k = kit(r.clone());
    let t = with_key();
    let q = series(BB, None);
    assert_eq!(t.lookup(&k, &q).await.unwrap_err().0, tvdb::RATE_LIMITED);
    assert_eq!(t.lookup(&k, &q).await.unwrap_err().0, tvdb::HELD);
    assert_eq!(r.requests().len(), 2);
    r.finish().unwrap();
}

/// An API error on a 200 holds the plugin for its `Retry-After` (5 s).
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let r = replayer("tvdb_api_error_then_match");
    let k = kit(r.clone());
    let t = with_key();
    let q = series(BB, None);
    assert_eq!(t.lookup(&k, &q).await.unwrap_err().0, tvdb::API_ERROR);
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(t.lookup(&k, &q).await.unwrap_err().0, tvdb::HELD);
    assert_eq!(r.requests().len(), 2, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(t.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    r.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = series(BB, None);
    for (case, want) in [
        ("tvdb_bad_body", tvdb::BAD_ANSWER),
        ("tvdb_html_body", tvdb::BAD_ANSWER),
        ("tvdb_server_error", tvdb::FAILED),
        ("tvdb_timeout", tvdb::FAILED),
        // Not followed: one request, a plain failure.
        ("tvdb_redirect", tvdb::FAILED),
    ] {
        let (got, r) = lookup(case, &q).await;
        r.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        no_secret(&e);
    }
}

#[tokio::test(start_paused = true)]
async fn an_oversize_answer_is_an_error() {
    let dir = fixture("tvdb_match");
    let head = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    let mut body = std::fs::read(dir.join("002.body")).unwrap();
    let close = body.pop().unwrap();
    body.resize(SEARCH_LIMITS.max_bytes + 1, b' ');
    body.push(close);
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(
            &head("001.http"),
            std::fs::read(dir.join("001.body")).unwrap(),
            std::fs::read(dir.join("001.req.body")).unwrap(),
        )
        .unwrap(),
        parse_exchange(&head("002.http"), body, Vec::new()).unwrap(),
    ]));
    let e = with_key()
        .lookup(&kit(r.clone()), &series(BB, None))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, tvdb::BAD_ANSWER);
    r.finish().unwrap();
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let t = with_key();
    let m = t.manifest();
    assert_eq!(m.id, "tvdb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "api4.thetvdb.com")]);
    assert_eq!(t.endpoints(), Endpoints::manifest(m.endpoints));
    // Account: nothing goes until the key is stored; the PIN is optional.
    assert_eq!(Tvdb::need(&[]), Some(Need::Credentials));
    assert_eq!(Tvdb::need(&["api_key".into()]), None);
    assert_eq!(Tvdb::need(&["pin".into()]), Some(Need::Credentials));
    // Without its spec, any stored name would do; with it, only the key.
    assert_eq!(need(m, None, &["api_key".into()]), None);

    let policy = t.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_secs(1));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    assert!(tvdb::ATTRIBUTION.starts_with("Metadata provided by TheTVDB"));
}

// ---- Under ENR-10's runtime ----

#[derive(Default)]
struct Up;

impl Switches for Up {
    fn kill_switch_tripped(&self) -> bool {
        false
    }
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

/// A bencoded info dict named `name` with one 900 MB video file.
fn info_dict(name: &str) -> Vec<u8> {
    let file = format!("{}.mkv", name.replace(' ', "."));
    let mut d = Vec::new();
    d.extend_from_slice(b"d5:filesld6:lengthi943718400e4:pathl");
    d.extend_from_slice(format!("{}:{file}", file.len()).as_bytes());
    d.extend_from_slice(b"eee4:name");
    d.extend_from_slice(format!("{}:{name}", name.len()).as_bytes());
    d.extend_from_slice(b"12:piece lengthi262144e6:pieces");
    d.extend_from_slice(format!("{}:", 20 * 3600).as_bytes());
    d.extend(std::iter::repeat_n(0u8, 20 * 3600));
    d.push(b'e');
    d
}

fn put(store: &mut Store, name: &str, source: SourceId, extra: i64) -> (i64, InfoHash) {
    let raw = info_dict(name);
    let info = Info::parse(&raw).unwrap();
    let id = store
        .insert(&[NewTorrent {
            info: &info,
            raw_info: None,
            discovered_at: T,
            metadata_at: T,
            extra_flags: extra,
            source,
        }])
        .unwrap()[0]
        .id();
    let files = file_infos(&info.files, info.path_encoding);
    store
        .set_enrichments(&[(id, enrich_with_rule(name, &files, None))])
        .unwrap();
    (id, info.swarm_hashes().next().unwrap())
}

/// A store with the DHT and the plugin's source registered; with its
/// key stored (only its sealed form's presence counts here) and enabled
/// only when `enabled`.
fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(tvdb::ID, "1", 0).unwrap();
    if enabled {
        s.update_source_secrets(
            src,
            &[("api_key", b"sealed")],
            &[],
            Some(SourceState::Enabled),
        )
        .unwrap();
    }
    (s, dht, src)
}

struct Ran {
    found: Vec<Discovery>,
    stats: Arc<IdentifyStats>,
}

async fn run(
    store: Store,
    fetch: Arc<dyn Fetch>,
    source: SourceId,
    opened: &[i64],
    done: impl Fn(&IdentifyStats) -> bool,
) -> Ran {
    let config = Config {
        background: false,
        idle: Duration::from_millis(10),
        clock: || 20_000 * 86_400 + 43_200,
        ..Config::default()
    };
    let (src, interest) = IdentifySource::new(
        Arc::new(with_key()),
        Arc::new(Mutex::new(store)),
        core_with(&["lexa"], &["rolea"]),
        AdultSources::default(),
        Arc::new(Up),
        config,
    )
    .unwrap();
    for &id in opened {
        assert!(interest.opened(id));
    }
    let stats = src.stats();
    let (tx, mut rx) = mpsc::channel(16);
    let (stop, shutdown) = Shutdown::new();
    let kit = Kit::with_limiter(
        Arc::new(MemoryState::new()),
        fetch,
        HostLimiter::new(with_key().policy().limits(&UserLimits::default())),
    );
    let ctx = SourceCtx {
        source,
        emitter: Emitter::new(source, tx),
        peer_requests: None,
        peer_replies: None,
        shutdown,
        kit,
    };
    let task = tokio::spawn(async move { src.run(ctx).await });
    until("the runtime", || done(&stats)).await;
    let _ = stop.send(true);
    let _ = task.await;
    let mut found = Vec::new();
    while let Ok(d) = rx.try_recv() {
        found.push(d);
    }
    Ran { found, stats }
}

/// Registered with no key stored and not enabled: nothing is sent.
#[tokio::test]
async fn off_by_default_nothing_is_sent() {
    let (mut s, dht, src) = store(false);
    let (id, _) = put(&mut s, "Breaking Bad S01 2008 1080p BluRay x264", dht, 0);
    let wire = Arc::new(Silent::default());
    let ran = run(s, wire.clone(), src, &[id], |st| n(&st.deferred) >= 1).await;
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    assert_eq!(n(&ran.stats.looked_up), 0);
    assert!(ran.found.is_empty());
}

/// A private item is never sent, even opened first; the public one is
/// looked up (exactly the two recorded requests).
#[tokio::test]
async fn a_private_item_is_never_sent() {
    let (mut s, dht, src) = store(true);
    let mut private = Vec::new();
    let privacy = [
        flags::PRIVATE_SOURCE,
        flags::PRIVATE,
        flags::REMOTE_UNVERIFIED,
    ];
    for (i, f) in privacy.into_iter().enumerate() {
        let (id, _) = put(
            &mut s,
            &format!("Breaking Bad S0{} 2008 720p", i + 2),
            dht,
            f,
        );
        private.push(id);
    }
    let (public, key) = put(&mut s, "Breaking Bad S01 2008 1080p BluRay x264", dht, 0);
    let r = replayer("tvdb_match");
    let opened = [private[0], private[1], private[2], public];
    let ran = run(s, r.clone(), src, &opened, |st| n(&st.done) >= 4).await;
    r.finish().unwrap();
    assert_eq!(n(&ran.stats.ineligible), 3);
    assert_eq!((n(&ran.stats.looked_up), n(&ran.stats.matched)), (1, 1));
    assert_eq!(ran.found.len(), 1);
    assert_eq!(ran.found[0].found.key, key);
}

fn ingest(
    store: &mut Store,
    filter: &Handle,
    found: &[Discovery],
    src: SourceId,
) -> UnverifiedStats {
    let stats = UnverifiedStats::default();
    let ctx = Ctx {
        quarantine_secs: 86_400,
        enrichers: lodestar_pipeline::adult_enrich::EnrichSources {
            listed: 0,
            kind: 1u64 << src.get(),
        },
        ..Ctx::default()
    };
    unverified::ingest(store, &filter.current(), ctx, found, &stats, T, &|_, _| {}).unwrap();
    stats
}

/// The answer goes through the writer's judge: clean, it's stored; when
/// one of its titles completes a child-safety pattern with the torrent's
/// name, the hash is held and nothing stored.
#[tokio::test]
async fn an_answer_is_judged_and_a_held_one_is_not_stored() {
    let name = "Breaking Bad S01 2008 1080p BluRay x264";
    let answer = || async {
        let (got, _) = lookup("tvdb_match", &series(BB, Some(2008))).await;
        let Ok(Lookup::Match(i)) = got else {
            panic!("a match")
        };
        i
    };
    let discovery = |key: InfoHash,
                     src: SourceId,
                     i: lodestar_sources::identify::Identification| {
        let mut found = lodestar_sources::Found::key(key, lodestar_pipeline::identify::STRATEGY)
            .with_claims(i.into_claims());
        found.clamp();
        Discovery {
            source: src,
            found,
            at: T,
            reply: false,
        }
    };

    let (mut s, dht, src) = store(true);
    let (id, key) = put(&mut s, name, dht, 0);
    let filter = core_with(&["lexa"], &["rolea"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 0);
    let row = s.torrent_claims(id).unwrap();
    let c = row
        .iter()
        .find(|c| c.source == src)
        .and_then(|c| c.claims.clone())
        .expect("stored");
    assert_eq!(c.ids.len(), 3);

    // "breaking" is a child role here and the answer's German title holds
    // the sexual term: alone the name is clean, with the answer a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, name, dht, 0);
    let filter = core_with(&["serie"], &["breaking"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
