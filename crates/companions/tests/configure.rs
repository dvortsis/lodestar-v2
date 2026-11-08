//! CMP-8b: configuring qBittorrent and Prowlarr through their APIs, against
//! stateful fake servers on loopback, through the real `CompanionHttp`.
//! Plan then apply, idempotence, consent, Q8's route refusals, Q19's
//! password replacement, hostile answers, and no credential in a URL, an
//! error or a log line.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lodestar_companions::CompanionId;
use lodestar_companions::adapter::{
    AdapterError, BoxFuture, ConfigPlan, Configure, ConfigureInput, CredentialError, Credentials,
    EnteredUrl, Target,
};
use lodestar_companions::config_files::Username;
use lodestar_companions::configure::{
    self, BIND_UNSUITED, Category, CategoryName, ConfigSink, Confirmation, DownloadClientEntry,
    Downloads, IndexerManagerEntry, InterfaceName, NOT_CONFIRMED, ROUTE_UNSUPPORTED, Route,
    SINK_REFUSED, SinkError, Socks5Proxy, WebUiBind, qbittorrent,
};
use lodestar_companions::detect::CompanionHttp;
use lodestar_companions::inputs::{HostPath, PortNumber};
use lodestar_integrations::secret::Secret;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ─── A local server ─────────────────────────────────────────────────────

enum Reply {
    Full {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Read the request and say nothing.
    Silent,
}

fn reply(status: u16, body: impl Into<Vec<u8>>) -> Reply {
    Reply::Full {
        status,
        headers: Vec::new(),
        body: body.into(),
    }
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    async fn start(handler: Handler) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(serve(sock, Arc::clone(&handler), Arc::clone(&log)));
            }
        });
        Server { addr, seen }
    }

    fn loopback(&self) -> Target {
        Target::Loopback(PortNumber::new(self.addr.port()).unwrap())
    }

    fn entered(&self) -> Target {
        Target::Entered(EnteredUrl::parse(&format!("http://{}", self.addr)).unwrap())
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

async fn serve(mut sock: TcpStream, handler: Handler, log: Arc<Mutex<Vec<Seen>>>) {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
        if head.len() > 64 * 1024 {
            return;
        }
    }
    let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(0) + 4;
    let mut body = head.split_off(end);
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let mut request_line = lines.next().unwrap_or("").split(' ');
    let method = request_line.next().unwrap_or("").to_owned();
    let path = request_line.next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .take_while(|l| !l.is_empty())
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(1 << 20);
    while body.len() < length {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    let seen = Seen {
        method,
        path,
        headers,
        body,
    };
    log.lock().unwrap().push(seen.clone());
    match handler(&seen) {
        Reply::Full {
            status,
            headers,
            body,
        } => {
            let mut out = format!("HTTP/1.1 {status} X\r\n");
            for (n, v) in headers {
                out.push_str(&format!("{n}: {v}\r\n"));
            }
            out.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ));
            let mut bytes = out.into_bytes();
            bytes.extend_from_slice(&body);
            let _ = sock.write_all(&bytes).await;
        }
        Reply::Silent => {
            let _ = sock.read(&mut buf).await;
        }
    }
    let _ = sock.shutdown().await;
}

/// `application/x-www-form-urlencoded`, decoded.
fn form_decode(body: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    let unescape = |s: &str| {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'%' if i + 2 < b.len() => {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                    i += 3;
                }
                b'+' => {
                    out.push(b' ');
                    i += 1;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8(out).unwrap()
    };
    text.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (unescape(k), unescape(v)))
        .collect()
}

fn fixture(path: &str) -> Map<String, Value> {
    let p = format!("{}/fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

// ─── Secrets the tests use, and the log capture ─────────────────────────

const GENERATED: &str = "GeneratedPw_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TEMPORARY: &str = "Tmp4Pass9x";
const WRONG: &str = "Wrong-Password-1";
const API_KEY: &str = "0123456789abcdef0123456789abcdef";
const SID_PREFIX: &str = "sessionvalue";

struct Capture(Mutex<Vec<String>>);

impl lodestar_log::Sink for Capture {
    fn record(&self, record: &lodestar_log::Record<'_>) {
        self.0.lock().unwrap().push(format!(
            "{} {}",
            record.event.id,
            record.text(lodestar_log::Mask::NONE, None)
        ));
    }
}

/// The binary-wide capture, installed by whichever test runs first.
fn capture() -> &'static Capture {
    static SINK: OnceLock<Capture> = OnceLock::new();
    let sink = SINK.get_or_init(|| Capture(Mutex::new(Vec::new())));
    let _ = lodestar_log::set_sink(sink);
    for e in lodestar_companions::log_events::CATALOGUE.events {
        e.set_on(true);
    }
    sink
}

struct Creds(HashMap<&'static str, &'static str>);

impl Credentials for Creds {
    fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError> {
        Ok(self.0.get(field).map(|v| Secret::new(*v)))
    }
}

fn qbit_creds(password: &'static str) -> Creds {
    Creds(HashMap::from([("webui_password", password)]))
}

fn http() -> CompanionHttp {
    CompanionHttp::new().unwrap()
}

/// Neither an error's `Debug` nor its `Display` carries a secret.
fn clean_error(e: &AdapterError) {
    for text in [format!("{e:?}"), e.to_string()] {
        for s in [GENERATED, TEMPORARY, WRONG, API_KEY, SID_PREFIX] {
            assert!(!text.contains(s), "{text}");
        }
    }
}

/// No request line carried a secret: credentials go only in a body or a
/// header.
fn clean_urls(seen: &[Seen]) {
    for s in seen {
        for secret in [GENERATED, TEMPORARY, WRONG, API_KEY, SID_PREFIX] {
            assert!(!s.path.contains(secret), "{}", s.path);
        }
    }
}

// ─── A fake qBittorrent ─────────────────────────────────────────────────

struct QState {
    username: String,
    password: String,
    sessions: Vec<String>,
    prefs: Map<String, Value>,
    categories: Map<String, Value>,
    banned: bool,
    ignore_password: bool,
    /// Replaces the preferences answer.
    hostile: Option<fn() -> Reply>,
    /// Login answers without a usable cookie.
    no_cookie: bool,
}

type Qbit = Arc<Mutex<QState>>;

fn qbit_state(password: &str) -> Qbit {
    Arc::new(Mutex::new(QState {
        username: "lodestar".into(),
        password: password.into(),
        sessions: Vec::new(),
        prefs: fixture("qbittorrent/configure/preferences.json"),
        categories: fixture("qbittorrent/configure/categories.json"),
        banned: false,
        ignore_password: false,
        hostile: None,
        no_cookie: false,
    }))
}

fn qbit_handler(state: Qbit) -> Handler {
    Arc::new(move |seen: &Seen| {
        let mut s = state.lock().unwrap();
        if seen.method == "POST" && seen.path == "/api/v2/auth/login" {
            if s.banned {
                return reply(403, "Your IP address has been banned");
            }
            let f = form_decode(&seen.body);
            if f.get("username") == Some(&s.username) && f.get("password") == Some(&s.password) {
                let sid = format!("{SID_PREFIX}{}", s.sessions.len());
                s.sessions.push(sid.clone());
                let cookie = if s.no_cookie {
                    "SID=bad value; HttpOnly".to_owned()
                } else {
                    format!("SID={sid}; HttpOnly; SameSite=Strict; path=/")
                };
                return Reply::Full {
                    status: 200,
                    headers: vec![("Set-Cookie".into(), cookie)],
                    body: b"Ok.".to_vec(),
                };
            }
            return reply(200, "Fails.");
        }
        let authed = seen
            .header("cookie")
            .and_then(|c| c.strip_prefix("SID="))
            .is_some_and(|sid| s.sessions.iter().any(|x| x == sid));
        if !authed {
            return reply(403, "Forbidden");
        }
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/api/v2/app/preferences") => match s.hostile {
                Some(h) => h(),
                None => reply(200, Value::Object(s.prefs.clone()).to_string()),
            },
            ("GET", "/api/v2/torrents/categories") => {
                reply(200, Value::Object(s.categories.clone()).to_string())
            }
            ("POST", "/api/v2/app/setPreferences") => {
                let f = form_decode(&seen.body);
                let Ok(Value::Object(m)) = serde_json::from_str(&f["json"]) else {
                    return reply(400, "");
                };
                for (k, v) in m {
                    if k == "web_ui_password" {
                        if !s.ignore_password {
                            s.password = v.as_str().unwrap().to_owned();
                        }
                    } else {
                        s.prefs.insert(k, v);
                    }
                }
                reply(200, "")
            }
            ("POST", p @ ("/api/v2/torrents/createCategory" | "/api/v2/torrents/editCategory")) => {
                let f = form_decode(&seen.body);
                let name = f["category"].clone();
                let exists = s.categories.contains_key(&name);
                if exists != p.ends_with("editCategory") {
                    return reply(409, "");
                }
                s.categories.insert(
                    name.clone(),
                    json!({"name": name, "savePath": f["savePath"], "download_path": null}),
                );
                reply(200, "")
            }
            _ => reply(404, "Not Found"),
        }
    })
}

fn downloads() -> Downloads {
    Downloads::new(
        HostPath::parse("/data/downloads").unwrap(),
        vec![
            Category {
                name: CategoryName::parse("lodestar").unwrap(),
                save_path: Some(HostPath::parse("/data/downloads/lodestar").unwrap()),
            },
            Category {
                name: CategoryName::parse("movies").unwrap(),
                save_path: Some(HostPath::parse("/downloads/movies").unwrap()),
            },
        ],
    )
    .unwrap()
}

fn tun0() -> Route {
    Route::Interface(InterfaceName::parse("tun0").unwrap())
}

fn names(plan: &ConfigPlan) -> Vec<&'static str> {
    plan.changes().iter().map(|k| k.as_str()).collect()
}

fn qbit() -> Box<dyn Configure> {
    configure::configurator(&CompanionId::parse("qbittorrent").unwrap()).unwrap()
}

fn prowlarr_adapter() -> Box<dyn Configure> {
    configure::configurator(&CompanionId::parse("prowlarr").unwrap()).unwrap()
}

// ─── qBittorrent ────────────────────────────────────────────────────────

#[tokio::test]
async fn qbittorrent_first_configuration_then_an_empty_plan() {
    capture();
    let state = qbit_state(GENERATED);
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .installed()
        .webui(WebUiBind::Loopback)
        .downloads(downloads())
        .route(tun0());
    let http = http();
    let adapter = qbit();

    let plan = adapter.plan(&http, &target, &input).await.unwrap();
    assert_eq!(
        names(&plan),
        [
            "downloads.categories",
            "downloads.save_path",
            "network.interface",
            "webui.bind_address"
        ]
    );
    // Planning wrote nothing.
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.method == "GET" || s.path.ends_with("/login"))
    );

    adapter.apply(&http, &target, &input, &plan).await.unwrap();
    {
        let s = state.lock().unwrap();
        assert_eq!(s.prefs["web_ui_address"], "127.0.0.1");
        assert_eq!(s.prefs["save_path"], "/data/downloads");
        assert_eq!(s.prefs["current_network_interface"], "tun0");
        assert_eq!(s.prefs["current_interface_address"], "");
        assert_eq!(
            s.categories["lodestar"]["savePath"],
            "/data/downloads/lodestar"
        );
        // The category that already matched was left alone.
        assert_eq!(s.categories["movies"]["download_path"], Value::Null);
    }
    let seen = server.seen();
    let creates: Vec<_> = seen
        .iter()
        .filter(|s| s.path.ends_with("Category"))
        .collect();
    assert_eq!(creates.len(), 1);
    assert!(creates[0].path.ends_with("/createCategory"));
    // Every call but the sign-in carries the session cookie and no
    // `Origin` or `Referer`.
    for s in &seen {
        assert!(s.header("origin").is_none() && s.header("referer").is_none());
        if !s.path.ends_with("/login") {
            assert!(s.header("cookie").unwrap().starts_with("SID=sessionvalue"));
        }
    }
    clean_urls(&seen);

    let again = adapter.plan(&http, &target, &input).await.unwrap();
    assert!(again.changes().is_empty(), "{:?}", names(&again));
    let before = server.count();
    adapter.apply(&http, &target, &input, &again).await.unwrap();
    assert_eq!(server.count(), before, "an empty plan sends nothing");
}

#[tokio::test]
async fn qbittorrent_already_configured_writes_nothing() {
    capture();
    let state = qbit_state(GENERATED);
    {
        let mut s = state.lock().unwrap();
        s.prefs.insert("web_ui_address".into(), "127.0.0.1".into());
        s.prefs
            .insert("save_path".into(), "/data/downloads/".into());
        s.prefs
            .insert("current_network_interface".into(), "tun0".into());
        s.categories.insert(
            "lodestar".into(),
            json!({"name": "lodestar", "savePath": "/data/downloads/lodestar"}),
        );
    }
    let server = Server::start(qbit_handler(state)).await;
    let target = server.loopback();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .installed()
        .webui(WebUiBind::Loopback)
        .downloads(downloads())
        .route(tun0());
    let plan = qbit().plan(&http(), &target, &input).await.unwrap();
    assert!(plan.changes().is_empty(), "{:?}", names(&plan));
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.method == "GET" || s.path.ends_with("/login"))
    );
}

#[tokio::test]
async fn qbittorrent_partly_configured_writes_only_what_differs() {
    capture();
    let state = qbit_state(GENERATED);
    state
        .lock()
        .unwrap()
        .prefs
        .insert("bypass_local_auth".into(), true.into());
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .installed()
        .downloads(Downloads::new(HostPath::parse("/downloads").unwrap(), Vec::new()).unwrap())
        .route(Route::Direct);
    let http = http();
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["webui.local_auth"]);
    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    let writes: Vec<_> = server
        .seen()
        .into_iter()
        .filter(|s| s.path.ends_with("/setPreferences"))
        .collect();
    assert_eq!(writes.len(), 1);
    let sent: Map<String, Value> =
        serde_json::from_str(&form_decode(&writes[0].body)["json"]).unwrap();
    assert_eq!(
        sent,
        json!({"bypass_local_auth": false})
            .as_object()
            .unwrap()
            .clone()
    );
    assert_eq!(state.lock().unwrap().prefs["web_ui_address"], "*");
}

#[tokio::test]
async fn qbittorrent_refused_credentials_and_a_ban() {
    capture();
    let state = qbit_state(GENERATED);
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = qbit_creds(WRONG);
    let input = ConfigureInput::new(&creds).installed();
    let e = qbit().plan(&http(), &target, &input).await.unwrap_err();
    assert_eq!(e, AdapterError::Unauthorized);
    clean_error(&e);
    assert_eq!(server.count(), 1, "one sign-in, nothing after it");

    state.lock().unwrap().banned = true;
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds).installed();
    let e = qbit().plan(&http(), &target, &input).await.unwrap_err();
    assert_eq!(e, AdapterError::Refused(qbittorrent::BANNED));
    assert_eq!(server.count(), 2, "a ban is never retried");

    let none = Creds(HashMap::new());
    let input = ConfigureInput::new(&none).installed();
    assert_eq!(
        qbit().plan(&http(), &target, &input).await,
        Err(AdapterError::Unauthorized)
    );
    clean_urls(&server.seen());
}

#[tokio::test]
async fn qbittorrent_guided_install_replaces_the_temporary_password() {
    capture();
    let state = qbit_state(TEMPORARY);
    state.lock().unwrap().username = "admin".into();
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.entered();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .guided(Confirmation::of(target.clone()), Secret::new(TEMPORARY))
        .username(Username::parse("admin").unwrap());
    let http = http();
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["webui.password"]);
    assert_eq!(
        state.lock().unwrap().password,
        TEMPORARY,
        "planning changed nothing"
    );

    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    assert_eq!(state.lock().unwrap().password, GENERATED);

    // The new password is what signs in now; the temporary one is gone.
    let again = qbit().plan(&http, &target, &input).await.unwrap();
    assert!(again.changes().is_empty());
    let last_login = server
        .seen()
        .into_iter()
        .filter(|s| s.path.ends_with("/login"))
        .next_back()
        .unwrap();
    assert_eq!(form_decode(&last_login.body)["password"], GENERATED);
    clean_urls(&server.seen());

    // A qBittorrent that ignores the new password fails the apply rather
    // than leaving the temporary one in place quietly.
    let stuck = qbit_state(TEMPORARY);
    {
        let mut s = stuck.lock().unwrap();
        s.username = "admin".into();
        s.ignore_password = true;
    }
    let server = Server::start(qbit_handler(Arc::clone(&stuck))).await;
    let target = server.entered();
    let input = ConfigureInput::new(&creds)
        .guided(Confirmation::of(target.clone()), Secret::new(TEMPORARY))
        .username(Username::parse("admin").unwrap());
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    let e = qbit()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap_err();
    assert_eq!(e, AdapterError::Refused(qbittorrent::PASSWORD_NOT_TAKEN));
    clean_error(&e);
}

#[tokio::test]
async fn qbittorrent_refuses_a_socks5_route_and_unsuited_bindings_before_sending() {
    capture();
    let server = Server::start(qbit_handler(qbit_state(GENERATED))).await;
    let creds = qbit_creds(GENERATED);
    let socks = Route::Socks5(Socks5Proxy::new("172.17.0.2:1080".parse().unwrap()).unwrap());
    let lo = server.loopback();
    let input = ConfigureInput::new(&creds).installed().route(socks);
    assert_eq!(
        qbit().plan(&http(), &lo, &input).await,
        Err(AdapterError::Refused(ROUTE_UNSUPPORTED))
    );
    // Binding to loopback would lock Lodestar out of a URL you entered.
    let entered = server.entered();
    let input = ConfigureInput::new(&creds)
        .confirmed(Confirmation::of(entered.clone()))
        .webui(WebUiBind::Loopback);
    assert_eq!(
        qbit().plan(&http(), &entered, &input).await,
        Err(AdapterError::Refused(BIND_UNSUITED))
    );
    let input = ConfigureInput::new(&creds)
        .installed()
        .webui(WebUiBind::ContainerNetwork);
    assert_eq!(
        qbit().plan(&http(), &lo, &input).await,
        Err(AdapterError::Refused(BIND_UNSUITED))
    );
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn unconfirmed_targets_are_refused_before_anything_is_sent() {
    capture();
    let server = Server::start(qbit_handler(qbit_state(GENERATED))).await;
    let entered = server.entered();
    let lo = server.loopback();
    let qcreds = qbit_creds(GENERATED);
    let pcreds = Creds(HashMap::from([("api_key", API_KEY)]));
    let sink = FakeSink::default();
    for (creds, adapter, id) in [
        (&qcreds, qbit(), "qbittorrent"),
        (&pcreds, prowlarr_adapter(), "prowlarr"),
    ] {
        let id = CompanionId::parse(id).unwrap();
        let inputs = [
            // Nothing stated.
            ConfigureInput::new(creds).route(Route::Direct),
            // Lodestar never installs at a URL you entered.
            ConfigureInput::new(creds).installed().route(Route::Direct),
            // A confirmation for another target.
            ConfigureInput::new(creds)
                .confirmed(Confirmation::of(lo.clone()))
                .route(Route::Direct),
            ConfigureInput::new(creds)
                .guided(Confirmation::of(lo.clone()), Secret::new(TEMPORARY))
                .route(Route::Direct),
        ];
        for input in &inputs {
            let refused = AdapterError::Refused(NOT_CONFIRMED);
            assert_eq!(adapter.plan(&http(), &entered, input).await, Err(refused));
            let plan = adapter
                .plan(
                    &http(),
                    &lo,
                    &ConfigureInput::new(creds).installed().route(Route::Direct),
                )
                .await;
            if let Ok(plan) = plan {
                assert_eq!(
                    adapter.apply(&http(), &entered, input, &plan).await,
                    Err(refused)
                );
            }
            assert_eq!(
                configure::hand_over(&sink, &id, &entered, input).await,
                Err(refused)
            );
        }
        // Not stated at all, even on loopback.
        let bare = ConfigureInput::new(creds);
        assert_eq!(
            adapter.plan(&http(), &lo, &bare).await,
            Err(AdapterError::Refused(NOT_CONFIRMED))
        );
    }
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.path != "/" && !s.path.is_empty())
    );
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.method == "GET" || s.path.ends_with("/login")),
        "only the loopback plans reached the server"
    );
    assert!(sink.entries.lock().unwrap().is_empty());
}

fn oversized() -> Reply {
    reply(200, vec![b' '; 300 * 1024])
}
fn malformed() -> Reply {
    reply(200, r#"{"save_path":"/x","save_path":"/y"}"#)
}
fn html() -> Reply {
    Reply::Full {
        status: 200,
        headers: vec![("Content-Type".into(), "text/html".into())],
        body: b"<html><title>Sign in to your router</title></html>".to_vec(),
    }
}
fn silent() -> Reply {
    Reply::Silent
}

#[tokio::test]
async fn qbittorrent_hostile_answers() {
    capture();
    let creds = qbit_creds(GENERATED);
    let http = http();
    for (hostile, want) in [
        (oversized as fn() -> Reply, AdapterError::TooLarge),
        (malformed, AdapterError::Protocol("preferences")),
        (html, AdapterError::Protocol("preferences")),
        (silent, AdapterError::Timeout),
    ] {
        let state = qbit_state(GENERATED);
        state.lock().unwrap().hostile = Some(hostile);
        let server = Server::start(qbit_handler(state)).await;
        let target = server.loopback();
        let input = ConfigureInput::new(&creds).installed();
        let started = std::time::Instant::now();
        let e = qbit().plan(&http, &target, &input).await.unwrap_err();
        assert_eq!(e, want);
        clean_error(&e);
        // The client's own deadline ended a silent server, not this test.
        assert!(started.elapsed() < Duration::from_secs(60));
    }
    // A sign-in whose cookie isn't usable isn't a session.
    let state = qbit_state(GENERATED);
    state.lock().unwrap().no_cookie = true;
    let server = Server::start(qbit_handler(state)).await;
    let input = ConfigureInput::new(&creds).installed();
    assert_eq!(
        qbit().plan(&http, &server.loopback(), &input).await,
        Err(AdapterError::Protocol("no session cookie"))
    );
}

#[tokio::test]
async fn apply_refuses_another_companions_plan() {
    capture();
    let p = Server::start(prowlarr_handler(prowlarr_state())).await;
    let q = Server::start(qbit_handler(qbit_state(GENERATED))).await;
    let pcreds = Creds(HashMap::from([("api_key", API_KEY)]));
    let pin = ConfigureInput::new(&pcreds).installed().route(socks5());
    let plan = prowlarr_adapter()
        .plan(&http(), &p.loopback(), &pin)
        .await
        .unwrap();
    assert!(!plan.changes().is_empty());
    let qcreds = qbit_creds(GENERATED);
    let qin = ConfigureInput::new(&qcreds).installed();
    assert_eq!(
        qbit().apply(&http(), &q.loopback(), &qin, &plan).await,
        Err(AdapterError::Refused("the plan is for another companion"))
    );
    assert_eq!(q.count(), 0);
}

// ─── A fake Prowlarr ────────────────────────────────────────────────────

struct PState {
    host: Map<String, Value>,
    hostile: Option<fn() -> Reply>,
}

type Prow = Arc<Mutex<PState>>;

fn prowlarr_state() -> Prow {
    Arc::new(Mutex::new(PState {
        host: fixture("prowlarr/configure/host.json"),
        hostile: None,
    }))
}

fn prowlarr_handler(state: Prow) -> Handler {
    Arc::new(move |seen: &Seen| {
        let mut s = state.lock().unwrap();
        if seen.header("x-api-key") != Some(API_KEY) {
            return reply(401, "");
        }
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/api/v1/config/host") => match s.hostile {
                Some(h) => h(),
                None => reply(200, Value::Object(s.host.clone()).to_string()),
            },
            ("PUT", "/api/v1/config/host/1") => {
                if seen.header("content-type") != Some("application/json") {
                    return reply(415, "");
                }
                let Ok(Value::Object(m)) = serde_json::from_slice(&seen.body) else {
                    return reply(400, "");
                };
                s.host = m;
                reply(202, "1")
            }
            _ => reply(404, ""),
        }
    })
}

fn socks5() -> Route {
    Route::Socks5(Socks5Proxy::new("172.17.0.2:1080".parse().unwrap()).unwrap())
}

#[tokio::test]
async fn prowlarr_first_configuration_then_an_empty_plan() {
    capture();
    let state = prowlarr_state();
    let server = Server::start(prowlarr_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = Creds(HashMap::from([("api_key", API_KEY)]));
    let input = ConfigureInput::new(&creds).installed().route(socks5());
    let http = http();
    let adapter = prowlarr_adapter();
    let plan = adapter.plan(&http, &target, &input).await.unwrap();
    assert_eq!(
        names(&plan),
        [
            "proxy.enabled",
            "proxy.type",
            "proxy.hostname",
            "proxy.port"
        ]
    );
    adapter.apply(&http, &target, &input, &plan).await.unwrap();
    {
        let s = state.lock().unwrap();
        assert_eq!(s.host["proxyEnabled"], true);
        assert_eq!(s.host["proxyType"], "socks5");
        assert_eq!(s.host["proxyHostname"], "172.17.0.2");
        assert_eq!(s.host["proxyPort"], 1080);
        // Everything else went back as it was.
        let mut rest = s.host.clone();
        let mut before = fixture("prowlarr/configure/host.json");
        for k in ["proxyEnabled", "proxyType", "proxyHostname", "proxyPort"] {
            rest.remove(k);
            before.remove(k);
        }
        assert_eq!(rest, before);
    }
    clean_urls(&server.seen());
    let again = adapter.plan(&http, &target, &input).await.unwrap();
    assert!(again.changes().is_empty());

    // Back to direct: only the switch changes.
    let input = ConfigureInput::new(&creds).installed().route(Route::Direct);
    let plan = adapter.plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["proxy.enabled"]);
    adapter.apply(&http, &target, &input, &plan).await.unwrap();
    assert_eq!(state.lock().unwrap().host["proxyEnabled"], false);
    assert_eq!(state.lock().unwrap().host["proxyHostname"], "172.17.0.2");
}

#[tokio::test]
async fn prowlarr_already_and_partly_configured() {
    capture();
    let state = prowlarr_state();
    let server = Server::start(prowlarr_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = Creds(HashMap::from([("api_key", API_KEY)]));
    let http = http();
    // Direct, and the proxy is already off.
    let input = ConfigureInput::new(&creds).installed().route(Route::Direct);
    assert!(
        prowlarr_adapter()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
    assert!(server.seen().iter().all(|s| s.method == "GET"));

    // No route: nothing planned and nothing sent.
    let before = server.count();
    let input = ConfigureInput::new(&creds).installed();
    assert!(
        prowlarr_adapter()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
    assert_eq!(server.count(), before);

    // On, SOCKS5, another host and port: only those two change.
    {
        let mut s = state.lock().unwrap();
        s.host.insert("proxyEnabled".into(), true.into());
        s.host.insert("proxyType".into(), "socks5".into());
        s.host.insert("proxyHostname".into(), "10.0.0.9".into());
    }
    let input = ConfigureInput::new(&creds).installed().route(socks5());
    let plan = prowlarr_adapter()
        .plan(&http, &target, &input)
        .await
        .unwrap();
    assert_eq!(names(&plan), ["proxy.hostname", "proxy.port"]);
    prowlarr_adapter()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap();
    assert!(
        prowlarr_adapter()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
}

#[tokio::test]
async fn prowlarr_refusals_and_hostile_answers() {
    capture();
    let state = prowlarr_state();
    let server = Server::start(prowlarr_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let http = http();

    // An interface or an address isn't a route Prowlarr can follow (Q8).
    let creds = Creds(HashMap::from([("api_key", API_KEY)]));
    for route in [
        tun0(),
        Route::BindAddress(configure::BindIp::new("10.8.0.2".parse().unwrap()).unwrap()),
    ] {
        let input = ConfigureInput::new(&creds).installed().route(route);
        assert_eq!(
            prowlarr_adapter().plan(&http, &target, &input).await,
            Err(AdapterError::Refused(ROUTE_UNSUPPORTED))
        );
    }
    assert_eq!(server.count(), 0);

    // A refused key.
    let wrong = Creds(HashMap::from([(
        "api_key",
        "ffffffffffffffffffffffffffffffff",
    )]));
    let input = ConfigureInput::new(&wrong).installed().route(socks5());
    let e = prowlarr_adapter()
        .plan(&http, &target, &input)
        .await
        .unwrap_err();
    assert_eq!(e, AdapterError::Unauthorized);
    clean_error(&e);
    // The key went as a header, never in the URL.
    assert!(
        server
            .seen()
            .iter()
            .all(|s| s.path == "/api/v1/config/host")
    );
    clean_urls(&server.seen());

    let input = ConfigureInput::new(&creds).installed().route(socks5());
    for (hostile, want) in [
        (
            (|| reply(200, vec![b' '; 65 * 1024])) as fn() -> Reply,
            AdapterError::TooLarge,
        ),
        (malformed, AdapterError::Protocol("host settings")),
        (html, AdapterError::Protocol("host settings")),
        (
            || {
                reply(
                    200,
                    r#"{"id":2,"proxyEnabled":false,"proxyType":"http","proxyHostname":null,"proxyPort":1}"#,
                )
            },
            AdapterError::Protocol("host settings"),
        ),
        (silent, AdapterError::Timeout),
    ] {
        state.lock().unwrap().hostile = Some(hostile);
        let e = prowlarr_adapter()
            .plan(&http, &target, &input)
            .await
            .unwrap_err();
        assert_eq!(e, want);
        clean_error(&e);
    }
}

// ─── Lodestar's own settings ────────────────────────────────────────────

#[derive(Default)]
struct FakeSink {
    entries: Mutex<Vec<String>>,
    refuse: bool,
}

impl ConfigSink for FakeSink {
    fn download_client<'a>(
        &'a self,
        e: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async move {
            if self.refuse {
                return Err(SinkError::Rejected);
            }
            self.entries.lock().unwrap().push(format!(
                "client {} {:?} {} {} {:?}",
                e.companion,
                e.target,
                e.username.as_str(),
                e.password.expose(),
                e.category.map(|c| c.as_str().to_owned())
            ));
            Ok(())
        })
    }

    fn indexer_manager<'a>(
        &'a self,
        e: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async move {
            self.entries.lock().unwrap().push(format!(
                "indexer {} {:?} {}",
                e.companion,
                e.target,
                e.api_key.expose()
            ));
            Ok(())
        })
    }
}

#[tokio::test]
async fn hand_over_writes_lodestars_entries_from_its_own_values() {
    capture();
    let lo = Target::Loopback(PortNumber::new(8080).unwrap());
    let sink = FakeSink::default();
    let qcreds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&qcreds)
        .installed()
        .downloads(downloads());
    let q = CompanionId::parse("qbittorrent").unwrap();
    configure::hand_over(&sink, &q, &lo, &input).await.unwrap();
    let pcreds = Creds(HashMap::from([("api_key", API_KEY)]));
    let input = ConfigureInput::new(&pcreds).installed();
    let p = CompanionId::parse("prowlarr").unwrap();
    configure::hand_over(&sink, &p, &lo, &input).await.unwrap();
    assert_eq!(
        *sink.entries.lock().unwrap(),
        [
            format!(
                "client qbittorrent Loopback(PortNumber(8080)) lodestar {GENERATED} Some(\"lodestar\")"
            ),
            format!("indexer prowlarr Loopback(PortNumber(8080)) {API_KEY}"),
        ]
    );
    let refusing = FakeSink {
        refuse: true,
        ..FakeSink::default()
    };
    let input = ConfigureInput::new(&qcreds).installed();
    assert_eq!(
        configure::hand_over(&refusing, &q, &lo, &input).await,
        Err(AdapterError::Refused(SINK_REFUSED))
    );
    let g = CompanionId::parse("gluetun").unwrap();
    assert!(configure::hand_over(&sink, &g, &lo, &input).await.is_err());
}

#[tokio::test]
async fn no_secret_reaches_a_log_line() {
    let sink = capture();
    // A guided run with a failed sign-in, an apply, and a refused key.
    let state = qbit_state(TEMPORARY);
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .guided(Confirmation::of(target.clone()), Secret::new(TEMPORARY))
        .downloads(downloads());
    let http = http();
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    let wrong = qbit_creds(WRONG);
    let _ = qbit()
        .plan(&http, &target, &ConfigureInput::new(&wrong).installed())
        .await;

    let lines = sink.0.lock().unwrap().clone();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("companions.config_applied") && l.contains("webui.password")),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("companions.config_failed"))
    );
    let port = server.addr.port().to_string();
    for l in &lines {
        for s in [
            GENERATED,
            TEMPORARY,
            WRONG,
            API_KEY,
            SID_PREFIX,
            "/data/downloads",
            &port,
        ] {
            assert!(!l.contains(s), "{l}");
        }
    }
}
