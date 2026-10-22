//! The qBittorrent adapter against responses recorded from qBittorrent 4.6.3
//! (WebUI API 2.9.3). No network: a local replay server plays them back.

mod support;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::clients::qbittorrent::QBittorrent;
use lodestar_integrations::{
    AddOptions, AuthFailure, Category, ClientError, ClientKind, DownloadClient, Magnet, Secret,
    Selection, TorrentFile,
};
use support::{Fixture, ReplayServer, fixture, fixture_bytes};

const PASSWORD: &str = "correct-horse-battery";
const SID: &str = "SID=I2qCXYtOVgSWxVHIylT1bZt5a/Hf5Bvr";
const FIXTURE_HASH: &str = "f16304349815c2941fe00ababb5fe4e9098e0ee4";
const MAGNET_HASH: &str = "c9e15763f722f23e98a29decdfae341b98d53056";

fn f(name: &str) -> Fixture {
    fixture("qbittorrent", name)
}

fn client(server: &ReplayServer) -> QBittorrent {
    QBittorrent::new(&server.url(), "admin".into(), Secret::new(PASSWORD))
        .unwrap()
        .with_metadata_wait(Duration::from_millis(200), Duration::from_millis(1))
}

fn magnet() -> Magnet {
    Magnet::parse(&format!(
        "magnet:?xt=urn:btih:{MAGNET_HASH}&dn=Lodestar+Magnet+Fixture"
    ))
    .unwrap()
}

#[tokio::test]
async fn test_logs_in_and_reports_the_version() {
    let server = ReplayServer::start(vec![f("login_ok"), f("version"), f("webapi_version")]).await;
    let info = client(&server).test().await.unwrap();
    assert_eq!(info.kind, ClientKind::QBittorrent);
    assert_eq!(info.version, "v4.6.3");
    assert_eq!(info.api_version.as_deref(), Some("2.9.3"));

    let reqs = server.finish();
    let login = &reqs[0];
    assert_eq!(login.path, "/api/v2/auth/login");
    assert_eq!(login.form_field("username").as_deref(), Some("admin"));
    assert_eq!(login.form_field("password").as_deref(), Some(PASSWORD));
    for req in &reqs {
        // qBittorrent's CSRF check: Origin and Referer match its host.
        assert_eq!(req.header("origin"), Some(server.origin().as_str()));
        assert_eq!(req.header("referer"), Some(server.url().as_str()));
    }
    assert_eq!(login.header("cookie"), None);
    assert_eq!(reqs[1].header("cookie"), Some(SID));
    assert_eq!(reqs[2].header("cookie"), Some(SID));
}

#[tokio::test]
async fn wrong_credentials_and_bans_are_auth_errors() {
    for (name, want) in [
        ("login_fails", AuthFailure::BadCredentials),
        ("login_banned", AuthFailure::Banned),
    ] {
        let server = ReplayServer::start(vec![f(name)]).await;
        assert_eq!(
            client(&server).test().await.unwrap_err(),
            ClientError::Auth(want),
            "{name}"
        );
        // One login, never retried.
        assert_eq!(server.finish().len(), 1);
    }
}

#[tokio::test]
async fn a_refused_origin_is_reported_as_such() {
    let server = ReplayServer::start(vec![f("login_ok"), f("csrf_foreign_origin")]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::OriginRefused)
    );
    server.finish();
}

#[tokio::test]
async fn an_expired_session_logs_in_again_once() {
    let server = ReplayServer::start(vec![
        f("login_ok"),
        f("forbidden_after_logout"),
        f("login_ok"),
        f("version"),
        f("webapi_version"),
    ])
    .await;
    let info = client(&server).test().await.unwrap();
    assert_eq!(info.version, "v4.6.3");
    let reqs = server.finish();
    assert_eq!(reqs[2].path, "/api/v2/auth/login");
    assert_eq!(reqs[3].header("cookie"), Some(SID));

    // Refused again straight after logging in: stop, don't loop.
    let server = ReplayServer::start(vec![
        f("login_ok"),
        f("forbidden_no_cookie"),
        f("login_ok"),
        f("forbidden_no_cookie"),
    ])
    .await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::SessionRefused)
    );
    server.finish();
}

#[tokio::test]
async fn the_session_is_reused_across_calls() {
    let server = ReplayServer::start(vec![f("login_ok"), f("categories"), f("categories")]).await;
    let qb = client(&server);
    qb.categories().await.unwrap();
    qb.categories().await.unwrap();
    assert_eq!(server.finish().len(), 3);
}

#[tokio::test]
async fn categories_are_listed_by_name() {
    let server = ReplayServer::start(vec![f("login_ok"), f("categories")]).await;
    let cats = client(&server).categories().await.unwrap();
    assert_eq!(
        cats,
        vec![
            Category {
                name: "Movies".into(),
                save_path: Some("/data/movies".into()),
            },
            Category {
                name: "Music".into(),
                save_path: None,
            },
        ]
    );
    server.finish();
}

#[tokio::test]
async fn adds_a_magnet_with_category_and_save_path() {
    let server = ReplayServer::start(vec![f("login_ok"), f("add_magnet_ok")]).await;
    let opts = AddOptions {
        category: Some("Movies".into()),
        save_path: Some("/data/movies".into()),
        ..AddOptions::default()
    };
    let out = client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.sent, 1);
    assert_eq!(out.selection, Selection::None);
    let reqs = server.finish();
    let add = &reqs[1];
    assert_eq!(add.path, "/api/v2/torrents/add");
    assert!(
        add.header("content-type")
            .unwrap()
            .starts_with("multipart/form-data")
    );
    assert_eq!(
        add.multipart_field("urls"),
        Some(magnet().as_str().to_owned())
    );
    assert_eq!(add.multipart_field("category").as_deref(), Some("Movies"));
    assert_eq!(
        add.multipart_field("savepath").as_deref(),
        Some("/data/movies")
    );
    assert_eq!(add.multipart_field("paused"), None);
    assert_eq!(add.header("cookie"), Some(SID));
}

#[tokio::test]
async fn bulk_send_puts_one_magnet_per_line_and_paused_sets_both_flags() {
    let server = ReplayServer::start(vec![f("login_ok"), f("add_magnet_ok")]).await;
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{FIXTURE_HASH}")).unwrap();
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
        reqs[1].multipart_field("urls"),
        Some(format!("{}\n{}", magnet().as_str(), other.as_str()))
    );
    assert_eq!(reqs[1].multipart_field("paused").as_deref(), Some("true"));
    assert_eq!(reqs[1].multipart_field("stopped").as_deref(), Some("true"));
}

#[tokio::test]
async fn refused_adds_are_rejections_with_the_clients_reason() {
    let server = ReplayServer::start(vec![f("login_ok"), f("add_fails")]).await;
    assert!(matches!(
        client(&server)
            .add_magnet(&magnet(), &AddOptions::default())
            .await,
        Err(ClientError::Rejected(_))
    ));
    server.finish();

    let server = ReplayServer::start(vec![f("login_ok"), f("add_torrent_invalid")]).await;
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    assert_eq!(
        client(&server)
            .add_torrent_file(&file, &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("HTTP 415: Error: 'x.torrent' is not a valid torrent file.".into())
    );
    server.finish();
}

#[tokio::test]
async fn selected_files_of_a_torrent_file() {
    let server = ReplayServer::start(vec![
        f("login_ok"),
        f("add_torrent_ok"),
        // Registered asynchronously: the first look may not find it.
        f("files_unknown"),
        f("files"),
        f("file_prio"),
        f("start_missing_v4"),
        f("resume"),
    ])
    .await;
    let bytes = fixture_bytes("qbittorrent", "fixture.torrent");
    let file = TorrentFile::new(bytes.clone(), FIXTURE_HASH).unwrap();
    let opts = AddOptions {
        category: Some("Music".into()),
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
    let add = &reqs[1];
    // Added paused, so nothing downloads before the selection applies.
    assert_eq!(add.multipart_field("paused").as_deref(), Some("true"));
    assert_eq!(add.multipart_field("stopped").as_deref(), Some("true"));
    let body = add.body.as_slice();
    assert!(body.windows(bytes.len()).any(|w| w == bytes.as_slice()));
    assert!(add.body_text().contains(
        "name=\"torrents\"; filename=\"lodestar.torrent\"\r\nContent-Type: application/x-bittorrent"
    ));
    assert_eq!(
        reqs[2].query.as_deref(),
        Some(format!("hash={FIXTURE_HASH}").as_str())
    );
    let prio = &reqs[4];
    assert_eq!(prio.form_field("hash").as_deref(), Some(FIXTURE_HASH));
    assert_eq!(prio.form_field("id").as_deref(), Some("1"));
    assert_eq!(prio.form_field("priority").as_deref(), Some("0"));
    assert_eq!(reqs[6].form_field("hashes").as_deref(), Some(FIXTURE_HASH));
}

#[tokio::test]
async fn selecting_every_file_sets_no_priorities_and_paused_stays_paused() {
    let server = ReplayServer::start(vec![f("login_ok"), f("add_torrent_ok"), f("files")]).await;
    let file = TorrentFile::new(
        fixture_bytes("qbittorrent", "fixture.torrent"),
        FIXTURE_HASH,
    )
    .unwrap();
    let opts = AddOptions {
        selected_files: Some(vec![0, 1, 2]),
        paused: true,
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_torrent_file(&file, &opts)
        .await
        .unwrap();
    assert_eq!(out.selection, Selection::Applied);
    server.finish();
}

#[tokio::test]
async fn a_magnet_without_metadata_in_time_is_left_paused() {
    let server = ReplayServer::start(vec![
        f("login_ok"),
        f("add_magnet_ok"),
        f("files_magnet_no_metadata"),
    ])
    .await;
    let qb = QBittorrent::new(&server.url(), "admin".into(), Secret::new(PASSWORD))
        .unwrap()
        .with_metadata_wait(Duration::ZERO, Duration::from_millis(1));
    let opts = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    let out = qb.add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.selection, Selection::Pending);
    let reqs = server.finish();
    assert_eq!(
        reqs[2].query.as_deref(),
        Some(format!("hash={MAGNET_HASH}").as_str())
    );
}

#[tokio::test]
async fn a_selection_past_the_last_file_is_refused_before_priorities() {
    let server = ReplayServer::start(vec![f("login_ok"), f("add_torrent_ok"), f("files")]).await;
    let file = TorrentFile::new(
        fixture_bytes("qbittorrent", "fixture.torrent"),
        FIXTURE_HASH,
    )
    .unwrap();
    let opts = AddOptions {
        selected_files: Some(vec![0, 3]),
        ..AddOptions::default()
    };
    assert!(matches!(
        client(&server).add_torrent_file(&file, &opts).await,
        Err(ClientError::InvalidRequest(_))
    ));
    server.finish();
}

#[tokio::test]
async fn invalid_requests_never_reach_the_client() {
    let server = ReplayServer::start(vec![]).await;
    let qb = client(&server);
    let select = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert!(matches!(
        qb.add_magnets(&[magnet(), magnet()], &select).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(matches!(
        qb.add_magnets(&[], &AddOptions::default()).await,
        Err(ClientError::InvalidRequest(_))
    ));
    let bad_category = AddOptions {
        category: Some("tv\nshows".into()),
        ..AddOptions::default()
    };
    assert!(matches!(
        qb.add_magnet(&magnet(), &bad_category).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn oversized_or_malformed_answers_are_protocol_errors() {
    let mut huge = f("categories");
    huge.response.body = format!("{{\"x\":\"{}\"}}", "a".repeat(2 << 20));
    let server = ReplayServer::start(vec![f("login_ok"), huge]).await;
    assert!(matches!(
        client(&server).categories().await,
        Err(ClientError::Protocol(_))
    ));

    let mut broken = f("categories");
    broken.response.body = "{\"Movies\":".into();
    let server = ReplayServer::start(vec![f("login_ok"), broken]).await;
    assert_eq!(
        client(&server).categories().await.unwrap_err(),
        ClientError::Protocol("the category list")
    );

    let mut not_version = f("version");
    not_version.response.body = "<html>proxy login</html>".into();
    let server = ReplayServer::start(vec![f("login_ok"), not_version]).await;
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
    let qb = QBittorrent::new(
        &format!("http://127.0.0.1:{port}/"),
        "admin".into(),
        Secret::new(PASSWORD),
    )
    .unwrap();
    assert_eq!(qb.test().await.unwrap_err(), ClientError::Unreachable);
}

/// Captures every log line, at every level, from every crate, for the whole
/// test binary. Global, because a thread-local subscriber loses events when
/// other test threads cache a callsite as uninteresting.
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
async fn no_password_or_session_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let server = ReplayServer::start(vec![
        f("login_ok"),
        f("forbidden_after_logout"),
        f("login_ok"),
        f("version"),
        f("webapi_version"),
        f("add_fails"),
    ])
    .await;
    let qb = client(&server);
    qb.test().await.unwrap();
    let err = qb
        .add_magnet(&magnet(), &AddOptions::default())
        .await
        .unwrap_err();
    let failed_login = ReplayServer::start(vec![f("login_fails")]).await;
    let bad = client(&failed_login).test().await.unwrap_err();

    let config = lodestar_integrations::ClientConfig::QBittorrent {
        url: server.url(),
        username: "admin".into(),
        password: Secret::new(PASSWORD),
    };
    let boxed = lodestar_integrations::clients::connect(config.clone()).unwrap();
    let sid_value = SID.split_once('=').unwrap().1;
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(
        logged.contains("qBittorrent request"),
        "capture works: {logged}"
    );
    for text in [
        logged,
        format!("{qb:?}"),
        format!("{boxed:?}"),
        format!("{config:?}"),
        format!("{err} {err:?}"),
        format!("{bad} {bad:?}"),
    ] {
        assert!(!text.contains(PASSWORD), "password leaked: {text}");
        assert!(!text.contains(sid_value), "session leaked: {text}");
    }
}
