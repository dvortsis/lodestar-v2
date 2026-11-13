//! Contract tests for the Wikidata plugin (ENR-14, §18.9), on the
//! fixtures in `fixtures/wikidata/` (see its `README.md`), with no
//! network. Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   request it sends, errors (429 back-off, a timeout, a redirect, a
//!   broken or oversize answer), and its manifest;
//! - the plugin under ENR-10's runtime and the writer's ingest: off by
//!   default, a private item never sent, a held answer never stored.
//!
//! No fixed sleeps: paused time for the plugin alone, and condition waits
//! of at least 10 s for the runtime.

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
use lodestar_sources::consent::{Ack, need};
use lodestar_sources::identify::{Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::wikidata::{ENTITY_LIMITS, SEARCH_LIMITS};
use lodestar_sources_plugins::wikidata::{self, Wikidata};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const SHAWSHANK: &str = "The Shawshank Redemption";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/wikidata")
        .join(case)
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        Wikidata::new().policy(),
        &UserLimits::default(),
    )
}

fn movie(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Movie).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = Wikidata::new().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, replayer) = lookup("wikidata_match", &movie(SHAWSHANK, Some(1994))).await;
    replayer.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    // The film's TMDB movie id; its (wrong) TMDB TV id isn't a movie's.
    assert_eq!(
        ids,
        [
            ("wikidata", "Q172241"),
            ("imdb", "tt0111161"),
            ("tmdb", "278")
        ]
    );
    assert_eq!(i.canonical_title.as_deref(), Some(SHAWSHANK));
    let alts: Vec<(&str, &str)> = i
        .alt_titles
        .iter()
        .map(|a| (a.language.as_str(), a.title.as_str()))
        .collect();
    assert_eq!(
        alts,
        [
            ("de", "Die Verurteilten"),
            ("ja", "ショーシャンクの空に"),
            ("en", "Shawshank"),
            ("fr", "Les Évadés"),
        ]
    );
    assert_eq!(
        (i.year, i.confidence),
        (Some(1994), wikidata::CONFIDENCE_WITH_YEAR)
    );
    assert!(i.genres.is_empty());
    // Nothing of the descriptions, sitelinks, search labels or people.
    let claims = i.clone().into_claims();
    let all = format!("{claims:?}{:?}{:?}", i.canonical_title, i.alt_titles);
    assert!(!all.contains("SENTINEL") && !all.contains("Q9001"), "{all}");
    // Its claims survive the clamp unchanged: already in stored form.
    assert_eq!(claims.ids.len(), 3);
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.alt_titles.len(), 4);
    assert_eq!(d.match_confidence, Some(90));
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match_after_one_request() {
    let (got, replayer) = lookup("wikidata_no_match", &movie("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(replayer.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_year_picks_among_candidates_and_a_tie_is_no_match() {
    // Two films and a novel called Dune: the year picks one film.
    let (got, r) = lookup("wikidata_candidates", &movie("Dune", Some(2021))).await;
    r.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    assert_eq!(i.ids[0].value, "Q900012");
    assert_eq!(i.ids[1].scheme, "tmdb");
    // No year: two films agree, so it doesn't guess.
    let (got, r) = lookup("wikidata_candidates", &movie("Dune", None)).await;
    r.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // A year none has, or a kind none is.
    let (got, _) = lookup("wikidata_candidates", &movie("Dune", Some(1999))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    let tv = Query::new("Dune", Some(2021), MediaKind::Tv).unwrap();
    let (got, _) = lookup("wikidata_candidates", &tv).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[test]
fn titles_compare_folded() {
    assert_eq!(
        wikidata::fold("  The SHAWSHANK: Redemption! "),
        "the shawshank redemption"
    );
    assert_eq!(wikidata::fold("Amélie"), "amélie");
    assert_eq!(wikidata::fold("..."), "");
}

// ---- What is sent ----

#[tokio::test(start_paused = true)]
async fn requests_carry_the_title_and_fixed_parameters_only() {
    let (_, replayer) = lookup("wikidata_match", &movie(SHAWSHANK, Some(1994))).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 2);
    let params = |req: &Request| -> Vec<(String, String)> {
        let url = url::Url::parse(&req.url).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(wikidata::HOST));
        assert_eq!(url.path(), wikidata::API);
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    };
    let p = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    assert_eq!(
        params(&seen[0].1),
        p(&[
            ("action", "wbsearchentities"),
            ("search", SHAWSHANK),
            ("language", "en"),
            ("uselang", "en"),
            ("type", "item"),
            ("limit", "5"),
            ("format", "json"),
            ("maxlag", "5"),
        ]),
        "the title, never the year, a hash or a file"
    );
    assert_eq!(
        params(&seen[1].1),
        p(&[
            ("action", "wbgetentities"),
            // The novella the search also found isn't named by the title,
            // so it isn't fetched.
            ("ids", "Q172241"),
            ("props", "labels|aliases|claims"),
            ("languages", wikidata::LANGUAGES),
            ("format", "json"),
            ("maxlag", "5"),
        ])
    );
    for (_, req) in &seen {
        assert!(req.body.is_empty());
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Accept", "User-Agent"], "no other header");
        let ua = req.headers.iter().find(|(n, _)| n == "User-Agent");
        assert_eq!(ua.map(|(_, v)| v.as_str()), Some(wikidata::USER_AGENT));
    }
}

#[test]
fn the_user_agent_names_the_project_only() {
    let ua = wikidata::USER_AGENT;
    assert!(ua.starts_with("Lodestar/"), "{ua}");
    assert!(ua.contains("(https://github.com/"), "a project URL: {ua}");
    assert!(!ua.contains('@'), "no email: {ua}");
    for word in ["localhost", "home", "user"] {
        assert!(!ua.to_lowercase().contains(word), "{word} in {ua}");
    }
}

/// Text that would inject parameters or SPARQL stays one encoded value.
#[test]
fn a_hostile_title_is_one_parameter() {
    let title = "x&action=delete&ids=Q1|Q2 } UNION { ?s ?p ?o } #";
    let req = wikidata::search_request(title).unwrap();
    let url = url::Url::parse(&req.url).unwrap();
    let searched: Vec<String> = url
        .query_pairs()
        .filter(|(k, _)| k == "search")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(searched, [title]);
    assert_eq!(url.query_pairs().filter(|(k, _)| k == "action").count(), 1);
    assert!(
        !format!("{req:?}").contains("UNION"),
        "Debug shows no query"
    );
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("wikidata_rate_limited")).unwrap());
    let k = kit(replayer.clone());
    let q = movie(SHAWSHANK, Some(1994));
    let e = Wikidata::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(e, wikidata::RATE_LIMITED);
    // Held by the limiter (ENR-10's client): the retry sends nothing.
    let again = Wikidata::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(again, wikidata::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = Wikidata::new().lookup(&k, &q).await.unwrap_err().0;
    assert_ne!(later, wikidata::HELD, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

/// `maxlag` comes as a 200 the host limiter doesn't see: the plugin holds
/// itself for the answer's `Retry-After` (5 s), sending nothing before it.
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let replayer = Arc::new(Replayer::load(&fixture("wikidata_maxlag_then_match")).unwrap());
    let k = kit(replayer.clone());
    let w = Wikidata::new();
    let q = movie(SHAWSHANK, Some(1994));
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, wikidata::API_ERROR);
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, wikidata::HELD);
    assert_eq!(replayer.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(w.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = movie(SHAWSHANK, Some(1994));
    for (case, want) in [
        ("wikidata_maxlag", wikidata::API_ERROR),
        ("wikidata_bad_body", "Wikidata's answer wasn't understood"),
        ("wikidata_html_body", "Wikidata's answer wasn't understood"),
        ("wikidata_server_error", "Wikidata's request failed"),
        ("wikidata_timeout", "Wikidata's request failed"),
        // Not followed: one request, a plain failure.
        ("wikidata_redirect", "Wikidata's request failed"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL"), "{case}");
    }
}

/// An answer past a byte cap is an error, never a cut-short match.
#[tokio::test(start_paused = true)]
async fn an_oversize_answer_is_an_error() {
    let search = std::fs::read(fixture("wikidata_match").join("001.body")).unwrap();
    let mut entities = std::fs::read(fixture("wikidata_match").join("002.body")).unwrap();
    let head = |n: usize| {
        std::fs::read_to_string(fixture("wikidata_match").join(format!("00{n}.http"))).unwrap()
    };
    // Whitespace before the closing brace keeps it valid JSON.
    let close = entities.pop().unwrap();
    entities.resize(ENTITY_LIMITS.max_bytes + 1, b' ');
    entities.push(close);
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&head(1), search.clone(), Vec::new()).unwrap(),
        parse_exchange(&head(2), entities, Vec::new()).unwrap(),
    ]));
    let e = Wikidata::new()
        .lookup(&kit(r.clone()), &movie(SHAWSHANK, Some(1994)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "Wikidata's answer wasn't understood");
    r.finish().unwrap();

    let mut big = search;
    big.resize(SEARCH_LIMITS.max_bytes + 1, b' ');
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&head(1), big, Vec::new()).unwrap(),
    ]));
    let e = Wikidata::new()
        .lookup(&kit(r.clone()), &movie(SHAWSHANK, Some(1994)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "Wikidata's answer wasn't understood");
}

/// An id the search named but the entities answer leaves out (missing,
/// or keyed by a redirect's target) is an error, never a cached miss.
#[tokio::test(start_paused = true)]
async fn a_requested_id_left_out_is_an_error() {
    let dir = fixture("wikidata_match");
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    let head = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
    for body in [
        &br#"{"entities":{},"success":1}"#[..],
        br#"{"entities":{"Q172241":{"id":"Q172241","missing":""}},"success":1}"#,
        br#"{"entities":{"Q999":{"id":"Q999","type":"item"}},"success":1}"#,
    ] {
        let r = Arc::new(Replayer::new(vec![
            parse_exchange(&head("001.http"), read("001.body"), Vec::new()).unwrap(),
            parse_exchange(&head("002.http"), body.to_vec(), Vec::new()).unwrap(),
        ]));
        let got = Wikidata::new()
            .lookup(&kit(r.clone()), &movie(SHAWSHANK, Some(1994)))
            .await;
        assert_eq!(
            got.unwrap_err().0,
            "Wikidata's answer wasn't understood",
            "{}",
            String::from_utf8_lossy(body)
        );
        r.finish().unwrap();
    }
}

/// A title that folds to nothing is no match, and nothing is sent.
#[tokio::test(start_paused = true)]
async fn a_title_of_punctuation_sends_nothing() {
    let wire = Arc::new(Silent::default());
    let got = Wikidata::new()
        .lookup(&kit(wire.clone()), &movie("...", None))
        .await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_host_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("wikidata_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = Wikidata::new()
        .lookup(&k, &movie(SHAWSHANK, Some(1994)))
        .await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = Wikidata::new()
        .lookup(&k, &movie("Nothing Like This", None))
        .await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(1), "one a second");
    }
    assert!(Wikidata::new().min_interval() >= Duration::from_secs(2));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let w = Wikidata::new();
    let m = w.manifest();
    assert_eq!(m.id, "wikidata");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "www.wikidata.org")]);
    assert_eq!(
        Wikidata::new().endpoints(),
        Endpoints::manifest(m.endpoints)
    );
    assert_eq!(Wikidata::new().kinds(), [MediaKind::Movie, MediaKind::Tv]);
    // External: nothing goes until the endpoint is acknowledged.
    assert!(need(m, None, &[]).is_some());
    assert_eq!(need(m, Some(&Ack::of(m)), &[]), None);

    let policy = w.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    assert!(policy.credentials.is_empty());
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_secs(1));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
}

// ---- Under ENR-10's runtime ----

#[derive(Default)]
struct Up;

impl Switches for Up {
    fn kill_switch_tripped(&self) -> bool {
        false
    }
}

/// A [`Fetch`] that counts requests and never answers: for runs where
/// nothing may be sent.
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

/// A store with the DHT and the plugin's source registered; the plugin's
/// enabled with its endpoint acknowledged only when `enabled`.
fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(wikidata::ID, "1", 0).unwrap();
    if enabled {
        let ack = Ack::of(Wikidata::new().manifest()).encode();
        s.enable_source(src, Some(&ack)).unwrap();
    }
    (s, dht, src)
}

struct Ran {
    found: Vec<Discovery>,
    stats: Arc<IdentifyStats>,
}

/// Runs the plugin under the runtime on `fetch` until `done` holds, with
/// `opened` asked for in order, and returns what it emitted.
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
        Arc::new(Wikidata::new()),
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
        HostLimiter::new(Wikidata::new().policy().limits(&UserLimits::default())),
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

/// Registered but not enabled: the runtime waits, and nothing is sent.
#[tokio::test]
async fn off_by_default_nothing_is_sent() {
    let (mut s, dht, src) = store(false);
    let (id, _) = put(
        &mut s,
        "The Shawshank Redemption 1994 1080p BluRay x264",
        dht,
        0,
    );
    let wire = Arc::new(Silent::default());
    let ran = run(s, wire.clone(), src, &[id], |st| n(&st.deferred) >= 1).await;
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    assert_eq!(n(&ran.stats.looked_up), 0);
    assert!(ran.found.is_empty());
}

/// A private item is never sent, even opened first; the public one is
/// looked up (exactly the two recorded requests) and its answer emitted
/// under the key.
#[tokio::test]
async fn a_private_item_is_never_sent() {
    let (mut s, dht, src) = store(true);
    let name = "The Shawshank Redemption 1994 1080p BluRay x264";
    let mut private = Vec::new();
    let privacy = [
        flags::PRIVATE_SOURCE,
        flags::PRIVATE,
        flags::REMOTE_UNVERIFIED,
    ];
    for (i, f) in privacy.into_iter().enumerate() {
        let (id, _) = put(
            &mut s,
            &format!("The Shawshank Redemption 1994 {i} 720p"),
            dht,
            f,
        );
        assert_ne!(f & flags::PRIVACY, 0);
        private.push(id);
    }
    let (public, key) = put(&mut s, name, dht, 0);
    let replayer = Arc::new(Replayer::load(&fixture("wikidata_match")).unwrap());
    let opened = [private[0], private[1], private[2], public];
    let ran = run(s, replayer.clone(), src, &opened, |st| n(&st.done) >= 4).await;
    replayer.finish().unwrap();
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

/// The answer goes through the writer's judge: clean, it's stored as the
/// plugin's claims; when one of its titles completes a child-safety
/// pattern with the torrent's name, the hash is held and nothing stored.
#[tokio::test]
async fn an_answer_is_judged_and_a_held_one_is_not_stored() {
    let name = "The Shawshank Redemption 1994 1080p BluRay x264";
    let answer = || async {
        let (got, _) = lookup("wikidata_match", &movie(SHAWSHANK, Some(1994))).await;
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

    // Clean.
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
    assert_eq!(
        c.details
            .as_deref()
            .and_then(|d| d.canonical_title.as_deref()),
        Some(SHAWSHANK)
    );

    // "shawshank" is a child role here and the answer's German title a
    // sexual term: alone the name is clean, with the answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, name, dht, 0);
    let filter = core_with(&["verurteilten"], &["shawshank"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}

// ---- Intake's harvest count (ENR-14-harvest-count, from ENR-10) ----

/// A source that emits what the test sends it.
struct Scripted {
    manifest: lodestar_sources::Manifest,
    rx: Mutex<Option<mpsc::Receiver<lodestar_sources::Found>>>,
}

impl lodestar_sources::SourcePlugin for Scripted {
    fn manifest(&self) -> &lodestar_sources::Manifest {
        &self.manifest
    }

    fn run(&self, mut ctx: SourceCtx) -> BoxFuture<'_, Result<(), lodestar_sources::PluginError>> {
        let mut rx = self.rx.lock().unwrap().take().expect("run once");
        Box::pin(async move {
            loop {
                tokio::select! {
                    _ = ctx.shutdown.requested() => return Ok(()),
                    Some(f) = rx.recv() => {
                        let sent = ctx.emitter.emit(f).await;
                        sent.map_err(|_| lodestar_sources::PluginError("closed".into()))?;
                    }
                }
            }
        })
    }
}

fn scripted(
    id: &'static str,
    kinds: &'static [Kind],
) -> (Arc<Scripted>, mpsc::Sender<lodestar_sources::Found>) {
    let (tx, rx) = mpsc::channel(8);
    let manifest = lodestar_sources::Manifest {
        id,
        name: id,
        kinds,
        consent: Consent::None,
        enabled_by_default: true,
        ..Wikidata::new().manifest().clone()
    };
    let rx = Mutex::new(Some(rx));
    (Arc::new(Scripted { manifest, rx }), tx)
}

/// Intake counts a feed's find as harvested, never an identification of
/// a stored torrent, which is still stored through the writer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identification_is_not_counted_as_harvested() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("lodestar.db");
    let (id, key) = {
        let mut s = Store::open(&db, StoreConfig::default()).unwrap();
        let dht = s.register_source("dht", "1", 0).unwrap();
        put(
            &mut s,
            "The Shawshank Redemption 1994 1080p BluRay x264",
            dht,
            0,
        )
    };
    let (ident, ident_tx) = scripted("test-identify", &[Kind::Enrichment]);
    let (feed, feed_tx) = scripted("test-feed", &[Kind::Discovery]);
    let config = harness::pipeline_config(core_with(&["lexa"], &["rolea"]));
    let pipeline = lodestar_pipeline::Pipeline::start(
        config,
        Store::open(&db, StoreConfig::default()).unwrap(),
        vec![ident, feed],
    )
    .await
    .unwrap();
    let (got, _) = lookup("wikidata_match", &movie(SHAWSHANK, Some(1994))).await;
    let Ok(Lookup::Match(i)) = got else {
        panic!("a match")
    };
    let strategy = lodestar_pipeline::identify::STRATEGY;
    let found = lodestar_sources::Found::key(key, strategy).with_claims(i.into_claims());
    ident_tx.send(found).await.unwrap();
    let named = lodestar_sources::Claims {
        name: Some("Other Release 2024".into()),
        ..Default::default()
    };
    let other = InfoHash([7; 20]);
    feed_tx
        .send(lodestar_sources::Found::key(other, 0).with_claims(named))
        .await
        .unwrap();
    let reader = Store::open_reader(&db, StoreConfig::default()).unwrap();
    let harvested = |r: &Store| -> i64 {
        r.rollup(lodestar_pipeline::metric::HARVESTED, 0, i64::MAX)
            .unwrap()
            .iter()
            .map(|c| c.value)
            .sum()
    };
    until("the identification and the feed's find", || {
        !reader.torrent_claims(id).unwrap().is_empty()
            && reader
                .unverified(lodestar_store::unverified::Network::Bt, &other.0)
                .unwrap()
                .is_some()
            && harvested(&reader) >= 1
    })
    .await;
    let store = pipeline.shutdown().await.unwrap();
    assert_eq!(harvested(&store), 1, "only the feed's find");
}
