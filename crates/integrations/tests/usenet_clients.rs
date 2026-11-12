//! The SABnzbd and NZBGet adapters against answers built from SABnzbd
//! 4.5.5's and nzbget-ng v24.8's source (`tests/fixtures/usenet_clients/`; each
//! file says where its answer comes from). No network: a local replay
//! server plays them back, and the tests check what the adapters sent.

mod support;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_integrations::clients::nzbget::Nzbget;
use lodestar_integrations::clients::sabnzbd::Sabnzbd;
use lodestar_integrations::clients::usenet::{
    Access, MAX_NZB_BYTES, Nzb, NzbOptions, NzbPriority, UsenetClient, UsenetConfig, UsenetKind,
    connect_usenet,
};
use lodestar_integrations::{AuthFailure, ClientError, Secret};
use serde_json::{Value, json};
use support::{Fixture, Received, ReplayServer, fixture_bytes, refused_addr};

const API_KEY: &str = "5f1e2d3c4b5a69788796a5b4c3d2e1f0";
const NZB_KEY: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f5";
const USER: &str = "lodestar-user-71b2";
const PASSWORD: &str = "correct-horse-usenet";
/// A token in the configured URL's query: dropped, never printed.
const QUERY_TOKEN: &str = "query-token-e3a9";

/// Loads `tests/fixtures/usenet_clients/<client>/<name>.json`.
fn fixture(client: &str, name: &str) -> Fixture {
    support::fixture(&format!("usenet_clients/{client}"), name)
}

fn nzb_bytes() -> Vec<u8> {
    fixture_bytes("usenet_clients", "fixture.nzb")
}

fn nzb() -> Nzb {
    Nzb::new(nzb_bytes(), "Lodestar.Fixture.2026").unwrap()
}

fn sab_f(name: &str) -> Fixture {
    fixture("sabnzbd", name)
}

fn sab(server: &ReplayServer) -> Sabnzbd {
    sab_with(server, API_KEY)
}

fn sab_with(server: &ReplayServer, key: &str) -> Sabnzbd {
    Sabnzbd::new(
        &format!("{}?apikey={QUERY_TOKEN}", server.url()),
        Secret::new(key),
    )
    .unwrap()
}

/// NZBGet's fixtures, with answer ids 1, 2, … in order, as a fresh adapter
/// numbers its calls.
fn nzbget_script(names: &[&str]) -> Vec<Fixture> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| with_id(fixture("nzbget", name), json!(i + 1)))
        .collect()
}

fn with_id(mut fx: Fixture, id: Value) -> Fixture {
    if fx.response.body.is_empty() {
        return fx;
    }
    let mut body: Value = serde_json::from_str(&fx.response.body).unwrap();
    body["id"] = id;
    fx.response.body = body.to_string();
    fx
}

fn nzbget(server: &ReplayServer) -> Nzbget {
    Nzbget::new(
        &format!("{}?x={QUERY_TOKEN}", server.url()),
        Secret::new(USER),
        Secret::new(PASSWORD),
    )
    .unwrap()
}

/// The JSON-RPC call a request carried: its method and parameters.
fn rpc(req: &Received) -> (String, Vec<Value>) {
    let b: Value = serde_json::from_slice(&req.body).unwrap();
    (
        b["method"].as_str().unwrap().to_owned(),
        b["params"].as_array().unwrap().clone(),
    )
}

/// The file part of a multipart body: its file name and bytes.
fn multipart_file(req: &Received, field: &str) -> (String, Vec<u8>) {
    let body = &req.body;
    let marker = format!("name=\"{field}\"; filename=\"");
    let at = find(body, marker.as_bytes()).expect("a file part") + marker.len();
    let end = at + find(&body[at..], b"\"").unwrap();
    let name = String::from_utf8(body[at..end].to_vec()).unwrap();
    let start = end + find(&body[end..], b"\r\n\r\n").unwrap() + 4;
    let stop = start + find(&body[start..], b"\r\n--").unwrap();
    (name, body[start..stop].to_vec())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                T[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|&c| c != b'=') {
        acc = acc << 6 | T.iter().position(|&t| t == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// A loopback address that accepts connections and never answers.
fn silent_addr() -> (std::net::TcpListener, std::net::SocketAddr) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

/// A fixture whose answer redirects to `to`.
fn redirect(mut fx: Fixture, to: &str) -> Fixture {
    fx.response.status = 302;
    fx.response.headers = vec![("location".into(), to.into())];
    fx.response.body = String::new();
    fx
}

/// Runs `fut`, failing the test if it hasn't finished within 30 s.
async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .expect("the adapter gave up within its own timeout")
}

// ---- SABnzbd -------------------------------------------------------------

#[tokio::test]
async fn sab_test_reports_the_version_and_a_full_key() {
    let server = ReplayServer::start(vec![sab_f("version"), sab_f("auth_apikey")]).await;
    let info = sab(&server).test().await.unwrap();
    assert_eq!(info.kind, UsenetKind::Sabnzbd);
    assert_eq!(info.version, "4.5.5");
    assert_eq!(info.access, Access::Full);
    let reqs = server.finish();
    for r in &reqs {
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/api");
        assert_eq!(r.query, None, "nothing goes in the query string");
        assert_eq!(r.form_field("output").as_deref(), Some("json"));
        assert_eq!(r.header("accept"), Some("application/json"));
    }
    assert_eq!(reqs[0].form_field("mode").as_deref(), Some("version"));
    assert_eq!(reqs[0].form_field("apikey"), None, "version needs no key");
    assert_eq!(reqs[1].form_field("mode").as_deref(), Some("auth"));
    assert_eq!(reqs[1].form_field("key").as_deref(), Some(API_KEY));
}

#[tokio::test]
async fn sab_test_tells_an_nzb_key_apart() {
    let server = ReplayServer::start(vec![sab_f("version"), sab_f("auth_nzbkey")]).await;
    let info = sab_with(&server, NZB_KEY).test().await.unwrap();
    assert_eq!(info.access, Access::AddOnly);
    server.finish();
}

#[tokio::test]
async fn sab_test_with_a_wrong_key_is_bad_credentials() {
    let server = ReplayServer::start(vec![sab_f("version"), sab_f("auth_badkey")]).await;
    let e = sab(&server).test().await.unwrap_err();
    assert_eq!(e, ClientError::Auth(AuthFailure::BadCredentials));
    server.finish();
}

#[tokio::test]
async fn sab_a_wrong_or_missing_key_is_bad_credentials() {
    for name in ["key_incorrect", "key_required"] {
        let server = ReplayServer::start(vec![sab_f(name)]).await;
        let e = sab(&server)
            .add_nzb(&nzb(), &NzbOptions::default())
            .await
            .unwrap_err();
        assert_eq!(e, ClientError::Auth(AuthFailure::BadCredentials), "{name}");
        server.finish();
    }
}

#[tokio::test]
async fn sab_address_and_host_refusals_are_told_apart() {
    let server = ReplayServer::start(vec![sab_f("access_denied"), sab_f("hostname_denied")]).await;
    let s = sab(&server);
    assert_eq!(
        s.categories().await.unwrap_err(),
        ClientError::Auth(AuthFailure::AddressRefused)
    );
    assert_eq!(
        s.categories().await.unwrap_err(),
        ClientError::Auth(AuthFailure::OriginRefused)
    );
    server.finish();
}

#[tokio::test]
async fn sab_add_uploads_the_bytes_and_returns_the_job() {
    let server = ReplayServer::start(vec![sab_f("addfile_ok")]).await;
    let opts = NzbOptions {
        category: Some("tv".into()),
        priority: Some(NzbPriority::High),
        paused: false,
    };
    let job = sab(&server).add_nzb(&nzb(), &opts).await.unwrap();
    assert_eq!(job.as_str(), "SABnzbd_nzo_p86tgx");
    let reqs = server.finish();
    let r = &reqs[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/api"));
    assert_eq!(r.query, None, "the key never goes in the URL");
    assert!(
        r.header("content-type")
            .unwrap()
            .starts_with("multipart/form-data")
    );
    assert_eq!(r.multipart_field("mode").as_deref(), Some("addfile"));
    assert_eq!(r.multipart_field("output").as_deref(), Some("json"));
    assert_eq!(r.multipart_field("apikey").as_deref(), Some(API_KEY));
    assert_eq!(r.multipart_field("cat").as_deref(), Some("tv"));
    assert_eq!(r.multipart_field("priority").as_deref(), Some("1"));
    let (name, bytes) = multipart_file(r, "name");
    assert_eq!(name, "Lodestar.Fixture.2026.nzb");
    assert_eq!(bytes, nzb_bytes());
    assert!(!r.body_text().contains("addurl"));
}

#[tokio::test]
async fn sab_paused_is_its_paused_priority_and_defaults_send_nothing() {
    let server = ReplayServer::start(vec![sab_f("addfile_ok"), sab_f("addfile_ok")]).await;
    let s = sab(&server);
    let paused = NzbOptions {
        priority: Some(NzbPriority::Force),
        paused: true,
        ..NzbOptions::default()
    };
    s.add_nzb(&nzb(), &paused).await.unwrap();
    s.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap();
    let reqs = server.finish();
    assert_eq!(reqs[0].multipart_field("priority").as_deref(), Some("-2"));
    assert_eq!(reqs[1].multipart_field("priority"), None);
    assert_eq!(reqs[1].multipart_field("cat"), None);
}

#[tokio::test]
async fn sab_a_refused_add_is_rejected() {
    let server = ReplayServer::start(vec![sab_f("addfile_refused")]).await;
    let e = sab(&server)
        .add_nzb(&nzb(), &NzbOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        e,
        ClientError::Rejected("the client refused the NZB".into())
    );
    server.finish();
}

#[tokio::test]
async fn sab_categories_drop_the_default() {
    let server = ReplayServer::start(vec![sab_f("get_cats")]).await;
    let cats = sab(&server).categories().await.unwrap();
    let names: Vec<_> = cats.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["movies", "software", "tv"]);
    let r = &server.finish()[0];
    assert_eq!(r.form_field("mode").as_deref(), Some("get_cats"));
    assert_eq!(r.form_field("apikey").as_deref(), Some(API_KEY));
    assert_eq!(r.query, None);
}

#[tokio::test]
async fn sab_bad_answers_are_protocol_errors() {
    let mut huge = sab_f("addfile_ok");
    huge.response.body = format!(
        r#"{{"status": true, "nzo_ids": ["{}"]}}"#,
        "a".repeat(2 << 20)
    );
    let mut html = sab_f("addfile_ok");
    html.response.body = "<html><body>Hello</body></html>".into();
    let mut wrong = sab_f("addfile_ok");
    wrong.response.body = r#"{"status": "yes", "nzo_ids": [1]}"#.into();
    let mut ids = sab_f("addfile_ok");
    ids.response.body = r#"{"status": true, "nzo_ids": ["../../x"]}"#.into();
    let server = ReplayServer::start(vec![huge, html, wrong, ids]).await;
    let s = sab(&server);
    for want in [
        "the response was too large",
        "the answer isn't SABnzbd's JSON",
        "the answer isn't SABnzbd's JSON",
        "the job id is malformed",
    ] {
        let e = s.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err();
        assert_eq!(e, ClientError::Protocol(want));
    }
    server.finish();
}

#[tokio::test]
async fn sab_a_redirect_is_not_followed() {
    let elsewhere = ReplayServer::start(Vec::new()).await;
    let server = ReplayServer::start(vec![redirect(
        sab_f("addfile_ok"),
        &format!("{}api", elsewhere.url()),
    )])
    .await;
    let e = sab(&server)
        .add_nzb(&nzb(), &NzbOptions::default())
        .await
        .unwrap_err();
    assert_eq!(e, ClientError::Protocol("the client redirected"));
    server.finish();
    assert!(
        elsewhere.finish().is_empty(),
        "the redirect target got nothing"
    );
}

// ---- NZBGet --------------------------------------------------------------

#[tokio::test]
async fn nzbget_test_checks_the_credentials() {
    let server = ReplayServer::start(nzbget_script(&["version"])).await;
    let info = nzbget(&server).test().await.unwrap();
    assert_eq!(info.kind, UsenetKind::Nzbget);
    assert_eq!(info.version, "24.8");
    assert_eq!(info.access, Access::Unknown);
    let r = &server.finish()[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/jsonrpc"));
    assert_eq!(r.query, None);
    let auth = r.header("authorization").unwrap();
    let decoded = base64_decode(auth.strip_prefix("Basic ").unwrap());
    assert_eq!(decoded, format!("{USER}:{PASSWORD}").into_bytes());
    assert_eq!(r.header("content-type"), Some("application/json"));
    let text = r.body_text();
    assert!(
        text.starts_with(r#"{"id":1,"method":"version","params":[]"#),
        "{text}"
    );
}

#[tokio::test]
async fn nzbget_without_credentials_sends_no_authorization() {
    let server = ReplayServer::start(nzbget_script(&["version"])).await;
    let n = Nzbget::new(&server.url(), Secret::new(""), Secret::new("")).unwrap();
    n.test().await.unwrap();
    assert_eq!(server.finish()[0].header("authorization"), None);
}

#[tokio::test]
async fn nzbget_a_wrong_password_is_bad_credentials() {
    let server = ReplayServer::start(nzbget_script(&["unauthorized", "unauthorized"])).await;
    let n = nzbget(&server);
    assert_eq!(
        n.test().await.unwrap_err(),
        ClientError::Auth(AuthFailure::BadCredentials)
    );
    assert_eq!(
        n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err(),
        ClientError::Auth(AuthFailure::BadCredentials)
    );
    server.finish();
}

#[tokio::test]
async fn nzbget_append_sends_base64_content_and_returns_the_id() {
    let server = ReplayServer::start(nzbget_script(&["append_ok"])).await;
    let opts = NzbOptions {
        category: Some("Series".into()),
        priority: Some(NzbPriority::High),
        paused: true,
    };
    let job = nzbget(&server).add_nzb(&nzb(), &opts).await.unwrap();
    assert_eq!(job.as_str(), "3");
    let r = &server.finish()[0];
    let (method, params) = rpc(r);
    assert_eq!(method, "append");
    assert_eq!(params.len(), 9);
    assert_eq!(params[0], json!("Lodestar.Fixture.2026.nzb"));
    let content = params[1].as_str().unwrap();
    assert!(!content.starts_with("http"), "never a URL");
    assert_eq!(base64_decode(content), nzb_bytes());
    assert_eq!(
        params[2..],
        [
            json!("Series"),
            json!(50),
            json!(false),
            json!(true),
            json!(""),
            json!(0),
            json!("SCORE")
        ]
    );
}

#[tokio::test]
async fn nzbget_priorities_map_to_its_scale() {
    let server = ReplayServer::start(nzbget_script(&["append_ok"; 5])).await;
    let n = nzbget(&server);
    for priority in [
        None,
        Some(NzbPriority::Low),
        Some(NzbPriority::Normal),
        Some(NzbPriority::High),
        Some(NzbPriority::Force),
    ] {
        let opts = NzbOptions {
            priority,
            ..NzbOptions::default()
        };
        n.add_nzb(&nzb(), &opts).await.unwrap();
    }
    let sent: Vec<_> = server
        .finish()
        .iter()
        .map(|r| rpc(r).1[3].clone())
        .collect();
    assert_eq!(
        sent,
        [json!(0), json!(-50), json!(0), json!(50), json!(900)]
    );
}

#[tokio::test]
async fn nzbget_refusals_and_rpc_errors_map() {
    let server = ReplayServer::start(nzbget_script(&[
        "append_refused",
        "append_bad_param",
        "access_denied",
    ]))
    .await;
    let n = nzbget(&server);
    assert_eq!(
        n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err(),
        ClientError::Rejected("the client refused the NZB".into())
    );
    assert_eq!(
        n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err(),
        ClientError::Rejected("Invalid parameter (AddPaused)".into())
    );
    assert_eq!(
        n.categories().await.unwrap_err(),
        ClientError::Rejected("Access denied".into())
    );
    server.finish();
}

#[tokio::test]
async fn nzbget_categories_keep_only_names() {
    let logs = capture_logs();
    let server = ReplayServer::start(nzbget_script(&["config"])).await;
    let cats = nzbget(&server).categories().await.unwrap();
    let names: Vec<_> = cats.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Movies", "Music", "Series", "Software"]);
    assert!(cats.iter().all(|c| c.save_path.is_none()));
    assert_eq!(rpc(&server.finish()[0]).0, "config");
    let all = format!("{cats:?}{}", String::from_utf8_lossy(&logs.lock().unwrap()));
    assert!(!all.contains("throwaway-"), "a client password leaked");
}

#[tokio::test]
async fn nzbget_bad_answers_are_protocol_errors() {
    let mut fixtures = nzbget_script(&["append_ok", "append_ok", "append_ok", "append_ok"]);
    fixtures[0].response.body = format!(r#"{{"id":1,"result":3,"pad":"{}"}}"#, "a".repeat(2 << 20));
    fixtures[1].response.body = "<html>nope</html>".into();
    fixtures[2].response.body = r#"{"id":3,"result":99999999999999999999}"#.into();
    // An answer for another call.
    fixtures[3].response.body = r#"{"id":1,"result":4}"#.into();
    let server = ReplayServer::start(fixtures).await;
    let n = nzbget(&server);
    for want in [
        "the response was too large",
        "the answer isn't NZBGet's JSON",
        "the answer isn't NZBGet's JSON",
        "the answer is for another request",
    ] {
        let e = n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err();
        assert_eq!(e, ClientError::Protocol(want));
    }
    server.finish();
}

#[tokio::test]
async fn nzbget_a_redirect_is_not_followed() {
    let elsewhere = ReplayServer::start(Vec::new()).await;
    let fixtures = vec![redirect(
        fixture("nzbget", "version"),
        &format!("{}jsonrpc", elsewhere.url()),
    )];
    let server = ReplayServer::start(fixtures).await;
    assert_eq!(
        nzbget(&server).test().await.unwrap_err(),
        ClientError::Protocol("the client redirected")
    );
    server.finish();
    assert!(
        elsewhere.finish().is_empty(),
        "the redirect target got nothing"
    );
}

// ---- Both ----------------------------------------------------------------

#[tokio::test]
async fn an_oversize_or_bogus_nzb_never_reaches_the_network() {
    let mut big = nzb_bytes();
    big.resize(MAX_NZB_BYTES + 1, b' ');
    assert_eq!(
        Nzb::new(big, "x").unwrap_err(),
        ClientError::InvalidRequest("the NZB is larger than 16 MiB")
    );
    assert!(Nzb::new(b"<html>401</html>".to_vec(), "x").is_err());
    // A bad category is refused before any request, by both adapters.
    let server = ReplayServer::start(Vec::new()).await;
    let clients: [Box<dyn UsenetClient>; 2] = [Box::new(sab(&server)), Box::new(nzbget(&server))];
    // A newline would inject; `""` or `\\` would desynchronise NZBGet 24's
    // request parser, which ends a string at two adjacent escapes.
    for category in ["tv\nX-Injected: 1", "a\"\"b", "a\\\\b"] {
        let bad = NzbOptions {
            category: Some(category.into()),
            ..NzbOptions::default()
        };
        for c in &clients {
            assert!(matches!(
                c.add_nzb(&nzb(), &bad).await.unwrap_err(),
                ClientError::InvalidRequest(_)
            ));
        }
    }
    assert!(server.finish().is_empty(), "no request was made");
}

#[tokio::test]
async fn a_silent_client_times_out() {
    let (_listener, addr) = silent_addr();
    let url = format!("http://{addr}/");
    let short = Duration::from_millis(300);
    let s = Sabnzbd::new(&url, Secret::new(API_KEY))
        .unwrap()
        .with_timeout(short);
    assert_eq!(within(s.test()).await.unwrap_err(), ClientError::Timeout);
    let n = Nzbget::new(&url, Secret::new(USER), Secret::new(PASSWORD))
        .unwrap()
        .with_timeout(short);
    assert_eq!(within(n.test()).await.unwrap_err(), ClientError::Timeout);
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let (_held, addr) = refused_addr();
    let url = format!("http://{addr}/");
    let s = Sabnzbd::new(&url, Secret::new(API_KEY)).unwrap();
    assert_eq!(s.test().await.unwrap_err(), ClientError::Unreachable);
    let n = Nzbget::new(&url, Secret::new(USER), Secret::new(PASSWORD)).unwrap();
    assert_eq!(n.test().await.unwrap_err(), ClientError::Unreachable);
}

#[tokio::test]
async fn refused_addresses_fail_at_construction() {
    for url in [
        "http://169.254.169.254/",
        "http://[fe80::1]/",
        "file:///etc/passwd",
    ] {
        assert!(
            connect_usenet(UsenetConfig::Sabnzbd {
                url: url.into(),
                api_key: Secret::new(API_KEY),
            })
            .is_err()
        );
        assert!(
            connect_usenet(UsenetConfig::Nzbget {
                url: url.into(),
                username: Secret::new(USER),
                password: Secret::new(PASSWORD),
            })
            .is_err()
        );
    }
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
        for e in lodestar_integrations::log_events::CATALOGUE.events {
            e.set_on(true);
        }
        buf
    }))
}

#[tokio::test]
async fn no_key_or_password_in_logs_debug_errors_or_urls() {
    let logs = capture_logs();
    let server = ReplayServer::start(vec![
        sab_f("version"),
        sab_f("auth_apikey"),
        sab_f("addfile_ok"),
        sab_f("key_incorrect"),
        sab_f("get_cats"),
    ])
    .await;
    let s = sab(&server);
    let mut seen = String::new();
    seen += &format!("{:?}", s.test().await);
    seen += &format!("{:?}", s.add_nzb(&nzb(), &NzbOptions::default()).await);
    let e = s.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err();
    seen += &format!("{e} {e:?}");
    seen += &format!("{:?}", s.categories().await);
    seen += &format!("{s:?}");
    let reqs = server.finish();

    let nserver = ReplayServer::start(nzbget_script(&["version", "unauthorized", "config"])).await;
    let n = nzbget(&nserver);
    seen += &format!("{:?}", n.test().await);
    let e = n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err();
    seen += &format!("{e} {e:?}");
    seen += &format!("{:?}", n.categories().await);
    seen += &format!("{n:?}");
    let nreqs = nserver.finish();

    // Errors from the network layer too.
    let (_held, addr) = refused_addr();
    let gone = format!("http://{addr}/?apikey={QUERY_TOKEN}");
    let boxed = connect_usenet(UsenetConfig::Sabnzbd {
        url: gone.clone(),
        api_key: Secret::new(API_KEY),
    })
    .unwrap();
    let e = boxed.test().await.unwrap_err();
    seen += &format!("{e} {e:?} {boxed:?}");
    let config = UsenetConfig::Nzbget {
        url: gone,
        username: Secret::new(USER),
        password: Secret::new(PASSWORD),
    };
    seen += &format!("{config:?}");
    let boxed = connect_usenet(config).unwrap();
    let e = boxed.test().await.unwrap_err();
    seen += &format!("{e} {e:?} {boxed:?}");

    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    // The Basic header's own form of the credentials, too.
    let basic = base64_encode(format!("{USER}:{PASSWORD}").as_bytes());
    assert_eq!(
        base64_decode(&basic),
        format!("{USER}:{PASSWORD}").into_bytes()
    );
    for secret in [API_KEY, USER, PASSWORD, QUERY_TOKEN, basic.as_str()] {
        assert!(
            !seen.contains(secret),
            "{secret} in Debug or Display: {seen}"
        );
        assert!(!logged.contains(secret), "{secret} in the logs");
    }
    // The key travels in bodies only: never in a path or query string.
    for r in reqs.iter().chain(&nreqs) {
        let url = format!("{}?{}", r.path, r.query.as_deref().unwrap_or(""));
        for secret in [API_KEY, USER, PASSWORD, QUERY_TOKEN] {
            assert!(!url.contains(secret), "{secret} in {url}");
        }
    }
    assert!(
        logged.contains("sabnzbd") && logged.contains("nzbget"),
        "{logged}"
    );
}

#[tokio::test]
async fn a_credential_inside_a_valid_answer_is_still_read() {
    // The password is part of the job id, and the user name is a field the
    // parser reads: the answer is read as the client sent it.
    let mut fixtures = nzbget_script(&["append_ok"]);
    let mut body: Value = serde_json::from_str(&fixtures[0].response.body).unwrap();
    body["result"] = json!(12024);
    fixtures[0].response.body = body.to_string();
    let server = ReplayServer::start(fixtures).await;
    let n = Nzbget::new(&server.url(), Secret::new("result"), Secret::new("2024")).unwrap();
    let job = n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap();
    assert_eq!(job.as_str(), "12024");
    server.finish();

    // The key is part of the nzo id.
    let server = ReplayServer::start(vec![sab_f("addfile_ok")]).await;
    let job = sab_with(&server, "p86tgx")
        .add_nzb(&nzb(), &NzbOptions::default())
        .await
        .unwrap();
    assert_eq!(job.as_str(), "SABnzbd_nzo_p86tgx");
    server.finish();
}

#[tokio::test]
async fn an_answer_that_echoes_a_credential_keeps_it_out_of_the_error() {
    // A reverse proxy's error page that repeats the request body, and a
    // client reason that repeats a credential.
    let mut sab_500 = sab_f("addfile_ok");
    sab_500.response.status = 500;
    sab_500.response.body = format!("Bad gateway for mode=addfile&apikey={API_KEY}&output=json");
    let mut sab_json = sab_f("addfile_refused");
    sab_json.response.body = format!(r#"{{"status": false, "error": "unknown key {API_KEY}"}}"#);
    let server = ReplayServer::start(vec![sab_500, sab_json]).await;
    let s = sab(&server);
    let mut errors = Vec::new();
    for _ in 0..2 {
        errors.push(s.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err());
    }
    server.finish();

    let mut fixtures = nzbget_script(&["append_bad_param", "append_ok", "append_ok"]);
    let mut rpc: Value = serde_json::from_str(&fixtures[0].response.body).unwrap();
    rpc["error"]["message"] = json!(format!("bad login {USER}:{PASSWORD}"));
    fixtures[0].response.body = rpc.to_string();
    fixtures[1].response.status = 502;
    fixtures[1].response.body = format!("upstream refused password={PASSWORD}");
    // A proxy's page that echoes the request headers: NZBGet's credentials
    // travel only as the Basic header's base64.
    let basic = base64_encode(format!("{USER}:{PASSWORD}").as_bytes());
    fixtures[2].response.status = 502;
    fixtures[2].response.body = format!("Bad gateway. Request had Authorization: Basic {basic}");
    let server = ReplayServer::start(fixtures).await;
    let n = nzbget(&server);
    for _ in 0..3 {
        errors.push(n.add_nzb(&nzb(), &NzbOptions::default()).await.unwrap_err());
    }
    server.finish();

    assert_eq!(errors.len(), 5);
    for e in &errors {
        assert!(matches!(e, ClientError::Rejected(_)), "{e:?}");
        let text = format!("{e} {e:?}");
        for secret in [API_KEY, USER, PASSWORD, basic.as_str()] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert!(text.contains("***"), "the reason is kept, scrubbed: {text}");
    }
}

#[tokio::test]
async fn category_errors_keep_credentials_out_too() {
    let mut sab_500 = sab_f("get_cats");
    sab_500.response.status = 500;
    sab_500.response.body = format!("Bad gateway for mode=get_cats&apikey={API_KEY}");
    let server = ReplayServer::start(vec![sab_500]).await;
    let sab_err = sab(&server).categories().await.unwrap_err();
    server.finish();

    let mut fixtures = nzbget_script(&["access_denied"]);
    let mut rpc: Value = serde_json::from_str(&fixtures[0].response.body).unwrap();
    rpc["error"]["message"] = json!(format!("Access denied for {USER} / {PASSWORD}"));
    fixtures[0].response.body = rpc.to_string();
    let server = ReplayServer::start(fixtures).await;
    let nzbget_err = nzbget(&server).categories().await.unwrap_err();
    server.finish();

    for e in [sab_err, nzbget_err] {
        assert!(matches!(e, ClientError::Rejected(_)), "{e:?}");
        let text = format!("{e} {e:?}");
        for secret in [API_KEY, USER, PASSWORD] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert!(text.contains("***"), "{text}");
    }
}
