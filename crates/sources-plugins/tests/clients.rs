//! Contract tests for the download-clients plugin (§18.9), on hand-written
//! fixtures in `fixtures/clients/` (`make_fixtures.py` writes them), with
//! no network and no fixed sleeps. They follow SRC-7's pattern
//! (`README.md`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use lodestar_integrations::clients::{BasicAuth, ClientConfig, Listed};
use lodestar_integrations::secret::Secret;
use lodestar_metainfo::InfoHash;
use lodestar_sources::testing::{FakeCtx, Replayer, wait_for};
use lodestar_sources::{Consent, Discovery, Distribution, SourcePlugin, UserLimits, floors};
use lodestar_sources_plugins::clients::{
    self, ClientKind, Clients, MAX_LIST_BYTES, MAX_LISTED, MAX_LISTED_NAME, MAX_SIZE,
    STRATEGY_EXPORT, STRATEGY_LIST, TorrentState, parse_list, to_found,
};
use lodestar_store::SourceId;
use proptest::prelude::*;

const POLL: Duration = Duration::from_secs(60);
/// The key of the fixtures' real `.torrent` (Big Buck Bunny).
const BUNNY: &str = "fb9ff1ef824d61bd83b24fb91811804db71c0c53";
const SINTEL: &str = "00112233445566778899aabbccddeeff00112233";
const DREAM: &str = "abcdefabcdefabcdefabcdefabcdefabcdef0123";
const TEARS: &str = "0123456789abcdef0123456789abcdef01234567";

fn fixture(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/clients")
        .join(case)
}

fn hash(hex: &str) -> InfoHash {
    hex.parse().unwrap()
}

fn qbittorrent() -> Arc<Clients> {
    Arc::new(Clients::new(ClientConfig::QBittorrent {
        url: "http://qbittorrent.example:8080/".into(),
        username: "REDACTED".into(),
        password: Secret::new("REDACTED"),
    }))
}

/// Runs `p` on `case` until the replayer has seen `requests` requests and
/// the plugin has sent `finds` discoveries; returns them and the replayer.
async fn replay(
    p: &Arc<Clients>,
    case: &str,
    requests: usize,
    finds: usize,
) -> (Vec<Discovery>, Arc<Replayer>) {
    let replayer = Arc::new(Replayer::load(&fixture(case)).unwrap());
    let mut ctx = FakeCtx::with_fetch(&**p, SourceId::new(7).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the recorded requests", || {
        replayer.requests().len() >= requests
    })
    .await;
    let mut got = Vec::new();
    wait_for("the discoveries", || {
        got.extend(ctx.drain());
        got.len() >= finds
    })
    .await;
    assert!(!running.is_finished(), "the plugin ended on its own");
    running.stop_within().await.unwrap();
    got.extend(ctx.drain());
    assert_eq!(got.len(), finds, "{got:#?}");
    assert_eq!(replayer.requests().len(), requests);
    replayer.finish().unwrap();
    (got, replayer)
}

/// Each list request is at least the poll interval after the last.
fn assert_spacing(replayer: &Replayer, is_list: impl Fn(&lodestar_sources::Request) -> bool) {
    let lists: Vec<_> = replayer
        .requests()
        .into_iter()
        .filter(|(_, r)| is_list(r))
        .map(|(at, _)| at)
        .collect();
    assert!(lists.len() >= 2, "two polls");
    for pair in lists.windows(2) {
        assert!(
            pair[1] - pair[0] >= POLL,
            "polled after {:?}",
            pair[1] - pair[0]
        );
    }
}

/// §18.7: every find is private, with nothing a public lookup or an
/// export could use. The pool refuses private keys to peer finders
/// (`crates/sources/tests/pool.rs`, `private_keys_never_reach_peer_finders`).
fn assert_private(got: &[Discovery]) {
    for d in got {
        let f = &d.found;
        assert!(f.private_source, "{f:?}");
        assert!(f.peers.is_empty() && !f.interest);
        let p = f.provenance.as_deref().unwrap();
        assert!(p.external_ref.is_none() && p.published_at.is_none());
        if let Some(c) = f.claims.as_deref() {
            assert!(c.trackers.is_empty() && c.publisher.is_none() && c.ids.is_empty());
        }
    }
}

fn by_key<'a>(got: &'a [Discovery], h: &str) -> Vec<&'a Discovery> {
    let key = hash(h);
    got.iter().filter(|d| d.found.key == key).collect()
}

#[tokio::test(start_paused = true)]
async fn qbittorrent_lists_exports_and_keeps_every_find_private() {
    let p = qbittorrent();
    // Poll 1: login, list (two good entries, two malformed), export both
    // new ones (one has no metadata yet). Poll 2: the list again; only the
    // new torrent is exported, and its `.torrent` is another torrent's.
    let (got, replayer) = replay(&p, "qbittorrent_poll", 6, 3).await;
    assert_spacing(&replayer, |r| r.url.ends_with("/torrents/info"));
    assert_private(&got);
    assert_eq!(p.skipped(), 2);

    let bunny = by_key(&got, BUNNY);
    assert_eq!(bunny.len(), 1, "found once, though listed twice");
    let f = &bunny[0].found;
    assert_eq!(f.strategy, STRATEGY_EXPORT);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(c.name.as_deref(), Some("Big Buck Bunny (2008)"));
    assert_eq!(c.size, Some(525_312));
    let files: Vec<_> = c.files.iter().map(|f| (f.path.as_str(), f.size)).collect();
    assert_eq!(
        files,
        [
            ("Big Buck Bunny.mkv", Some(524_288)),
            ("Subs/en.srt", Some(1024))
        ]
    );

    // No metadata yet: the list's own claims; size -1 isn't believed.
    let sintel = by_key(&got, SINTEL);
    let f = &sintel[0].found;
    assert_eq!(f.strategy, STRATEGY_LIST);
    let c = f.claims.as_deref().unwrap();
    assert_eq!((c.name.as_deref(), c.size), (Some("Sintel (2010)"), None));

    // The exported `.torrent` hashes to another key: its info dict is
    // unused, and the list's claims stand.
    let dream = by_key(&got, DREAM);
    let f = &dream[0].found;
    assert_eq!(f.strategy, STRATEGY_LIST);
    let c = f.claims.as_deref().unwrap();
    assert_eq!(c.name.as_deref(), Some("Elephants Dream (2006) 720p"));
    assert_eq!(c.size, Some(815_000_000));
    assert!(c.files.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_refused_login_stops_the_plugin_with_no_secret() {
    let p = qbittorrent();
    let replayer = Arc::new(Replayer::load(&fixture("qbittorrent_refused")).unwrap());
    let mut ctx = FakeCtx::with_fetch(&*p, SourceId::new(7).unwrap(), replayer.clone());
    let running = ctx.start(p.clone());
    wait_for("the plugin to stop", || running.is_finished()).await;
    let err = running
        .stop()
        .await
        .unwrap()
        .result
        .unwrap_err()
        .to_string();
    assert_eq!(
        err,
        "the download client: the client refused the user name or password"
    );
    assert!(!err.contains("REDACTED") && !err.contains("example"));
    assert!(ctx.drain().is_empty());
    replayer.finish().unwrap();
}

#[tokio::test(start_paused = true)]
async fn transmission_lists_after_its_session_id_and_exports_nothing() {
    let p = Arc::new(Clients::new(ClientConfig::Transmission {
        url: "http://transmission.example:9091/".into(),
        auth: Some(BasicAuth {
            username: "REDACTED".into(),
            password: Secret::new("REDACTED"),
        }),
    }));
    let (got, replayer) = replay(&p, "transmission_poll", 3, 3).await;
    assert_private(&got);
    assert_eq!(p.skipped(), 1);
    // The 409 and its retry are one poll; the next list is a poll later.
    let at: Vec<_> = replayer.requests().into_iter().map(|(t, _)| t).collect();
    assert!(at[2] - at[1] >= POLL, "{:?}", at[2] - at[1]);
    for d in &got {
        assert_eq!(d.found.strategy, STRATEGY_LIST);
    }
    let c = by_key(&got, TEARS)[0].found.claims.as_deref().unwrap();
    assert_eq!(
        (c.name.as_deref(), c.size),
        (Some("Tears of Steel (2012)"), Some(6_000_000_000))
    );
    assert_eq!(by_key(&got, BUNNY).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn deluge_lists_and_logs_in_again_when_the_session_expires() {
    let p = Arc::new(Clients::new(ClientConfig::Deluge {
        url: "http://deluge.example:8112/".into(),
        password: Secret::new("REDACTED"),
    }));
    let (got, replayer) = replay(&p, "deluge_poll", 7, 3).await;
    assert_private(&got);
    // "short" isn't a hash.
    assert_eq!(p.skipped(), 1);
    let at: Vec<_> = replayer.requests().into_iter().map(|(t, _)| t).collect();
    assert!(at[3] - at[2] >= POLL, "{:?}", at[3] - at[2]);
    assert_eq!(by_key(&got, TEARS).len(), 1);
    assert_eq!(by_key(&got, SINTEL).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn setup_failures_end_the_run_before_any_request() {
    let cases: Vec<(Clients, &str)> = vec![
        (
            Clients::unconfigured(),
            "needs setup: no download client is set up in Settings → Integrations",
        ),
        (
            Clients::new(ClientConfig::RTorrent {
                url: "http://rtorrent.example/RPC2".into(),
                auth: None,
            }),
            "listing torrents works with qBittorrent, Transmission and Deluge only",
        ),
        (
            Clients::new(ClientConfig::Deluge {
                url: "http://169.254.169.254/".into(),
                password: Secret::new("REDACTED"),
            }),
            "needs setup: the download client's address isn't usable",
        ),
    ];
    for (p, want) in cases {
        let p = Arc::new(p);
        // NoEgress: any request would fail the run with another error.
        let mut ctx = FakeCtx::new(&*p, SourceId::new(7).unwrap());
        let running = ctx.start(p.clone());
        wait_for("the plugin to stop", || running.is_finished()).await;
        let err = running.stop().await.unwrap().result.unwrap_err();
        assert_eq!(err.to_string(), want);
    }
}

#[test]
fn caps_consent_and_endpoints() {
    let p = qbittorrent();
    let m = p.manifest();
    assert_eq!(m.id, clients::ID);
    assert_eq!(m.consent, Consent::Account);
    assert!(m.endpoints.is_empty() && !m.enabled_by_default);
    let policy = p.policy();
    assert!(policy.caps.min_poll_interval >= POLL);
    let limits = policy.limits(&UserLimits::default());
    assert!(limits.poll_interval >= floors::POLL_INTERVAL);
    assert!(limits.per_host.interval >= floors::HOST_INTERVAL);
    assert!(limits.per_host.burst <= floors::MAX_BURST);
    // A user setting can't poll faster than once a minute.
    let fast = UserLimits {
        poll_interval: Some(Duration::from_secs(5)),
        ..UserLimits::default()
    };
    assert!(policy.limits(&fast).poll_interval >= POLL);
    assert_eq!(policy.distribution, Distribution::Coordinator);
    assert!(!policy.config_schema.contains("password"));
    assert!(p.endpoints().is_some());
    assert!(Clients::unconfigured().endpoints().is_none());
    let debug = format!("{p:?}");
    assert!(!debug.contains("REDACTED") && debug.contains("qbittorrent.example"));
}

fn listed(hash: &str) -> Listed {
    Listed {
        hash: hash.into(),
        name: Some("From the list".into()),
        size: Some(10),
        state: TorrentState::Seeding,
    }
}

#[test]
fn exports_are_used_only_when_they_hash_to_the_listed_key() {
    let bunny = std::fs::read(fixture("qbittorrent_poll").join("003.body")).unwrap();
    let f = to_found(&listed(BUNNY), Some(&bunny)).unwrap();
    assert_eq!(f.strategy, STRATEGY_EXPORT);
    assert!(f.private_source);
    // Never the announce URL: a private tracker's holds the passkey.
    assert!(f.claims.as_deref().unwrap().trackers.is_empty());
    // The same bytes listed under another key, garbage, and nothing.
    for (key, bytes) in [
        (SINTEL, Some(&bunny[..])),
        (BUNNY, Some(&b"d4:infoi1ee"[..])),
        (BUNNY, Some(&b""[..])),
        (BUNNY, None),
    ] {
        let f = to_found(&listed(key), bytes).unwrap();
        assert_eq!(f.strategy, STRATEGY_LIST);
        assert!(f.private_source);
        let c = f.claims.as_deref().unwrap();
        assert_eq!(
            (c.name.as_deref(), c.size),
            (Some("From the list"), Some(10))
        );
    }
    assert!(to_found(&listed("nope"), None).is_none());
    let huge = Listed {
        size: Some(MAX_SIZE + 1),
        name: None,
        ..listed(BUNNY)
    };
    let f = to_found(&huge, None).unwrap();
    assert!(f.claims.is_none() && f.private_source);
}

#[test]
fn list_parsers_are_bounded() {
    let too_big = vec![b' '; MAX_LIST_BYTES + 1];
    for kind in [
        ClientKind::QBittorrent,
        ClientKind::Transmission,
        ClientKind::Deluge,
    ] {
        assert!(parse_list(kind, &too_big).is_err());
        assert!(parse_list(kind, b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]]").is_err());
    }
    let long = format!(
        r#"[{{"hash":"{BUNNY}","name":"{}"}}]"#,
        "x".repeat(MAX_LISTED_NAME + 1)
    );
    let l = parse_list(ClientKind::QBittorrent, long.as_bytes()).unwrap();
    assert_eq!((l.torrents.len(), l.malformed), (0, 1));
    const { assert!(MAX_LISTED >= 10_000) };
}

proptest! {
    #[test]
    fn list_parsing_never_panics_and_keeps_its_bounds(
        body in prop::collection::vec(any::<u8>(), 0..2048),
        kind in 0usize..3,
    ) {
        let kind = [ClientKind::QBittorrent, ClientKind::Transmission, ClientKind::Deluge][kind];
        if let Ok(l) = parse_list(kind, &body) {
            prop_assert!(l.torrents.len() <= MAX_LISTED);
            for t in &l.torrents {
                prop_assert!(t.hash.len() == 40 && t.hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
                prop_assert!(t.name.as_ref().is_none_or(|n| n.len() <= MAX_LISTED_NAME && !n.chars().any(char::is_control)));
                prop_assert!(to_found(t, Some(&body)).is_some_and(|f| f.private_source));
            }
        }
    }

    #[test]
    fn json_shaped_lists_never_panic(
        entries in prop::collection::vec(
            (prop::option::of("[0-9a-fA-F]{38,42}"), prop::option::of(".{0,8}"), any::<i64>()),
            0..20,
        ),
    ) {
        let rows: Vec<String> = entries
            .iter()
            .map(|(h, n, s)| {
                let mut m: HashMap<&str, serde_json::Value> = HashMap::new();
                if let Some(h) = h { m.insert("hash", h.clone().into()); }
                if let Some(n) = n { m.insert("name", n.clone().into()); }
                m.insert("total_size", (*s).into());
                serde_json::to_string(&m).unwrap()
            })
            .collect();
        let body = format!("[{}]", rows.join(","));
        let l = parse_list(ClientKind::QBittorrent, body.as_bytes()).unwrap();
        prop_assert_eq!(l.torrents.len() + l.malformed + l.over_cap, entries.len());
        for t in &l.torrents {
            prop_assert!(t.size.is_none_or(|s| s > 0));
        }
    }
}
