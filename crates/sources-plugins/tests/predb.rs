//! Contract tests for the PreDB plugin (ENR-predb, §18.9), on the fixtures
//! in `fixtures/predb/` (see its `README.md`), with no network. Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, a
//!   series, the local pre match, the request it sends (the folded title
//!   and year as one parameter), errors (429 back-off, an error answer's
//!   hold, a timeout, a redirect, a broken or oversize answer), and its
//!   manifest;
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
use lodestar_sources_feeds::predb::{Nuke, SEARCH_LIMITS, parse_search};
use lodestar_sources_plugins::predb::{self, After, PreDb, after_title, choose, pre_for};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const TITLE: &str = "Quiet Harbor";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/predb")
        .join(case)
}

fn body(case: &str) -> Vec<u8> {
    std::fs::read(fixture(case).join("001.body")).unwrap()
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        PreDb::new().policy(),
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
    let got = PreDb::new().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

fn matched(got: Result<Lookup, String>) -> Identification {
    let Ok(Lookup::Match(i)) = got else {
        panic!("a match, not {got:?}")
    };
    i
}

/// What `choose` makes of a fixture's answer for `q`, with no request.
fn chosen(case: &str, q: &Query) -> Lookup {
    let s = parse_search(&body(case), &SEARCH_LIMITS);
    assert!(s.page.whole());
    let refs: Vec<_> = s.releases.iter().collect();
    choose(q, &refs)
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, replayer) = lookup("predb_match", &movie(TITLE, Some(2021))).await;
    replayer.finish().unwrap();
    let i = matched(got);
    assert!(i.ids.is_empty() && i.canonical_title.is_none() && i.alt_titles.is_empty());
    // The film's genre once; the longer title's "Comedy" isn't its.
    assert_eq!(i.genres, ["Drama"]);
    assert_eq!(
        (i.year, i.confidence),
        (Some(2021), predb::CONFIDENCE_WITH_YEAR)
    );
    let claims = i.into_claims();
    assert!(!format!("{claims:?}").contains("SENTINEL"));
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.match_confidence, Some(predb::CONFIDENCE_WITH_YEAR));
    assert_eq!(d.year, Some(2021));
}

/// A film whose agreeing release is in the scene's `XXX` section is
/// marked adult, so the runtime refuses it; any other section is unmarked.
#[test]
fn an_xxx_section_is_marked_adult_and_refused() {
    use lodestar_sources_feeds::predb::{Nuke, PreRelease};
    let pre = |section: &str| PreRelease {
        name: "Velvet.Hours.2015.1080p.WEB.x264-GRP".to_owned(),
        pretime: None,
        section: Some(section.to_owned()),
        nuke: Nuke::Clean,
        reason: None,
        genre: None,
    };
    let terms = *Identifier::terms(&PreDb::new());
    let q = movie("Velvet Hours", Some(2015));
    for (section, adult) in [("XXX", Some(true)), ("X264-HD", None)] {
        let r = pre(section);
        let Lookup::Match(i) = choose(&q, &[&r]) else {
            panic!("expected a match for {section}");
        };
        assert_eq!(i.adult, adult, "{section}");
        assert_eq!(terms.accepts(i.adult), adult.is_none(), "{section}");
    }
}

#[test]
fn the_year_must_agree_and_two_years_are_no_match() {
    // A year none of the releases has.
    assert_eq!(
        chosen("predb_match", &movie(TITLE, Some(1999))),
        Lookup::NoMatch
    );
    // "glass orchard" with no year: a 2019 and a 1987 film tie.
    let glass = "Glass Orchard";
    assert_eq!(
        chosen("predb_candidates", &movie(glass, None)),
        Lookup::NoMatch
    );
    // The year picks one, with its genre.
    let Lookup::Match(i) = chosen("predb_candidates", &movie(glass, Some(1987))) else {
        panic!("a match")
    };
    assert_eq!(
        (i.genres.as_slice(), i.year),
        (&["Horror".to_owned()][..], Some(1987))
    );
    // The season counts only for a series.
    let Lookup::Match(i) = chosen("predb_candidates", &tv(glass, Some(1987))) else {
        panic!("a match")
    };
    assert_eq!(i.year, Some(1987));
    // A title that only starts the same way is never a match.
    assert_eq!(
        chosen("predb_match", &movie("Quiet", None)),
        Lookup::NoMatch
    );
}

#[test]
fn the_title_is_read_from_the_start_of_a_name() {
    let want = "quiet harbor";
    assert_eq!(
        after_title("Quiet.Harbor.2021.1080p-GRP", want),
        Some(After::Year(2021))
    );
    assert_eq!(
        after_title("Quiet_Harbor_S01E02_720p-GRP", want),
        Some(After::Season)
    );
    assert_eq!(
        after_title("Quiet.Harbor.S02-GRP", want),
        Some(After::Season)
    );
    for no in [
        "Quiet.Harbor.Lights.2021-GRP",
        "Quiet.Harbor-GRP",
        "Quiet.Harbor.1850-GRP",
        "Quiet.Harbor.Sx-GRP",
        "Quiet.Harbor.S01E-GRP",
        "Quiet.Harbors.2021-GRP",
        "The.Quiet.Harbor.2021-GRP",
    ] {
        assert_eq!(after_title(no, want), None, "{no}");
    }
}

#[test]
fn the_pre_claim_is_matched_on_the_name_inside_the_install() {
    let s = parse_search(&body("predb_match"), &SEARCH_LIMITS);
    let c = pre_for("quiet.harbor.2021.1080p.web.h264-synth", &s.releases).unwrap();
    assert_eq!(c.release, "Quiet.Harbor.2021.1080p.WEB.H264-SYNTH");
    assert_eq!(c.pretime, 1_700_000_200);
    assert_eq!(c.section.as_deref(), Some("X264-HD"));
    assert_eq!(
        (c.nuke, c.reason.as_deref()),
        (Nuke::Nuked, Some("mislabeled.source"))
    );
    let c = pre_for("Quiet.Harbor.2021.PROPER.1080p.WEB.H264-OTHER", &s.releases).unwrap();
    assert_eq!(c.nuke, Nuke::Unnuked);
    // Not a release of the answer, or not a scene name: nothing.
    assert_eq!(pre_for("Quiet.Harbor.2021.720p-ELSE", &s.releases), None);
    assert_eq!(pre_for("Quiet Harbor 2021", &s.releases), None);
    // Two records of one name that disagree: nothing.
    let mut twice = s.releases.clone();
    let mut other = twice[0].clone();
    other.pretime = Some(T);
    twice.push(other);
    assert_eq!(pre_for(&s.releases[0].name, &twice), None);
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match_after_one_request() {
    let (got, replayer) = lookup("predb_no_match", &tv("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(replayer.requests().len(), 1);
}

// ---- The request ----

#[tokio::test(start_paused = true)]
async fn the_request_carries_the_folded_title_and_year_only() {
    let (_, replayer) = lookup("predb_match", &movie("Quiet: Harbor!", Some(2021))).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let req = &seen[0].1;
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(predb::HOST));
    assert_eq!(url.path(), predb::API);
    let params: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let want: Vec<(String, String)> = [
        ("type", "search"),
        ("q", "quiet harbor 2021"),
        ("limit", "100"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    assert_eq!(params, want, "the title and year, never a hash or a file");
    assert!(req.body.is_empty());
    let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "User-Agent"], "no other header");
    let ua = req.headers.iter().find(|(n, _)| n == "User-Agent");
    assert_eq!(ua.map(|(_, v)| v.as_str()), Some(predb::USER_AGENT));
    assert!(predb::USER_AGENT.starts_with("Lodestar/"));
}

/// Text that would add a parameter, or a search operator, stays inside
/// `q`, and its punctuation is folded away before it's sent.
#[test]
fn a_hostile_title_is_one_parameter() {
    for hostile in [
        "a&type=nfo&release=x",
        "a#frag",
        "a\u{0}b?q=1",
        "-tag=foreign",
    ] {
        let t = predb::fold(hostile);
        let req = predb::search_request(&t, None).unwrap();
        let url = url::Url::parse(&req.url).unwrap();
        let keys: Vec<String> = url.query_pairs().map(|(k, _)| k.into_owned()).collect();
        assert_eq!(keys, ["type", "q", "limit"], "{hostile}");
        let q = url.query_pairs().find(|(k, _)| k == "q").unwrap().1;
        assert!(q.chars().all(|c| c.is_alphanumeric() || c == ' '), "{q}");
        assert!(url.fragment().is_none());
    }
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("predb_rate_limited")).unwrap());
    let k = kit(replayer.clone());
    let q = movie(TITLE, Some(2021));
    let e = PreDb::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(e, predb::RATE_LIMITED);
    let again = PreDb::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(again, predb::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = PreDb::new().lookup(&k, &q).await.unwrap_err().0;
    assert_ne!(later, predb::HELD, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

/// An error answer on a 200 isn't seen by the host limiter: the plugin
/// holds itself for the answer's `Retry-After` (5 s), sending nothing
/// before it.
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let replayer = Arc::new(Replayer::load(&fixture("predb_error_then_match")).unwrap());
    let k = kit(replayer.clone());
    let w = PreDb::new();
    let q = movie(TITLE, Some(2021));
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, predb::API_ERROR);
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(w.lookup(&k, &q).await.unwrap_err().0, predb::HELD);
    assert_eq!(replayer.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(w.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = movie(TITLE, Some(2021));
    for (case, want) in [
        ("predb_error", predb::API_ERROR),
        ("predb_bad_body", "PreDB's answer wasn't understood"),
        ("predb_html_body", "PreDB's answer wasn't understood"),
        ("predb_server_error", "PreDB's request failed"),
        ("predb_timeout", "PreDB's request failed"),
        // Not followed: one request, a plain failure.
        ("predb_redirect", "PreDB's request failed"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL"), "{case}");
    }
}

/// An answer past the byte cap, or with a record that isn't one, is an
/// error, never a cut-short match or a miss.
#[tokio::test(start_paused = true)]
async fn an_oversize_or_spoiled_answer_is_an_error() {
    let dir = fixture("predb_match");
    let head = std::fs::read_to_string(dir.join("001.http")).unwrap();
    let body = body("predb_match");
    let mut big = body.clone();
    let close = big.iter().rposition(|&b| b == b'}').unwrap();
    big.splice(
        close..close,
        std::iter::repeat_n(b' ', SEARCH_LIMITS.max_bytes),
    );
    let spoiled = String::from_utf8(body)
        .unwrap()
        .replace("\"pretime\":1700000100", "\"pretime\":\"1700000100\"")
        .into_bytes();
    for answer in [big, spoiled] {
        let r = Arc::new(Replayer::new(vec![
            parse_exchange(&head, answer, Vec::new()).unwrap(),
        ]));
        let e = PreDb::new()
            .lookup(&kit(r.clone()), &movie(TITLE, Some(2021)))
            .await
            .unwrap_err()
            .0;
        assert_eq!(e, "PreDB's answer wasn't understood");
        r.finish().unwrap();
    }
}

/// A title that folds to nothing, or a kind PreDB isn't asked about, is
/// no match, and nothing is sent.
#[tokio::test(start_paused = true)]
async fn nothing_is_sent_for_punctuation_or_another_kind() {
    let wire = Arc::new(Silent::default());
    let k = kit(wire.clone());
    for q in [
        movie("...", None),
        Query::new(TITLE, None, MediaKind::Music).unwrap(),
        Query::new(TITLE, None, MediaKind::Game).unwrap(),
    ] {
        assert_eq!(PreDb::new().lookup(&k, &q).await.unwrap(), Lookup::NoMatch);
    }
    assert_eq!(wire.count.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_host_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("predb_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = PreDb::new().lookup(&k, &movie(TITLE, Some(2021))).await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = PreDb::new()
        .lookup(&k, &movie("Nothing Like This", None))
        .await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(2), "30 a minute");
    }
    assert!(PreDb::new().min_interval() >= Duration::from_secs(2));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let w = PreDb::new();
    let m = w.manifest();
    assert_eq!(m.id, "predb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "api.predb.net")]);
    assert_eq!(w.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(w.kinds(), [MediaKind::Movie, MediaKind::Tv]);
    assert!(need(m, None, &[]).is_some());
    assert_eq!(need(m, Some(&Ack::of(m)), &[]), None);

    let policy = w.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    assert!(policy.credentials.is_empty());
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    // The owner's cap is one a second; the site's 30 a minute is stricter.
    assert!(limits.per_host.interval >= Duration::from_secs(2));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    assert!(predb::SENDS.contains("api.predb.net"));
    assert!(predb::SENDS.contains("Never the info hash"));
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
struct Silent {
    count: AtomicUsize,
}

impl Fetch for Silent {
    fn fetch<'a>(&'a self, _: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        self.count.fetch_add(1, Ordering::SeqCst);
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
    let src = s.register_source(predb::ID, "1", 0).unwrap();
    if enabled {
        let ack = Ack::of(PreDb::new().manifest()).encode();
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
        Arc::new(PreDb::new()),
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
        HostLimiter::new(PreDb::new().policy().limits(&UserLimits::default())),
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

const NAME: &str = "Quiet.Harbor.2021.1080p.WEB.H264-SYNTH";

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
/// looked up (exactly the one recorded request: its parsed title and
/// year, not its name) and its answer emitted under the key.
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
    let replayer = Arc::new(Replayer::load(&fixture("predb_match")).unwrap());
    let opened = [private[0], private[1], private[2], public];
    let ran = run(s, replayer.clone(), src, &opened, |st| n(&st.done) >= 4).await;
    replayer.finish().unwrap();
    assert_eq!(n(&ran.stats.ineligible), 3);
    assert_eq!((n(&ran.stats.looked_up), n(&ran.stats.matched)), (1, 1));
    assert_eq!(ran.found.len(), 1);
    assert_eq!(ran.found[0].found.key, key);
    let url = &replayer.requests()[0].1.url;
    assert!(!url.contains("SYNTH") && !url.contains("1080p"), "{url}");
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
/// plugin's claims; when its genre completes a child-safety pattern with
/// the torrent's name, the hash is held and nothing stored.
#[tokio::test]
async fn an_answer_is_judged_and_a_held_one_is_not_stored() {
    let answer = || async {
        let (got, _) = lookup("predb_match", &movie(TITLE, Some(2021))).await;
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
    assert_eq!(c.details.as_deref().and_then(|d| d.year), Some(2021));

    // "harbor" is a child role here and the answer's genre a sexual term:
    // alone the torrent's name is clean, with the answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, NAME, dht, 0);
    let filter = core_with(&["drama"], &["harbor"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
