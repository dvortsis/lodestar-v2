//! Contract tests for torrent-cache enrichment (SRC-13; §18.9), with no
//! network: a recorded fixture (`fixtures/torrent_cache/`) replayed for
//! the normal lookup, and a fake cache server for the hostile replies.
//! Everything runs on tokio's paused clock, and every wait is a condition
//! with a deadline (`wait_for`), never a fixed sleep.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use lodestar_metainfo::InfoHash;
use lodestar_sources::consent::{self, Ack, Need};
use lodestar_sources::http::{BodyStream, StreamLimits, StreamResponse};
use lodestar_sources::testing::{FakeCtx, Replayer, Running, wait_for};
use lodestar_sources::{
    BoxFuture, Confidence, Consent, Distribution, Fetch, HttpError, Kind, Request, Response,
    SourcePlugin, UserLimits,
};
use lodestar_sources_plugins::torrent_cache::{
    self, CONFIG_SCHEMA, Config, Lookup, Miss, SUGGESTED, TorrentCache, Verified, Why,
};
use lodestar_store::SourceId;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The fixture's key: the SHA-1 of `hit/002.body`'s info dict.
const FIXTURE_KEY: &str = "695b0b3debb14473d741f7a3a86561e42009d6dc";

fn fixture(case: &str) -> Arc<Replayer> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/torrent_cache")
        .join(case);
    Arc::new(Replayer::load(&dir).unwrap())
}

fn key_of(hex: &str) -> InfoHash {
    hex.parse().unwrap()
}

// ---- bencode, for the fake server's files ----

fn bytes(s: &[u8]) -> Vec<u8> {
    let mut v = format!("{}:", s.len()).into_bytes();
    v.extend_from_slice(s);
    v
}

/// A single-file info dict named `name`.
fn info(name: &str, private: bool) -> Vec<u8> {
    let mut v = b"d6:lengthi1048576e4:name".to_vec();
    v.extend(bytes(name.as_bytes()));
    v.extend_from_slice(b"12:piece lengthi262144e6:pieces");
    v.extend(bytes(&[7u8; 80]));
    if private {
        v.extend_from_slice(b"7:privatei1e");
    }
    v.push(b'e');
    v
}

fn torrent_of(info: &[u8], announce: &str) -> Vec<u8> {
    let mut v = b"d8:announce".to_vec();
    v.extend(bytes(announce.as_bytes()));
    v.extend_from_slice(b"4:info");
    v.extend_from_slice(info);
    v.push(b'e');
    v
}

/// The key `info` hashes to, from metainfo's own parser.
fn key_for(info: &[u8]) -> InfoHash {
    lodestar_metainfo::Info::parse(info).unwrap().v1.unwrap()
}

// ---- the fake cache server ----

#[derive(Clone)]
enum Answer {
    File(Vec<u8>),
    Status(u16),
    /// A body larger than the cap, with or without a Content-Length.
    Huge {
        declared: bool,
    },
    /// Headers at once, then a body that never comes.
    Stall,
}

/// Answers by host, and records every request with its clock time.
#[derive(Default)]
struct FakeCache {
    answers: Mutex<HashMap<String, Answer>>,
    requests: Mutex<Vec<(Instant, String)>>,
}

impl FakeCache {
    fn new(answers: &[(&str, Answer)]) -> Arc<FakeCache> {
        Arc::new(FakeCache {
            answers: Mutex::new(
                answers
                    .iter()
                    .map(|(h, a)| ((*h).to_owned(), a.clone()))
                    .collect(),
            ),
            requests: Mutex::default(),
        })
    }

    fn urls(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|(_, u)| u.clone())
            .collect()
    }

    fn answer(&self, req: &Request) -> Answer {
        self.requests
            .lock()
            .unwrap()
            .push((Instant::now(), req.url.clone()));
        let host = req.host().unwrap();
        self.answers
            .lock()
            .unwrap()
            .get(&host)
            .cloned()
            .unwrap_or(Answer::Status(404))
    }
}

fn response(status: u16, headers: &[(&str, String)], body: Vec<u8>) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), v.clone()))
            .collect(),
        body,
    }
}

/// 64 KiB chunks forever.
struct Endless;

impl BodyStream for Endless {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
        Box::pin(async { Ok(Some(vec![b'x'; 64 * 1024])) })
    }
}

/// Never answers.
struct Stalled;

impl BodyStream for Stalled {
    fn next_chunk(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, HttpError>> {
        Box::pin(std::future::pending())
    }
}

impl Fetch for FakeCache {
    fn fetch<'a>(&'a self, req: &'a Request) -> BoxFuture<'a, Result<Response, HttpError>> {
        let a = self.answer(req);
        Box::pin(async move {
            match a {
                Answer::File(b) => Ok(response(200, &[], b)),
                Answer::Status(s) => Ok(response(s, &[], Vec::new())),
                Answer::Huge { .. } | Answer::Stall => {
                    Err(HttpError::Transport("streamed only".into()))
                }
            }
        })
    }

    fn fetch_stream<'a>(
        &'a self,
        req: &'a Request,
        _: StreamLimits,
    ) -> BoxFuture<'a, Result<StreamResponse, HttpError>> {
        let a = self.answer(req);
        Box::pin(async move {
            let streamed = |headers: Vec<(String, String)>, body: Box<dyn BodyStream>| {
                Ok(StreamResponse {
                    status: 200,
                    headers,
                    body,
                })
            };
            match a {
                Answer::File(b) => Ok(StreamResponse::buffered(response(200, &[], b))),
                Answer::Status(s) => Ok(StreamResponse::buffered(response(s, &[], Vec::new()))),
                Answer::Huge { declared: true } => streamed(
                    vec![("Content-Length".into(), (9u64 << 20).to_string())],
                    Box::new(Endless),
                ),
                Answer::Huge { declared: false } => streamed(Vec::new(), Box::new(Endless)),
                Answer::Stall => streamed(Vec::new(), Box::new(Stalled)),
            }
        })
    }
}

// ---- running the plugin ----

struct Run {
    plugin: Arc<TorrentCache>,
    verified: mpsc::Receiver<Verified>,
    running: Running,
    _ctx: FakeCtx,
}

fn start(templates: &[&str], fetch: Arc<dyn Fetch>, before: &[Lookup]) -> Run {
    let (plugin, queue, verified) = TorrentCache::new(Config::new(templates).unwrap());
    for l in before {
        assert!(queue.push(*l));
    }
    let plugin = Arc::new(plugin);
    let mut ctx = FakeCtx::with_fetch(&*plugin, SourceId::new(9).unwrap(), fetch);
    let running = ctx.start(plugin.clone());
    drop(queue);
    Run {
        plugin,
        verified,
        running,
        _ctx: ctx,
    }
}

fn unverified(key: InfoHash) -> Lookup {
    Lookup::new(key, Why::Unverified, false).unwrap()
}

fn n(c: &std::sync::atomic::AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
}

// ---- the normal lookup, from the recorded fixture ----

#[tokio::test(start_paused = true)]
async fn a_verified_file_comes_from_the_second_cache() {
    let replayer = fixture("hit");
    let key = key_of(FIXTURE_KEY);
    let mut run = start(
        &[
            "https://first.example/torrent/{hash}.torrent",
            "https://second.example/get?h={HASH}",
        ],
        replayer.clone(),
        &[unverified(key)],
    );
    let v = run.verified.recv().await.unwrap();
    assert_eq!(v.key(), key);
    assert_eq!(v.why(), Why::Unverified);
    assert!(!v.is_v2());
    assert_eq!(v.host(), "second.example");
    assert_eq!(v.torrent().info.name.decode(), "Sintel.2010.1080p.mkv");

    // Out-of-info fields become claims: the public tracker only (the
    // passkey-shaped path and the query are dropped), and the comment.
    let c = v.claims();
    assert_eq!(c.trackers, ["udp://tracker.public.example:1337/announce"]);
    assert_eq!(
        c.description.as_deref(),
        Some("Sintel, an open movie by the Blender Foundation (CC BY 3.0)")
    );
    let p = v.provenance();
    assert_eq!(p.published_at, Some(1_285_977_600));
    assert_eq!(p.confidence, Confidence::Medium);
    assert_eq!(p.external_ref, None);
    assert_eq!(torrent_cache::PROVENANCE, "torrent cache");

    replayer.finish().unwrap();
    assert_eq!(n(&run.plugin.stats().verified), 1);
    run.running.stop_within().await.unwrap();
}

// ---- the hash check ----

#[tokio::test(start_paused = true)]
async fn a_file_that_hashes_to_another_key_is_refused() {
    let wanted = InfoHash([0x11; 20]);
    let other = torrent_of(&info("Big Buck Bunny", false), "udp://t.example:1/announce");
    let fake = FakeCache::new(&[
        ("liar.example", Answer::File(other)),
        ("empty.example", Answer::Status(404)),
    ]);
    let mut run = start(
        &[
            "https://liar.example/{hash}.torrent",
            "https://empty.example/{hash}.torrent",
        ],
        fake.clone(),
        &[unverified(wanted)],
    );
    let stats = &run.plugin.stats();
    wait_for("both caches asked", || fake.urls().len() >= 2).await;
    wait_for("the lookup to end", || n(&stats.missed) == 1).await;
    assert_eq!(n(&stats.mismatched), 1);
    assert_eq!(n(&stats.verified), 0);
    assert!(
        run.verified.try_recv().is_err(),
        "a mismatched file was used"
    );
    run.running.stop_within().await.unwrap();
}

#[tokio::test]
async fn verify_accepts_only_the_keys_hash() {
    let raw = info("Sintel", false);
    let file =
        lodestar_metainfo::TorrentFile::parse(&torrent_of(&raw, "udp://t.example:1/a")).unwrap();
    assert_eq!(torrent_cache::verify(&file, &key_for(&raw)), Some(false));
    assert_eq!(torrent_cache::verify(&file, &InfoHash([0; 20])), None);
}

// ---- caps on bytes and time ----

async fn capped(answer: Answer) {
    let key = InfoHash([0x22; 20]);
    let fake = FakeCache::new(&[("big.example", answer)]);
    let mut run = start(
        &["https://big.example/{hash}.torrent"],
        fake.clone(),
        &[unverified(key)],
    );
    let stats = &run.plugin.stats();
    wait_for("the capped reply to fail", || n(&stats.failed) == 1).await;
    assert_eq!(n(&stats.verified), 0);
    assert!(run.verified.try_recv().is_err());
    run.running.stop_within().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_declared_oversize_reply_is_refused() {
    capped(Answer::Huge { declared: true }).await;
}

#[tokio::test(start_paused = true)]
async fn an_endless_reply_stops_at_the_byte_cap() {
    capped(Answer::Huge { declared: false }).await;
}

#[tokio::test(start_paused = true)]
async fn a_stalled_reply_times_out() {
    let started = Instant::now();
    capped(Answer::Stall).await;
    // The idle cap, not the whole timeout, ended it.
    assert!(started.elapsed() >= torrent_cache::STREAM.idle);
    assert!(started.elapsed() < torrent_cache::STREAM.timeout);
}

#[test]
fn caps_are_within_the_kits() {
    let s = torrent_cache::STREAM;
    assert_eq!(s, s.clamped());
    assert_eq!(s.max_bytes, 8 << 20);
    assert_eq!(torrent_cache::LIMITS.max_torrent_bytes, 8 << 20);
}

// ---- private_source keys never leave ----

#[test]
fn no_lookup_exists_for_a_private_key() {
    let key = InfoHash([0x33; 20]);
    assert!(Lookup::new(key, Why::Unverified, true).is_none());
    assert!(Lookup::new(key, Why::Interest, true).is_none());
    assert!(Lookup::new(key, Why::Interest, false).is_some());
}

#[tokio::test(start_paused = true)]
async fn only_queued_public_keys_are_requested() {
    // The host can hand over only what `Lookup::new` gives it: a private
    // key turns into nothing, so no request is ever built for it.
    let private = InfoHash([0x44; 20]);
    let public = InfoHash([0x55; 20]);
    let fake = FakeCache::new(&[]);
    let lookups: Vec<Lookup> = [(private, true), (public, false)]
        .into_iter()
        .filter_map(|(k, p)| Lookup::new(k, Why::Unverified, p))
        .collect();
    let run = start(&["https://c.example/{hash}"], fake.clone(), &lookups);
    wait_for("the public lookup", || !fake.urls().is_empty()).await;
    wait_for("it to end", || n(&run.plugin.stats().missed) == 1).await;
    let urls = fake.urls();
    assert_eq!(urls, [format!("https://c.example/{public}")]);
    assert!(!urls.iter().any(|u| u.contains(&private.to_string())));
    run.running.stop_within().await.unwrap();
}

// ---- consent and setup ----

#[test]
fn manifest_is_external_consent_and_off_by_default() {
    let (p, _, _) = TorrentCache::new(Config::default());
    let m = p.manifest();
    assert_eq!(m.id, "torrent_cache");
    assert_eq!(m.consent, Consent::External);
    assert!(!m.enabled_by_default);
    assert!(m.is(Kind::Enrichment));
    assert!(m.endpoints.is_empty());
    assert_eq!(p.policy().distribution, Distribution::Coordinator);
    // SRC-4's gate: enabling needs the endpoint list acknowledged.
    assert_eq!(consent::need(m, None, &[]), Some(Need::Endpoints));
    assert_eq!(consent::need(m, Some(&Ack::of(m)), &[]), None);
    // And nothing may be reached until a cache is listed.
    assert!(p.endpoints().is_none());
}

#[test]
fn policy_limits_are_within_the_floors() {
    let (p, _, _) = TorrentCache::new(Config::default());
    let caps = p.policy().caps;
    let limits = p.policy().limits(&UserLimits::default());
    assert!(limits.within(&caps));
    assert!(limits.poll_interval >= std::time::Duration::from_secs(60));
    assert!(limits.per_host.interval >= std::time::Duration::from_secs(10));
}

#[tokio::test(start_paused = true)]
async fn an_empty_list_fails_before_any_request() {
    let fake = FakeCache::new(&[]);
    let (plugin, queue, _rx) = TorrentCache::new(Config::default());
    assert!(queue.push(unverified(InfoHash([1; 20]))));
    let plugin = Arc::new(plugin);
    let mut ctx = FakeCtx::with_fetch(&*plugin, SourceId::new(9).unwrap(), fake.clone());
    let err = plugin.run(ctx.take()).await.unwrap_err();
    assert!(err.0.starts_with("needs setup"), "{err}");
    assert!(fake.urls().is_empty());
}

#[test]
fn the_list_starts_empty_and_suggestions_are_not_enabled() {
    let schema: serde_json::Value = serde_json::from_str(CONFIG_SCHEMA).unwrap();
    let caches = &schema["properties"]["caches"];
    assert_eq!(caches["default"], serde_json::json!([]));
    assert_eq!(caches["maxItems"], 8);
    assert!(
        Config::from_json(r#"{"caches":[]}"#)
            .unwrap()
            .caches()
            .is_empty()
    );
    // The form's examples are exactly the suggestions, each valid.
    let templates: Vec<&str> = SUGGESTED.iter().map(|s| s.template).collect();
    let examples = if templates.is_empty() {
        serde_json::json!([])
    } else {
        serde_json::json!([templates])
    };
    assert_eq!(caches["examples"], examples);
    for t in templates {
        Config::new(&[t]).unwrap();
    }
}

#[test]
fn config_refusals_name_the_rule() {
    let bad = [
        ("http://c.example/{hash}", "https"),
        ("https://c.example/torrent", "exactly once"),
        ("https://c.example/{hash}/{HASH}", "exactly once"),
        ("https://{hash}.example/t", "path or the query"),
        ("https://user:pw@c.example/{hash}", "user name"),
        ("https://c.example/{hash}#x", "fragment"),
        ("https://192.168.1.10/{hash}", "public"),
        ("https://127.0.0.1/{hash}", "public"),
        ("not a url {hash}", "valid address"),
    ];
    for (t, rule) in bad {
        let e = Config::new(&[t]).unwrap_err();
        assert!(e.0.contains(rule), "{t}: {e}");
        assert!(!e.0.contains("c.example"), "the error holds the value");
    }
    let nine: Vec<String> = (0..9)
        .map(|i| format!("https://c{i}.example/{{hash}}"))
        .collect();
    assert!(Config::new(&nine).is_err());
    assert!(Config::new(&["https://c.example/{hash}", "https://c.example/{hash}"]).is_err());
    assert!(Config::from_json(r#"{"caches":[],"extra":1}"#).is_err());
    let long = format!("https://c.example/{}{{hash}}", "a".repeat(600));
    assert!(Config::new(&[long]).is_err());
}

#[test]
fn endpoints_are_the_listed_hosts() {
    let (p, _, _) = TorrentCache::new(
        Config::new(&["https://a.example/{hash}", "https://b.example/x?h={HASH}"]).unwrap(),
    );
    assert!(p.endpoints().is_some());
    let hosts: Vec<&str> = p.config().caches().iter().map(|c| c.host()).collect();
    assert_eq!(hosts, ["a.example", "b.example"]);
}

// ---- order, claims and stopping ----

#[tokio::test(start_paused = true)]
async fn interest_is_looked_up_before_unverified_items() {
    let fake = FakeCache::new(&[]);
    let (u, i) = (InfoHash([0x66; 20]), InfoHash([0x77; 20]));
    let run = start(
        &["https://c.example/{hash}"],
        fake.clone(),
        &[unverified(u), Lookup::new(i, Why::Interest, false).unwrap()],
    );
    wait_for("both lookups", || fake.urls().len() >= 2).await;
    let urls = fake.urls();
    assert_eq!(urls[0], format!("https://c.example/{i}"));
    assert_eq!(urls[1], format!("https://c.example/{u}"));
    // One cache, one lookup a minute.
    let times: Vec<Instant> = fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(t, _)| *t)
        .collect();
    assert!(times[1] - times[0] >= std::time::Duration::from_secs(60));
    run.running.stop_within().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_private_torrents_trackers_are_not_kept() {
    let raw = info("Elephants Dream", true);
    let key = key_for(&raw);
    let fake = FakeCache::new(&[(
        "c.example",
        Answer::File(torrent_of(&raw, "https://tracker.example/announce")),
    )]);
    let mut run = start(&["https://c.example/{hash}"], fake, &[unverified(key)]);
    let v = run.verified.recv().await.unwrap();
    assert!(v.claims().trackers.is_empty());
    run.running.stop_within().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_full_lane_drops_and_counts() {
    let fake = FakeCache::new(&[]);
    let (plugin, queue, _rx) =
        TorrentCache::new(Config::new(&["https://c.example/{hash}"]).unwrap());
    // Not running: nothing drains the lane.
    for i in 0..torrent_cache::QUEUE {
        assert!(queue.push(unverified(InfoHash([i as u8; 20]))));
    }
    assert!(!queue.push(unverified(InfoHash([0xff; 20]))));
    assert_eq!(n(&plugin.stats().dropped), 1);
    assert!(fake.urls().is_empty());
}

#[tokio::test(start_paused = true)]
async fn it_stops_mid_lookup() {
    let fake = FakeCache::new(&[("c.example", Answer::Stall)]);
    let run = start(
        &["https://c.example/{hash}"],
        fake.clone(),
        &[unverified(InfoHash([9; 20]))],
    );
    wait_for("the stalled request", || !fake.urls().is_empty()).await;
    run.running.stop_within().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_egress_being_down_ends_a_lookup_without_asking_further() {
    // `NoEgress` stands for a route that's down: nothing is sent.
    let (plugin, queue, _rx) = TorrentCache::new(
        Config::new(&["https://a.example/{hash}", "https://b.example/{hash}"]).unwrap(),
    );
    let plugin = Arc::new(plugin);
    let mut ctx = FakeCtx::new(&*plugin, SourceId::new(9).unwrap());
    let ctx_ = ctx.take();
    let r = plugin
        .lookup(&ctx_.kit.http, &unverified(InfoHash([3; 20])))
        .await;
    assert_eq!(r.unwrap_err(), Miss::EgressDown);
    assert_eq!(n(&plugin.stats().failed), 1);
    drop(queue);
}

proptest::proptest! {
    /// The template parser never panics, and anything it accepts builds an
    /// https address on its own host for every key.
    #[test]
    fn templates_never_panic(t in "(https?://)?[a-z0-9.:@/?#{}A-Z%-]{0,80}", k in proptest::array::uniform20(0u8..)) {
        if let Ok(c) = Config::new(&[t.as_str()]) {
            let cache = &c.caches()[0];
            let key = InfoHash(k);
            let lookup = unverified(key);
            let req = TorrentCache::request(cache, &lookup);
            proptest::prop_assert!(req.url.starts_with("https://"));
            let host = req.host();
            proptest::prop_assert_eq!(host.as_deref(), Some(cache.host()));
            proptest::prop_assert!(req.url.to_ascii_lowercase().contains(&key.to_string()));
        }
    }

    /// Arbitrary bytes never verify for a key they don't hash to.
    #[test]
    fn random_bytes_never_verify(body in proptest::collection::vec(0u8.., 0..512)) {
        if let Ok(f) = lodestar_metainfo::TorrentFile::parse_with(&body, &torrent_cache::LIMITS) {
            proptest::prop_assert_eq!(torrent_cache::verify(&f, &InfoHash([0xa5; 20])), None);
        }
    }
}

// ---- v2 and hybrid keys (Q8: SHA-1 or the v2 hash) ----

/// A one-file v2 info dict (BEP 52), or a hybrid one with a v1 layout
/// too.
fn v2_info(hybrid: bool) -> Vec<u8> {
    let mut v = b"d9:file treed8:film.mkvd0:d6:lengthi70000e11:pieces root".to_vec();
    v.extend(bytes(&[2u8; 32]));
    v.extend_from_slice(b"eee");
    if hybrid {
        v.extend_from_slice(b"6:lengthi70000e");
    }
    v.extend_from_slice(b"12:meta versioni2e4:name8:film.mkv12:piece lengthi16384e");
    if hybrid {
        v.extend_from_slice(b"6:pieces");
        v.extend(bytes(&[5u8; 100]));
    }
    v.push(b'e');
    v
}

fn parsed(info: &[u8]) -> (lodestar_metainfo::TorrentFile, lodestar_metainfo::Info) {
    let file =
        lodestar_metainfo::TorrentFile::parse(&torrent_of(info, "udp://t.example:1/a")).unwrap();
    (file, lodestar_metainfo::Info::parse(info).unwrap())
}

#[test]
fn a_v2_key_is_the_truncated_sha256() {
    let (file, info) = parsed(&v2_info(false));
    assert!(info.v1.is_none());
    let full = info.v2.unwrap();
    assert_eq!(torrent_cache::verify(&file, &full.truncated()), Some(true));
    // The other end of the SHA-256 isn't the key.
    let mut tail = [0u8; 20];
    tail.copy_from_slice(&full.0[12..]);
    assert_eq!(torrent_cache::verify(&file, &InfoHash(tail)), None);
}

#[test]
fn a_hybrid_verifies_under_either_key() {
    let (file, info) = parsed(&v2_info(true));
    let (v1, v2) = (info.v1.unwrap(), info.v2.unwrap().truncated());
    assert_ne!(v1, v2);
    assert_eq!(torrent_cache::verify(&file, &v1), Some(false));
    assert_eq!(torrent_cache::verify(&file, &v2), Some(true));
}

#[tokio::test(start_paused = true)]
async fn a_v2_file_is_verified_and_marked_v2() {
    let raw = v2_info(false);
    let key = parsed(&raw).1.v2.unwrap().truncated();
    let fake = FakeCache::new(&[(
        "c.example",
        Answer::File(torrent_of(&raw, "udp://t.example:1/a")),
    )]);
    let mut run = start(&["https://c.example/{hash}"], fake, &[unverified(key)]);
    let v = run.verified.recv().await.unwrap();
    assert_eq!(v.key(), key);
    assert!(v.is_v2());
    run.running.stop_within().await.unwrap();
}

#[test]
fn debug_never_shows_the_key() {
    let key = InfoHash([0xcd; 20]);
    let shown = format!("{:?}", unverified(key));
    assert!(!shown.contains(&key.to_string()), "{shown}");
}
