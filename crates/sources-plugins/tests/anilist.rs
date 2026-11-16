//! Contract tests for the AniList plugin (ENR-anilist, §18.9), on the
//! fixtures in `fixtures/anilist/` (see its `README.md`), with no
//! network. Two parts:
//!
//! - the plugin on its own: a match, an adult result refused, no match,
//!   several candidates, the request it sends (a fixed document, the title
//!   one variable), errors (429 back-off, a GraphQL error's hold, a
//!   timeout, a redirect, a broken or oversize answer), and its manifest;
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
use lodestar_sources::http::Method;
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::anilist::{Format, Media, MediaId, SEARCH_LIMITS};
use lodestar_sources_plugins::anilist::{self, AniList, SEARCH_ANIME};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use serde_json::Value;
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const PERFECT_BLUE: &str = "Perfect Blue";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/anilist")
        .join(case)
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        AniList::new().policy(),
        &UserLimits::default(),
    )
}

fn movie(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Movie).unwrap()
}

fn tv(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Tv).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = AniList::new().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

fn matched(got: Result<Lookup, String>) -> Identification {
    let Ok(Lookup::Match(i)) = got else {
        panic!("a match, not {got:?}")
    };
    i
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, replayer) = lookup("anilist_match", &movie(PERFECT_BLUE, Some(1997))).await;
    replayer.finish().unwrap();
    let i = matched(got);
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("anilist", "900001")]);
    assert_eq!(i.canonical_title.as_deref(), Some(PERFECT_BLUE));
    // The other titles, once each, with their languages; the canonical
    // one isn't repeated.
    let alts: Vec<(&str, &str)> = i
        .alt_titles
        .iter()
        .map(|a| (a.language.as_str(), a.title.as_str()))
        .collect();
    assert_eq!(
        alts,
        [
            ("ja", "パーフェクトブルー"),
            ("und", "Pāfekuto Burū"),
            ("und", "Perfect Blue Stage"),
        ]
    );
    assert_eq!(
        (i.year, i.confidence),
        (Some(1997), anilist::CONFIDENCE_WITH_YEAR)
    );
    assert!(i.genres.is_empty());
    // AniList's own marker, not adult, so its terms take it (ENR-terms).
    assert_eq!(i.adult, Some(false));
    assert!(anilist::TERMS.accepts(i.adult));
    // Nothing of the description, the staff or the adult media.
    let claims = i.clone().into_claims();
    let all = format!("{claims:?}{:?}{:?}", i.canonical_title, i.alt_titles);
    assert!(
        !all.contains("SENTINEL") && !all.contains("900002"),
        "{all}"
    );
    // Its claims survive the clamp unchanged: already in stored form.
    assert_eq!(claims.ids.len(), 1);
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.alt_titles.len(), 3);
    assert_eq!(d.match_confidence, Some(90));
}

/// The answer holds a film with the same title and year marked adult,
/// although `isAdult: false` was asked: it's never chosen, so the one
/// that isn't wins; and without that one there's no match.
#[test]
fn an_adult_or_unmarked_media_is_never_chosen() {
    let media = |id: u64, adult: Option<bool>| Media {
        id: MediaId(id),
        format: Some(Format::Movie),
        adult,
        country: Some("JP".into()),
        season_year: Some(1997),
        start_year: Some(1997),
        romaji: Some(PERFECT_BLUE.into()),
        english: None,
        native: None,
        synonyms: Vec::new(),
        truncated: false,
    };
    let q = movie(PERFECT_BLUE, Some(1997));
    let (clean, adult, unmarked) = (media(1, Some(false)), media(2, Some(true)), media(3, None));
    assert!(matches!(
        anilist::choose(&q, &[&adult, &clean, &unmarked]),
        Lookup::Match(i) if i.ids[0].value == "1"
    ));
    assert_eq!(anilist::choose(&q, &[&adult]), Lookup::NoMatch);
    assert_eq!(anilist::choose(&q, &[&unmarked]), Lookup::NoMatch);
    // Nor would the runtime take either (ENR-terms): AniList's terms need
    // the marker.
    assert!(!anilist::TERMS.accepts(adult.adult) && !anilist::TERMS.accepts(unmarked.adult));
}

#[tokio::test(start_paused = true)]
async fn the_year_and_kind_must_agree() {
    // No year: the start year, at the lower confidence.
    let (got, _) = lookup("anilist_match", &movie(PERFECT_BLUE, None)).await;
    let i = matched(got);
    assert_eq!(
        (i.ids[0].value.as_str(), i.year, i.confidence),
        ("900001", Some(1997), anilist::CONFIDENCE_WITHOUT_YEAR)
    );
    // A year it doesn't have is no match.
    let (got, _) = lookup("anilist_match", &movie(PERFECT_BLUE, Some(1990))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // A series isn't a film.
    let (got, _) = lookup("anilist_match", &tv(PERFECT_BLUE, Some(1997))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match_after_one_request() {
    let (got, replayer) = lookup("anilist_no_match", &tv("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(replayer.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_year_picks_among_candidates_and_a_tie_is_no_match() {
    // Three series and a music video called Frieren (a synonym of each).
    let (got, r) = lookup("anilist_candidates", &tv("Frieren", Some(2026))).await;
    r.finish().unwrap();
    assert_eq!(matched(got).ids[0].value, "900012");
    // 2023: a TV series and an ONA agree; the music video never fits.
    let (got, _) = lookup("anilist_candidates", &tv("Frieren", Some(2023))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    let (got, _) = lookup("anilist_candidates", &tv("Frieren", None)).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // The English title picks one.
    let q = tv("Frieren: Beyond Journey's End", None);
    let found = lodestar_sources_feeds::anilist::parse_search(
        &std::fs::read(fixture("anilist_candidates").join("001.body")).unwrap(),
        &SEARCH_LIMITS,
    );
    let refs: Vec<&Media> = found.media.iter().collect();
    assert!(matches!(anilist::choose(&q, &refs), Lookup::Match(i) if i.ids[0].value == "900011"));
    // No film among them.
    assert_eq!(
        anilist::choose(&movie("Frieren", Some(2023)), &refs),
        Lookup::NoMatch
    );
}

#[test]
fn formats_fit_kinds() {
    use anilist::fits;
    assert!(fits(MediaKind::Movie, Some(Format::Movie)));
    for f in [
        Format::Tv,
        Format::TvShort,
        Format::Ova,
        Format::Ona,
        Format::Special,
    ] {
        assert!(fits(MediaKind::Tv, Some(f)) && !fits(MediaKind::Movie, Some(f)));
    }
    for f in [
        Format::Music,
        Format::Manga,
        Format::Novel,
        Format::OneShot,
        Format::Other,
    ] {
        assert!(!fits(MediaKind::Tv, Some(f)) && !fits(MediaKind::Movie, Some(f)));
    }
    assert!(!fits(MediaKind::Tv, None));
    assert!(!fits(MediaKind::Comic, Some(Format::Manga)));
}

#[test]
fn titles_compare_folded() {
    assert_eq!(
        anilist::fold("  Re:ZERO -Starting Life- "),
        "re zero starting life"
    );
    assert_eq!(anilist::fold("Pāfekuto Burū"), "pāfekuto burū");
    assert_eq!(anilist::fold("..."), "");
}

// ---- What is sent ----

/// The body's JSON: the document and the variables, nothing else.
fn sent(req: &Request) -> (String, serde_json::Map<String, Value>) {
    let v: Value = serde_json::from_slice(&req.body).unwrap();
    let o = v.as_object().unwrap();
    assert_eq!(o.len(), 2, "query and variables only: {o:?}");
    (
        o["query"].as_str().unwrap().to_owned(),
        o["variables"].as_object().unwrap().clone(),
    )
}

#[tokio::test(start_paused = true)]
async fn the_request_is_the_fixed_document_and_three_variables() {
    let (_, replayer) = lookup("anilist_match", &movie(PERFECT_BLUE, Some(1997))).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let req = &seen[0].1;
    assert_eq!(req.method, Method::Post);
    assert_eq!(req.url, "https://graphql.anilist.co");
    let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "Content-Type"], "no other header");
    assert!(
        req.headers
            .contains(&("Content-Type".into(), "application/json".into()))
    );
    let (query, vars) = sent(req);
    assert_eq!(query, SEARCH_ANIME);
    assert_eq!(
        Value::Object(vars),
        serde_json::json!({"search": PERFECT_BLUE, "perPage": 10, "isAdult": false}),
        "the title, never the year, a hash or a file"
    );
}

/// Text that would close the document, open a field, comment out the
/// rest or ask for the schema stays one string variable, and the document
/// is the same byte for byte.
#[tokio::test(start_paused = true)]
async fn a_hostile_title_never_changes_the_query() {
    let hostile = [
        r#"x" } } } { __schema { types { name } } } #"#,
        "a\"b\\c'd",
        "} { Page { media(isAdult: true) { id } } }",
        "$isAdult: true",
        "line\nbreak\r\n# comment\u{2028}",
        "{{{{[[[[",
    ];
    for title in hostile {
        let req = anilist::search_request(title).unwrap();
        let (query, vars) = sent(&req);
        assert_eq!(query, SEARCH_ANIME, "{title:?}");
        assert_eq!(vars.len(), 3);
        assert_eq!(vars["search"], Value::String(title.to_owned()));
        assert_eq!(vars["isAdult"], Value::Bool(false));
        assert_eq!(vars["perPage"], Value::from(10));
    }
    // Through the plugin: what reaches the wire is the same.
    let wire = Arc::new(Silent::default());
    let k = kit(wire.clone());
    let q = movie(hostile[0], None);
    let w = AniList::new();
    let fut = w.lookup(&k, &q);
    // The request is sent and never answered: look at what was sent.
    tokio::select! {
        _ = fut => panic!("never answered"),
        () = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
    let bodies = wire.sent.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1);
    let v: Value = serde_json::from_slice(&bodies[0]).unwrap();
    assert_eq!(v["query"], Value::String(SEARCH_ANIME.to_owned()));
    assert_eq!(
        v["variables"]["search"],
        Value::String(hostile[0].to_owned())
    );
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("anilist_rate_limited")).unwrap());
    let k = kit(replayer.clone());
    let q = movie(PERFECT_BLUE, Some(1997));
    let e = AniList::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(e, anilist::RATE_LIMITED);
    // Held by the limiter (ENR-10's client): the retry sends nothing.
    let again = AniList::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(again, anilist::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = AniList::new().lookup(&k, &q).await.unwrap_err().0;
    assert_ne!(later, anilist::HELD, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

/// A GraphQL error on a 200 isn't seen by the host limiter: the plugin
/// holds itself for the answer's `Retry-After` (5 s), sending nothing
/// before it.
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let replayer = Arc::new(Replayer::load(&fixture("anilist_error_then_match")).unwrap());
    let k = kit(replayer.clone());
    let w = AniList::new();
    let q = movie(PERFECT_BLUE, Some(1997));
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, anilist::API_ERROR);
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, anilist::HELD);
    assert_eq!(replayer.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(w.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = movie(PERFECT_BLUE, Some(1997));
    for (case, want) in [
        ("anilist_error", anilist::API_ERROR),
        ("anilist_bad_body", "AniList's answer wasn't understood"),
        ("anilist_html_body", "AniList's answer wasn't understood"),
        ("anilist_server_error", "AniList's request failed"),
        ("anilist_timeout", "AniList's request failed"),
        // Not followed: one request, a plain failure.
        ("anilist_redirect", "AniList's request failed"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL"), "{case}");
    }
}

/// An answer past the byte cap, or with a media that isn't one, is an
/// error, never a cut-short match or a miss.
#[tokio::test(start_paused = true)]
async fn an_oversize_or_spoiled_answer_is_an_error() {
    let dir = fixture("anilist_match");
    let head = std::fs::read_to_string(dir.join("001.http")).unwrap();
    let req = std::fs::read(dir.join("001.req.body")).unwrap();
    let body = std::fs::read(dir.join("001.body")).unwrap();
    let mut big = body.clone();
    // Whitespace before the closing brace keeps it valid JSON.
    let close = big.iter().rposition(|&b| b == b'}').unwrap();
    big.splice(
        close..close,
        std::iter::repeat_n(b' ', SEARCH_LIMITS.max_bytes),
    );
    let spoiled = String::from_utf8(body)
        .unwrap()
        .replace("\"id\": 900003", "\"id\": \"900003\"")
        .into_bytes();
    for answer in [big, spoiled] {
        let r = Arc::new(Replayer::new(vec![
            parse_exchange(&head, answer, req.clone()).unwrap(),
        ]));
        let e = AniList::new()
            .lookup(&kit(r.clone()), &movie(PERFECT_BLUE, Some(1997)))
            .await
            .unwrap_err()
            .0;
        assert_eq!(e, "AniList's answer wasn't understood");
        r.finish().unwrap();
    }
}

/// A title that folds to nothing, or a kind AniList has none of, is no
/// match, and nothing is sent.
#[tokio::test(start_paused = true)]
async fn nothing_is_sent_for_punctuation_or_another_kind() {
    let wire = Arc::new(Silent::default());
    let k = kit(wire.clone());
    for q in [
        movie("...", None),
        Query::new(PERFECT_BLUE, None, MediaKind::Comic).unwrap(),
        Query::new(PERFECT_BLUE, None, MediaKind::Music).unwrap(),
    ] {
        assert_eq!(
            AniList::new().lookup(&k, &q).await.unwrap(),
            Lookup::NoMatch
        );
    }
    assert_eq!(wire.count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_host_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("anilist_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = AniList::new()
        .lookup(&k, &movie(PERFECT_BLUE, Some(1997)))
        .await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = AniList::new()
        .lookup(&k, &movie("Nothing Like This", None))
        .await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(2), "30 a minute");
    }
    assert!(AniList::new().min_interval() >= Duration::from_secs(2));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let w = AniList::new();
    let m = w.manifest();
    assert_eq!(m.id, "anilist");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "graphql.anilist.co")]);
    assert_eq!(w.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(w.kinds(), [MediaKind::Movie, MediaKind::Tv]);
    // External: nothing goes until the endpoint is acknowledged.
    assert!(need(m, None, &[]).is_some());
    assert_eq!(need(m, Some(&Ack::of(m)), &[]), None);

    let policy = w.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    assert!(policy.credentials.is_empty());
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_secs(2));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    assert!(anilist::SENDS.contains("graphql.anilist.co"));
}

// ---- Under ENR-10's runtime ----

#[derive(Default)]
struct Up;

impl Switches for Up {
    fn kill_switch_tripped(&self) -> bool {
        false
    }
}

/// A [`Fetch`] that records request bodies and never answers: for runs
/// where nothing may be sent, or to see what would be.
#[derive(Default)]
struct Silent {
    count: AtomicUsize,
    sent: Mutex<Vec<Vec<u8>>>,
}

impl Fetch for Silent {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.sent.lock().unwrap().push(req.body.clone());
        Box::pin(std::future::pending())
    }
}

/// A bencoded info dict named `name` with one 900 MB video file.
fn info_dict(name: &str) -> Vec<u8> {
    let file = format!("{name}.mkv");
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
    let src = s.register_source(anilist::ID, "1", 0).unwrap();
    if enabled {
        let ack = Ack::of(AniList::new().manifest()).encode();
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
        Arc::new(AniList::new()),
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
        HostLimiter::new(AniList::new().policy().limits(&UserLimits::default())),
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

const NAME: &str = "Perfect.Blue.1997.1080p.BluRay.x264-GRP";

/// Registered but not enabled: the runtime waits, and nothing is sent.
#[tokio::test]
async fn off_by_default_nothing_is_sent() {
    let (mut s, dht, src) = store(false);
    let (id, _) = put(&mut s, NAME, dht, 0);
    let wire = Arc::new(Silent::default());
    let ran = run(s, wire.clone(), src, &[id], |st| n(&st.deferred) >= 1).await;
    assert_eq!(wire.count.load(Ordering::SeqCst), 0);
    assert_eq!(n(&ran.stats.looked_up), 0);
    assert!(ran.found.is_empty());
}

/// A private item is never sent, even opened first; the public one is
/// looked up (exactly the one recorded request) and its answer emitted
/// under the key.
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
        let (id, _) = put(&mut s, &format!("{NAME}.{i}"), dht, f);
        assert_ne!(f & flags::PRIVACY, 0);
        private.push(id);
    }
    let (public, key) = put(&mut s, NAME, dht, 0);
    let replayer = Arc::new(Replayer::load(&fixture("anilist_match")).unwrap());
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
    let answer = || async {
        let (got, _) = lookup("anilist_match", &movie(PERFECT_BLUE, Some(1997))).await;
        matched(got)
    };
    let discovery = |key: InfoHash, src: SourceId, i: Identification| {
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
    let (id, key) = put(&mut s, NAME, dht, 0);
    let filter = core_with(&["lexa"], &["rolea"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 0);
    let row = s.torrent_claims(id).unwrap();
    let c = row
        .iter()
        .find(|c| c.source == src)
        .and_then(|c| c.claims.clone())
        .expect("stored");
    assert_eq!(c.ids.len(), 1);
    assert_eq!(
        c.details
            .as_deref()
            .and_then(|d| d.canonical_title.as_deref()),
        Some(PERFECT_BLUE)
    );

    // "perfect" is a child role here and a word of one of the answer's
    // synonyms a sexual term: alone the torrent's name is clean, with the
    // answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, NAME, dht, 0);
    let filter = core_with(&["stage"], &["perfect"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
