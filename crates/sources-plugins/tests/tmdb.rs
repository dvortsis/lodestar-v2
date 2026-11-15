//! Contract tests for the TMDB plugin (ENR-tmdb, §18.9), on the fixtures
//! in `fixtures/tmdb/` (see its `README.md`), with no network. Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   request it sends, the token, errors (401, 429 back-off, a timeout, a
//!   redirect, a broken or oversize answer), and its manifest;
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
use lodestar_sources::consent::{Need, need_with};
use lodestar_sources::identify::{Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::tmdb::{Form, LIMITS, parse_search};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::tmdb::{self, Tmdb};
use lodestar_store::{NewTorrent, SourceId, SourceState, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const SHAWSHANK: &str = "The Shawshank Redemption";
/// Made up, the same as in every fixture's `Authorization` header.
const TOKEN: &str = "fake-tmdb-token.0000";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/tmdb")
        .join(case)
}

fn plugin() -> Tmdb {
    Tmdb::new(Arc::new(StaticCredentials::new([(tmdb::TOKEN, TOKEN)])))
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        plugin().policy(),
        &UserLimits::default(),
    )
}

fn movie(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Movie).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = plugin().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

fn body(case: &str) -> Vec<u8> {
    std::fs::read(fixture(case).join("001.body")).unwrap()
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, replayer) = lookup("tmdb_match", &movie(SHAWSHANK, Some(1994))).await;
    replayer.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("tmdb", "278")]);
    assert_eq!(i.canonical_title.as_deref(), Some(SHAWSHANK));
    assert!(i.alt_titles.is_empty(), "the original title is the same");
    assert_eq!(i.genres, ["Drama", "Crime"]);
    assert_eq!(
        (i.year, i.confidence),
        (Some(1994), tmdb::CONFIDENCE_WITH_YEAR)
    );
    let claims = i.clone().into_claims();
    let all = format!("{claims:?}{:?}{:?}", i.canonical_title, i.genres);
    assert!(
        !all.contains("SENTINEL") && !all.contains("900001"),
        "{all}"
    );
    assert_eq!(claims.ids.len(), 1);
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.genres.len(), 2);
    assert_eq!(d.match_confidence, Some(90));
}

#[tokio::test(start_paused = true)]
async fn a_series_keeps_its_original_title() {
    let q = Query::new("Money Heist", Some(2017), MediaKind::Tv).unwrap();
    let (got, replayer) = lookup("tmdb_tv_match", &q).await;
    replayer.finish().unwrap();
    let Lookup::Match(i) = got.unwrap() else {
        panic!("a match")
    };
    assert_eq!(i.ids[0].value, "71446");
    assert_eq!(i.canonical_title.as_deref(), Some("Money Heist"));
    let alts: Vec<(&str, &str)> = i
        .alt_titles
        .iter()
        .map(|a| (a.language.as_str(), a.title.as_str()))
        .collect();
    assert_eq!(alts, [("es", "La casa de papel")]);
    // TV's own genre names; the unknown id is dropped.
    assert_eq!(i.genres, ["Crime", "Drama", "Sci-Fi & Fantasy"]);
    let d = i.into_claims();
    assert_eq!(d.details.unwrap().alt_titles.len(), 1, "survives the clamp");
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match() {
    let (got, replayer) = lookup("tmdb_no_match", &movie("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn a_tie_is_no_match_and_the_year_picks_one() {
    // Two films called Dune and no year: it doesn't guess.
    let (got, r) = lookup("tmdb_candidates", &movie("Dune", None)).await;
    r.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // The same answer with a year picks one; a year none has, none.
    let hits = parse_search(&body("tmdb_candidates"), Form::Movie, &LIMITS).hits;
    let Lookup::Match(i) = tmdb::choose(&movie("Dune", Some(2021)), &hits) else {
        panic!("a match")
    };
    assert_eq!(i.ids[0].value, "438631");
    assert_eq!(
        tmdb::choose(&movie("Dune", Some(1999)), &hits),
        Lookup::NoMatch
    );
    // A longer title is never the title.
    assert_eq!(
        tmdb::choose(&movie("Dune Drifter", None), &hits),
        tmdb::choose(&movie("dune: drifter!", None), &hits)
    );
}

#[tokio::test(start_paused = true)]
async fn an_adult_result_is_never_chosen() {
    let (got, r) = lookup("tmdb_adult", &movie("Example Adult Title", None)).await;
    r.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[test]
fn titles_compare_folded() {
    assert_eq!(
        tmdb::fold("  The SHAWSHANK: Redemption! "),
        "the shawshank redemption"
    );
    assert_eq!(tmdb::fold("..."), "");
}

// ---- What is sent ----

#[tokio::test(start_paused = true)]
async fn requests_carry_the_title_year_and_fixed_parameters_only() {
    let (_, replayer) = lookup("tmdb_match", &movie(SHAWSHANK, Some(1994))).await;
    let tv = Query::new("Money Heist", None, MediaKind::Tv).unwrap();
    let tv_req = tmdb::search_request(&tv, &secret()).unwrap();
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1, "one request a lookup");
    let params = |req: &Request, path: &str| -> Vec<(String, String)> {
        let url = url::Url::parse(&req.url).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(tmdb::HOST));
        assert_eq!(url.path(), path);
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
        params(&seen[0].1, "/3/search/movie"),
        p(&[
            ("query", SHAWSHANK),
            ("include_adult", "false"),
            ("language", "en-US"),
            ("page", "1"),
            ("primary_release_year", "1994"),
        ]),
        "the title and year, never a hash or a file"
    );
    assert_eq!(
        params(&tv_req, "/3/search/tv"),
        p(&[
            ("query", "Money Heist"),
            ("include_adult", "false"),
            ("language", "en-US"),
            ("page", "1"),
        ]),
        "no year, none sent"
    );
    for req in [&seen[0].1, &tv_req] {
        assert!(req.body.is_empty());
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["Accept", "Authorization"], "no other header");
        assert!(!req.url.contains(TOKEN), "the token is never in the URL");
        assert!(!format!("{req:?}").contains(TOKEN), "nor in Debug");
    }
}

fn secret() -> lodestar_integrations::secret::Secret {
    lodestar_integrations::secret::Secret::new(TOKEN.to_owned())
}

#[test]
fn a_hostile_title_is_one_parameter() {
    let title = "x&query=y&include_adult=true&api_key=z#frag";
    let req = tmdb::search_request(&movie(title, Some(2000)), &secret()).unwrap();
    let url = url::Url::parse(&req.url).unwrap();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let q: Vec<&str> = pairs
        .iter()
        .filter(|(k, _)| k == "query")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(q, [title]);
    assert_eq!(pairs.len(), 5);
    assert!(pairs.contains(&("include_adult".into(), "false".into())));
    assert_eq!(url.fragment(), None);
}

/// No token stored, or one that isn't a token: nothing is sent, and the
/// error names no token.
#[tokio::test(start_paused = true)]
async fn without_a_valid_token_nothing_is_sent() {
    for (creds, want) in [
        (StaticCredentials::default(), tmdb::NO_TOKEN),
        (
            StaticCredentials::new([(tmdb::TOKEN, "bad token\r\nX-Evil: 1")]),
            "the stored TMDB read access token isn't a valid token",
        ),
    ] {
        let wire = Arc::new(Silent::default());
        let e = Tmdb::new(Arc::new(creds))
            .lookup(&kit(wire.clone()), &movie(SHAWSHANK, None))
            .await
            .unwrap_err()
            .0;
        assert_eq!(e, want);
        assert!(!e.contains("Evil"));
        assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    }
    assert!(!format!("{:?}", plugin()).contains(TOKEN));
}

/// Kinds it doesn't search, or a title of punctuation: no match, and
/// nothing is sent.
#[tokio::test(start_paused = true)]
async fn nothing_is_sent_for_what_it_cant_search() {
    let wire = Arc::new(Silent::default());
    for q in [
        movie("...", None),
        Query::new("Some Album", Some(2001), MediaKind::Music).unwrap(),
        Query::new("Some Game", None, MediaKind::Game).unwrap(),
    ] {
        let got = plugin().lookup(&kit(wire.clone()), &q).await;
        assert_eq!(got.unwrap(), Lookup::NoMatch);
    }
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_refusal_or_429_backs_off_and_the_next_lookup_waits() {
    for (case, first) in [
        ("tmdb_refused", tmdb::REFUSED),
        ("tmdb_rate_limited", tmdb::RATE_LIMITED),
    ] {
        let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
        let k = kit(replayer.clone());
        let q = movie(SHAWSHANK, Some(1994));
        let e = plugin().lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(e, first, "{case}");
        assert!(!e.contains(TOKEN));
        // Held by the limiter: the retry sends nothing.
        let again = plugin().lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(again, tmdb::HELD, "{case}");
        assert_eq!(replayer.requests().len(), 1, "{case}");
        replayer.finish().unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = movie(SHAWSHANK, Some(1994));
    for (case, want) in [
        ("tmdb_error_200", "TMDB's answer wasn't understood"),
        ("tmdb_bad_body", "TMDB's answer wasn't understood"),
        ("tmdb_html_body", "TMDB's answer wasn't understood"),
        ("tmdb_server_error", "TMDB's request failed"),
        ("tmdb_timeout", "TMDB's request failed"),
        // Not followed: one request, a plain failure.
        ("tmdb_redirect", "TMDB's request failed"),
    ] {
        let (got, replayer) = lookup(case, &q).await;
        replayer.finish().unwrap();
        let e = got.unwrap_err();
        assert_eq!(e, want, "{case}");
        assert!(!e.contains("SENTINEL") && !e.contains(TOKEN), "{case}");
    }
}

/// An answer past the byte cap is an error, never a cut-short match.
#[tokio::test(start_paused = true)]
async fn an_oversize_answer_is_an_error() {
    let head = std::fs::read_to_string(fixture("tmdb_match").join("001.http")).unwrap();
    let mut big = body("tmdb_match");
    let close = big.pop().unwrap();
    big.resize(LIMITS.max_bytes + 1, b' ');
    big.push(close);
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&head, big, Vec::new()).unwrap(),
    ]));
    let e = plugin()
        .lookup(&kit(r.clone()), &movie(SHAWSHANK, Some(1994)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "TMDB's answer wasn't understood");
    r.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_host_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("tmdb_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = plugin().lookup(&k, &movie(SHAWSHANK, Some(1994))).await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = plugin().lookup(&k, &movie("Nothing Like This", None)).await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(
            w[1].0 - w[0].0 >= Duration::from_millis(100),
            "ten a second"
        );
    }
    assert!(plugin().min_interval() >= Duration::from_millis(100));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let p = plugin();
    let m = p.manifest();
    assert_eq!(m.id, "tmdb");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "api.themoviedb.org")]);
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(p.kinds(), [MediaKind::Movie, MediaKind::Tv]);

    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    let names: Vec<&str> = policy.credentials.iter().map(|c| c.name).collect();
    assert_eq!(names, [tmdb::TOKEN]);
    // Account: the token is needed before it can be enabled.
    assert!(matches!(Tmdb::need(&[]), Some(Need::Credentials)));
    assert_eq!(Tmdb::need(&[tmdb::TOKEN.to_owned()]), None);
    assert_eq!(
        need_with(m, policy.credentials, None, &[tmdb::TOKEN.to_owned()]),
        None
    );
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    assert!(limits.per_host.interval >= Duration::from_millis(100));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
}

/// The Sources row's text names exactly what leaves, and the notice is
/// TMDB's required wording.
#[test]
fn what_is_sent_and_the_attribution() {
    for word in ["title", "year", "film", "TV series", "token", tmdb::HOST] {
        assert!(tmdb::SENDS.contains(word), "{word}");
    }
    assert!(tmdb::SENDS.contains("Never the info hash"));
    assert!(tmdb::ATTRIBUTION.contains("uses TMDB and the TMDB APIs"));
    assert!(tmdb::ATTRIBUTION.contains("not endorsed, certified, or otherwise approved by TMDB"));
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

/// A store with the DHT and the plugin's source registered; the token
/// stored and the source enabled only when `enabled`.
fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(tmdb::ID, "1", 0).unwrap();
    if enabled {
        s.update_source_secrets(
            src,
            &[(tmdb::TOKEN, b"sealed")],
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

/// Registered but not enabled, with no token in the store: the runtime
/// waits, and nothing is sent.
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
/// looked up (exactly the one recorded request) and its answer emitted.
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
    let replayer = Arc::new(Replayer::load(&fixture("tmdb_match")).unwrap());
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
/// plugin's claims; when its original title completes a child-safety
/// pattern with the torrent's name, the hash is held and nothing stored.
#[tokio::test]
async fn an_answer_is_judged_and_a_held_one_is_not_stored() {
    let name = "Money Heist 2017 S01 1080p WEB x264";
    let answer = || async {
        let q = Query::new("Money Heist", Some(2017), MediaKind::Tv).unwrap();
        let (got, _) = lookup("tmdb_tv_match", &q).await;
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
    assert_eq!(c.ids.len(), 1);
    assert_eq!(
        c.details
            .as_deref()
            .and_then(|d| d.canonical_title.as_deref()),
        Some("Money Heist")
    );

    // "heist" is a child role here and the answer's Spanish title holds a
    // sexual term: alone the name is clean, with the answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, name, dht, 0);
    let filter = core_with(&["papel"], &["heist"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
