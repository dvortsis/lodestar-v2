//! The rTorrent adapter against XML-RPC answers recorded from rTorrent 0.9.8
//! behind nginx. No network: a local replay server plays them back.

mod support;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::clients::rtorrent::RTorrent;
use lodestar_integrations::clients::{BasicAuth, ClientConfig, connect};
use lodestar_integrations::{
    AddOptions, AuthFailure, Category, ClientError, ClientKind, DownloadClient, Feature, Magnet,
    Secret, Selection, TorrentFile,
};
use support::{Fixture, Received, ReplayServer, fixture, fixture_bytes, refused_addr};

const PASSWORD: &str = "correct-horse-battery";
const FIXTURE_HASH: &str = "F16304349815C2941FE00ABABB5FE4E9098E0EE4";
const MAGNET_HASH: &str = "C9E15763F722F23E98A29DECDFAE341B98D53056";

/// A token in the configured URL's query: dropped, never printed.
const QUERY_TOKEN: &str = "query-token-8c1f";

fn f(name: &str) -> Fixture {
    fixture("rtorrent", name)
}

fn script(names: &[&str]) -> Vec<Fixture> {
    names.iter().map(|n| f(n)).collect()
}

fn auth() -> Option<BasicAuth> {
    Some(BasicAuth {
        username: "lodestar".into(),
        password: Secret::new(PASSWORD),
    })
}

fn client(server: &ReplayServer) -> RTorrent {
    RTorrent::new(&format!("{}RPC2", server.url()), auth())
        .unwrap()
        .with_metadata_wait(Duration::from_millis(200), Duration::from_millis(1))
}

fn magnet() -> Magnet {
    Magnet::parse(&format!(
        "magnet:?xt=urn:btih:{}&dn=Lodestar+Magnet+Fixture",
        MAGNET_HASH.to_ascii_lowercase()
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

/// The method name and the `<value>` contents of a request, in order.
fn call(req: &Received) -> (String, Vec<String>) {
    let body = req.body_text();
    let method = between(&body, "<methodName>", "</methodName>")
        .unwrap()
        .to_owned();
    let params = body
        .split("<param>")
        .skip(1)
        .map(|p| {
            p.trim_end_matches("</params></methodCall>")
                .trim_end_matches("</param>")
                .to_owned()
        })
        .collect();
    (method, params)
}

fn between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = s.find(start)? + start.len();
    Some(&s[from..from + s[from..].find(end)?])
}

fn string(v: &str) -> String {
    format!("<value><string>{v}</string></value>")
}

#[tokio::test]
async fn test_reports_the_client_and_library_versions() {
    let server = ReplayServer::start(script(&["client_version", "library_version"])).await;
    let info = client(&server).test().await.unwrap();
    assert_eq!(info.kind, ClientKind::RTorrent);
    assert_eq!(info.version, "0.9.8");
    assert_eq!(info.api_version.as_deref(), Some("0.13.8"));
    let reqs = server.finish();
    assert_eq!(reqs[0].path, "/RPC2");
    assert_eq!(call(&reqs[0]), ("system.client_version".into(), vec![]));
    assert_eq!(call(&reqs[1]).0, "system.library_version");
    for req in &reqs {
        assert_eq!(req.header("content-type"), Some("text/xml"));
        assert_eq!(
            req.header("authorization"),
            Some("Basic bG9kZXN0YXI6Y29ycmVjdC1ob3JzZS1iYXR0ZXJ5")
        );
    }
}

#[tokio::test]
async fn the_web_servers_refusals_are_auth_errors() {
    let server = ReplayServer::start(script(&["login_fails"])).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::BadCredentials)
    );
    assert_eq!(server.finish().len(), 1);

    let mut forbidden = f("login_fails");
    forbidden.response.status = 403;
    let server = ReplayServer::start(vec![forbidden]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::AddressRefused)
    );
}

#[tokio::test]
async fn adds_a_magnet_with_its_path_and_label_as_load_commands() {
    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_start_magnet",
        "hash_present",
    ]))
    .await;
    let opts = AddOptions {
        category: Some("Movies".into()),
        save_path: Some("/data/movies, \"new\"".into()),
        ..AddOptions::default()
    };
    let out = client(&server).add_magnet(&magnet(), &opts).await.unwrap();
    assert_eq!(out.sent, 1);
    assert_eq!(out.selection, Selection::None);
    let reqs = server.finish();
    // Not there before; there after.
    assert_eq!(call(&reqs[0]), ("d.hash".into(), vec![string(MAGNET_HASH)]));
    assert_eq!(call(&reqs[2]), ("d.hash".into(), vec![string(MAGNET_HASH)]));
    assert_eq!(
        call(&reqs[1]),
        (
            "load.start".into(),
            vec![
                string(""),
                string(&magnet().as_str().replace('&', "&amp;")),
                string(r#"d.directory.set="/data/movies, \"new\"""#),
                string(r#"d.custom1.set="Movies""#),
            ]
        )
    );
}

#[tokio::test]
async fn paused_loads_without_starting_and_bulk_skips_duplicates() {
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{}", "1".repeat(40))).unwrap();
    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_start_magnet",
        "hash_present",
        "hash_present",
    ]))
    .await;
    let opts = AddOptions {
        paused: true,
        ..AddOptions::default()
    };
    let out = client(&server)
        .add_magnets(&[magnet(), other], &opts)
        .await
        .unwrap();
    assert_eq!(out.sent, 2);
    let reqs = server.finish();
    assert_eq!(call(&reqs[1]).0, "load.normal");
    assert_eq!(
        call(&reqs[3]),
        ("d.hash".into(), vec![string(&"1".repeat(40))])
    );
}

#[tokio::test]
async fn a_duplicate_is_rejected_without_loading() {
    let server = ReplayServer::start(script(&["hash_present"])).await;
    assert_eq!(
        client(&server)
            .add_magnet(&magnet(), &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Rejected("the torrent is already in the client".into())
    );
    assert_eq!(server.finish().len(), 1);
}

#[tokio::test]
async fn a_load_rtorrent_drops_is_a_rejection() {
    let rt = |server: &ReplayServer| {
        RTorrent::new(&format!("{}RPC2", server.url()), auth())
            .unwrap()
            .with_metadata_wait(Duration::ZERO, Duration::from_millis(1))
    };
    // A magnet without trackers while DHT is off: load answers 0, adds nothing.
    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_start_magnet",
        "hash_missing",
    ]))
    .await;
    assert!(matches!(
        rt(&server).add_magnet(&magnet(), &AddOptions::default()).await,
        Err(ClientError::Rejected(why)) if why.contains("DHT")
    ));
    server.finish();

    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_raw_invalid",
        "hash_missing",
    ]))
    .await;
    let file = TorrentFile::new(b"garbage".to_vec(), FIXTURE_HASH).unwrap();
    assert!(matches!(
        rt(&server)
            .add_torrent_file(&file, &AddOptions::default())
            .await,
        Err(ClientError::Rejected(_))
    ));
    let reqs = server.finish();
    assert_eq!(
        call(&reqs[1]),
        (
            "load.raw_start".into(),
            vec![
                string(""),
                "<value><base64>Z2FyYmFnZQ==</base64></value>".into()
            ]
        )
    );
}

#[tokio::test]
async fn selected_files_of_a_torrent_file() {
    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_raw",
        // Registered on rTorrent's next pass: the first look may miss it.
        "hash_missing",
        "hash_present",
        "is_meta_false",
        "size_files",
        "priorities",
        "update_priorities",
        "start",
    ]))
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
    // Loaded without starting, so nothing downloads before the selection.
    let (method, params) = call(&reqs[1]);
    assert_eq!(method, "load.raw");
    assert!(params[1].starts_with("<value><base64>ZDg6YW5ub3VuY2U"));
    assert_eq!(params[2], string(r#"d.directory.set="/data/music""#));
    assert_eq!(
        call(&reqs[4]),
        ("d.is_meta".into(), vec![string(FIXTURE_HASH)])
    );
    assert_eq!(
        call(&reqs[5]),
        ("d.size_files".into(), vec![string(FIXTURE_HASH)])
    );
    let multicall = reqs[6].body_text();
    assert!(multicall.contains("<methodName>system.multicall</methodName>"));
    assert!(multicall.contains(&format!(
        "<value><string>f.priority.set</string></value></member><member><name>params</name><value><array><data><value><string>{FIXTURE_HASH}:f1</string></value><value><i8>0</i8></value></data></array>"
    )));
    assert_eq!(multicall.matches("f.priority.set").count(), 1);
    assert_eq!(
        call(&reqs[7]),
        ("d.update_priorities".into(), vec![string(FIXTURE_HASH)])
    );
    assert_eq!(
        call(&reqs[8]),
        ("d.start".into(), vec![string(FIXTURE_HASH)])
    );
}

#[tokio::test]
async fn selecting_every_file_sets_nothing_and_paused_stays_stopped() {
    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_raw",
        "hash_present",
        "is_meta_false",
        "size_files",
    ]))
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
    let server = ReplayServer::start(script(&[
        "hash_missing",
        "load_raw",
        "hash_present",
        "is_meta_false",
        "size_files",
    ]))
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
async fn a_fault_inside_the_multicall_is_a_rejection() {
    let mut faulted = f("priorities");
    faulted.response.body = "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data><value><struct><member><name>faultCode</name><value><i4>-501</i4></value></member><member><name>faultString</name><value><string>Could not find info-hash.</string></value></member></struct></value></data></array></value></param></params></methodResponse>".into();
    let mut fixtures = script(&[
        "hash_missing",
        "load_raw",
        "hash_present",
        "is_meta_false",
        "size_files",
    ]);
    fixtures.push(faulted);
    let server = ReplayServer::start(fixtures).await;
    // One skipped file: one result, a fault.
    let opts = AddOptions {
        selected_files: Some(vec![0, 1]),
        ..AddOptions::default()
    };
    assert_eq!(
        client(&server)
            .add_torrent_file(&torrent(), &opts)
            .await
            .unwrap_err(),
        ClientError::Rejected("Could not find info-hash.".into())
    );
    server.finish();
}

#[tokio::test]
async fn categories_are_the_decoded_labels_in_use() {
    let mut labels = f("labels");
    labels.response.body = labels
        .response
        .body
        .replace("<string></string>", "<string>TV%20Shows</string>");
    let server = ReplayServer::start(vec![labels, f("labels")]).await;
    let rt = client(&server);
    assert_eq!(
        rt.categories().await.unwrap(),
        vec![
            Category {
                name: "Movies".into(),
                save_path: None,
            },
            Category {
                name: "TV Shows".into(),
                save_path: None,
            },
        ]
    );
    // An empty label is no category.
    assert_eq!(rt.categories().await.unwrap().len(), 1);
    let reqs = server.finish();
    assert_eq!(
        call(&reqs[0]),
        (
            "d.multicall2".into(),
            vec![string(""), string("main"), string("d.custom1=")]
        )
    );
}

#[tokio::test]
async fn invalid_requests_never_reach_the_client() {
    let server = ReplayServer::start(vec![]).await;
    let rt = client(&server);
    for opts in [
        AddOptions {
            save_path: Some("$execute=rm,-rf,/".into()),
            ..AddOptions::default()
        },
        AddOptions {
            category: Some("tv\nshows".into()),
            ..AddOptions::default()
        },
    ] {
        assert!(matches!(
            rt.add_magnet(&magnet(), &opts).await,
            Err(ClientError::InvalidRequest(_))
        ));
    }
    let select = AddOptions {
        selected_files: Some(vec![0]),
        ..AddOptions::default()
    };
    assert!(matches!(
        rt.add_magnets(&[magnet(), magnet()], &select).await,
        Err(ClientError::InvalidRequest(_))
    ));
    assert!(server.finish().is_empty());
}

#[tokio::test]
async fn unknown_methods_and_bad_answers() {
    let mut unknown = f("unknown_method");
    unknown.request.path = "/RPC2".into();
    let server = ReplayServer::start(vec![unknown]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Rejected("Method 'd.frobnicate' not defined".into())
    );

    let mut huge = f("client_version");
    huge.response.body = format!(
        "<methodResponse><params><param><value><string>{}</string></value></param></params></methodResponse>",
        "a".repeat(2 << 20)
    );
    let mut truncated = f("client_version");
    truncated.response.body.truncate(60);
    let mut html = f("client_version");
    html.response.body = "<html><body>ruTorrent login</body></html>".into();
    let mut doctype = f("client_version");
    doctype.response.body = "<?xml version=\"1.0\"?><!DOCTYPE m [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><methodResponse><params><param><value><string>&x;</string></value></param></params></methodResponse>".into();
    let mut deep = f("client_version");
    deep.response.body = format!(
        "<methodResponse><params><param>{}{}</param></params></methodResponse>",
        "<value><array><data>".repeat(10_000),
        "</data></array></value>".repeat(10_000)
    );
    for bad in [huge, truncated, html, doctype, deep] {
        let server = ReplayServer::start(vec![bad]).await;
        let err = client(&server).test().await.unwrap_err();
        assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    }

    let mut gateway = f("login_fails");
    gateway.response.status = 502;
    gateway.response.body = "<html><body>502 Bad Gateway</body></html>".into();
    let server = ReplayServer::start(vec![gateway]).await;
    assert_eq!(
        client(&server).test().await.unwrap_err(),
        ClientError::Rejected("HTTP 502: <html><body>502 Bad Gateway</body></html>".into())
    );
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let (_held, addr) = refused_addr();
    let rt = RTorrent::new(&format!("http://{addr}/RPC2"), auth()).unwrap();
    assert_eq!(rt.test().await.unwrap_err(), ClientError::Unreachable);
}

#[tokio::test]
async fn a_hostile_file_count_is_refused() {
    for count in ["20000000", "9223372036854775807", "-1"] {
        let mut size = f("size_files");
        size.response.body = size
            .response
            .body
            .replace("<i8>3</i8>", &format!("<i8>{count}</i8>"));
        let mut fixtures = script(&["hash_missing", "load_raw", "hash_present", "is_meta_false"]);
        fixtures.push(size);
        let server = ReplayServer::start(fixtures).await;
        let opts = AddOptions {
            selected_files: Some(vec![0]),
            ..AddOptions::default()
        };
        assert!(
            matches!(
                client(&server).add_torrent_file(&torrent(), &opts).await,
                Err(ClientError::Protocol(_))
            ),
            "{count}"
        );
        server.finish();
    }
}

#[tokio::test]
async fn a_bulk_send_that_fails_part_way_says_how_many_went_in() {
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{}", "1".repeat(40))).unwrap();
    let mut fault = f("unknown_method");
    fault.request.path = "/RPC2".into();
    let mut fixtures = script(&["hash_missing", "load_start_magnet", "hash_present"]);
    fixtures.push(fault);
    let server = ReplayServer::start(fixtures).await;
    assert_eq!(
        client(&server)
            .add_magnets(&[magnet(), other], &AddOptions::default())
            .await
            .unwrap_err(),
        ClientError::Partial {
            sent: 1,
            cause: Box::new(ClientError::Rejected(
                "Method 'd.frobnicate' not defined".into()
            )),
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
async fn no_password_in_logs_debug_or_errors() {
    let logs = capture_logs();
    let server = ReplayServer::start(script(&[
        "client_version",
        "library_version",
        "hash_missing",
        "load_raw",
        "hash_present",
        "is_meta_false",
        "size_files",
        "priorities",
        "update_priorities",
        "start",
        "unknown_method",
    ]))
    .await;
    let rt = client(&server);
    rt.test().await.unwrap();
    let opts = AddOptions {
        selected_files: Some(vec![0, 2]),
        ..AddOptions::default()
    };
    rt.add_torrent_file(&torrent(), &opts).await.unwrap();
    let err = rt.categories().await.unwrap_err();
    let failed = ReplayServer::start(script(&["login_fails"])).await;
    let bad = client(&failed).test().await.unwrap_err();

    let config = ClientConfig::RTorrent {
        url: format!("{}?token={QUERY_TOKEN}", server.url()),
        auth: auth(),
    };
    let boxed = connect(config.clone()).unwrap();
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(logged.contains("client=rtorrent"), "capture works");
    for text in [
        logged,
        format!("{rt:?}"),
        format!("{boxed:?}"),
        format!("{config:?}"),
        format!("{err} {err:?}"),
        format!("{bad} {bad:?}"),
    ] {
        assert!(!text.contains(QUERY_TOKEN), "query token leaked: {text}");
        assert!(!text.contains(PASSWORD), "password leaked: {text}");
        assert!(
            !text.contains("Y29ycmVjdC1ob3Jz"),
            "basic auth leaked: {text}"
        );
    }
}

/// Once against a running rTorrent behind a web server:
/// `LODESTAR_LIVE_RTORRENT=http://127.0.0.1/RPC2 LODESTAR_LIVE_USER=…
/// LODESTAR_LIVE_PASSWORD=… cargo test --test rtorrent -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs a running rTorrent"]
async fn live() {
    let url = std::env::var("LODESTAR_LIVE_RTORRENT").unwrap();
    let auth = std::env::var("LODESTAR_LIVE_USER").ok().map(|u| BasicAuth {
        username: u,
        password: Secret::new(std::env::var("LODESTAR_LIVE_PASSWORD").unwrap()),
    });
    let rt = RTorrent::new(&url, auth)
        .unwrap()
        .with_metadata_wait(Duration::from_secs(5), Duration::from_millis(250));
    println!("test: {:?}", rt.test().await);
    let tracker = "&tr=udp%3A%2F%2Ftracker.invalid%3A1337%2Fannounce";
    let with_tracker = Magnet::parse(&format!("{}{tracker}", magnet().as_str())).unwrap();
    let opts = AddOptions {
        category: Some("Lodestar Live".into()),
        save_path: Some("/tmp/lodestar-live/rtorrent".into()),
        paused: true,
        ..AddOptions::default()
    };
    println!(
        "add magnet: {:?}",
        rt.add_magnet(&with_tracker, &opts).await
    );
    println!("add again: {:?}", rt.add_magnet(&with_tracker, &opts).await);
    let bare = Magnet::parse(&format!("magnet:?xt=urn:btih:{}", "2".repeat(40))).unwrap();
    println!(
        "magnet without trackers: {:?}",
        rt.add_magnet(&bare, &AddOptions::default()).await
    );
    let select = AddOptions {
        selected_files: Some(vec![0, 2]),
        ..AddOptions::default()
    };
    println!(
        "add file, files 0 and 2: {:?}",
        rt.add_torrent_file(&torrent(), &select).await
    );
    let other = Magnet::parse(&format!("magnet:?xt=urn:btih:{}{tracker}", "1".repeat(40))).unwrap();
    println!(
        "magnet with a selection: {:?}",
        rt.add_magnet(
            &other,
            &AddOptions {
                selected_files: Some(vec![0]),
                ..AddOptions::default()
            }
        )
        .await
    );
    println!("categories: {:?}", rt.categories().await);
}
