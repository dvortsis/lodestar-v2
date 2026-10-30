//! The Aria2 adapter against JSON-RPC answers recorded from aria2 1.37.0.
//! No network: a local replay server plays them back.
//!
//! The adapter numbers its requests from 1 and checks each answer's id, so
//! [`script`] rewrites the recorded ids to the ones a fresh adapter sends.

mod support;

use std::sync::{Arc, Mutex, OnceLock};

use lodestar_integrations::clients::aria2::Aria2;
use lodestar_integrations::clients::{ClientConfig, connect};
use lodestar_integrations::{
    AddOptions, AuthFailure, ClientError, ClientKind, DownloadClient, Feature, Magnet, Secret,
    Selection, TorrentFile,
};
use serde_json::{Value, json};
use support::{Fixture, Received, ReplayServer, fixture, fixture_bytes, refused_addr};

const SECRET: &str = "correct-horse-battery";
const FIXTURE_HASH: &str = "f16304349815c2941fe00ababb5fe4e9098e0ee4";
const MAGNET_HASH: &str = "c9e15763f722f23e98a29decdfae341b98d53056";

/// A token in the configured URL's query: dropped, never printed.
const QUERY_TOKEN: &str = "query-token-8c1f";

fn f(name: &str) -> Fixture {
    fixture("aria2", name)
}

/// The fixtures, with answer ids "1", "2", … in order.
fn script(names: &[&str]) -> Vec<Fixture> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| with_id(f(name), json!((i + 1).to_string())))
        .collect()
}

fn with_id(mut fx: Fixture, id: Value) -> Fixture {
    let mut body: Value = serde_json::from_str(&fx.response.body).unwrap();
    body["id"] = id;
    fx.response.body = body.to_string();
    fx
}

fn client(server: &ReplayServer) -> Aria2 {
    Aria2::new(
        &format!("{}jsonrpc", server.url()),
        Some(Secret::new(SECRET)),
    )
    .unwrap()
}

fn magnet() -> Magnet {
    Magnet::parse(&format!(
        "magnet:?xt=urn:btih:{MAGNET_HASH}&dn=Lodestar+Magnet+Fixture"
    ))
    .unwrap()
}

fn torrent() -> TorrentFile {
    TorrentFile::new(
        fixture_bytes("qbittorrent", "fixture.torrent"),
        FIXTURE_HASH,
    )
    .unwrap()
}

/// The download id a recorded add answered.
fn gid(name: &str) -> String {
    let body: Value = serde_json::from_str(&f(name).response.body).unwrap();
    body["result"].as_str().unwrap().to_owned()
}

fn call(req: &Received) -> (String, Value) {
    let b: Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(b["jsonrpc"], "2.0");
    (
        b["method"].as_str().unwrap().to_owned(),
        b["params"].clone(),
    )
}

fn token() -> String {
    format!("token:{SECRET}")
}

const STATUS_KEYS: [&str; 3] = ["status", "errorCode", "errorMessage"];

#[tokio::test]
async fn test_sends_the_token_and_reports_the_version() {
    let server = ReplayServer::start(script(&["version"])).await;
    let info = client(&server).test().await.unwrap();
    assert_eq!(info.kind, ClientKind::Aria2);
    assert_eq!(info.version, "1.37.0");
    assert_eq!(info.api_version, None);
    let reqs = server.finish();
    assert_eq!(reqs[0].path, "/jsonrpc");
    assert_eq!(
        call(&reqs[0]),
        ("aria2.getVersion".into(), json!([token()]))
    );
    let id: Value = serde_json::from_slice::<Value>(&reqs[0].body).unwrap()["id"].clone();
    assert_eq!(id, "1");

    let server = ReplayServer::start(script(&["version"])).await;
    let a2 = Aria2::new(&format!("{}jsonrpc", server.url()), None).unwrap();
    a2.test().await.unwrap();
    assert_eq!(call(&server.finish()[0]).1, json!([]));
}

#[tokio::test]
async fn a_wrong_token_is_an_auth_error() {
    let server = ReplayServer::start(script(&["login_fails"])).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::BadCredentials)
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn adds_a_magnet_and_checks_it_started() {
    let server = ReplayServer::start(script(&["add_uri", "status_active"])).await;
    let opts = AddOptions {
        save_path: Some("/data/movies".into()),
        ..AddOptions::default()
    };
    let out = client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.sent, 1);
    assert_eq!(out.selection, Selection::None);
    let reqs = server.finish();
    assert_eq!(
        call(&reqs[0]),
        (
            "aria2.addUri".into(),
            json!([token(), [magnet().as_str()], { "dir": "/data/movies" }])
        )
    );
    assert_eq!(
        call(&reqs[1]),
        (
            "aria2.tellStatus".into(),
            json!([token(), gid("add_uri"), STATUS_KEYS])
        )
    );
}

#[tokio::test]
async fn a_selection_goes_with_the_magnet_and_paused_skips_the_check() {
    let server = ReplayServer::start(script(&["add_uri"])).await;
    let opts = AddOptions {
        paused: true,
        selected_files: Some(vec![2, 0]),
        ..AddOptions::default()
    };
    let out = client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    // aria2 applies it itself when the metadata arrives.
    assert_eq!(out.selection, Selection::Applied);
    let reqs = server.finish();
    assert_eq!(
        call(&reqs[0]).1[2],
        json!({ "pause": "true", "select-file": "1,3" })
    );
}

#[tokio::test]
async fn bulk_adds_one_at_a_time_and_skips_duplicates() {
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{FIXTURE_HASH}")).unwrap();
    let server = ReplayServer::start(script(&[
        "add_uri",
        "status_active",
        "add_uri",
        "status_duplicate",
    ]))
    .await;
    let out = client(&server)
        .add_magnets(&[magnet(), other.clone()], &AddOptions::default())
        .await
        .unwrap();
    assert_eq!(out.sent, 2);
    let reqs = server.finish();
    assert_eq!(call(&reqs[2]).1[1], json!([other.as_str()]));

    let server = ReplayServer::start(script(&["add_uri", "status_duplicate"])).await;
    assert_eq!(
        client(&server)
            .add_magnet(&magnet(), &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("the torrent is already in the client".into())
    );
    server.finish();
}

#[tokio::test]
async fn selected_files_of_a_torrent_file_are_checked_then_started() {
    let server = ReplayServer::start(script(&[
        "add_torrent_paused",
        "files",
        "unpause",
        "status_active",
    ]))
    .await;
    let file = torrent();
    let opts = AddOptions {
        selected_files: Some(vec![2, 0]),
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_torrent_file(&file, &opts)
        .await
        .unwrap();
    assert_eq!(out.selection, Selection::Applied);
    let reqs = server.finish();
    let (method, params) = call(&reqs[0]);
    assert_eq!(method, "aria2.addTorrent");
    assert_eq!(params[0], token());
    assert_eq!(
        params[1].as_str().unwrap().len(),
        file.bytes.len().div_ceil(3) * 4
    );
    assert_eq!(params[2], json!([]));
    assert_eq!(params[3], json!({ "pause": "true", "select-file": "1,3" }));
    assert_eq!(
        call(&reqs[1]),
        (
            "aria2.getFiles".into(),
            json!([token(), gid("add_torrent_paused")])
        )
    );
    assert_eq!(
        call(&reqs[2]),
        (
            "aria2.unpause".into(),
            json!([token(), gid("add_torrent_paused")])
        )
    );
    assert_eq!(call(&reqs[3]).0, "aria2.tellStatus");
}

#[tokio::test]
async fn a_paused_selection_stays_paused_and_past_the_last_file_is_refused() {
    let server = ReplayServer::start(script(&["add_torrent_paused", "files"])).await;
    let opts = AddOptions {
        selected_files: Some(vec![0, 1, 2]),
        paused: true,
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_torrent_file(&torrent(), &opts)
        .await
        .unwrap();
    assert_eq!(out.selection, Selection::Applied);
    server.finish();

    let server = ReplayServer::start(script(&["add_torrent_paused", "files"])).await;
    let opts = AddOptions {
        selected_files: Some(vec![0, 3]),
        ..AddOptions::default()
    };
    assert!(matches!(
        client(&server).add_torrent_file(&torrent(), &opts).await,
        Err(ClientError::InvalidRequest(_))
    ));
    server.finish();
}

#[tokio::test]
async fn a_duplicate_torrent_file_fails_as_it_starts() {
    let server = ReplayServer::start(script(&["add_torrent", "status_duplicate"])).await;
    assert_eq!(
        client(&server)
            .add_torrent_file(&torrent(), &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("the torrent is already in the client".into())
    );
    let reqs = server.finish();
    assert_eq!(call(&reqs[0]).1[3], json!({}));
    assert_eq!(call(&reqs[1]).1[1], gid("add_torrent"));
}

#[tokio::test]
async fn refusals_carry_aria2s_reason() {
    let server = ReplayServer::start(script(&["add_torrent_invalid"])).await;
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    assert_eq!(
        client(&server)
            .add_torrent_file(&file, &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("Bencode decoding failed".into())
    );
    assert_eq!(call(&server.finish()[0]).1[1], "Z2FyYmFnZQ==");

    let mut failed = f("status_duplicate");
    failed.response.body = failed.response.body.replace("\"12\"", "\"9\"").replace(
        "InfoHash f16304349815c2941fe00ababb5fe4e9098e0ee4 is already registered.",
        "No space left on device",
    );
    let mut fixtures = script(&["add_torrent"]);
    fixtures.push(with_id(failed, json!("2")));
    let server = ReplayServer::start(fixtures).await;
    assert_eq!(
        client(&server)
            .add_torrent_file(&torrent(), &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("No space left on device".into())
    );

    let server = ReplayServer::start(script(&["unknown_method"])).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Rejected("No such method: aria2.frobnicate".into())
    );
}

#[tokio::test]
async fn categories_and_unsupported_options_make_no_request() {
    let server = ReplayServer::start(vec![]).await;
    let a2 = client(&server);
    assert_eq!(
        a2.categories().await.unwrap_err(),
        ClientError::Unsupported(Feature::Category)
    );
    let labelled = AddOptions {
        category: Some("movies".into()),
        ..AddOptions::default()
    };
    assert_eq!(
        a2.add_magnet(&magnet(), &labelled).await.unwrap_err(),
        ClientError::Unsupported(Feature::Category)
    );
    let select = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert!(matches!(
        a2.add_magnets(&[magnet(), magnet()], &select).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn mismatched_ids_and_bad_answers_are_protocol_errors() {
    let server = ReplayServer::start(vec![with_id(f("version"), json!("99"))]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Protocol("the answer is for another request")
    );

    let mut huge = f("version");
    huge.response.body = format!("{{\"id\":\"1\",\"result\":\"{}\"}}", "a".repeat(2 << 20));
    let mut truncated = with_id(f("version"), json!("1"));
    truncated.response.body.truncate(25);
    let mut html = f("version");
    html.response.body = "<html><body>AriaNg</body></html>".into();
    let mut no_gid = with_id(f("add_uri"), json!("1"));
    no_gid.response.body = r#"{"id":"1","jsonrpc":"2.0","result":"not-a-gid"}"#.into();
    for bad in [huge, truncated, html] {
        let server = ReplayServer::start(vec![bad]).await;
        assert!(matches!(
            client(&server).test().await,
            Err(ClientError::Protocol(_))
        ));
    }
    let server = ReplayServer::start(vec![no_gid]).await;
    assert!(matches!(
        client(&server)
            .add_magnet(&magnet(), &AddOptions::default())
            .await,
        Err(ClientError::Protocol(_))
    ));
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let (_held, addr) = refused_addr();
    let a2 = Aria2::new(&format!("http://{addr}/jsonrpc"), Some(Secret::new(SECRET))).unwrap();
    assert_eq!(a2.test().await.unwrap_err(), ClientError::Unreachable);
}

#[tokio::test]
async fn a_hostile_file_count_is_refused() {
    let mut files = with_id(f("files"), json!("2"));
    let list = format!("[{}{{}}]", "{},".repeat(1_000_001));
    files.response.body = format!(r#"{{"id":"2","jsonrpc":"2.0","result":{list}}}"#);
    let mut fixtures = script(&["add_torrent_paused"]);
    fixtures.push(files);
    let server = ReplayServer::start(fixtures).await;
    let opts = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert!(matches!(
        client(&server).add_torrent_file(&torrent(), &opts).await,
        Err(ClientError::Protocol(_))
    ));
    server.finish();
}

#[tokio::test]
async fn a_bulk_send_that_fails_part_way_says_how_many_went_in() {
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{FIXTURE_HASH}")).unwrap();
    let server =
        ReplayServer::start(script(&["add_uri", "status_active", "add_torrent_invalid"])).await;
    assert_eq!(
        client(&server)
            .add_magnets(&[magnet(), other], &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Partial {
            sent: 1,
            cause: Box::new(ClientError::Rejected("Bencode decoding failed".into())),
        }
    );
    server.finish();
}

/// Captures every log line, at every level, from every crate, for the whole
/// test binary (see `tests/qbittorrent.rs`).
fn capture_logs() -> Arc<Mutex<Vec<u8>>> {
    struct W(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for W {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static BUF: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    Arc::clone(BUF.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || W(Arc::clone(&sink)))
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        // Every event on, at every level, so a secret in any of them fails.
        for e in lodestar_integrations::log_events::CATALOGUE.events {
            e.set_on(true);
        }
        buf
    }))
}

#[tokio::test]
async fn no_token_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let server = ReplayServer::start(script(&[
        "version",
        "add_torrent_paused",
        "files",
        "unpause",
        "status_active",
        "add_torrent_invalid",
    ]))
    .await;
    let a2 = client(&server);
    a2.test().await.unwrap();
    let opts = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    a2.add_torrent_file(&torrent(), &opts).await.unwrap();
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    let err = a2
        .add_torrent_file(&file, &AddOptions::default())
        .await
        .unwrap_err();
    let failed = ReplayServer::start(script(&["login_fails"])).await;
    let bad = client(&failed).test().await.unwrap_err();

    let config = ClientConfig::Aria2 {
        url: format!("{}?token={QUERY_TOKEN}", server.url()),
        secret: Some(Secret::new(SECRET)),
    };
    let boxed = connect(config.clone()).unwrap();
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(logged.contains("client=aria2"), "capture works");
    for text in [
        logged,
        format!("{a2:?}"),
        format!("{boxed:?}"),
        format!("{config:?}"),
        format!("{err} {err:?}"),
        format!("{bad} {bad:?}"),
    ] {
        assert!(!text.contains(QUERY_TOKEN), "query token leaked: {text}");
        assert!(!text.contains(SECRET), "token leaked: {text}");
    }
}

/// Once against a running aria2:
/// `LODESTAR_LIVE_ARIA2=http://127.0.0.1:6800/jsonrpc LODESTAR_LIVE_PASSWORD=…
/// cargo test --test aria2 -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a running aria2"]
async fn live() {
    let url = std::env::var("LODESTAR_LIVE_ARIA2").unwrap();
    let secret = std::env::var("LODESTAR_LIVE_PASSWORD")
        .ok()
        .map(Secret::new);
    let a2 = Aria2::new(&url, secret).unwrap();
    println!("test: {:?}", a2.test().await);
    let opts = AddOptions {
        save_path: Some("/tmp/lodestar-live/aria2".into()),
        ..AddOptions::default()
    };
    println!("add magnet: {:?}", a2.add_magnet(&magnet(), &opts).await);
    let select = AddOptions {
        selected_files: Some(vec![0, 2]),
        ..AddOptions::default()
    };
    println!(
        "add file, files 0 and 2: {:?}",
        a2.add_torrent_file(&torrent(), &select).await
    );
    println!(
        "add file again: {:?}",
        a2.add_torrent_file(&torrent(), &AddOptions::default())
            .await
    );
    println!(
        "past the last file: {:?}",
        a2.add_torrent_file(
            &torrent(),
            &AddOptions {
                selected_files: Some(vec![5]),
                paused: true,
                ..AddOptions::default()
            }
        )
        .await
    );
    println!("categories: {:?}", a2.categories().await);
}
