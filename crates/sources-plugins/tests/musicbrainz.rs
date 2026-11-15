//! Contract tests for the MusicBrainz plugin (ENR-mb, §18.9), on the
//! fixtures in `fixtures/musicbrainz/` (see its `README.md`), with no
//! network. Two parts:
//!
//! - the plugin on its own: a match, no match, several candidates, the
//!   request it sends, errors (503 back-off, a timeout, a redirect, a
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
use lodestar_sources_feeds::musicbrainz::SEARCH_LIMITS;
use lodestar_sources_plugins::musicbrainz::{self, MusicBrainz, Wanted};
use lodestar_store::{NewTorrent, SourceId, Store, StoreConfig, flags};
use proptest::prelude::*;
use tokio::sync::mpsc;

use harness::{core_with, n, until};

const TITLE: &str = "The Quiet Engines - Harbour Lights";
const NAME: &str = "The Quiet Engines - Harbour Lights (2004) [FLAC]";
const ALBUM_ID: &str = "a1000000-0000-4000-8000-e00000000001";
const T: i64 = 1_700_000_000;

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/musicbrainz")
        .join(case)
}

fn kit(fetch: Arc<dyn Fetch>) -> Kit {
    Kit::new(
        Arc::new(MemoryState::new()),
        fetch,
        MusicBrainz::new().policy(),
        &UserLimits::default(),
    )
}

fn music(title: &str, year: Option<u16>) -> Query {
    Query::new(title, year, MediaKind::Music).unwrap()
}

async fn lookup(case: &str, q: &Query) -> (Result<Lookup, String>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let got = MusicBrainz::new().lookup(&kit(replayer.clone()), q).await;
    (got.map_err(|e| e.0), replayer)
}

fn matched(got: Result<Lookup, String>) -> lodestar_sources::identify::Identification {
    match got {
        Ok(Lookup::Match(i)) => i,
        other => panic!("a match, got {other:?}"),
    }
}

// ---- Answers ----

#[tokio::test(start_paused = true)]
async fn a_recorded_match_maps_to_an_identification() {
    let (got, replayer) = lookup("musicbrainz_match", &music(TITLE, Some(2004))).await;
    replayer.finish().unwrap();
    let i = matched(got);
    let ids: Vec<(&str, &str)> = i
        .ids
        .iter()
        .map(|x| (x.scheme.as_str(), x.value.as_str()))
        .collect();
    assert_eq!(ids, [("mbid", ALBUM_ID)]);
    assert_eq!(i.canonical_title.as_deref(), Some("Harbour Lights"));
    assert!(i.alt_titles.is_empty() && i.genres.is_empty());
    assert_eq!(
        (i.year, i.confidence),
        (Some(2004), musicbrainz::CONFIDENCE_ARTIST_YEAR)
    );
    // Nothing of the tags, comments, sort names or aliases.
    let claims = i.clone().into_claims();
    assert!(!format!("{claims:?}{:?}", i.canonical_title).contains("SENTINEL"));
    // Its claims survive the clamp unchanged: already in stored form.
    assert_eq!(claims.ids.len(), 1);
    let d = claims.details.as_deref().unwrap();
    assert_eq!(d.canonical_title.as_deref(), Some("Harbour Lights"));
    assert_eq!(d.match_confidence, Some(90));
}

#[tokio::test(start_paused = true)]
async fn the_artist_must_agree() {
    // The same album title by another credit, a credit that only starts
    // with the artist, or no year: the artist decides.
    let (got, _) = lookup("musicbrainz_match", &music(TITLE, None)).await;
    let i = matched(got);
    assert_eq!(i.ids[0].value, ALBUM_ID);
    assert_eq!(
        (i.year, i.confidence),
        (Some(2004), musicbrainz::CONFIDENCE_ARTIST)
    );
    // Any credited name, or the whole credit as printed, agrees; a part
    // of a name doesn't.
    let s = lodestar_sources_feeds::musicbrainz::parse_search(
        &std::fs::read(fixture("musicbrainz_match").join("001.body")).unwrap(),
        &SEARCH_LIMITS,
    );
    let hits: Vec<_> = s.hits.iter().collect();
    for (title, want) in [
        ("Marla Venn - Harbour Lights", Some(1)),
        (
            "Marla Venn & The Quiet Engines Orchestra - Harbour Lights",
            Some(1),
        ),
        ("The Quiet Engines Orchestra - Harbour Lights", Some(1)),
        ("The Quiet Engines - Harbour Lights", Some(0)),
        ("Someone Else - Harbour Lights", None),
        ("The Quiet - Harbour Lights", None),
        ("The Quiet Engines - Harbour", None),
    ] {
        let got = match musicbrainz::choose(&music(title, None), &hits) {
            Lookup::Match(i) => s.hits.iter().position(|h| h.id.as_str() == i.ids[0].value),
            Lookup::NoMatch => None,
        };
        assert_eq!(got, want, "{title}");
    }
    // Only Music is answered.
    let film = Query::new(TITLE, Some(2004), MediaKind::Movie).unwrap();
    assert_eq!(musicbrainz::choose(&film, &hits), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn the_year_and_the_album_type_pick_among_candidates() {
    // 1999: one album called Echoes.
    let (got, r) = lookup("musicbrainz_candidates", &music("Echoes", Some(1999))).await;
    r.finish().unwrap();
    let i = matched(got);
    assert_eq!(i.ids[0].value, "e1000000-0000-4000-8000-e00000000001");
    assert_eq!(i.confidence, musicbrainz::CONFIDENCE_YEAR);
    // 2010: an album and a single; the album.
    let (got, _) = lookup("musicbrainz_candidates", &music("Echoes", Some(2010))).await;
    assert_eq!(
        matched(got).ids[0].value,
        "e1000000-0000-4000-8000-e00000000002"
    );
    // No year: two albums agree, so it doesn't guess.
    let (got, _) = lookup("musicbrainz_candidates", &music("Echoes", None)).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    // A year none has.
    let (got, _) = lookup("musicbrainz_candidates", &music("Echoes", Some(2001))).await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
}

#[tokio::test(start_paused = true)]
async fn no_result_is_no_match() {
    let (got, replayer) = lookup(
        "musicbrainz_no_match",
        &music("Nobody Here - Nothing Like This", None),
    )
    .await;
    replayer.finish().unwrap();
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(replayer.requests().len(), 1);
}

#[test]
fn titles_split_and_compare_folded() {
    assert_eq!(
        musicbrainz::wanted(TITLE),
        Some(Wanted {
            artist: Some("The Quiet Engines"),
            album: "Harbour Lights"
        })
    );
    // The first ` - ` splits; a side with nothing to compare is no split.
    assert_eq!(
        musicbrainz::wanted("A - B - C").map(|w| (w.artist, w.album)),
        Some((Some("A"), "B - C"))
    );
    assert_eq!(
        musicbrainz::wanted("!!! - Album").map(|w| (w.artist, w.album)),
        Some((None, "!!! - Album"))
    );
    assert_eq!(musicbrainz::wanted("Echoes").map(|w| w.artist), Some(None));
    assert_eq!(musicbrainz::wanted("..."), None);
    assert_eq!(musicbrainz::fold("  Harbour: LIGHTS! "), "harbour lights");
    assert_eq!(musicbrainz::fold("Sigur Rós"), "sigur rós");
}

// ---- What is sent ----

fn params(req: &Request) -> Vec<(String, String)> {
    let url = url::Url::parse(&req.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(musicbrainz::HOST));
    assert_eq!(url.path(), musicbrainz::PATH);
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_request_carries_the_title_and_fixed_parameters_only() {
    let (_, replayer) = lookup("musicbrainz_match", &music(TITLE, Some(2004))).await;
    let seen = replayer.requests();
    assert_eq!(seen.len(), 1);
    let (_, req) = &seen[0];
    let want: Vec<(String, String)> = [
        (
            "query",
            r#"releasegroup:"Harbour Lights" AND artist:"The Quiet Engines""#,
        ),
        ("limit", "5"),
        ("fmt", "json"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    assert_eq!(
        params(req),
        want,
        "the title, never the year, a hash or a file"
    );
    assert!(req.body.is_empty());
    let names: Vec<&str> = req.headers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Accept", "User-Agent"], "no other header");
    let ua = req.headers.iter().find(|(n, _)| n == "User-Agent");
    assert_eq!(ua.map(|(_, v)| v.as_str()), Some(musicbrainz::USER_AGENT));
}

#[test]
fn the_user_agent_names_the_project_only() {
    let ua = musicbrainz::USER_AGENT;
    assert!(ua.starts_with("Lodestar/"), "{ua}");
    assert!(ua.ends_with(" )"), "{ua}");
    assert!(ua.contains("( https://github.com/"), "a project URL: {ua}");
    assert!(!ua.contains('@'), "no email: {ua}");
    for word in ["localhost", "home", "user"] {
        assert!(!ua.to_lowercase().contains(word), "{word} in {ua}");
    }
}

/// Text that would add a parameter, close the phrase, add a field or an
/// operator stays one encoded parameter and one phrase.
#[test]
fn a_hostile_title_is_one_phrase_in_one_parameter() {
    let title = r#"x" OR artist:* - y&query=z\" AND (rgid:1)"#;
    let want = musicbrainz::wanted(title).unwrap();
    let req = musicbrainz::search_request(&want).unwrap();
    let p = params(&req);
    let queries: Vec<&str> = p
        .iter()
        .filter(|(k, _)| k == "query")
        .map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(queries.len(), 1);
    assert_eq!(p.len(), 3);
    assert_eq!(
        phrases(queries[0]),
        [
            (
                "releasegroup:".to_owned(),
                r#"y&query=z\" AND (rgid:1)"#.to_owned()
            ),
            (" AND artist:".to_owned(), r#"x" OR artist:*"#.to_owned()),
        ]
    );
}

/// Splits a query into `(text outside, phrase content unescaped)` pairs,
/// reading it as Lucene does: a backslash escapes the next character, an
/// unescaped `"` opens or closes a phrase. Panics on an unclosed phrase
/// or an escape outside one.
fn phrases(q: &str) -> Vec<(String, String)> {
    let (mut out, mut outside, mut inside) = (Vec::new(), String::new(), None::<String>);
    let mut chars = q.chars();
    while let Some(c) = chars.next() {
        match (&mut inside, c) {
            (Some(p), '\\') => p.push(chars.next().expect("an escape at the end")),
            (Some(p), '"') => {
                out.push((std::mem::take(&mut outside), std::mem::take(p)));
                inside = None;
            }
            (Some(p), c) => p.push(c),
            (None, '"') => inside = Some(String::new()),
            (None, '\\') => panic!("an escape outside a phrase"),
            (None, c) => outside.push(c),
        }
    }
    assert!(inside.is_none(), "an unclosed phrase in {q}");
    assert!(outside.is_empty(), "text after the last phrase in {q}");
    out
}

proptest! {
    /// Whatever the title, the query is the fixed template with the
    /// album (and artist) each one phrase holding exactly its text, and
    /// nothing but the template outside them.
    #[test]
    fn any_title_is_one_phrase_each(title in "\\PC{1,80}") {
        let Some(q) = Query::new(&title, None, MediaKind::Music) else { return Ok(()) };
        let Some(want) = musicbrainz::wanted(q.title()) else { return Ok(()) };
        let got = phrases(&musicbrainz::lucene(&want));
        let mut expect = vec![("releasegroup:".to_owned(), want.album.to_owned())];
        if let Some(a) = want.artist {
            expect.push((" AND artist:".to_owned(), a.to_owned()));
        }
        prop_assert_eq!(got, expect);
    }
}

// ---- Errors ----

#[tokio::test(start_paused = true)]
async fn a_503_backs_off_and_the_next_lookup_waits_without_a_request() {
    let replayer = Arc::new(Replayer::load(&fixture("musicbrainz_rate_limited")).unwrap());
    let k = kit(replayer.clone());
    let q = music(TITLE, Some(2004));
    let e = MusicBrainz::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(e, musicbrainz::RATE_LIMITED);
    // Held by the limiter (ENR-10's client): the retry sends nothing.
    let again = MusicBrainz::new().lookup(&k, &q).await.unwrap_err().0;
    assert_eq!(again, musicbrainz::HELD);
    assert_eq!(replayer.requests().len(), 1);
    replayer.finish().unwrap();
    tokio::time::advance(Duration::from_secs(6 * 3600 + 1)).await;
    let later = MusicBrainz::new().lookup(&k, &q).await.unwrap_err().0;
    assert_ne!(later, musicbrainz::HELD, "{later}");
    assert_eq!(replayer.requests().len(), 2, "the host was asked again");
}

/// An error object on a 200, which the host limiter doesn't see: the
/// plugin holds itself for the answer's `Retry-After` (5 s).
#[tokio::test(start_paused = true)]
async fn an_api_error_holds_for_its_retry_after() {
    let replayer = Arc::new(Replayer::load(&fixture("musicbrainz_error_then_match")).unwrap());
    let k = kit(replayer.clone());
    let m = MusicBrainz::new();
    let q = music(TITLE, Some(2004));
    assert_eq!(
        m.lookup(&k, &q).await.unwrap_err().0,
        musicbrainz::API_ERROR
    );
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(m.lookup(&k, &q).await.unwrap_err().0, musicbrainz::HELD);
    assert_eq!(replayer.requests().len(), 1, "nothing sent while held");
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(m.lookup(&k, &q).await, Ok(Lookup::Match(_))));
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn errors_are_never_a_miss_and_hold_no_service_text() {
    let q = music(TITLE, Some(2004));
    for (case, want) in [
        ("musicbrainz_error", musicbrainz::API_ERROR),
        (
            "musicbrainz_bad_body",
            "MusicBrainz's answer wasn't understood",
        ),
        (
            "musicbrainz_html_body",
            "MusicBrainz's answer wasn't understood",
        ),
        ("musicbrainz_server_error", "MusicBrainz's request failed"),
        ("musicbrainz_timeout", "MusicBrainz's request failed"),
        // Not followed: one request, a plain failure.
        ("musicbrainz_redirect", "MusicBrainz's request failed"),
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
    let dir = fixture("musicbrainz_match");
    let mut body = std::fs::read(dir.join("001.body")).unwrap();
    let head = std::fs::read_to_string(dir.join("001.http")).unwrap();
    // Whitespace before the closing brace keeps it valid JSON.
    while body.last() != Some(&b'}') {
        body.pop();
    }
    body.pop();
    body.resize(SEARCH_LIMITS.max_bytes + 1, b' ');
    body.push(b'}');
    let r = Arc::new(Replayer::new(vec![
        parse_exchange(&head, body, Vec::new()).unwrap(),
    ]));
    let e = MusicBrainz::new()
        .lookup(&kit(r.clone()), &music(TITLE, Some(2004)))
        .await
        .unwrap_err()
        .0;
    assert_eq!(e, "MusicBrainz's answer wasn't understood");
    r.finish().unwrap();
}

/// A title that names no album is no match, and nothing is sent.
#[tokio::test(start_paused = true)]
async fn a_title_of_punctuation_sends_nothing() {
    let wire = Arc::new(Silent::default());
    let got = MusicBrainz::new()
        .lookup(&kit(wire.clone()), &music("... - !!!", None))
        .await;
    assert_eq!(got.unwrap(), Lookup::NoMatch);
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn lookups_are_paced_at_the_host_rate() {
    let replayer = Arc::new(Replayer::load(&fixture("musicbrainz_two_lookups")).unwrap());
    let k = kit(replayer.clone());
    let first = MusicBrainz::new()
        .lookup(&k, &music(TITLE, Some(2004)))
        .await;
    assert!(matches!(first, Ok(Lookup::Match(_))));
    let second = MusicBrainz::new()
        .lookup(&k, &music("Nobody Here - Nothing Like This", None))
        .await;
    assert_eq!(second.unwrap(), Lookup::NoMatch);
    replayer.finish().unwrap();
    let seen = replayer.requests();
    for w in seen.windows(2) {
        assert!(w[1].0 - w[0].0 >= Duration::from_secs(1), "one a second");
    }
    assert!(MusicBrainz::new().min_interval() >= Duration::from_secs(1));
}

// ---- The manifest ----

#[test]
fn manifest_and_policy() {
    let m = MusicBrainz::new();
    let man = m.manifest();
    assert_eq!(man.id, "musicbrainz");
    assert_eq!(man.kinds, [Kind::Enrichment]);
    assert_eq!(man.consent, Consent::External);
    assert!(!man.enabled_by_default && !man.always_adult && !man.finds_private);
    assert!(lodestar_pipeline::identify::admitted(man));
    let hosts: Vec<_> = man.endpoints.iter().map(|e| (e.protocol, e.host)).collect();
    assert_eq!(hosts, [("https", "musicbrainz.org")]);
    assert_eq!(m.endpoints(), Endpoints::manifest(man.endpoints));
    assert_eq!(m.kinds(), [MediaKind::Music]);
    // External: nothing goes until the endpoint is acknowledged.
    assert!(need(man, None, &[]).is_some());
    assert_eq!(need(man, Some(&Ack::of(man)), &[]), None);

    let policy = m.policy();
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

/// A bencoded info dict named `name` with ten 30 MB FLAC tracks.
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

/// A store with the DHT and the plugin's source registered; the plugin's
/// enabled with its endpoint acknowledged only when `enabled`.
fn store(enabled: bool) -> (Store, SourceId, SourceId) {
    let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
    let dht = s.register_source("dht", "1", 0).unwrap();
    let src = s.register_source(musicbrainz::ID, "1", 0).unwrap();
    if enabled {
        let ack = Ack::of(MusicBrainz::new().manifest()).encode();
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
        Arc::new(MusicBrainz::new()),
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
        HostLimiter::new(MusicBrainz::new().policy().limits(&UserLimits::default())),
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
    let (id, _) = put(&mut s, NAME, dht, 0);
    let wire = Arc::new(Silent::default());
    let ran = run(s, wire.clone(), src, &[id], |st| n(&st.deferred) >= 1).await;
    assert_eq!(wire.0.load(Ordering::SeqCst), 0);
    assert_eq!(n(&ran.stats.looked_up), 0);
    assert!(ran.found.is_empty());
}

/// A private item is never sent, even opened first; the public one is
/// looked up (exactly the recorded request) and its answer emitted under
/// the key.
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
            &format!("The Quiet Engines - Harbour Lights (2004) [FLAC {i}]"),
            dht,
            f,
        );
        assert_ne!(f & flags::PRIVACY, 0);
        private.push(id);
    }
    let (public, key) = put(&mut s, NAME, dht, 0);
    let replayer = Arc::new(Replayer::load(&fixture("musicbrainz_match")).unwrap());
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
        let (got, _) = lookup("musicbrainz_match", &music(TITLE, Some(2004))).await;
        matched(got)
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
        Some("Harbour Lights")
    );

    // "engines" is a child role here and "lights" a sexual term: the
    // torrent's name holds only the first, the answer's title the second.
    // Alone the name is clean; with the answer it's a hit.
    let (mut s, dht, src) = store(true);
    let (_, key) = put(&mut s, "The Quiet Engines 2004 FLAC", dht, 0);
    let filter = core_with(&["lights"], &["engines"]);
    let st = ingest(&mut s, &filter, &[discovery(key, src, answer().await)], src);
    assert_eq!(n(&st.held), 1);
    assert!(s.held_among(&[key.0]).unwrap()[0], "held");
    assert!(s.find(&key).unwrap().is_none(), "nothing stored");
}
