//! Contract tests for the srrDB plugin (ENR-srrdb, §18.9), on the
//! fixtures in `fixtures/srrdb/` (see its `README.md`), with no network.
//! Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   request it sends, errors (429 back-off, a timeout, a redirect, a
//!   broken or oversize answer), and its manifest;
//! - the plugin under ENR-10's runtime and the writer's ingest: off by
//!   default, a private item never sent, the answer stored as claims.
//!
//! No fixed sleeps: paused time for the plugin alone, and condition waits
//! of at least 10 s for the runtime.

#[path = "support/harness.rs"]
mod harness;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_metainfo::{Info, InfoHash};
use lodestar_pipeline::enrich::{enrich_with_rule, file_infos};
use lodestar_pipeline::identify::{Config, IdentifySource, IdentifyStats, Switches};
use lodestar_pipeline::source_adult::AdultSources;
use lodestar_pipeline::unverified::{self, Ctx};
use lodestar_sources::consent::{Ack, need};
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::srrdb::{SEARCH_LIMITS, parse_search};
use lodestar_sources_plugins::srrdb::{self, Srrdb};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use proptest::prelude::*;
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const TITLE: &str = "Harbour Lights";
const NAME: &str = "Harbour.Lights.2004.1080p.BluRay.x264-QUiETGRP";
const IMDB: &str = "tt0999901";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/srrdb")
        .join(case)
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        Srrdb::new().policy(),
        &UserLimits::default(),
    )
}

fn film(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Movie).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = Srrdb::new().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

fn matched(got: Result<Lookup, String>) -> Identification {
    match got {
        Ok(Lookup::Match(i)) => i,
        other => panic!("a match, got {other:?}"),
    }
}

fn seed(name: &str) -> lodestar_sources_feeds::srrdb::Search {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/seeds/srrdb")
        .join(name);
    parse_search(&std::fs::read(path).unwrap(), &SEARCH_LIMITS)
}

fn imdb_of(l: Lookup) -> Option<String> {
    match l {
        Lookup::Match(i) => Some(i.ids[0].value.clone()),
        Lookup::NoMatch => None,
    }
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, replayer) = lookup("srrdb_match", &film(TITLE, Some(2004))).await;
    replayer.finish().unwrap();
    let i = matched(got);
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("imdb", IMDB)]);
    // No text of srrDB's: scene names aren't titles.
    assert!(i.canonical_title.is_none() && i.alt_titles.is_empty() && i.genres.is_empty());
    assert_eq!((i.year, i.confidence), (Some(2004), srrdb::CONFIDENCE_YEAR));
    let claims = i.into_claims();
    assert!(!format!("{claims:?}").contains("SENTINEL"));
    // The id survives the clamp: the store's one spelling.
    assert_eq!(claims.ids.len(), 1);
    assert_eq!(
        claims.details.as_deref().unwrap().match_confidence,
        Some(80)
    );
}

#[test]
fn the_title_and_year_must_agree() {
    let s = seed("search.json");
    for (title, year, want) in [
        (TITLE, Some(2004), Some(IMDB)),
        // No year: every agreeing release says 2004.
        (TITLE, None, Some(IMDB)),
        ("harbour: LIGHTS!", Some(2004), Some(IMDB)),
        (TITLE, Some(2005), None),
        // Its own id, from one release.
        ("Harbour Lights Revisited", Some(2004), Some("tt0999902")),
        ("Harbour", Some(2004), None),
        ("OST Harbour Lights", Some(2004), None),
    ] {
        let got = imdb_of(srrdb::choose(&film(title, year), &s.hits));
        assert_eq!(got.as_deref(), want, "{title} {year:?}");
    }
    let i = match srrdb::choose(&film(TITLE, None), &s.hits) {
        Lookup::Match(i) => i,
        Lookup::NoMatch => panic!("a match"),
    };
    assert_eq!(
        (i.year, i.confidence),
        (Some(2004), srrdb::CONFIDENCE_TITLE)
    );
    // Only films are answered.
    let tv = Query::new(TITLE, Some(2004), MediaKind::Tv).unwrap();
    assert_eq!(srrdb::choose(&tv, &s.hits), Lookup::NoMatch);
}

/// A film whose agreeing release carries the scene's `XXX` tag is marked
/// adult, so the runtime refuses it; an untagged one is unmarked.
#[test]
fn an_xxx_tagged_release_is_marked_adult_and_refused() {
    use lodestar_sources_feeds::srrdb::{Hit, ImdbId};
    let hit = |release: &str| Hit {
        release: release.to_owned(),
        imdb: ImdbId::parse("0888802"),
        size: None,
    };
    let terms = *Identifier::terms(&Srrdb::new());
    let tagged = [hit("Velvet.Hours.2015.XXX.1080p.WEB.x264-GRP")];
    let Lookup::Match(i) = srrdb::choose(&film("Velvet Hours", Some(2015)), &tagged) else {
        panic!("expected a match");
    };
    assert_eq!(i.adult, Some(true));
    assert!(!terms.accepts(i.adult));
    let plain = [hit("Velvet.Hours.2015.1080p.WEB.x264-GRP")];
    let Lookup::Match(i) = srrdb::choose(&film("Velvet Hours", Some(2015)), &plain) else {
        panic!("expected a match");
    };
    assert_eq!(i.adult, None);
    assert!(terms.accepts(i.adult));
}

#[test]
fn candidates_never_guess() {
    let s = seed("candidates.json");
    assert!(s.page.whole());
    // 1999: two releases, one id.
    assert_eq!(
        imdb_of(srrdb::choose(&film("Echoes", Some(1999)), &s.hits)).as_deref(),
        Some("tt0888801")
    );
    // 2010: two releases with two ids.
    assert_eq!(
        srrdb::choose(&film("Echoes", Some(2010)), &s.hits),
        Lookup::NoMatch
    );
    // No year: two films.
    assert_eq!(
        srrdb::choose(&film("Echoes", None), &s.hits),
        Lookup::NoMatch
    );
    // A year none has.
    assert_eq!(
        srrdb::choose(&film("Echoes", Some(2001)), &s.hits),
        Lookup::NoMatch
    );
    // No release with an id.
    let none: Vec<_> = s
        .hits
        .iter()
        .cloned()
        .map(|mut h| {
            h.imdb = None;
            h
        })
        .collect();
    assert_eq!(
        srrdb::choose(&film("Echoes", Some(1999)), &none),
        Lookup::NoMatch
    );
}

#[test]
fn scene_names_read_to_a_title_and_year() {
    for (name, want) in [
        (NAME, Some(("harbour lights", 2004))),
        (
            "Harbour_Lights_2004_PAL_DVDR-GRP",
            Some(("harbour lights", 2004)),
        ),
        ("Harbour.Lights.2004-GRP", Some(("harbour lights", 2004))),
        ("1917.2019.1080p.BluRay.x264-GRP", Some(("1917", 2019))),
        (
            "Blade.Runner.2049.2017.720p-GRP",
            Some(("blade runner 2049", 2017)),
        ),
        ("Spider-Man.2002.DVDRip-GRP", Some(("spider man", 2002))),
        ("Show.Name.2019.S01E02.720p-GRP", None),
        ("Show.Name.2019.s01.720p-GRP", None),
        ("OST-Harbour_Lights-2004-GRP", None),
        ("Artist-Album-WEB-2004-GRP", None),
        ("2004.1080p-GRP", None),
        ("No.Year.Here-GRP", None),
        ("", None),
    ] {
        let got = srrdb::scene_title(name);
        assert_eq!(got.as_ref().map(|(t, y)| (t.as_str(), *y)), want, "{name}");
    }
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match() {
    let (got, replayer) = lookup("srrdb_no_match", &film("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(replayer.requests().len(), 1);
}

// ---- What is sent ----

fn segments(req: &Request) -> Vec<String> {
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(srrdb::HOST));
    assert_eq!(url.query(), None, "no query string");
    assert_eq!(url.fragment(), None);
    let path = url.path().strip_prefix(srrdb::PATH).unwrap();
    path.split('/')
        .skip(1)
        .map(|s| {
            url::form_urlencoded::parse(format!("x={s}").as_bytes())
                .next()
                .unwrap()
                .1
                .into_owned()
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_request_carries_the_title_words_and_year_only() {
    let (_, replayer) = lookup("srrdb_match", &film(TITLE, Some(2004))).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let (_, req) = &seen[0];
    assert_eq!(segments(req), ["harbour", "lights", "2004"]);
    assert!(req.body.is_empty());
    let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "User-Agent"], "no other header");
    let ua = req.headers.iter().find(|(n, _)| n == "User-Agent");
    assert_eq!(ua.map(|(_, v)| v.as_str()), Some(srrdb::USER_AGENT));
}

#[test]
fn the_user_agent_names_the_project_only() {
    let ua = srrdb::USER_AGENT;
    assert!(ua.starts_with("Lodestar/"), "{ua}");
    assert!(ua.ends_with(" )"), "{ua}");
    assert!(ua.contains("( https://github.com/"), "a project URL: {ua}");
    assert!(!ua.contains('@'), "no email: {ua}");
    assert!(srrdb::SENDS.contains("title") && srrdb::SENDS.contains("year"));
}

/// Text that would add a path segment, a search operator, a query string
/// or a fragment is folded to plain words.
#[test]
fn a_hostile_title_is_plain_words() {
    let q = film(r"x/../r:Evil.*?a=b#frag imdb:tt1 -y %2F\z", None);
    let req = srrdb::search_request(&q).unwrap().unwrap();
    assert_eq!(
        segments(&req),
        [
            "x", "r", "evil", "a", "b", "frag", "imdb", "tt1", "y", "2f", "z"
        ]
    );
    // Past MAX_WORDS, the rest are left out.
    let long = "a ".repeat(40);
    let req = srrdb::search_request(&film(&long, Some(1999)))
        .unwrap()
        .unwrap();
    assert_eq!(segments(&req).len(), srrdb::MAX_WORDS + 1);
}

proptest! {
    /// Whatever the title, the path is the fixed prefix and one segment
    /// for each folded word (letters and digits only), then the year.
    #[test]
    fn any_title_is_plain_segments(title in "\\PC{1,80}", year in proptest::option::of(1u16..=9999)) {
        let Some(q) = Query::new(&title, year, MediaKind::Movie) else { return Ok(()) };
        let Some(req) = srrdb::search_request(&q).unwrap() else {
            prop_assert!(srrdb::words(q.title()).is_empty());
            return Ok(());
        };
        let got = segments(&req);
        let mut want = srrdb::words(q.title());
        prop_assert!(want.len() <= srrdb::MAX_WORDS);
        for w in &want {
            prop_assert!(!w.is_empty() && w.chars().all(char::is_alphanumeric));
        }
        if let Some(y) = year { want.push(y.to_string()); }
        prop_assert_eq!(got, want);
    }

    /// Any release name reads without panic to a folded title and a year
    /// in range, or to nothing.
    #[test]
    fn any_scene_name_reads_or_not(name in "\\PC{0,120}") {
        if let Some((t, y)) = srrdb::scene_title(&name) {
            prop_assert!(!t.is_empty() && srrdb::fold(&t) == t);
            prop_assert!((1880..=2100).contains(&y));
        }
    }
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("srrdb_rate_limited")).unwrap());
    let k = kit(replayer.clone());
    let q = film(TITLE, Some(2004));
    let e = Srrdb::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(e, srrdb::RATE_LIMITED);
    let again = Srrdb::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(again, srrdb::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = Srrdb::new().lookup(&k, &q).await.unwrap_err().0;
    assert_ne!(later, srrdb::HELD, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

/// An error object on a 200, which the host limiter doesn't see: the
/// plugin holds itself for the answer's `Retry-After` (15 s).
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let replayer = Arc::new(Replayer::load(&fixture("srrdb_error_then_match")).unwrap());
    let k = kit(replayer.clone());
    let m = Srrdb::new();
    let q = film(TITLE, Some(2004));
    assert_eq!(m.lookup(&k, &q).await.unwrap_err().0, srrdb::API_ERROR);
    tokio::time::advance(Duration::from_secs(14)).await;
    assert_eq!(m.lookup(&k, &q).await.unwrap_err().0, srrdb::HELD);
    assert_eq!(replayer.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(m.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = film(TITLE, Some(2004));
    for (case, want) in [
        ("srrdb_error", srrdb::API_ERROR),
        ("srrdb_bad_body", "srrDB's answer wasn't understood"),
        ("srrdb_html_body", "srrDB's answer wasn't understood"),
        ("srrdb_server_error", "srrDB's request failed"),
        ("srrdb_timeout", "srrDB's request failed"),
        // Not followed: one request, a plain failure.
        ("srrdb_redirect", "srrDB's request failed"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL"), "{case}");
    }
}

/// An answer past the byte cap is an error, never a cut-short match.
#[tokio::test(start_paused = true)]
async fn an_oversize_answer_is_an_error() {
    let dir = fixture("srrdb_match");
    let mut body = std::fs::read(dir.join("001.body")).unwrap();
    let head = std::fs::read_to_string(dir.join("001.http")).unwrap();
    while body.last() != Some(&b'}') {
        body.pop();
    }
    body.pop();
    body.resize(SEARCH_LIMITS.max_bytes + 1, b' ');
    body.push(b'}');
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&head, body, Vec::new()).unwrap(),
    ]));
    let e = Srrdb::new()
        .lookup(&kit(r.clone()), &film(TITLE, Some(2004)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "srrDB's answer wasn't understood");
    r.finish().unwrap();
}

/// A title with no word, or a kind other than Movie, is no match, and
/// nothing is sent.
#[tokio::test(start_paused = true)]
async fn nothing_to_search_sends_nothing() {
    let wire = Arc::new(Silent::default());
    let k = kit(wire.clone());
    let got = Srrdb::new().lookup(&k, &film("... !!!", None)).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    let tv = Query::new(TITLE, Some(2004), MediaKind::Tv).unwrap();
    assert_eq!(Srrdb::new().lookup(&k, &tv).await.unwrap(), Lookup::NoMatch);
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_crawl_delay() {
    let replayer = Arc::new(Replayer::load(&fixture("srrdb_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = Srrdb::new().lookup(&k, &film(TITLE, Some(2004))).await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = Srrdb::new()
        .lookup(&k, &film("Nothing Like This", None))
        .await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(10), "one per 10 s");
    }
    assert_eq!(Srrdb::new().min_interval(), Duration::from_secs(10));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let m = Srrdb::new();
    let man = m.manifest();
    assert_eq!(man.id, "srrdb");
    assert_eq!(man.kinds, [Kind::Enrichment]);
    assert_eq!(man.consent, Consent::External);
    assert!(!man.enabled_by_default && !man.always_adult && !man.finds_private);
    assert!(lodestar_pipeline::identify::admitted(man));
    let hosts: Vec<_> = man.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "api.srrdb.com")]);
    assert_eq!(m.endpoints(), Endpoints::manifest(man.endpoints));
    assert_eq!(m.kinds(), [MediaKind::Movie]);
    assert!(need(man, None, &[]).is_some());
    assert_eq!(need(man, Some(&Ack::of(man)), &[]), None);

    let policy = m.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    assert!(policy.credentials.is_empty());
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_secs(10));
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

/// A [`Fetch`] that counts requests and never answers.
#[derive(Default)]
struct Silent(AtomicUsize);

impl Fetch for Silent {
    fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

/// A bencoded single-file info dict: `name`.mkv, 1 GiB.
fn info_dict(name: &str) -> Vec<u8> {
    let file = format!("{name}.mkv");
    let (len, piece) = (1_073_741_824u64, 4_194_304u64);
    let pieces = usize::try_from(len / piece).unwrap();
    let mut d = Vec::new();
    d.extend_from_slice(format!("d6:lengthi{len}e4:name{}:{file}", file.len()).as_bytes());
    d.extend_from_slice(format!("12:piece lengthi{piece}e6:pieces{}:", 20 * pieces).as_bytes());
    d.extend(std::iter::repeat_n(0u8, 20 * pieces));
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

fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(srrdb::ID, "1", 0).unwrap();
    if enabled {
        let ack = Ack::of(Srrdb::new().manifest()).encode();
        s.enable_source(src, Some(&ack)).unwrap();
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
        Arc::new(Srrdb::new()),
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
        HostLimiter::new(Srrdb::new().policy().limits(&UserLimits::default())),
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

#[tokio::test]
async fn off_by_default_nothing_is_sent() {
    let (mut s, dht, src) = store(false);
    let (id, _) = put(&mut s, NAME, dht, 0);
    let wire = Arc::new(Silent::default());
    let ran = run(s, wire.clone(), src, &[id], |st| n(&st.deferred) >= 1).await;
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    assert_eq!(n(&ran.stats.looked_up), 0);
    assert!(ran.found.is_empty());
}

/// A private item is never sent, even opened first; the public one is
/// looked up (exactly the recorded request: its parsed title and year,
/// never its name) and its answer emitted under the key.
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
            &format!("Harbour.Lights.2004.1080p.BluRay.x264-P{i}"),
            dht,
            f,
        );
        assert_ne!(f & flags::PRIVACY, 0);
        private.push(id);
    }
    let (public, key) = put(&mut s, NAME, dht, 0);
    let replayer = Arc::new(Replayer::load(&fixture("srrdb_match")).unwrap());
    let opened = [private[0], private[1], private[2], public];
    let ran = run(s, replayer.clone(), src, &opened, |st| n(&st.done) >= 4).await;
    replayer.finish().unwrap();
    assert_eq!(n(&ran.stats.ineligible), 3);
    assert_eq!((n(&ran.stats.looked_up), n(&ran.stats.matched)), (1, 1));
    assert_eq!(ran.found.len(), 1);
    assert_eq!(ran.found[0].found.key, key);
}

/// The answer goes through the writer's ingest and is stored as the
/// plugin's claims: an IMDb id and a year, no text of srrDB's.
#[tokio::test]
async fn an_answer_is_stored_through_the_ingest() {
    let (got, _) = lookup("srrdb_match", &film(TITLE, Some(2004))).await;
    let i = matched(got);
    let (mut s, dht, src) = store(true);
    let (id, key) = put(&mut s, NAME, dht, 0);
    let mut found = lodestar_sources::Found::key(key, lodestar_pipeline::identify::STRATEGY)
        .with_claims(i.into_claims());
    found.clamp();
    let d = Discovery {
        source: src,
        found,
        at: T,
        reply: false,
    };
    let stats = unverified::UnverifiedStats::default();
    let ctx = Ctx {
        quarantine_secs: 86_400,
        enrichers: lodestar_pipeline::adult_enrich::EnrichSources {
            listed: 0,
            kind: 1u64 << src.get(),
        },
        ..Ctx::default()
    };
    let filter = core_with(&["lexa"], &["rolea"]);
    unverified::ingest(&mut s, &filter.current(), ctx, &[d], &stats, T, &|_, _| {}).unwrap();
    assert_eq!(n(&stats.held), 0);
    let row = s.torrent_claims(id).unwrap();
    let c = row
        .iter()
        .find(|c| c.source == src)
        .and_then(|c| c.claims.clone())
        .expect("stored");
    let ids: Vec<_> = c
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("imdb", IMDB)]);
}
