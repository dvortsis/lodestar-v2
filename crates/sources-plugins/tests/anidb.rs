//! Contract tests for the AniDB plugin (ENR-anidb, §18.9), on the
//! hand-written fixtures in `fixtures/anidb/` (see its `README.md`), with
//! no network. Three parts:
//!
//! - the plugin on its own: a match, no match, an ambiguous title, the
//!   requests it sends (never a title), the dump's once-a-day rule across
//!   restarts, the day's cap, errors (a ban, an API error, 429, a
//!   timeout, a redirect, a broken answer), and its manifest;
//! - the client credential: read on each lookup, nothing sent without it;
//! - the plugin under ENR-10's runtime and the writer's ingest: off by
//!   default nothing is sent, a private item never sent, a held answer
//!   never stored.
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
use lodestar_sources::consent::need;
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::Replayer;
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Emitter, Endpoints, Fetch, HostLimiter, HttpError, Kind, Kit,
    MemoryState, PluginState, Request, Response, Shutdown, SourceCtx, SourcePlugin, UserLimits,
};
use lodestar_sources_feeds::anidb::gzip;
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::anidb::{self, AniDb};
use lodestar_store::{NewTorrent, SourceId, SourceState, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const T: i64 = 1_700_000_000;
/// The wall clock the plugin reads in these tests (mid-day UTC).
const NOW: i64 = 20_000 * 86_400 + 43_200;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/anidb")
        .join(case)
}

fn creds() -> Arc<StaticCredentials> {
    Arc::new(StaticCredentials::new([
        (anidb::CLIENT, "lodestartest"),
        (anidb::CLIENT_VERSION, "1"),
    ]))
}

fn plugin() -> AniDb {
    AniDb::with_clock(creds(), Arc::new(|| NOW))
}

fn kit_with(fetch: Arc<dyn Fetch>, state: Arc<MemoryState>) -> Kit {
    Kit::new(state, fetch, plugin().policy(), &UserLimits::default())
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    kit_with(fetch, Arc::new(MemoryState::new()))
}

fn tv(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Tv).unwrap()
}

fn replayer(case: &str) -> Arc<Replayer> {
    Arc::new(Replayer::load(&fixture(case)).unwrap())
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let r = replayer(case);
    let got = plugin().lookup(&kit(r.clone()), q).await;
    (got.map_err(|e| e.0), r)
}

fn matched(got: Result<Lookup, String>) -> Identification {
    let Ok(Lookup::Match(i)) = got else {
        panic!("a match, not {got:?}")
    };
    i
}

/// No request ever carries the title, or anything but the fixed template.
fn nothing_of(r: &Replayer, title: &str) {
    for (_, req) in r.requests() {
        for word in title.split(' ') {
            assert!(!req.url.contains(word), "{}", req.url);
        }
    }
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_match_maps_to_an_identification() {
    let q = tv("Star Fragments", Some(2004));
    let (got, r) = lookup("anidb_match", &q).await;
    r.finish().unwrap();
    nothing_of(&r, "Star Fragments");
    let i = matched(got);
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("anidb", "9001")]);
    assert_eq!(i.canonical_title.as_deref(), Some("Hoshi no Kakera"));
    let alts: Vec<(&str, &str)> = i
        .alt_titles
        .iter()
        .map(|a| (a.language.as_str(), a.title.as_str()))
        .collect();
    // Official and synonym titles; never the short one.
    assert_eq!(
        alts,
        [
            ("en", "Star Fragments"),
            ("ja", "星のかけら"),
            ("en", "Fragments of Stars")
        ]
    );
    assert_eq!(
        (i.year, i.confidence, i.adult),
        (Some(2004), anidb::CONFIDENCE_WITH_YEAR, Some(false))
    );
    assert!(i.genres.is_empty());
    let claims = i.clone().into_claims();
    assert!(!format!("{claims:?}{:?}", i.alt_titles).contains("SENTINEL"));
    assert_eq!(claims.ids.len(), 1);
    assert_eq!(claims.details.as_deref().unwrap().alt_titles.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn the_main_title_and_no_year_match_too() {
    let (got, _) = lookup("anidb_match", &tv("Hoshi no Kakera", None)).await;
    let i = matched(got);
    assert_eq!(
        (i.year, i.confidence),
        (Some(2004), anidb::CONFIDENCE_WITHOUT_YEAR)
    );
}

/// The record must agree: a film isn't a series, a year outside its run
/// isn't it.
#[tokio::test(start_paused = true)]
async fn a_record_that_disagrees_is_no_match() {
    let film = Query::new("Star Fragments", Some(2004), MediaKind::Movie).unwrap();
    let (got, _) = lookup("anidb_match", &film).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    let (got, _) = lookup("anidb_match", &tv("Star Fragments", Some(2010))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

/// A title the dump doesn't name, or one two anime share, is no match
/// after the dump alone: no record is asked for.
#[tokio::test(start_paused = true)]
async fn a_title_not_named_once_sends_nothing_more() {
    let r = replayer("anidb_no_match");
    let k = kit(r.clone());
    let p = plugin();
    for title in ["Nothing Like This", "Twin Title", "HnK", "...."] {
        assert_eq!(
            p.lookup(&k, &tv(title, None)).await.unwrap(),
            Lookup::NoMatch
        );
    }
    r.finish().unwrap();
    assert_eq!(r.requests().len(), 1, "the dump only");
}

/// AniDB's 18+ flag is passed on, and the terms refuse it.
#[tokio::test(start_paused = true)]
async fn a_restricted_anime_is_marked_adult() {
    let (got, _) = lookup("anidb_restricted", &tv("Yoru no Hana", Some(1999))).await;
    let i = matched(got);
    assert_eq!(i.adult, Some(true));
    assert!(!anidb::TERMS.accepts(i.adult));
}

/// AniDB answers gzip: read the same as plain.
#[tokio::test(start_paused = true)]
async fn gzip_answers_are_read() {
    struct Gz(Arc<Replayer>);
    impl Fetch for Gz {
        fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
            Box::pin(async move {
                let mut res = self.0.fetch(req).await?;
                res.body = gzip(&res.body);
                Ok(res)
            })
        }
    }
    let r = replayer("anidb_match");
    let got = plugin()
        .lookup(
            &kit(Arc::new(Gz(r.clone()))),
            &tv("Star Fragments", Some(2004)),
        )
        .await;
    assert_eq!(matched(got.map_err(|e| e.0)).ids[0].value, "9001");
}

// ---- What is sent, and how often ----

#[tokio::test(start_paused = true)]
async fn the_requests_are_the_fixed_templates() {
    let (_, r) = lookup("anidb_match", &tv("Star Fragments", Some(2004))).await;
    let seen = r.requests();
    assert_eq!(seen[0].1.url, anidb::DUMP_URL);
    assert_eq!(
        seen[1].1.url,
        "http://api.anidb.net:9001/httpapi?request=anime&client=lodestartest&clientver=1&protover=1&aid=9001"
    );
    for (_, req) in &seen {
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names
                .iter()
                .all(|n| ["User-Agent", "Accept-Encoding"].contains(n))
        );
        let ua = req.headers.iter().find(|(n, _)| n == "User-Agent");
        assert_eq!(ua.map(|(_, v)| v.as_str()), Some(anidb::USER_AGENT));
    }
    assert!(anidb::USER_AGENT.starts_with("Lodestar/"));
    assert!(!anidb::USER_AGENT.contains('@'));
}

/// The dump once, the records paced at 2 s, and a record asked for again
/// within the day answered from memory.
#[tokio::test(start_paused = true)]
async fn the_dump_is_fetched_once_and_records_are_paced_and_cached() {
    let r = replayer("anidb_two_lookups");
    let k = kit(r.clone());
    let p = plugin();
    let star = tv("Star Fragments", Some(2004));
    assert!(matches!(p.lookup(&k, &star).await, Ok(Lookup::Match(_))));
    let film = Query::new("Sea and Sky", Some(2010), MediaKind::Movie).unwrap();
    assert!(matches!(p.lookup(&k, &film).await, Ok(Lookup::Match(_))));
    assert!(matches!(p.lookup(&k, &star).await, Ok(Lookup::Match(_))));
    r.finish().unwrap();
    let seen = r.requests();
    assert_eq!(seen.len(), 3);
    let api: Vec<_> = seen
        .iter()
        .filter(|(_, q)| q.url.starts_with(anidb::API_URL))
        .collect();
    assert_eq!(api.len(), 2);
    assert!(api[1].0 - api[0].0 >= anidb::MIN_INTERVAL, "one every 2 s");
    assert!(p.min_interval() >= Duration::from_secs(2));
}

/// The last dump attempt is kept in state: a restarted plugin within the
/// day waits rather than fetching it again, and fetches it after.
#[tokio::test(start_paused = true)]
async fn a_restart_within_the_day_never_fetches_the_dump_again() {
    let state = Arc::new(MemoryState::new());
    let at = (NOW - 3600).to_string().into_bytes();
    state.set(anidb::STATE_DUMP_AT, at).await.unwrap();
    let wire = Arc::new(Silent::default());
    let got = plugin()
        .lookup(
            &kit_with(wire.clone(), state.clone()),
            &tv("Star Fragments", None),
        )
        .await;
    assert_eq!(got.unwrap_err().0, anidb::WAITING);
    assert_eq!(wire.count.load(Ordering::SeqCst), 0);
    // A day later it's fetched.
    let later = AniDb::with_clock(creds(), Arc::new(|| NOW + 23 * 3600));
    let r = replayer("anidb_no_match");
    let got = later
        .lookup(&kit_with(r.clone(), state.clone()), &tv("Nothing", None))
        .await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    r.finish().unwrap();
    let kept = state.get(anidb::STATE_DUMP_AT).await.unwrap().unwrap();
    assert_eq!(kept, (NOW + 23 * 3600).to_string().into_bytes());
}

/// A failed dump download counts: the next lookup waits a day.
#[tokio::test(start_paused = true)]
async fn a_failed_dump_is_not_retried_within_the_day() {
    let r = replayer("anidb_server_error");
    let k = kit(r.clone());
    let p = plugin();
    let q = tv("Star Fragments", None);
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::FAILED);
    tokio::time::advance(Duration::from_secs(7 * 3600)).await;
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::WAITING);
    assert_eq!(r.requests().len(), 1);
}

/// A record from the future (a clock set back) restarts the wait from
/// now instead of holding the plugin for ever.
#[tokio::test(start_paused = true)]
async fn a_clock_set_back_restarts_the_wait() {
    let state = Arc::new(MemoryState::new());
    let future = (NOW + 30 * 86_400).to_string().into_bytes();
    state.set(anidb::STATE_DUMP_AT, future).await.unwrap();
    let wire = Arc::new(Silent::default());
    let got = plugin()
        .lookup(
            &kit_with(wire.clone(), state.clone()),
            &tv("Star Fragments", None),
        )
        .await;
    assert_eq!(got.unwrap_err().0, anidb::WAITING);
    assert_eq!(wire.count.load(Ordering::SeqCst), 0);
    let kept = state.get(anidb::STATE_DUMP_AT).await.unwrap().unwrap();
    assert_eq!(kept, NOW.to_string().into_bytes());
}

/// At most [`anidb::DAILY_RECORDS`] records a UTC day, kept in state.
#[tokio::test(start_paused = true)]
async fn the_days_cap_holds_across_restarts() {
    let state = Arc::new(MemoryState::new());
    let today = NOW.div_euclid(86_400);
    let used = format!("{today} {}", anidb::DAILY_RECORDS).into_bytes();
    state.set(anidb::STATE_DAY, used).await.unwrap();
    let r = replayer("anidb_no_match");
    let got = plugin()
        .lookup(
            &kit_with(r.clone(), state.clone()),
            &tv("Star Fragments", None),
        )
        .await;
    assert_eq!(got.unwrap_err().0, anidb::DAILY_CAP);
    r.finish().unwrap();
    assert_eq!(r.requests().len(), 1, "the dump, never the record");
    // Yesterday's count doesn't hold today.
    let state = Arc::new(MemoryState::new());
    let old = format!("{} {}", today - 1, anidb::DAILY_RECORDS).into_bytes();
    state.set(anidb::STATE_DAY, old).await.unwrap();
    let r = replayer("anidb_match");
    let got = plugin()
        .lookup(
            &kit_with(r.clone(), state.clone()),
            &tv("Star Fragments", None),
        )
        .await;
    assert!(matches!(got, Ok(Lookup::Match(_))));
    let kept = state.get(anidb::STATE_DAY).await.unwrap().unwrap();
    assert_eq!(kept, format!("{today} 1").into_bytes());
}

// ---- The client credential ----

#[tokio::test(start_paused = true)]
async fn without_a_valid_client_nothing_is_sent() {
    let cases: [(&[(&str, &str)], &str); 4] = [
        (&[], anidb::NO_CLIENT),
        (&[(anidb::CLIENT, "lodestartest")], anidb::NO_CLIENT),
        (
            &[(anidb::CLIENT, "bad&name"), (anidb::CLIENT_VERSION, "1")],
            anidb::BAD_CLIENT,
        ),
        (
            &[
                (anidb::CLIENT, "lodestartest"),
                (anidb::CLIENT_VERSION, "1.0"),
            ],
            anidb::BAD_CLIENT,
        ),
    ];
    for (stored, want) in cases {
        let wire = Arc::new(Silent::default());
        let p = AniDb::with_clock(
            Arc::new(StaticCredentials::new(stored.iter().copied())),
            Arc::new(|| NOW),
        );
        let got = p
            .lookup(&kit(wire.clone()), &tv("Star Fragments", None))
            .await;
        let e = got.unwrap_err().0;
        assert_eq!(e, want);
        assert!(!e.contains("bad&name"));
        assert_eq!(wire.count.load(Ordering::SeqCst), 0);
    }
    assert!(!format!("{:?}", plugin()).contains("lodestartest"));
}

// ---- Errors ----

/// `<error>Banned</error>` holds the plugin a day, sending nothing.
#[tokio::test(start_paused = true)]
async fn a_ban_holds_the_plugin_for_a_day() {
    let r = replayer("anidb_banned");
    let k = kit(r.clone());
    let p = plugin();
    let q = tv("Star Fragments", None);
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::BANNED);
    tokio::time::advance(Duration::from_secs(23 * 3600)).await;
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::HELD);
    r.finish().unwrap();
    tokio::time::advance(Duration::from_secs(3601)).await;
    assert_ne!(p.lookup(&k, &q).await.unwrap_err().0, anidb::HELD);
}

#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_an_hour() {
    let r = replayer("anidb_api_error");
    let k = kit(r.clone());
    let p = plugin();
    let q = tv("Star Fragments", None);
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::API_ERROR);
    tokio::time::advance(Duration::from_secs(3599)).await;
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::HELD);
    assert_eq!(r.requests().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_429_backs_off_through_the_host_limiter() {
    let r = replayer("anidb_rate_limited");
    let k = kit(r.clone());
    let p = plugin();
    let q = tv("Star Fragments", None);
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::RATE_LIMITED);
    assert_eq!(p.lookup(&k, &q).await.unwrap_err().0, anidb::HELD);
    assert_eq!(r.requests().len(), 2);
}

/// No error is a miss, and none holds the service's text.
#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let cases = [
        ("anidb_bad_body", anidb::BAD_ANSWER),
        ("anidb_html_body", anidb::BAD_ANSWER),
        ("anidb_wrong_id", anidb::BAD_ANSWER),
        ("anidb_timeout", anidb::FAILED),
        ("anidb_redirect", anidb::FAILED),
        ("anidb_server_error", anidb::FAILED),
    ];
    for (case, want) in cases {
        let (got, r) = lookup(case, &tv("Star Fragments", None)).await;
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL") && !e.contains("elsewhere"));
        r.finish().unwrap();
    }
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let p = plugin();
    let m = p.manifest();
    assert_eq!(m.id, "anidb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(
        hosts,
        [("https", "anidb.net"), ("http", "api.anidb.net:9001")]
    );
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(p.kinds(), [MediaKind::Movie, MediaKind::Tv]);
    // Account: nothing goes until both credentials are stored.
    assert!(need(m, None, &[]).is_some());
    assert!(AniDb::need(&["client".into()]).is_some());
    let policy = p.policy();
    let names: Vec<&str> = policy.credentials.iter().map(|c| c.name).collect();
    assert_eq!(names, [anidb::CLIENT, anidb::CLIENT_VERSION]);
    assert!(policy.credentials.iter().all(|c| c.required));
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_secs(2));
    assert_eq!(limits.per_host.burst, 1);
    let t = p.terms();
    assert!(!t.background_walk && !t.redistribute && t.adult_marker_required);
    assert!(anidb::SENDS.contains("No title"));
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
/// client stored (only the sealed forms' presence counts here) and
/// enabled only when `enabled`.
fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(anidb::ID, "1", 0).unwrap();
    if enabled {
        s.update_source_secrets(
            src,
            &[
                (anidb::CLIENT, b"sealed"),
                (anidb::CLIENT_VERSION, b"sealed"),
            ],
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
        clock: || NOW,
        ..Config::default()
    };
    let (src, interest) = IdentifySource::new(
        Arc::new(plugin()),
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
        HostLimiter::new(plugin().policy().limits(&UserLimits::default())),
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

const NAME: &str = "Hoshi no Kakera S01 2004 1080p BluRay x264";

/// Registered with no client stored and not enabled: nothing is sent.
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
/// looked up (exactly the recorded dump and record) and emitted.
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
            &format!("Hoshi no Kakera S0{} 2004 720p", i + 2),
            dht,
            f,
        );
        private.push(id);
    }
    let (public, key) = put(&mut s, NAME, dht, 0);
    let r = replayer("anidb_match");
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
    let answer = || async {
        let (got, _) = lookup("anidb_match", &tv("Hoshi no Kakera", Some(2004))).await;
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

    // "hoshi" is a child role here and a word of the answer's English
    // title a sexual term: alone the name is clean, with the answer a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, NAME, dht, 0);
    let filter = core_with(&["fragments"], &["hoshi"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
