//! Contract tests for the Open Library plugin (ENR-ol, §18.9), on the
//! fixtures in `fixtures/openlibrary/` (see its `README.md`), with no
//! network. Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   request it sends, errors (429 back-off, an error object's hold, a
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
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::openlibrary::{Olid, SEARCH_LIMITS, Work};
use lodestar_sources_plugins::openlibrary::{self, OpenLibrary};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const FRANKENSTEIN: &str = "Frankenstein";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/openlibrary")
        .join(case)
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        OpenLibrary::new().policy(),
        &UserLimits::default(),
    )
}

fn ebook(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Ebook).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = OpenLibrary::new().lookup(&kit(replayer.clone()), q).await;
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
    let (got, replayer) = lookup("openlibrary_match", &ebook(FRANKENSTEIN, Some(1818))).await;
    replayer.finish().unwrap();
    let i = matched(got);
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("olid", "OL900001W")]);
    assert_eq!(i.canonical_title.as_deref(), Some(FRANKENSTEIN));
    // The other titles, once each, in no known language; the canonical
    // one isn't repeated.
    let alts: Vec<(&str, &str)> = i
        .alt_titles
        .iter()
        .map(|a| (a.language.as_str(), a.title.as_str()))
        .collect();
    assert_eq!(
        alts,
        [
            ("und", "Frankenstein; or, The Modern Prometheus"),
            ("und", "Frankenstein, ou le Prométhée moderne"),
        ]
    );
    assert_eq!(
        (i.year, i.confidence),
        (Some(1818), openlibrary::CONFIDENCE_WITH_YEAR)
    );
    assert!(i.genres.is_empty());
    // Nothing of the authors, subjects or other works.
    let claims = i.clone().into_claims();
    let all = format!("{claims:?}{:?}{:?}", i.canonical_title, i.alt_titles);
    assert!(
        !all.contains("SENTINEL") && !all.contains("900002"),
        "{all}"
    );
    // Its claims survive the clamp unchanged: already in stored form.
    assert_eq!(claims.ids.len(), 1);
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.alt_titles.len(), 2);
    assert_eq!(d.match_confidence, Some(80));
    assert_eq!(d.canonical_title.as_deref(), Some(FRANKENSTEIN));
}

#[tokio::test(start_paused = true)]
async fn a_later_edition_year_or_an_alternative_title_matches_too() {
    // A year among its publication years, not the first.
    let (got, _) = lookup("openlibrary_match", &ebook(FRANKENSTEIN, Some(1831))).await;
    let i = matched(got);
    assert_eq!((i.ids[0].value.as_str(), i.year), ("OL900001W", Some(1831)));
    // No year: the work's first publication year, at the lower confidence.
    let (got, _) = lookup("openlibrary_match", &ebook(FRANKENSTEIN, None)).await;
    let i = matched(got);
    assert_eq!(
        (i.year, i.confidence),
        (Some(1818), openlibrary::CONFIDENCE_WITHOUT_YEAR)
    );
    // A year it doesn't have is no match.
    let (got, _) = lookup("openlibrary_match", &ebook(FRANKENSTEIN, Some(1900))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match_after_one_request() {
    let (got, replayer) = lookup("openlibrary_no_match", &ebook("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(replayer.requests().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_year_picks_among_candidates_and_a_tie_is_no_match() {
    // Two works called Dune, and Dune Messiah: the year picks one.
    let (got, r) = lookup("openlibrary_candidates", &ebook("Dune", Some(1965))).await;
    r.finish().unwrap();
    assert_eq!(matched(got).ids[0].value, "OL900011W");
    let (got, _) = lookup("openlibrary_candidates", &ebook("Dune", Some(1984))).await;
    assert_eq!(matched(got).ids[0].value, "OL900012W");
    // No year: two works agree, so it doesn't guess.
    let (got, _) = lookup("openlibrary_candidates", &ebook("Dune", None)).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // A year none has.
    let (got, _) = lookup("openlibrary_candidates", &ebook("Dune", Some(1999))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // An audiobook is looked up the same way.
    let audio = Query::new("Dune", Some(1965), MediaKind::Audiobook).unwrap();
    let (got, _) = lookup("openlibrary_candidates", &audio).await;
    assert_eq!(matched(got).ids[0].value, "OL900011W");
}

#[test]
fn choose_needs_exactly_one_work() {
    let work = |id: u64, title: &str, year: Option<u16>| Work {
        id: Olid(id),
        title: Some(title.into()),
        alternatives: Vec::new(),
        first_year: year,
        years: Vec::new(),
        truncated: false,
    };
    let a = work(1, "Emma", Some(1815));
    let b = work(2, "EMMA!", Some(1815));
    let q = ebook("emma", Some(1815));
    assert_eq!(openlibrary::choose(&q, &[&a, &b]), Lookup::NoMatch);
    assert!(matches!(openlibrary::choose(&q, &[&a]), Lookup::Match(_)));
    assert_eq!(openlibrary::choose(&q, &[]), Lookup::NoMatch);
    // A work with no title and no year never agrees.
    let bare = Work {
        title: None,
        first_year: None,
        ..a.clone()
    };
    assert_eq!(openlibrary::choose(&q, &[&bare]), Lookup::NoMatch);
}

#[test]
fn the_book_title_drops_an_author_before_it() {
    use openlibrary::book_title;
    assert_eq!(book_title("Mary Shelley - Frankenstein"), "Frankenstein");
    assert_eq!(book_title("A - B - C"), "B - C");
    assert_eq!(book_title("Frankenstein"), "Frankenstein");
    assert_eq!(book_title("Mary Shelley - ..."), "Mary Shelley - ...");
    assert_eq!(
        book_title("Mary-Shelley-Frankenstein"),
        "Mary-Shelley-Frankenstein"
    );
}

/// A parsed `Author - Title` sends the title only, and matches on it.
#[tokio::test(start_paused = true)]
async fn an_author_before_the_title_is_never_sent() {
    let q = ebook("Mary Shelley - Frankenstein", Some(1818));
    let (got, replayer) = lookup("openlibrary_match", &q).await;
    replayer.finish().unwrap();
    assert_eq!(matched(got).ids[0].value, "OL900001W");
    let sent = &replayer.requests()[0].1.url;
    assert!(!sent.to_lowercase().contains("shelley"), "{sent}");
}

#[test]
fn titles_compare_folded() {
    assert_eq!(
        openlibrary::fold("  FRANKENSTEIN: or, the Modern Prometheus! "),
        "frankenstein or the modern prometheus"
    );
    assert_eq!(openlibrary::fold("Les Misérables"), "les misérables");
    assert_eq!(openlibrary::fold("..."), "");
}

// ---- What is sent ----

#[tokio::test(start_paused = true)]
async fn the_request_carries_the_folded_title_and_fixed_parameters_only() {
    let q = ebook("Frankenstein:", Some(1818));
    let (_, replayer) = lookup("openlibrary_match", &q).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let req = &seen[0].1;
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(openlibrary::HOST));
    assert_eq!(url.path(), openlibrary::API);
    let params: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let want: Vec<(String, String)> = [
        ("title", "frankenstein"),
        ("fields", openlibrary::FIELDS),
        ("limit", "10"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    assert_eq!(params, want, "the title, never the year, a hash or a file");
    assert!(req.body.is_empty());
    let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "User-Agent"], "no other header");
    let ua = req.headers.iter().find(|(n, _)| n == "User-Agent");
    assert_eq!(ua.map(|(_, v)| v.as_str()), Some(openlibrary::USER_AGENT));
}

#[test]
fn the_user_agent_names_the_project_only() {
    let ua = openlibrary::USER_AGENT;
    assert!(ua.starts_with("Lodestar/"), "{ua}");
    assert!(ua.contains("(https://github.com/"), "a project URL: {ua}");
    assert!(!ua.contains('@'), "no email: {ua}");
    for word in ["localhost", "home", "user"] {
        assert!(!ua.to_lowercase().contains(word), "{word} in {ua}");
    }
}

/// Text that would add parameters or reach the service's query syntax is
/// folded to words, and stays one encoded value.
#[tokio::test(start_paused = true)]
async fn a_hostile_title_is_one_folded_parameter() {
    let title = "x&limit=1000&fields=* author:\"Someone\" OR (title:*) #";
    let wire = Arc::new(Silent::default());
    let k = kit(wire.clone());
    let q = ebook(title, None);
    let w = OpenLibrary::new();
    let fut = w.lookup(&k, &q);
    // The request is sent and never answered: look at what was sent.
    tokio::select! {
        _ = fut => panic!("never answered"),
        () = tokio::time::sleep(Duration::from_secs(1)) => {}
    }
    let sent = wire.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    let url = url::Url::parse(&sent[0]).unwrap();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(pairs.len(), 3, "{pairs:?}");
    assert_eq!(
        pairs[0],
        (
            "title".to_owned(),
            "x limit 1000 fields author someone or title".to_owned()
        )
    );
    assert_eq!(pairs[2], ("limit".to_owned(), "10".to_owned()));
    let req = openlibrary::search_request(title).unwrap();
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.query_pairs().filter(|(k, _)| k == "title").count(), 1);
    assert_eq!(url.query_pairs().filter(|(k, _)| k == "limit").count(), 1);
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("openlibrary_rate_limited")).unwrap());
    let k = kit(replayer.clone());
    let q = ebook(FRANKENSTEIN, Some(1818));
    let e = OpenLibrary::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(e, openlibrary::RATE_LIMITED);
    // Held by the limiter (ENR-10's client): the retry sends nothing.
    let again = OpenLibrary::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(again, openlibrary::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = OpenLibrary::new().lookup(&k, &q).await.unwrap_err().0;
    assert_ne!(later, openlibrary::HELD, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

/// An error object on a 200 isn't seen by the host limiter: the plugin
/// holds itself for the answer's `Retry-After` (5 s), sending nothing
/// before it.
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let replayer = Arc::new(Replayer::load(&fixture("openlibrary_error_then_match")).unwrap());
    let k = kit(replayer.clone());
    let w = OpenLibrary::new();
    let q = ebook(FRANKENSTEIN, Some(1818));
    assert_eq!(
        w.lookup(&k, &q).await.unwrap_err().0,
        openlibrary::API_ERROR
    );
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, openlibrary::HELD);
    assert_eq!(replayer.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(w.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = ebook(FRANKENSTEIN, Some(1818));
    for (case, want) in [
        ("openlibrary_error", openlibrary::API_ERROR),
        (
            "openlibrary_bad_body",
            "Open Library's answer wasn't understood",
        ),
        (
            "openlibrary_html_body",
            "Open Library's answer wasn't understood",
        ),
        ("openlibrary_server_error", "Open Library's request failed"),
        ("openlibrary_timeout", "Open Library's request failed"),
        // Not followed: one request, a plain failure.
        ("openlibrary_redirect", "Open Library's request failed"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL"), "{case}");
    }
}

/// An answer past the byte cap, or with a work that isn't one, is an
/// error, never a cut-short match or a miss.
#[tokio::test(start_paused = true)]
async fn an_oversize_or_spoiled_answer_is_an_error() {
    let dir = fixture("openlibrary_match");
    let head = std::fs::read_to_string(dir.join("001.http")).unwrap();
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
        .replace("/works/OL900002W", "/authors/OL900002A")
        .into_bytes();
    for answer in [big, spoiled] {
        let r = Arc::new(Replayer::new(vec![
            parse_exchange(&head, answer, Vec::new()).unwrap(),
        ]));
        let e = OpenLibrary::new()
            .lookup(&kit(r.clone()), &ebook(FRANKENSTEIN, Some(1818)))
            .await
            .unwrap_err()
            .0;
        assert_eq!(e, "Open Library's answer wasn't understood");
        r.finish().unwrap();
    }
}

/// A title that folds to nothing is no match, and nothing is sent.
#[tokio::test(start_paused = true)]
async fn a_title_of_punctuation_sends_nothing() {
    let wire = Arc::new(Silent::default());
    let got = OpenLibrary::new()
        .lookup(&kit(wire.clone()), &ebook("...", None))
        .await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(wire.count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_host_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("openlibrary_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = OpenLibrary::new()
        .lookup(&k, &ebook(FRANKENSTEIN, Some(1818)))
        .await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = OpenLibrary::new()
        .lookup(&k, &ebook("Nothing Like This", None))
        .await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(1), "one a second");
    }
    assert!(OpenLibrary::new().min_interval() >= Duration::from_secs(1));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let w = OpenLibrary::new();
    let m = w.manifest();
    assert_eq!(m.id, "openlibrary");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "openlibrary.org")]);
    assert_eq!(w.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(w.kinds(), [MediaKind::Ebook, MediaKind::Audiobook]);
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

/// A [`Fetch`] that records requests and never answers: for runs where
/// nothing may be sent, or to see what would be.
#[derive(Default)]
struct Silent {
    count: AtomicUsize,
    sent: Mutex<Vec<String>>,
}

impl Fetch for Silent {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.sent.lock().unwrap().push(req.url.clone());
        Box::pin(std::future::pending())
    }
}

/// A bencoded info dict named `name` with one 2 MB EPUB file.
fn info_dict(name: &str) -> Vec<u8> {
    let file = "Frankenstein.epub";
    let mut d = Vec::new();
    d.extend_from_slice(b"d5:filesld6:lengthi2097152e4:pathl");
    d.extend_from_slice(format!("{}:{file}", file.len()).as_bytes());
    d.extend_from_slice(b"eee4:name");
    d.extend_from_slice(format!("{}:{name}", name.len()).as_bytes());
    d.extend_from_slice(b"12:piece lengthi262144e6:pieces");
    d.extend_from_slice(format!("{}:", 20 * 8).as_bytes());
    d.extend(std::iter::repeat_n(0u8, 20 * 8));
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
    let src = s.register_source(openlibrary::ID, "1", 0).unwrap();
    if enabled {
        let ack = Ack::of(OpenLibrary::new().manifest()).encode();
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
        Arc::new(OpenLibrary::new()),
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
        HostLimiter::new(OpenLibrary::new().policy().limits(&UserLimits::default())),
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

const NAME: &str = "Mary Shelley - Frankenstein (2003) [EPUB]";

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
        let (id, _) = put(
            &mut s,
            &format!("Mary Shelley - Frankenstein (2003) [EPUB] {i}"),
            dht,
            f,
        );
        assert_ne!(f & flags::PRIVACY, 0);
        private.push(id);
    }
    let (public, key) = put(&mut s, NAME, dht, 0);
    let replayer = Arc::new(Replayer::load(&fixture("openlibrary_match")).unwrap());
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
        let (got, _) = lookup("openlibrary_match", &ebook(FRANKENSTEIN, Some(1818))).await;
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
        Some(FRANKENSTEIN)
    );

    // "frankenstein" is a child role here and a word of the answer's
    // French title a sexual term: alone the torrent's name is clean, with
    // the answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, NAME, dht, 0);
    let filter = core_with(&["moderne"], &["frankenstein"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
