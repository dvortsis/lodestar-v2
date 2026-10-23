//! The Transmission adapter against responses recorded from Transmission
//! 4.0.5 (RPC 17). No network: a local replay server plays them back.

mod support;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::clients::transmission::Transmission;
use lodestar_integrations::clients::{BasicAuth, ClientConfig, connect};
use lodestar_integrations::{
    AddOptions, AuthFailure, Category, ClientError, ClientKind, DownloadClient, Feature, Magnet,
    Secret, Selection, TorrentFile,
};
use serde_json::{Value, json};
use support::{Fixture, Received, ReplayServer, fixture, fixture_bytes};

const PASSWORD: &str = "correct-horse-battery";
const FIXTURE_HASH: &str = "f16304349815c2941fe00ababb5fe4e9098e0ee4";
const MAGNET_HASH: &str = "c9e15763f722f23e98a29decdfae341b98d53056";

/// A token in the configured URL's query: dropped, never printed.
const QUERY_TOKEN: &str = "query-token-8c1f";

fn f(name: &str) -> Fixture {
    fixture("transmission", name)
}

fn auth() -> Option<BasicAuth> {
    Some(BasicAuth {
        username: "lodestar".into(),
        password: Secret::new(PASSWORD),
    })
}

fn client(server: &ReplayServer) -> Transmission {
    Transmission::new(&server.url(), auth())
        .unwrap()
        .with_metadata_wait(Duration::from_millis(200), Duration::from_millis(1))
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

fn body(req: &Received) -> Value {
    serde_json::from_slice(&req.body).unwrap()
}

/// The session id a recorded 409 hands out.
fn session_id(name: &str) -> String {
    f(name)
        .response
        .headers
        .iter()
        .find(|(k, _)| k == "x-transmission-session-id")
        .map(|(_, v)| v.clone())
        .unwrap()
}

#[tokio::test]
async fn test_handshakes_the_session_id_and_reports_the_version() {
    let server = ReplayServer::start(vec![f("session_conflict"), f("session_get")]).await;
    let info = client(&server).test().await.unwrap();
    assert_eq!(info.kind, ClientKind::Transmission);
    assert_eq!(info.version, "4.0.5 (a6fe2a64aa)");
    assert_eq!(info.api_version.as_deref(), Some("17"));

    let reqs = server.finish();
    assert_eq!(reqs[0].path, "/transmission/rpc");
    assert_eq!(reqs[0].header("x-transmission-session-id"), None);
    // The retry carries the id from the 409, once.
    assert_eq!(
        reqs[1].header("x-transmission-session-id"),
        Some(session_id("session_conflict").as_str())
    );
    for req in &reqs {
        assert_eq!(
            req.header("authorization"),
            Some("Basic bG9kZXN0YXI6Y29ycmVjdC1ob3JzZS1iYXR0ZXJ5")
        );
        assert_eq!(req.header("content-type"), Some("application/json"));
        assert_eq!(body(req)["method"], "session-get");
    }
}

#[tokio::test]
async fn no_password_sends_no_authorization() {
    let server = ReplayServer::start(vec![f("session_conflict"), f("session_get")]).await;
    let tr = Transmission::new(&server.url(), None).unwrap();
    tr.test().await.unwrap();
    assert!(
        server
            .finish()
            .iter()
            .all(|r| r.header("authorization").is_none())
    );
}

#[tokio::test]
async fn refusals_are_auth_errors_and_never_retried() {
    for (name, want) in [
        ("login_fails", AuthFailure::BadCredentials),
        ("ip_whitelist", AuthFailure::AddressRefused),
        ("login_banned", AuthFailure::AddressRefused),
        ("host_whitelist", AuthFailure::OriginRefused),
    ] {
        let server = ReplayServer::start(vec![f(name)]).await;
        assert_eq!(
            client(&server).test().await.unwrap_err(),
            ClientError::Auth(want),
            "{name}"
        );
        assert_eq!(server.finish().len(), 1, "{name}");
    }
}

#[tokio::test]
async fn an_expired_session_id_is_replaced_once() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("session_get"),
        f("session_expired"),
        f("labels"),
    ])
    .await;
    let tr = client(&server);
    tr.test().await.unwrap();
    tr.categories().await.unwrap();
    let reqs = server.finish();
    assert_eq!(
        reqs[3].header("x-transmission-session-id"),
        Some(session_id("session_expired").as_str())
    );

    // Refused again with the new id: stop, don't loop.
    let server = ReplayServer::start(vec![f("session_conflict"), f("session_expired")]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::SessionRefused)
    );
    server.finish();
}

#[tokio::test]
async fn adds_a_magnet_with_label_and_download_dir() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("session_get"),
        f("torrent_add_magnet"),
    ])
    .await;
    let opts = AddOptions {
        category: Some("Movies".into()),
        save_path: Some("/data/movies".into()),
        ..AddOptions::default()
    };
    let out = client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.sent, 1);
    assert_eq!(out.selection, Selection::None);
    let reqs = server.finish();
    assert_eq!(
        body(&reqs[2]),
        json!({
            "method": "torrent-add",
            "arguments": {
                "filename": magnet().as_str(),
                "download-dir": "/data/movies",
                "labels": ["Movies"],
            }
        })
    );
}

#[tokio::test]
async fn labels_go_through_torrent_set_on_rpc_16_and_are_dropped_before() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("session_get_rpc16"),
        f("torrent_add_magnet"),
        f("torrent_set_unwanted"),
    ])
    .await;
    let opts = AddOptions {
        category: Some("Movies".into()),
        ..AddOptions::default()
    };
    client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    let reqs = server.finish();
    assert_eq!(body(&reqs[2])["arguments"].get("labels"), None);
    assert_eq!(
        body(&reqs[3]),
        json!({
            "method": "torrent-set",
            "arguments": { "ids": [MAGNET_HASH], "labels": ["Movies"] }
        })
    );

    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("session_get_rpc15"),
        f("torrent_add_magnet"),
    ])
    .await;
    let tr = client(&server);
    tr.add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(body(&server.finish()[2])["arguments"].get("labels"), None);
    // No labels before RPC 16, so no categories either (version cached).
    let server2 = ReplayServer::start(vec![]).await;
    assert_eq!(
        tr.categories().await.unwrap_err(),
        ClientError::Unsupported(Feature::Category)
    );
    assert!(server2.finish().is_empty());
}

#[tokio::test]
async fn a_duplicate_is_rejected_and_bulk_adds_one_at_a_time() {
    let server = ReplayServer::start(vec![f("session_conflict"), f("torrent_add_duplicate")]).await;
    assert_eq!(
        client(&server)
            .add_magnet(&magnet(), &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("the torrent is already in the client".into())
    );
    server.finish();

    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{FIXTURE_HASH}")).unwrap();
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_magnet"),
        f("torrent_add_duplicate"),
    ])
    .await;
    let opts = AddOptions {
        paused: true,
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_magnets(&[magnet(), other.clone()], &opts)
        .await
        .unwrap();
    assert_eq!(out.sent, 2);
    let reqs = server.finish();
    assert_eq!(body(&reqs[1])["arguments"]["filename"], magnet().as_str());
    assert_eq!(body(&reqs[2])["arguments"]["filename"], other.as_str());
    assert_eq!(body(&reqs[2])["arguments"]["paused"], true);

    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_duplicate"),
        f("torrent_add_duplicate"),
    ])
    .await;
    assert!(matches!(
        client(&server)
            .add_magnets(&[magnet(), other], &AddOptions::default())
            .await,
        Err(ClientError::Rejected(_))
    ));
    server.finish();
}

#[tokio::test]
async fn refusals_carry_the_daemons_reason() {
    let server = ReplayServer::start(vec![f("session_conflict"), f("torrent_add_invalid")]).await;
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    assert_eq!(
        client(&server)
            .add_torrent_file(&file, &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("unrecognized info".into())
    );
    let reqs = server.finish();
    assert_eq!(body(&reqs[1])["arguments"]["metainfo"], "Z2FyYmFnZQ==");

    let server = ReplayServer::start(vec![f("session_conflict"), f("unknown_method")]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Rejected("method name not recognized".into())
    );
    server.finish();
}

#[tokio::test]
async fn selected_files_of_a_torrent_file() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_file_paused"),
        // Registered asynchronously: the first look may not find it.
        f("files_unknown"),
        f("files"),
        f("torrent_set_unwanted"),
        f("torrent_start"),
    ])
    .await;
    let file = torrent();
    let opts = AddOptions {
        save_path: Some("/data/music".into()),
        selected_files: Some(vec![2, 0]),
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_torrent_file(&file, &opts)
        .await
        .unwrap();
    assert_eq!(out.selection, Selection::Applied);
    let reqs = server.finish();
    let add = body(&reqs[1]);
    // Added paused, so nothing downloads before the selection applies.
    assert_eq!(add["arguments"]["paused"], true);
    assert_eq!(add["arguments"]["download-dir"], "/data/music");
    let metainfo = add["arguments"]["metainfo"].as_str().unwrap();
    assert_eq!(metainfo.len(), file.bytes.len().div_ceil(3) * 4);
    assert_eq!(
        body(&reqs[2])["arguments"],
        json!({ "ids": [FIXTURE_HASH], "fields": ["files"] })
    );
    assert_eq!(
        body(&reqs[4]),
        json!({
            "method": "torrent-set",
            "arguments": { "ids": [FIXTURE_HASH], "files-unwanted": [1] }
        })
    );
    assert_eq!(
        body(&reqs[5]),
        json!({ "method": "torrent-start", "arguments": { "ids": [FIXTURE_HASH] } })
    );
}

#[tokio::test]
async fn selecting_every_file_sets_nothing_and_paused_stays_paused() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_file_paused"),
        f("files"),
    ])
    .await;
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
}

#[tokio::test]
async fn a_magnets_files_cant_be_selected() {
    // A paused magnet fetches no metadata, so nothing could be selected
    // before it downloads: refused before any request.
    let server = ReplayServer::start(vec![]).await;
    let opts = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert_eq!(
        client(&server)
            .add_magnet(&magnet(), &opts)
            .await
            .unwrap_err(),
        ClientError::Unsupported(Feature::MagnetFileSelection)
    );
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn a_selection_past_the_last_file_is_refused_before_changes() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_file_paused"),
        f("files"),
    ])
    .await;
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
async fn categories_are_the_labels_in_use() {
    let server =
        ReplayServer::start(vec![f("session_conflict"), f("session_get"), f("labels")]).await;
    let cats = client(&server).categories().await.unwrap();
    assert_eq!(
        cats,
        vec![Category {
            name: "Movies".into(),
            save_path: None,
        }]
    );
    let reqs = server.finish();
    assert_eq!(
        body(&reqs[2]),
        json!({ "method": "torrent-get", "arguments": { "fields": ["labels"] } })
    );
}

#[tokio::test]
async fn invalid_requests_never_reach_the_client() {
    let server = ReplayServer::start(vec![]).await;
    let tr = client(&server);
    let select = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert!(matches!(
        tr.add_magnets(&[magnet(), magnet()], &select).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(matches!(
        tr.add_magnets(&[], &AddOptions::default()).await,
        Err(ClientError::InvalidRequest(_))
    ));
    let bad = AddOptions {
        save_path: Some("/data\n/x".into()),
        ..AddOptions::default()
    };
    assert!(matches!(
        tr.add_magnet(&magnet(), &bad).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn oversized_truncated_or_html_answers_are_protocol_errors() {
    let mut huge = f("session_get");
    huge.response.body = format!("{{\"x\":\"{}\"}}", "a".repeat(2 << 20));
    let mut truncated = f("session_get");
    truncated.response.body.truncate(30);
    let mut html = f("session_get");
    html.response.body = "<html><body>Sign in to your proxy</body></html>".into();
    let mut no_version = f("session_get");
    no_version.response.body = r#"{"arguments":{},"result":"success"}"#.into();
    for bad in [huge, truncated, html, no_version] {
        let server = ReplayServer::start(vec![f("session_conflict"), bad]).await;
        assert!(matches!(
            client(&server).test().await,
            Err(ClientError::Protocol(_))
        ));
        server.finish();
    }
    // A 409 without a usable id is the proxy's, not Transmission's.
    let mut bad_409 = f("session_conflict");
    bad_409
        .response
        .headers
        .retain(|(k, _)| k != "x-transmission-session-id");
    let server = ReplayServer::start(vec![bad_409]).await;
    assert!(matches!(
        client(&server).test().await,
        Err(ClientError::Protocol(_))
    ));
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let tr = Transmission::new(&format!("http://127.0.0.1:{port}/"), auth()).unwrap();
    assert_eq!(tr.test().await.unwrap_err(), ClientError::Unreachable);
}

#[tokio::test]
async fn a_hostile_file_count_is_refused() {
    let mut files = f("files");
    let list = format!("[{}{{}}]", "{},".repeat(1_000_001));
    files.response.body =
        format!(r#"{{"arguments":{{"torrents":[{{"files":{list}}}]}},"result":"success"}}"#);
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_file_paused"),
        files,
    ])
    .await;
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
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("torrent_add_magnet"),
        f("torrent_add_invalid"),
    ])
    .await;
    assert_eq!(
        client(&server)
            .add_magnets(&[magnet(), other], &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Partial {
            sent: 1,
            cause: Box::new(ClientError::Rejected("unrecognized info".into())),
        }
    );
    server.finish();
}

#[tokio::test]
async fn a_refused_label_on_rpc_16_keeps_the_added_torrent() {
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("session_get_rpc16"),
        f("torrent_add_magnet"),
        f("unknown_method"),
    ])
    .await;
    let opts = AddOptions {
        category: Some("Movies".into()),
        ..AddOptions::default()
    };
    let out = client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.sent, 1);
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
async fn no_password_or_session_id_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let server = ReplayServer::start(vec![
        f("session_conflict"),
        f("session_get"),
        f("session_expired"),
        f("labels"),
        f("torrent_add_invalid"),
    ])
    .await;
    let tr = client(&server);
    tr.test().await.unwrap();
    tr.categories().await.unwrap();
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    let err = tr
        .add_torrent_file(&file, &AddOptions::default())
        .await
        .unwrap_err();
    let failed = ReplayServer::start(vec![f("login_fails")]).await;
    let bad = client(&failed).test().await.unwrap_err();
    let refused = ReplayServer::start(vec![f("session_conflict"), f("session_expired")]).await;
    let refused = client(&refused).test().await.unwrap_err();

    let config = ClientConfig::Transmission {
        url: format!("{}?token={QUERY_TOKEN}", server.url()),
        auth: auth(),
    };
    let boxed = connect(config.clone()).unwrap();
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(logged.contains("client=transmission"), "capture works");
    for text in [
        logged,
        format!("{tr:?}"),
        format!("{boxed:?}"),
        format!("{config:?}"),
        format!("{err} {err:?}"),
        format!("{bad} {bad:?}"),
        format!("{refused} {refused:?}"),
    ] {
        assert!(!text.contains(QUERY_TOKEN), "query token leaked: {text}");
        assert!(!text.contains(PASSWORD), "password leaked: {text}");
        assert!(
            !text.contains("Y29ycmVjdC1ob3Jz"),
            "basic auth leaked: {text}"
        );
        for id in ["session_conflict", "session_expired"] {
            assert!(!text.contains(&session_id(id)), "session id leaked: {text}");
        }
    }
}

/// Once against a running daemon:
/// `LODESTAR_LIVE_TRANSMISSION=http://127.0.0.1:9091/ LODESTAR_LIVE_USER=…
/// LODESTAR_LIVE_PASSWORD=… cargo test --test transmission -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a running Transmission"]
async fn live() {
    let url = std::env::var("LODESTAR_LIVE_TRANSMISSION").unwrap();
    let auth = std::env::var("LODESTAR_LIVE_USER").ok().map(|u| BasicAuth {
        username: u,
        password: Secret::new(std::env::var("LODESTAR_LIVE_PASSWORD").unwrap()),
    });
    let tr = Transmission::new(&url, auth)
        .unwrap()
        .with_metadata_wait(Duration::from_secs(5), Duration::from_millis(250));
    println!("test: {:?}", tr.test().await);
    let opts = AddOptions {
        category: Some("Lodestar".into()),
        save_path: Some("/tmp/lodestar-live/transmission".into()),
        paused: true,
        ..AddOptions::default()
    };
    println!("add magnet: {:?}", tr.add_magnet(&magnet(), &opts).await);
    println!("add again: {:?}", tr.add_magnet(&magnet(), &opts).await);
    let select = AddOptions {
        selected_files: Some(vec![0, 2]),
        ..AddOptions::default()
    };
    println!(
        "add file, files 0 and 2: {:?}",
        tr.add_torrent_file(&torrent(), &select).await
    );
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{}", "1".repeat(40))).unwrap();
    println!(
        "magnet with a selection: {:?}",
        tr.add_magnet(
            &other,
            &AddOptions {
                selected_files: Some(vec![0]),
                ..AddOptions::default()
            }
        )
        .await
    );
    println!("categories: {:?}", tr.categories().await);
}
