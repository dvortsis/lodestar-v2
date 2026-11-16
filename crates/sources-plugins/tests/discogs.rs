//! Contract tests for the Discogs plugin (ENR-discogs, §18.9), on the
//! fixtures in `fixtures/discogs/` (see its `README.md`), with no network.
//! Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   request it sends, the token, the rate-limit header, errors (401, 429
//!   back-off, a timeout, a redirect, a broken or oversize answer), and
//!   its manifest;
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
use lodestar_sources::identify::{Identification, Identifier, Lookup, MediaKind, Query};
use lodestar_sources::testing::{Replayer, parse_exchange};
use lodestar_sources::{
    BoxFuture, Consent, Discovery, Distribution, Emitter, Endpoints, Fetch, HostLimiter, HttpError,
    Kind, Kit, MemoryState, Request, Response, Shutdown, SourceCtx, SourcePlugin as _, UserLimits,
    floors,
};
use lodestar_sources_feeds::discogs::{LIMITS, parse_search};
use lodestar_sources_plugins::StaticCredentials;
use lodestar_sources_plugins::discogs::{self, Discogs, Wanted};
use lodestar_store::{NewTorrent, SourceId, SourceState, Store, StoreConfig, flags};
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const TITLE: &str = "Synthetic Orchestra - Paper Lanterns";
const NAME: &str = "Synthetic Orchestra - Paper Lanterns (1998) [FLAC]";
/// Made up, the same as in every fixture's `Authorization` header.
const TOKEN: &str = "fakeDiscogsToken0000";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/discogs")
        .join(case)
}

fn plugin() -> Discogs {
    Discogs::new(Arc::new(StaticCredentials::new([(discogs::TOKEN, TOKEN)])))
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        plugin().policy(),
        &UserLimits::default(),
    )
}

fn music(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Music).unwrap()
}

async fn lookup_with(
    p: &Discogs,
    case: &str,
    q: &Query,
) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = p.lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    lookup_with(&plugin(), case, q).await
}

fn body(case: &str) -> Vec<u8> {
    std::fs::read(fixture(case).join("001.body")).unwrap()
}

fn matched(got: Result<Lookup, String>) -> Identification {
    match got {
        Ok(Lookup::Match(i)) => i,
        other => panic!("a match, got {other:?}"),
    }
}

fn secret() -> lodestar_integrations::secret::Secret {
    lodestar_integrations::secret::Secret::new(TOKEN.to_owned())
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_match_maps_to_an_identification() {
    let (got, replayer) = lookup("discogs_match", &music(TITLE, Some(1998))).await;
    replayer.finish().unwrap();
    let i = matched(got);
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("discogs", "m900001")]);
    assert_eq!(i.canonical_title.as_deref(), Some("Paper Lanterns"));
    assert!(i.alt_titles.is_empty() && i.genres.is_empty());
    assert_eq!(
        (i.year, i.confidence),
        (Some(1998), discogs::CONFIDENCE_ARTIST_YEAR)
    );
    let claims = i.clone().into_claims();
    let all = format!("{claims:?}{:?}", i.canonical_title);
    assert!(
        !all.contains("SENTINEL") && !all.contains("900002"),
        "{all}"
    );
    assert_eq!(claims.ids.len(), 1, "the store's discogs rule takes m<id>");
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.match_confidence, Some(90));
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match() {
    let (got, replayer) = lookup("discogs_no_match", &music("Nothing Like This", None)).await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn a_tie_is_no_match_and_the_year_picks_one() {
    // `Echo Field` and `Echo Field (2)` both have a `Harbour`: no year, no
    // guess.
    let q = music("Echo Field - Harbour", None);
    let (got, r) = lookup("discogs_candidates", &q).await;
    r.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    let hits = parse_search(&body("discogs_candidates"), &LIMITS).hits;
    let pick = |q: &Query| match discogs::choose(q, &hits) {
        Lookup::Match(i) => Some(i.ids[0].value.clone()),
        Lookup::NoMatch => None,
    };
    assert_eq!(
        pick(&music("Echo Field - Harbour", Some(2004))).as_deref(),
        Some("m900011")
    );
    // The number Discogs adds to tell artists apart isn't the name.
    assert_eq!(
        pick(&music("Echo Field - Harbour", Some(2011))).as_deref(),
        Some("m900012")
    );
    assert_eq!(pick(&music("Echo Field - Harbour", Some(1999))), None);
    // A longer title is never the title; `*` (a name variation) is the name.
    assert_eq!(
        pick(&music("Echo Field - Harbour Lights", None)).as_deref(),
        Some("m900013")
    );
    // No artist: the album alone, and a tie is still a tie.
    assert_eq!(pick(&music("Harbour", None)), None);
    assert_eq!(
        pick(&music("Harbour", Some(2011))).as_deref(),
        Some("m900012")
    );
    // Another artist with the album is no match.
    assert_eq!(pick(&music("Other Band - Harbour", Some(2004))), None);
    // Not music: no match.
    let movie = Query::new("Echo Field - Harbour", Some(2004), MediaKind::Movie).unwrap();
    assert_eq!(discogs::choose(&movie, &hits), Lookup::NoMatch);
}

#[test]
fn a_repeated_id_names_the_hit_that_agreed() {
    // The parser keeps a repeated id; the choice skips the hit whose
    // title disagrees and takes its title from the one that agreed.
    let body = br#"{"results":[{"id":5,"type":"master","title":"X - Foo","year":"2000"},{"id":5,"type":"master","title":"A - B","year":"2001"}]}"#;
    let hits = parse_search(body, &LIMITS).hits;
    assert_eq!(hits.len(), 2);
    let Lookup::Match(i) = discogs::choose(&music("B", Some(2001)), &hits) else {
        panic!("no match");
    };
    assert_eq!(i.ids[0].value, "m5");
    assert_eq!(i.canonical_title.as_deref(), Some("B"));
}

#[test]
fn titles_split_and_compare_as_musicbrainz_does() {
    assert_eq!(
        discogs::wanted("Synthetic Orchestra - Paper Lanterns"),
        Some(Wanted {
            artist: Some("Synthetic Orchestra"),
            album: "Paper Lanterns"
        })
    );
    assert_eq!(
        discogs::wanted(" - Paper Lanterns"),
        Some(Wanted {
            artist: None,
            album: "- Paper Lanterns"
        })
    );
    assert_eq!(discogs::wanted("..."), None);
    assert_eq!(discogs::fold("  Paper: LANTERNS! "), "paper lanterns");
    assert_eq!(discogs::artist_name("Echo Field (2)"), "Echo Field");
}

// ---- What is sent ----

#[tokio::test(start_paused = true)]
async fn requests_carry_the_album_and_artist_only() {
    let (_, replayer) = lookup("discogs_match", &music(TITLE, Some(1998))).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1, "one request a lookup");
    let alone = discogs::search_request(
        &Wanted {
            artist: None,
            album: "Paper Lanterns",
        },
        &secret(),
    )
    .unwrap();
    let params = |req: &Request| -> Vec<(String, String)> {
        let url = url::Url::parse(&req.url).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(discogs::HOST));
        assert_eq!(url.path(), discogs::PATH);
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
            ("type", "master"),
            ("release_title", "Paper Lanterns"),
            ("artist", "Synthetic Orchestra"),
            ("per_page", "5"),
            ("page", "1"),
        ]),
        "the album and artist, never the year, a hash or a file"
    );
    assert_eq!(
        params(&alone),
        p(&[
            ("type", "master"),
            ("release_title", "Paper Lanterns"),
            ("per_page", "5"),
            ("page", "1"),
        ]),
        "no artist, none sent"
    );
    for req in [&seen[0].1, &alone] {
        assert!(req.body.is_empty());
        let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["Accept", "User-Agent", "Authorization"],
            "no other header"
        );
        let header = |n: &str| req.headers.iter().find(|(k, _)| k == n).unwrap().1.clone();
        assert_eq!(header("User-Agent"), discogs::USER_AGENT);
        assert_eq!(header("Authorization"), format!("Discogs token={TOKEN}"));
        assert!(!req.url.contains(TOKEN), "the token is never in the URL");
        assert!(!req.url.contains("token"), "nor its parameter");
        assert!(!format!("{req:?}").contains(TOKEN), "nor in Debug");
    }
    assert!(discogs::USER_AGENT.starts_with("Lodestar/"));
    assert!(discogs::USER_AGENT.contains("( https://github.com/"));
}

#[test]
fn a_hostile_title_is_its_own_parameters() {
    let want = Wanted {
        artist: Some("a&type=release&token=x"),
        album: "b&per_page=100#frag",
    };
    let req = discogs::search_request(&want, &secret()).unwrap();
    let url = url::Url::parse(&req.url).unwrap();
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(pairs.len(), 5);
    assert!(pairs.contains(&("type".into(), "master".into())));
    assert!(pairs.contains(&("artist".into(), want.artist.unwrap().into())));
    assert!(pairs.contains(&("release_title".into(), want.album.into())));
    assert!(pairs.contains(&("per_page".into(), "5".into())));
    assert_eq!(url.fragment(), None);
}

/// No token stored, or one that isn't a token: nothing is sent, and the
/// error names no token.
#[tokio::test(start_paused = true)]
async fn without_a_valid_token_nothing_is_sent() {
    for (creds, want) in [
        (StaticCredentials::default(), discogs::NO_TOKEN),
        (
            StaticCredentials::new([(discogs::TOKEN, "bad token\r\nX-Evil: 1")]),
            "the stored Discogs personal access token isn't a valid token",
        ),
    ] {
        let wire = Arc::new(Silent::default());
        let e = Discogs::new(Arc::new(creds))
            .lookup(&kit(wire.clone()), &music(TITLE, None))
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
        music("...", None),
        music(" - ", None),
        Query::new("Some Film", Some(2001), MediaKind::Movie).unwrap(),
        Query::new("Some Game", None, MediaKind::Game).unwrap(),
    ] {
        let got = plugin().lookup(&kit(wire.clone()), &q).await;
        assert_eq!(got.unwrap(), Lookup::NoMatch);
    }
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}

// ---- Errors and pacing ----

#[tokio::test(start_paused = true)]
async fn a_refusal_or_429_backs_off_and_the_next_lookup_waits() {
    for (case, first) in [
        ("discogs_refused", discogs::REFUSED),
        ("discogs_rate_limited", discogs::RATE_LIMITED),
    ] {
        let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
        let k = kit(replayer.clone());
        let q = music(TITLE, Some(1998));
        let e = plugin().lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(e, first, "{case}");
        assert!(!e.contains(TOKEN));
        // Held by the limiter: the retry sends nothing.
        let again = plugin().lookup(&k, &q).await.unwrap_err().0;
        assert_eq!(again, discogs::HELD, "{case}");
        assert_eq!(replayer.requests().len(), 1, "{case}");
        replayer.finish().unwrap();
    }
}

/// When Discogs says none of the minute's requests remain, the answer is
/// used and the plugin sends nothing until the minute is out.
#[tokio::test(start_paused = true)]
async fn no_requests_remaining_waits_out_the_minute() {
    let p = plugin();
    let q = music(TITLE, Some(1998));
    let (got, r) = lookup_with(&p, "discogs_none_remaining", &q).await;
    r.finish().unwrap();
    matched(got);
    let wire = Arc::new(Silent::default());
    let e = p.lookup(&kit(wire.clone()), &q).await.unwrap_err().0;
    assert_eq!(e, discogs::HELD);
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    tokio::time::advance(discogs::WINDOW).await;
    let (got, r) = lookup_with(&p, "discogs_match", &q).await;
    r.finish().unwrap();
    matched(got);
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = music(TITLE, Some(1998));
    for (case, want) in [
        ("discogs_error_200", "Discogs' answer wasn't understood"),
        ("discogs_bad_body", "Discogs' answer wasn't understood"),
        ("discogs_html_body", "Discogs' answer wasn't understood"),
        ("discogs_server_error", "Discogs' request failed"),
        ("discogs_timeout", "Discogs' request failed"),
        // Not followed: one request, a plain failure.
        ("discogs_redirect", "Discogs' request failed"),
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
    let head = std::fs::read_to_string(fixture("discogs_match").join("001.http")).unwrap();
    let mut big = body("discogs_match").trim_ascii_end().to_vec();
    let close = big.pop().unwrap();
    big.resize(LIMITS.max_bytes + 1, b' ');
    big.push(close);
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&head, big, Vec::new()).unwrap(),
    ]));
    let e = plugin()
        .lookup(&kit(r.clone()), &music(TITLE, Some(1998)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "Discogs' answer wasn't understood");
    r.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_plans_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("discogs_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = plugin().lookup(&k, &music(TITLE, Some(1998))).await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = plugin().lookup(&k, &music("Nothing Like This", None)).await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(
            w[1].0 - w[0].0 >= Duration::from_millis(2400),
            "25 a minute"
        );
    }
    assert!(plugin().min_interval() >= Duration::from_millis(2400));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let p = plugin();
    let m = p.manifest();
    assert_eq!(m.id, "discogs");
    assert_eq!(m.kinds, [Kind::Enrichment]);
    assert_eq!(m.consent, Consent::Account);
    assert!(!m.enabled_by_default && !m.always_adult && !m.finds_private);
    assert!(lodestar_pipeline::identify::admitted(m));
    let hosts: Vec<_> = m.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "api.discogs.com")]);
    assert_eq!(p.endpoints(), Endpoints::manifest(m.endpoints));
    assert_eq!(p.kinds(), [MediaKind::Music]);

    let policy = p.policy();
    assert_eq!(policy.distribution, Distribution::Coordinator);
    let names: Vec<&str> = policy.credentials.iter().map(|c| c.name).collect();
    assert_eq!(names, [discogs::TOKEN]);
    // Account: the token is needed before it can be enabled.
    assert!(matches!(Discogs::need(&[]), Some(Need::Credentials)));
    assert_eq!(Discogs::need(&[discogs::TOKEN.to_owned()]), None);
    assert_eq!(
        need_with(m, policy.credentials, None, &[discogs::TOKEN.to_owned()]),
        None
    );
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.within(&policy.caps));
    // The plan's cap: at most 25 a minute, no burst.
    assert!(limits.per_host.interval >= Duration::from_millis(2400));
    assert_eq!(limits.per_host.burst, 1);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
}

/// The Sources row's text names exactly what leaves, and the credit is
/// Discogs' wording, linked to Discogs.
#[test]
fn what_is_sent_and_the_attribution() {
    for word in ["album", "artist", "token", discogs::HOST] {
        assert!(discogs::SENDS.contains(word), "{word}");
    }
    assert!(discogs::SENDS.contains("Never the info hash"));
    assert_eq!(discogs::ATTRIBUTION, "Data provided by Discogs.");
    assert_eq!(discogs::ATTRIBUTION_URL, "https://www.discogs.com/");
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

/// A bencoded info dict named `name` with ten FLAC tracks.
fn info_dict(name: &str) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"d5:filesl");
    for i in 1..=10 {
        let file = format!("{i:02} Track {i}.flac");
        d.extend_from_slice(b"d6:lengthi31457280e4:pathl");
        d.extend_from_slice(format!("{}:{file}", file.len()).as_bytes());
        d.extend_from_slice(b"ee");
    }
    d.extend_from_slice(b"e4:name");
    d.extend_from_slice(format!("{}:{name}", name.len()).as_bytes());
    let pieces = 10 * 31_457_280 / 262_144;
    d.extend_from_slice(b"12:piece lengthi262144e6:pieces");
    d.extend_from_slice(format!("{}:", 20 * pieces).as_bytes());
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

/// A store with the DHT and the plugin's source registered; the token
/// stored and the source enabled only when `enabled`.
fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(discogs::ID, "1", 0).unwrap();
    if enabled {
        s.update_source_secrets(
            src,
            &[(discogs::TOKEN, b"sealed")],
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
    let (id, _) = put(&mut s, NAME, dht, 0);
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
    let mut private = Vec::new();
    let privacy = [
        flags::PRIVATE_SOURCE,
        flags::PRIVATE,
        flags::REMOTE_UNVERIFIED,
    ];
    for (i, f) in privacy.into_iter().enumerate() {
        let (id, _) = put(
            &mut s,
            &format!("Synthetic Orchestra - Paper Lanterns (1998) [FLAC {i}]"),
            dht,
            f,
        );
        assert_ne!(f & flags::PRIVACY, 0);
        private.push(id);
    }
    let (public, key) = put(&mut s, NAME, dht, 0);
    let replayer = Arc::new(Replayer::load(&fixture("discogs_match")).unwrap());
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
/// plugin's claims; when its title completes a child-safety pattern with
/// the torrent's name, the hash is held and nothing stored.
#[tokio::test]
async fn an_answer_is_judged_and_a_held_one_is_not_stored() {
    let answer = || async {
        let (got, _) = lookup("discogs_match", &music(TITLE, Some(1998))).await;
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
        Some("Paper Lanterns")
    );

    // "orchestra" is a child role here and "lanterns" a sexual term: the
    // torrent's name holds only the first, the answer's title the second.
    // Alone the name is clean; with the answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, "Synthetic Orchestra 1998 FLAC", dht, 0);
    let filter = core_with(&["lanterns"], &["orchestra"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
