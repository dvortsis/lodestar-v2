//! The Deluge adapter against responses recorded from Deluge 2.1.2's Web UI.
//! No network: a local replay server plays them back.
//!
//! The adapter numbers its requests from 1 and checks each answer's id, so
//! [`script`] rewrites the recorded ids to the ones a fresh adapter sends.

mod support;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::clients::deluge::Deluge;
use lodestar_integrations::clients::{ClientConfig, connect};
use lodestar_integrations::{
    AddOptions, AuthFailure, Category, ClientError, ClientKind, DownloadClient, Feature, Magnet,
    Secret, Selection, TorrentFile,
};
use serde_json::{Value, json};
use support::{Fixture, Received, ReplayServer, fixture, fixture_bytes};

const PASSWORD: &str = "correct-horse-battery";
const FIXTURE_HASH: &str = "f16304349815c2941fe00ababb5fe4e9098e0ee4";
const MAGNET_HASH: &str = "c9e15763f722f23e98a29decdfae341b98d53056";

fn f(name: &str) -> Fixture {
    fixture("deluge", name)
}

/// The fixtures, with answer ids 1, 2, … in order.
fn script(names: &[&str]) -> Vec<Fixture> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| with_id(f(name), json!(i + 1)))
        .collect()
}

fn with_id(mut fx: Fixture, id: Value) -> Fixture {
    let mut body: Value = serde_json::from_str(&fx.response.body).unwrap();
    if !body["id"].is_null() {
        body["id"] = id;
    }
    fx.response.body = body.to_string();
    fx
}

/// A login to a Web UI that's already connected to its daemon.
const LOGIN: [&str; 2] = ["login_ok", "connected_true"];

fn with_login(rest: &[&str]) -> Vec<Fixture> {
    let names: Vec<&str> = LOGIN.iter().chain(rest).copied().collect();
    script(&names)
}

fn client(server: &ReplayServer) -> Deluge {
    Deluge::new(&server.url(), Secret::new(PASSWORD))
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

fn call(req: &Received) -> (String, Value) {
    let b = body(req);
    (
        b["method"].as_str().unwrap().to_owned(),
        b["params"].clone(),
    )
}

fn cookie() -> String {
    let fx = f("login_ok");
    let header = &fx
        .response
        .headers
        .iter()
        .find(|(k, _)| k == "set-cookie")
        .unwrap()
        .1;
    header.split(';').next().unwrap().to_owned()
}

#[tokio::test]
async fn test_logs_in_connects_the_web_ui_and_reports_the_version() {
    let server = ReplayServer::start(script(&[
        "login_ok",
        "connected_false",
        "get_hosts",
        "connect",
        "connected_true",
        "version",
    ]))
    .await;
    let info = client(&server).test().await.unwrap();
    assert_eq!(info.kind, ClientKind::Deluge);
    assert_eq!(info.version, "2.1.2.dev0");
    assert_eq!(info.api_version, None);

    let reqs = server.finish();
    assert_eq!(reqs[0].path, "/json");
    assert_eq!(call(&reqs[0]), ("auth.login".into(), json!([PASSWORD])));
    assert_eq!(reqs[0].header("cookie"), None);
    assert_eq!(
        call(&reqs[3]),
        (
            "web.connect".into(),
            json!(["aeb5867e480141ceabae6294403ecd6c"])
        )
    );
    for (i, req) in reqs.iter().enumerate() {
        assert_eq!(body(req)["id"], json!(i + 1));
        assert_eq!(req.header("content-type"), Some("application/json"));
        if i > 0 {
            assert_eq!(req.header("cookie"), Some(cookie().as_str()));
        }
    }
}

#[tokio::test]
async fn a_wrong_password_is_an_auth_error() {
    let server = ReplayServer::start(script(&["login_fails"])).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::BadCredentials)
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn a_web_ui_that_cant_reach_its_daemon_is_reported() {
    let mut failed = script(&[
        "login_ok",
        "connected_false",
        "get_hosts",
        "connect",
        "connected_false",
    ]);
    // web.connect answers null, not an error, when the daemon is down.
    failed[3].response.body = r#"{"result": null, "error": null, "id": 4}"#.into();
    let server = ReplayServer::start(failed).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Rejected("Deluge's Web UI couldn't connect to its daemon".into())
    );
    server.finish();

    let mut none = script(&["login_ok", "connected_false", "get_hosts"]);
    none[2].response.body = r#"{"result": [], "error": null, "id": 3}"#.into();
    let server = ReplayServer::start(none).await;
    assert!(matches!(
        client(&server).test().await,
        Err(ClientError::Rejected(_))
    ));
    server.finish();
}

#[tokio::test]
async fn an_expired_session_logs_in_again_once() {
    let server = ReplayServer::start(with_login(&[
        "version",
        "not_authenticated",
        "login_ok",
        "connected_true",
        "version",
    ]))
    .await;
    let dl = client(&server);
    dl.test().await.unwrap();
    assert_eq!(dl.test().await.unwrap().version, "2.1.2.dev0");
    let reqs = server.finish();
    assert_eq!(call(&reqs[4]).0, "auth.login");
    assert_eq!(reqs[4].header("cookie"), None);

    // Refused again straight after logging in: stop, don't loop.
    let server = ReplayServer::start(with_login(&[
        "not_authenticated",
        "login_ok",
        "connected_true",
        "not_authenticated",
    ]))
    .await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::SessionRefused)
    );
    server.finish();
}

#[tokio::test]
async fn adds_a_magnet_creating_its_label_first() {
    let server = ReplayServer::start(with_login(&[
        "plugins_label",
        "labels_empty",
        "label_add",
        "add_magnet_ok",
        "label_set_torrent",
    ]))
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
    // Deluge's labels are lowercase.
    assert_eq!(call(&reqs[4]), ("label.add".into(), json!(["movies"])));
    assert_eq!(
        call(&reqs[5]),
        (
            "core.add_torrent_magnet".into(),
            json!([magnet().as_str(), { "download_location": "/data/movies" }])
        )
    );
    assert_eq!(
        call(&reqs[6]),
        ("label.set_torrent".into(), json!([MAGNET_HASH, "movies"]))
    );
}

#[tokio::test]
async fn an_existing_label_is_reused() {
    let server = ReplayServer::start(with_login(&[
        "plugins_label",
        "labels",
        "add_magnet_ok",
        "label_set_torrent",
    ]))
    .await;
    let opts = AddOptions {
        category: Some("movies".into()),
        ..AddOptions::default()
    };
    client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    server.finish();
}

#[tokio::test]
async fn labels_need_the_label_plugin() {
    let server = ReplayServer::start(with_login(&["plugins_none"])).await;
    let dl = client(&server);
    let opts = AddOptions {
        category: Some("movies".into()),
        ..AddOptions::default()
    };
    // Refused before anything is added.
    assert_eq!(
        dl.add_magnet(&magnet(), &opts).await.unwrap_err(),
        ClientError::Unsupported(Feature::Category)
    );
    server.finish();

    let server = ReplayServer::start(with_login(&["plugins_none"])).await;
    assert_eq!(
        client(&server).categories().await.unwrap_err(),
        ClientError::Unsupported(Feature::Category)
    );
    server.finish();
}

#[tokio::test]
async fn a_refused_label_adds_nothing() {
    let server = ReplayServer::start(with_login(&[
        "plugins_label",
        "labels_empty",
        "label_add_invalid",
    ]))
    .await;
    let opts = AddOptions {
        category: Some("bad label!".into()),
        ..AddOptions::default()
    };
    assert_eq!(
        client(&server)
            .add_magnet(&magnet(), &opts)
            .await
            .unwrap_err(),
        ClientError::Rejected("Invalid label, valid characters:[a-z0-9_-]".into())
    );
    server.finish();
}

#[tokio::test]
async fn a_duplicate_is_rejected_and_bulk_adds_one_at_a_time() {
    let server = ReplayServer::start(with_login(&["add_magnet_duplicate"])).await;
    assert_eq!(
        client(&server)
            .add_magnet(&magnet(), &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("the torrent is already in the client".into())
    );
    server.finish();

    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{FIXTURE_HASH}")).unwrap();
    let server = ReplayServer::start(with_login(&["add_magnet_ok", "add_magnet_duplicate"])).await;
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
    assert_eq!(
        call(&reqs[3]).1,
        json!([other.as_str(), { "add_paused": true }])
    );
}

#[tokio::test]
async fn refusals_carry_the_daemons_reason() {
    let server = ReplayServer::start(with_login(&["add_file_invalid"])).await;
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    assert_eq!(
        client(&server)
            .add_torrent_file(&file, &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected(
            "Unable to add torrent, decoding filedump failed: expected value (list, dict, int or string) in bencoded string [bdecode:4]"
                .into()
        )
    );
    let reqs = server.finish();
    assert_eq!(
        call(&reqs[2]),
        (
            "core.add_torrent_file".into(),
            json!(["lodestar.torrent", "Z2FyYmFnZQ==", {}])
        )
    );

    let server = ReplayServer::start(with_login(&["unknown_method"])).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Rejected("Deluge doesn't know daemon.get_version".into())
    );
    server.finish();
}

#[tokio::test]
async fn selected_files_of_a_torrent_file() {
    let server = ReplayServer::start(with_login(&[
        "add_file_paused",
        // Registered asynchronously: the first look may not find it.
        "files_unknown",
        "files",
        "set_priorities",
        "resume",
    ]))
    .await;
    let opts = AddOptions {
        selected_files: Some(vec![2, 0]),
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_torrent_file(&torrent(), &opts)
        .await
        .unwrap();
    assert_eq!(out.selection, Selection::Applied);
    let reqs = server.finish();
    let (method, params) = call(&reqs[2]);
    assert_eq!(method, "core.add_torrent_file");
    // Added paused, so nothing downloads before the selection applies.
    assert_eq!(params[2], json!({ "add_paused": true }));
    assert_eq!(
        call(&reqs[3]),
        (
            "core.get_torrent_status".into(),
            json!([FIXTURE_HASH, ["files", "file_priorities"]])
        )
    );
    assert_eq!(
        call(&reqs[5]),
        (
            "core.set_torrent_options".into(),
            json!([[FIXTURE_HASH], { "file_priorities": [4, 0, 4] }])
        )
    );
    assert_eq!(
        call(&reqs[6]),
        ("core.resume_torrent".into(), json!([[FIXTURE_HASH]]))
    );
}

#[tokio::test]
async fn existing_priorities_are_kept_for_selected_files() {
    let mut files = f("files");
    let mut b: Value = serde_json::from_str(&files.response.body).unwrap();
    b["result"]["file_priorities"] = json!([7, 1, 0]);
    files.response.body = b.to_string();
    let mut fixtures = with_login(&["add_file_paused"]);
    fixtures.push(with_id(files, json!(4)));
    fixtures.push(with_id(f("set_priorities"), json!(5)));
    let server = ReplayServer::start(fixtures).await;
    let opts = AddOptions {
        selected_files: Some(vec![0, 2]),
        paused: true,
        ..AddOptions::default()
    };
    client(&server)
        .add_torrent_file(&torrent(), &opts)
        .await
        .unwrap();
    let reqs = server.finish();
    assert_eq!(call(&reqs[4]).1[1], json!({ "file_priorities": [7, 0, 4] }));
}

#[tokio::test]
async fn selecting_every_file_sets_nothing_and_paused_stays_paused() {
    let server = ReplayServer::start(with_login(&["add_file_paused", "files"])).await;
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
async fn a_magnet_without_metadata_in_time_is_left_paused() {
    let server =
        ReplayServer::start(with_login(&["add_magnet_ok", "files_magnet_no_metadata"])).await;
    let dl = Deluge::new(&server.url(), Secret::new(PASSWORD))
        .unwrap()
        .with_metadata_wait(Duration::ZERO, Duration::from_millis(1));
    let opts = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    let out = dl.add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.selection, Selection::Pending);
    let reqs = server.finish();
    assert_eq!(call(&reqs[2]).1[1], json!({ "add_paused": true }));
}

#[tokio::test]
async fn a_selection_past_the_last_file_is_refused_before_changes() {
    let server = ReplayServer::start(with_login(&["add_file_paused", "files"])).await;
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
async fn categories_are_the_label_plugins_labels() {
    let server = ReplayServer::start(with_login(&["plugins_label", "labels"])).await;
    assert_eq!(
        client(&server).categories().await.unwrap(),
        vec![Category {
            name: "movies".into(),
            save_path: None,
        }]
    );
    server.finish();
}

#[tokio::test]
async fn invalid_requests_never_reach_the_client() {
    let server = ReplayServer::start(vec![]).await;
    let dl = client(&server);
    let select = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert!(matches!(
        dl.add_magnets(&[magnet(), magnet()], &select).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(matches!(
        dl.add_magnets(&[], &AddOptions::default()).await,
        Err(ClientError::InvalidRequest(_))
    ));
    let bad = AddOptions {
        category: Some("tv\nshows".into()),
        ..AddOptions::default()
    };
    assert!(matches!(
        dl.add_magnet(&magnet(), &bad).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn mismatched_ids_and_bad_answers_are_protocol_errors() {
    let mut fixtures = with_login(&[]);
    fixtures.push(with_id(f("version"), json!(99)));
    let server = ReplayServer::start(fixtures).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Protocol("the answer is for another request")
    );

    let mut huge = f("version");
    huge.response.body = format!("{{\"result\":\"{}\",\"id\":3}}", "a".repeat(2 << 20));
    let mut truncated = with_id(f("version"), json!(3));
    truncated.response.body.truncate(20);
    let mut html = f("version");
    html.response.body = "<html><body>Deluge is starting</body></html>".into();
    // The Web UI couldn't read a request: its message quotes the request.
    let mut unreadable = f("version");
    unreadable.response.body = format!(
        r#"{{"result": null, "id": null, "error": {{"code": 5, "message": "JSONException: Invalid JSON request, missing param 'id' in {{'params': ['{PASSWORD}']}}"}}}}"#
    );
    for bad in [huge, truncated, html, unreadable] {
        let mut fixtures = with_login(&[]);
        fixtures.push(bad);
        let server = ReplayServer::start(fixtures).await;
        let err = client(&server).test().await.unwrap_err();
        assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
        assert!(!err.to_string().contains(PASSWORD));
        server.finish();
    }
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dl = Deluge::new(&format!("http://127.0.0.1:{port}/"), Secret::new(PASSWORD)).unwrap();
    assert_eq!(dl.test().await.unwrap_err(), ClientError::Unreachable);
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
        buf
    }))
}

#[tokio::test]
async fn no_password_or_cookie_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let server = ReplayServer::start(with_login(&[
        "version",
        "not_authenticated",
        "login_ok",
        "connected_true",
        "version",
        "add_file_invalid",
    ]))
    .await;
    let dl = client(&server);
    dl.test().await.unwrap();
    dl.test().await.unwrap();
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    let err = dl
        .add_torrent_file(&file, &AddOptions::default())
        .await
        .unwrap_err();
    let failed = ReplayServer::start(script(&["login_fails"])).await;
    let bad = client(&failed).test().await.unwrap_err();

    let config = ClientConfig::Deluge {
        url: server.url(),
        password: Secret::new(PASSWORD),
    };
    let boxed = connect(config.clone()).unwrap();
    let session = cookie().split_once('=').unwrap().1.to_owned();
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(logged.contains("Deluge request"), "capture works");
    for text in [
        logged,
        format!("{dl:?}"),
        format!("{boxed:?}"),
        format!("{config:?}"),
        format!("{err} {err:?}"),
        format!("{bad} {bad:?}"),
    ] {
        assert!(!text.contains(PASSWORD), "password leaked: {text}");
        assert!(!text.contains(&session), "cookie leaked: {text}");
    }
}

/// Once against a running Web UI:
/// `LODESTAR_LIVE_DELUGE=http://127.0.0.1:8112/ LODESTAR_LIVE_PASSWORD=…
/// cargo test --test deluge -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a running Deluge"]
async fn live() {
    let url = std::env::var("LODESTAR_LIVE_DELUGE").unwrap();
    let password = Secret::new(std::env::var("LODESTAR_LIVE_PASSWORD").unwrap());
    let dl = Deluge::new(&url, password)
        .unwrap()
        .with_metadata_wait(Duration::from_secs(5), Duration::from_millis(250));
    println!("test: {:?}", dl.test().await);
    println!("categories: {:?}", dl.categories().await);
    let opts = AddOptions {
        category: Some("Lodestar".into()),
        save_path: Some("/tmp/lodestar-live/deluge".into()),
        paused: true,
        ..AddOptions::default()
    };
    println!("add magnet: {:?}", dl.add_magnet(&magnet(), &opts).await);
    println!("add again: {:?}", dl.add_magnet(&magnet(), &opts).await);
    let select = AddOptions {
        selected_files: Some(vec![0, 2]),
        ..AddOptions::default()
    };
    println!(
        "add file, files 0 and 2: {:?}",
        dl.add_torrent_file(&torrent(), &select).await
    );
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{}", "1".repeat(40))).unwrap();
    println!(
        "magnet with a selection: {:?}",
        dl.add_magnet(
            &other,
            &AddOptions {
                selected_files: Some(vec![0]),
                ..AddOptions::default()
            }
        )
        .await
    );
    println!("categories: {:?}", dl.categories().await);
}
