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
    Downloads, IndexerManagerEntry, InstallReceipt, InterfaceName, NOT_CONFIRMED,
    ROUTE_UNSUPPORTED, Route, SINK_REFUSED, SinkError, Socks5Proxy, WOULD_LOOSEN, WebUiBind,
    qbittorrent,
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

fn receipt(target: &Target) -> InstallReceipt {
    InstallReceipt::for_tests(target.clone())
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
    /// Answer as qBittorrent 5.2 does: `QBT_SID_<port>`, `204` for a
    /// sign-in or a write, `401` for a wrong password.
    v52: bool,
    /// Skips sign-in for every request, as a subnet whitelist does: any
    /// password signs in and the version endpoint answers without one.
    bypass: bool,
    /// Skips sign-in while `bypass_local_auth` is on, as for loopback.
    bypass_from_prefs: bool,
    /// Replaces the sign-in answer.
    hostile_login: Option<fn() -> Reply>,
    /// Replaces the categories answer.
    hostile_categories: Option<fn() -> Reply>,
    /// Takes the next password write but never answers it.
    silent_password_once: bool,
    /// Bans every sign-in that isn't bypassed, as after five refusals.
    banned_unless_bypassed: bool,
    /// Refuses a write that turns the localhost bypass back on.
    refuse_restore: bool,
    /// Sign-ins that may still succeed; after that every one is refused.
    logins_left: Option<u32>,
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
        v52: false,
        bypass: false,
        bypass_from_prefs: false,
        hostile_login: None,
        hostile_categories: None,
        silent_password_once: false,
        banned_unless_bypassed: false,
        refuse_restore: false,
        logins_left: None,
    }))
}

fn qbit_handler(state: Qbit) -> Handler {
    Arc::new(move |seen: &Seen| {
        let mut s = state.lock().unwrap();
        let bypassing =
            s.bypass || (s.bypass_from_prefs && s.prefs["bypass_local_auth"] == json!(true));
        if seen.path == "/api/v2/app/version" {
            return if bypassing {
                reply(200, "v5.0.3")
            } else {
                reply(403, "Forbidden")
            };
        }
        if seen.method == "POST" && seen.path == "/api/v2/auth/login" {
            if let Some(h) = s.hostile_login {
                return h();
            }
            if s.banned || (s.banned_unless_bypassed && !bypassing) {
                return reply(403, "Your IP address has been banned");
            }
            let f = form_decode(&seen.body);
            let matches =
                f.get("username") == Some(&s.username) && f.get("password") == Some(&s.password);
            let allowed = s.logins_left.is_none_or(|n| n > 0);
            if (matches || bypassing) && allowed {
                if let Some(n) = s.logins_left.as_mut() {
                    *n -= 1;
                }
                let sid = format!("{SID_PREFIX}{}", s.sessions.len());
                s.sessions.push(sid.clone());
                let name = if s.v52 { "QBT_SID_8080" } else { "SID" };
                let cookie = if s.no_cookie {
                    format!("{name}=bad value; HttpOnly")
                } else {
                    format!("{name}={sid}; HttpOnly; SameSite=Strict; path=/")
                };
                let (status, body) = if s.v52 {
                    (204, &b""[..])
                } else {
                    (200, &b"Ok."[..])
                };
                return Reply::Full {
                    status,
                    headers: vec![("Set-Cookie".into(), cookie)],
                    body: body.to_vec(),
                };
            }
            return if s.v52 {
                reply(401, "")
            } else {
                reply(200, "Fails.")
            };
        }
        let prefix = if s.v52 { "QBT_SID_8080=" } else { "SID=" };
        let authed = bypassing
            || seen
                .header("cookie")
                .and_then(|c| c.strip_prefix(prefix))
                .is_some_and(|sid| s.sessions.iter().any(|x| x == sid));
        if !authed {
            return reply(403, "Forbidden");
        }
        match (seen.method.as_str(), seen.path.as_str()) {
            ("GET", "/api/v2/app/preferences") => match s.hostile {
                Some(h) => h(),
                None => reply(200, Value::Object(s.prefs.clone()).to_string()),
            },
            ("GET", "/api/v2/torrents/categories") => match s.hostile_categories {
                Some(h) => h(),
                None => reply(200, Value::Object(s.categories.clone()).to_string()),
            },
            ("POST", "/api/v2/app/setPreferences") => {
                let f = form_decode(&seen.body);
                let Ok(Value::Object(m)) = serde_json::from_str(&f["json"]) else {
                    return reply(400, "");
                };
                if s.refuse_restore && m.get("bypass_local_auth") == Some(&json!(true)) {
                    return reply(400, "");
                }
                let mut silent = false;
                for (k, v) in m {
                    if k == "web_ui_password" {
                        if !s.ignore_password {
                            s.password = v.as_str().unwrap().to_owned();
                        }
                        silent = std::mem::take(&mut s.silent_password_once);
                    } else {
                        s.prefs.insert(k, v);
                    }
                }
                if silent {
                    return Reply::Silent;
                }
                reply(if s.v52 { 204 } else { 200 }, "")
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
                reply(if s.v52 { 204 } else { 200 }, "")
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
    Box::new(qbittorrent::QBittorrent::new(
        CompanionId::parse("qbittorrent").unwrap(),
    ))
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
        .installed(receipt(&target))
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
        .installed(receipt(&target))
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
        .installed(receipt(&target))
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
    // Applied, then planned again: nothing left.
    assert!(
        qbit()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
}

#[tokio::test]
async fn qbittorrent_refused_credentials_and_a_ban() {
    capture();
    let state = qbit_state(GENERATED);
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = qbit_creds(WRONG);
    let input = ConfigureInput::new(&creds).installed(receipt(&target));
    let e = qbit().plan(&http(), &target, &input).await.unwrap_err();
    assert_eq!(e, AdapterError::Unauthorized);
    clean_error(&e);
    assert_eq!(server.count(), 1, "one sign-in, nothing after it");

    state.lock().unwrap().banned = true;
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds).installed(receipt(&target));
    let e = qbit().plan(&http(), &target, &input).await.unwrap_err();
    assert_eq!(e, AdapterError::Refused(qbittorrent::BANNED));
    assert_eq!(server.count(), 2, "a ban is never retried");

    let none = Creds(HashMap::new());
    let input = ConfigureInput::new(&none).installed(receipt(&target));
    assert_eq!(
        qbit().plan(&http(), &target, &input).await,
        Err(AdapterError::Unauthorized)
    );
    clean_urls(&server.seen());
}

/// A guided plan holding the temporary password as the credential too (no
/// password generated yet) signs in once: a wrong one costs one refused
/// sign-in, not two (#653's review, L2).
#[tokio::test]
async fn qbittorrent_guided_plan_with_only_the_temporary_password_signs_in_once() {
    capture();
    let state = qbit_state(TEMPORARY);
    state.lock().unwrap().username = "admin".into();
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.entered();
    let logins = |s: &Server| {
        s.seen()
            .iter()
            .filter(|r| r.path.ends_with("/login"))
            .count()
    };
    let input_with = |creds: &'static Creds, t: &'static str| {
        ConfigureInput::new(creds)
            .guided(Confirmation::of(target.clone()), Secret::new(t))
            .username(Username::parse("admin").unwrap())
    };
    let right: &'static Creds = Box::leak(Box::new(qbit_creds(TEMPORARY)));
    let plan = qbit()
        .plan(&http(), &target, &input_with(right, TEMPORARY))
        .await
        .unwrap();
    assert_eq!(names(&plan), ["webui.password"]);
    assert_eq!(logins(&server), 1);
    let wrong: &'static Creds = Box::leak(Box::new(qbit_creds("wrong")));
    assert_eq!(
        qbit()
            .plan(&http(), &target, &input_with(wrong, "wrong"))
            .await,
        Err(AdapterError::Unauthorized)
    );
    assert_eq!(logins(&server), 2, "one refused sign-in");
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
        .rfind(|s| s.path.ends_with("/login"))
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
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&lo))
        .route(socks);
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
        .installed(receipt(&lo))
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
    // The refused target is a server of its own, which must see nothing.
    let refused_server = Server::start(Arc::new(|_: &Seen| reply(404, ""))).await;
    let entered = refused_server.entered();
    let q = Server::start(qbit_handler(qbit_state(GENERATED))).await;
    let p = Server::start(prowlarr_handler(prowlarr_state())).await;
    let qcreds = qbit_creds(GENERATED);
    let pcreds = Creds(HashMap::from([("api_key", API_KEY)]));
    let sink = FakeSink::default();
    let qlo = q.loopback();
    let plo = p.loopback();
    let cases = [
        (&qcreds, qbit(), "qbittorrent", &q, &qlo),
        (&pcreds, prowlarr_adapter(), "prowlarr", &p, &plo),
    ];
    for (creds, adapter, id, server, lo) in cases {
        let id = CompanionId::parse(id).unwrap();
        // A real, non-empty plan from the companion's own server.
        let good = ConfigureInput::new(creds)
            .installed(receipt(lo))
            .downloads(downloads())
            .route(socks5_or_direct(id.as_str()));
        let plan = adapter.plan(&http(), lo, &good).await.unwrap();
        assert!(!plan.changes().is_empty(), "{id}");
        let inputs = [
            // Nothing stated.
            ConfigureInput::new(creds).route(Route::Direct),
            // Lodestar never installs at a URL you entered.
            ConfigureInput::new(creds)
                .installed(receipt(&entered))
                .route(Route::Direct),
            // A receipt for another target.
            ConfigureInput::new(creds)
                .installed(receipt(lo))
                .route(Route::Direct),
            // A confirmation for another target.
            ConfigureInput::new(creds)
                .confirmed(Confirmation::of(lo.clone()))
                .route(Route::Direct),
            ConfigureInput::new(creds)
                .guided(Confirmation::of(lo.clone()), Secret::new(TEMPORARY))
                .route(Route::Direct),
        ];
        let refused = AdapterError::Refused(NOT_CONFIRMED);
        let before = server.count();
        for input in &inputs {
            assert_eq!(adapter.plan(&http(), &entered, input).await, Err(refused));
            assert_eq!(
                adapter.apply(&http(), &entered, input, &plan).await,
                Err(refused)
            );
            assert_eq!(
                configure::hand_over(&sink, &id, &entered, input).await,
                Err(refused)
            );
        }
        // Not stated at all, even on the companion's own loopback target.
        let bare = ConfigureInput::new(creds);
        assert_eq!(adapter.plan(&http(), lo, &bare).await, Err(refused));
        assert_eq!(adapter.apply(&http(), lo, &bare, &plan).await, Err(refused));
        assert_eq!(
            server.count(),
            before,
            "{id}: nothing reached the companion"
        );
    }
    assert_eq!(
        refused_server.count(),
        0,
        "nothing reached the refused target"
    );
    assert!(sink.entries.lock().unwrap().is_empty());
}

/// A route each adapter can follow and that changes something on its
/// fixture: an interface for qBittorrent, SOCKS5 for Prowlarr.
fn socks5_or_direct(id: &str) -> Route {
    if id == "prowlarr" { socks5() } else { tun0() }
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
        let input = ConfigureInput::new(&creds).installed(receipt(&target));
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
    let input = ConfigureInput::new(&creds).installed(receipt(&server.loopback()));
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
    let pin = ConfigureInput::new(&pcreds)
        .installed(receipt(&p.loopback()))
        .route(socks5());
    let plan = prowlarr_adapter()
        .plan(&http(), &p.loopback(), &pin)
        .await
        .unwrap();
    assert!(!plan.changes().is_empty());
    let qcreds = qbit_creds(GENERATED);
    let qin = ConfigureInput::new(&qcreds).installed(receipt(&q.loopback()));
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
                // As `ConfigService.SaveConfigDictionary` does: a null
                // value leaves the stored one as it is.
                for (k, v) in m {
                    if !v.is_null() {
                        s.host.insert(k, v);
                    }
                }
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
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .route(socks5());
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
        // The old proxy's credentials are emptied, not left in place.
        assert_eq!(s.host["proxyUsername"], "");
        assert_eq!(s.host["proxyPassword"], "");
        for k in [
            "proxyEnabled",
            "proxyType",
            "proxyHostname",
            "proxyPort",
            "proxyUsername",
            "proxyPassword",
        ] {
            rest.remove(k);
            before.remove(k);
        }
        assert_eq!(rest, before);
    }
    let seen = server.seen();
    clean_urls(&seen);
    // The PUT is GET's object with only the planned proxy fields changed,
    // and the key goes in `X-Api-Key`; the body holds it only as GET's own
    // echoed `apiKey`.
    let puts: Vec<_> = seen.iter().filter(|s| s.method == "PUT").collect();
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].header("x-api-key"), Some(API_KEY));
    let mut sent: Map<String, Value> = serde_json::from_slice(&puts[0].body).unwrap();
    let mut got = fixture("prowlarr/configure/host.json");
    assert_eq!(sent["apiKey"], got["apiKey"]);
    for (k, v) in [
        ("proxyEnabled", json!(true)),
        ("proxyType", json!("socks5")),
        ("proxyHostname", json!("172.17.0.2")),
        ("proxyPort", json!(1080)),
        ("proxyUsername", json!("")),
        ("proxyPassword", json!("")),
    ] {
        assert_eq!(sent.remove(k), Some(v), "{k}");
        got.remove(k);
    }
    assert_eq!(sent, got, "every other field went back as GET sent it");
    let again = adapter.plan(&http, &target, &input).await.unwrap();
    assert!(again.changes().is_empty());

    // Back to direct: only the switch changes.
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .route(Route::Direct);
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
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .route(Route::Direct);
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
    let input = ConfigureInput::new(&creds).installed(receipt(&target));
    assert!(
        prowlarr_adapter()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
    assert_eq!(server.count(), before);

    // On, SOCKS5, another host and port, and a bypass filter: only those
    // three change.
    {
        let mut s = state.lock().unwrap();
        s.host.insert("proxyEnabled".into(), true.into());
        s.host.insert("proxyType".into(), "socks5".into());
        s.host.insert("proxyHostname".into(), "10.0.0.9".into());
        s.host.insert("proxyBypassFilter".into(), "*.org".into());
        s.host.insert("proxyUsername".into(), "olduser".into());
        s.host.insert("proxyPassword".into(), "oldproxypass".into());
    }
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .route(socks5());
    let plan = prowlarr_adapter()
        .plan(&http, &target, &input)
        .await
        .unwrap();
    assert_eq!(
        names(&plan),
        ["proxy.hostname", "proxy.port", "proxy.bypass_filter"]
    );
    prowlarr_adapter()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap();
    // A new host never gets the old proxy's credentials.
    assert_eq!(state.lock().unwrap().host["proxyUsername"], "");
    assert_eq!(state.lock().unwrap().host["proxyPassword"], "");
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
        let input = ConfigureInput::new(&creds)
            .installed(receipt(&target))
            .route(route);
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
    let input = ConfigureInput::new(&wrong)
        .installed(receipt(&target))
        .route(socks5());
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

    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .route(socks5());
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
        .installed(receipt(&lo))
        .downloads(downloads());
    let q = CompanionId::parse("qbittorrent").unwrap();
    configure::hand_over(&sink, &q, &lo, &input).await.unwrap();
    let pcreds = Creds(HashMap::from([("api_key", API_KEY)]));
    let input = ConfigureInput::new(&pcreds).installed(receipt(&lo));
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
    let input = ConfigureInput::new(&qcreds).installed(receipt(&lo));
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
        .plan(
            &http,
            &target,
            &ConfigureInput::new(&wrong).installed(receipt(&target)),
        )
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

#[tokio::test]
async fn qbittorrent_5_2_sign_in_cookie_and_answers() {
    capture();
    let state = qbit_state(TEMPORARY);
    {
        let mut s = state.lock().unwrap();
        s.v52 = true;
        s.username = "admin".into();
    }
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.entered();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .guided(Confirmation::of(target.clone()), Secret::new(TEMPORARY))
        .username(Username::parse("admin").unwrap())
        .downloads(downloads())
        .route(tun0());
    let http = http();
    // The stored password is refused with 401, so the temporary one is
    // tried, and 5.2's `204` and `QBT_SID_<port>` make a session.
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(
        names(&plan),
        [
            "webui.password",
            "downloads.categories",
            "downloads.save_path",
            "network.interface"
        ]
    );
    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    assert_eq!(state.lock().unwrap().password, GENERATED);
    assert!(
        qbit()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
    // Every call but the sign-in and the unauthenticated bypass probe
    // carries 5.2's cookie.
    for s in server
        .seen()
        .iter()
        .filter(|s| !s.path.ends_with("/login") && !s.path.ends_with("/version"))
    {
        assert!(
            s.header("cookie")
                .unwrap()
                .starts_with("QBT_SID_8080=sessionvalue")
        );
    }
    clean_urls(&server.seen());

    // A wrong password on 5.2 is `Unauthorized`, as on 5.1.
    let wrong = qbit_creds(WRONG);
    let input = ConfigureInput::new(&wrong).confirmed(Confirmation::of(target.clone()));
    assert_eq!(
        qbit().plan(&http, &target, &input).await,
        Err(AdapterError::Unauthorized)
    );
}

// ─── Review round: binding, bypass, lockout, loosening ─────────────────

#[tokio::test]
async fn webui_binding_is_only_for_companions_lodestar_installed() {
    capture();
    let server = Server::start(qbit_handler(qbit_state(GENERATED))).await;
    let lo = server.loopback();
    let creds = qbit_creds(GENERATED);
    // Your own qBittorrent on loopback may be a container with a published
    // port: binding it to 127.0.0.1 would cut off Docker's forward.
    for input in [
        ConfigureInput::new(&creds)
            .confirmed(Confirmation::of(lo.clone()))
            .webui(WebUiBind::Loopback),
        ConfigureInput::new(&creds)
            .guided(Confirmation::of(lo.clone()), Secret::new(TEMPORARY))
            .webui(WebUiBind::Loopback),
    ] {
        assert_eq!(
            qbit().plan(&http(), &lo, &input).await,
            Err(AdapterError::Refused(BIND_UNSUITED))
        );
    }
    assert_eq!(server.count(), 0);
}

#[tokio::test]
async fn guided_install_under_an_auth_bypass_always_writes_the_password() {
    capture();
    let state = qbit_state(TEMPORARY);
    {
        let mut s = state.lock().unwrap();
        s.bypass = true;
        s.username = "admin".into();
    }
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.entered();
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds)
        .guided(Confirmation::of(target.clone()), Secret::new(TEMPORARY))
        .username(Username::parse("admin").unwrap());
    let http = http();
    // Any password signs in, so the stored one "works" too; the probe sees
    // the bypass and the plan writes the new password anyway.
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["webui.password"]);
    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    assert_eq!(state.lock().unwrap().password, GENERATED);
    // The check can't be made under a bypass, so a guided plan keeps
    // writing it (decisions file); CMP-5g confirms the origin after.
    let again = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&again), ["webui.password"]);
    let confirmed = ConfigureInput::new(&creds)
        .confirmed(Confirmation::of(target.clone()))
        .username(Username::parse("admin").unwrap());
    assert!(
        qbit()
            .plan(&http, &target, &confirmed)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
    clean_urls(&server.seen());
}

#[tokio::test]
async fn turning_local_auth_on_never_locks_lodestar_out() {
    capture();
    // qBittorrent lets loopback in without a password, and the password
    // Lodestar holds is wrong.
    let state = qbit_state(GENERATED);
    {
        let mut s = state.lock().unwrap();
        s.bypass_from_prefs = true;
        s.prefs.insert("bypass_local_auth".into(), true.into());
    }
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let wrong = qbit_creds(WRONG);
    let input = ConfigureInput::new(&wrong).installed(receipt(&target));
    let http = http();
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["webui.local_auth"]);
    let e = qbit()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap_err();
    assert_eq!(e, AdapterError::Refused(qbittorrent::LOCAL_AUTH_LOCKOUT));
    clean_error(&e);
    // The bypass went back on, so Lodestar still reaches it.
    assert_eq!(state.lock().unwrap().prefs["bypass_local_auth"], true);

    // After failed sign-ins qBittorrent bans the address: the fresh
    // sign-in gets `403`, and the bypass still goes back on.
    state.lock().unwrap().banned_unless_bypassed = true;
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    let e = qbit()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap_err();
    assert_eq!(e, AdapterError::Refused(qbittorrent::LOCAL_AUTH_LOCKOUT));
    clean_error(&e);
    assert_eq!(state.lock().unwrap().prefs["bypass_local_auth"], true);

    // If putting it back fails, that's its own code: the bypass is off and
    // the password doesn't work, which you have to be told.
    state.lock().unwrap().refuse_restore = true;
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    let e = qbit()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap_err();
    assert_eq!(
        e,
        AdapterError::Refused(qbittorrent::LOCAL_AUTH_RESTORE_FAILED)
    );
    clean_error(&e);
    assert_eq!(state.lock().unwrap().prefs["bypass_local_auth"], false);
    {
        let mut s = state.lock().unwrap();
        s.banned_unless_bypassed = false;
        s.refuse_restore = false;
        s.prefs.insert("bypass_local_auth".into(), true.into());
    }

    // Only what this run turned off is put back. The plan is made while
    // the bypass is on; by the apply someone else has turned it off. The
    // apply's own sign-in works and the fresh one after the batch doesn't:
    // the refusal comes back, and the bypass is left off as found.
    let good = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&good).installed(receipt(&target));
    {
        let mut s = state.lock().unwrap();
        s.bypass_from_prefs = false;
        s.prefs.insert("bypass_local_auth".into(), true.into());
    }
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["webui.local_auth"]);
    {
        let mut s = state.lock().unwrap();
        s.prefs.insert("bypass_local_auth".into(), false.into());
        s.logins_left = Some(1);
    }
    let before = server.count();
    let e = qbit()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap_err();
    assert_eq!(e, AdapterError::Refused(qbittorrent::LOCAL_AUTH_LOCKOUT));
    clean_error(&e);
    assert_eq!(state.lock().unwrap().prefs["bypass_local_auth"], false);
    let restores = server.seen()[before..]
        .iter()
        .filter(|s| s.path.ends_with("/setPreferences"))
        .filter(|s| {
            let json: Value = serde_json::from_str(&form_decode(&s.body)["json"]).unwrap();
            json.get("bypass_local_auth") == Some(&json!(true))
        })
        .count();
    assert_eq!(restores, 0, "nothing this run didn't turn off is put back");
    {
        let mut s = state.lock().unwrap();
        s.logins_left = None;
        s.bypass_from_prefs = true;
        s.prefs.insert("bypass_local_auth".into(), true.into());
    }

    // With the right password it turns off and stays off.
    let creds = qbit_creds(GENERATED);
    let input = ConfigureInput::new(&creds).installed(receipt(&target));
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    assert_eq!(state.lock().unwrap().prefs["bypass_local_auth"], false);
    assert!(
        qbit()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
}

#[tokio::test]
async fn a_bind_address_route_is_idempotent() {
    capture();
    let state = qbit_state(GENERATED);
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let creds = qbit_creds(GENERATED);
    let ip = configure::BindIp::new("10.8.0.2".parse().unwrap()).unwrap();
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .route(Route::BindAddress(ip));
    let http = http();
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["network.bind_address"]);
    qbit().apply(&http, &target, &input, &plan).await.unwrap();
    assert_eq!(
        state.lock().unwrap().prefs["current_interface_address"],
        "10.8.0.2"
    );
    assert_eq!(state.lock().unwrap().prefs["current_network_interface"], "");
    assert!(
        qbit()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
}

#[tokio::test]
async fn your_own_companions_routes_are_never_loosened() {
    capture();
    // qBittorrent you set up yourself, bound to a VPN interface.
    let state = qbit_state(GENERATED);
    state
        .lock()
        .unwrap()
        .prefs
        .insert("current_network_interface".into(), "tun0".into());
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.entered();
    let creds = qbit_creds(GENERATED);
    let http = http();
    let yours = |route: Route| {
        ConfigureInput::new(&creds)
            .confirmed(Confirmation::of(target.clone()))
            .route(route)
    };
    for route in [
        Route::Direct,
        Route::Interface(InterfaceName::parse("wg0").unwrap()),
        Route::BindAddress(configure::BindIp::new("10.8.0.2".parse().unwrap()).unwrap()),
    ] {
        assert_eq!(
            qbit().plan(&http, &target, &yours(route)).await,
            Err(AdapterError::Refused(WOULD_LOOSEN))
        );
    }
    assert!(
        qbit()
            .plan(&http, &target, &yours(tun0()))
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
    // A plan made for an install is refused at apply for your own: the
    // binding is read again first.
    let lo_target = server.loopback();
    let installed = ConfigureInput::new(&creds)
        .installed(receipt(&lo_target))
        .route(Route::Direct);
    let plan = qbit().plan(&http, &lo_target, &installed).await.unwrap();
    assert_eq!(names(&plan), ["network.interface"]);
    assert_eq!(
        qbit()
            .apply(&http, &target, &yours(Route::Direct), &plan)
            .await,
        Err(AdapterError::Refused(WOULD_LOOSEN))
    );
    assert_eq!(
        state.lock().unwrap().prefs["current_network_interface"],
        "tun0"
    );
    // Binding one where none is set only narrows it.
    state
        .lock()
        .unwrap()
        .prefs
        .insert("current_network_interface".into(), "".into());
    assert_eq!(
        names(&qbit().plan(&http, &target, &yours(tun0())).await.unwrap()),
        ["network.interface"]
    );

    // Prowlarr you set up yourself, already behind a SOCKS5 proxy.
    let pstate = prowlarr_state();
    {
        let mut s = pstate.lock().unwrap();
        s.host.insert("proxyEnabled".into(), true.into());
        s.host.insert("proxyType".into(), "socks5".into());
        s.host.insert("proxyHostname".into(), "10.0.0.9".into());
        s.host.insert("proxyPort".into(), 1080.into());
    }
    let pserver = Server::start(prowlarr_handler(Arc::clone(&pstate))).await;
    let ptarget = pserver.entered();
    let pcreds = Creds(HashMap::from([("api_key", API_KEY)]));
    for route in [Route::Direct, socks5()] {
        let input = ConfigureInput::new(&pcreds)
            .confirmed(Confirmation::of(ptarget.clone()))
            .route(route);
        assert_eq!(
            prowlarr_adapter().plan(&http, &ptarget, &input).await,
            Err(AdapterError::Refused(WOULD_LOOSEN))
        );
    }
    assert!(pserver.seen().iter().all(|s| s.method == "GET"));
    // Lodestar's own install may be switched back to direct.
    let plo = pserver.loopback();
    let input = ConfigureInput::new(&pcreds)
        .installed(receipt(&plo))
        .route(Route::Direct);
    assert_eq!(
        names(&prowlarr_adapter().plan(&http, &plo, &input).await.unwrap()),
        ["proxy.enabled"]
    );
}

fn login_oversized() -> Reply {
    reply(200, vec![b'O'; 300 * 1024])
}
fn login_error() -> Reply {
    reply(500, "Internal Server Error")
}
fn categories_malformed() -> Reply {
    reply(200, r#"{"lodestar":{"savePath":"/a"},"lodestar":{}}"#)
}

#[tokio::test]
async fn qbittorrent_hostile_sign_in_categories_and_a_lost_password_write() {
    capture();
    let creds = qbit_creds(GENERATED);
    let http = http();
    for (login, want) in [
        (login_oversized as fn() -> Reply, AdapterError::TooLarge),
        (login_error, AdapterError::Protocol("sign-in answer")),
    ] {
        let state = qbit_state(GENERATED);
        state.lock().unwrap().hostile_login = Some(login);
        let server = Server::start(qbit_handler(state)).await;
        let target = server.loopback();
        let input = ConfigureInput::new(&creds).installed(receipt(&target));
        assert_eq!(qbit().plan(&http, &target, &input).await, Err(want));
    }
    let state = qbit_state(GENERATED);
    state.lock().unwrap().hostile_categories = Some(categories_malformed);
    let server = Server::start(qbit_handler(state)).await;
    let target = server.loopback();
    let input = ConfigureInput::new(&creds)
        .installed(receipt(&target))
        .downloads(downloads());
    assert_eq!(
        qbit().plan(&http, &target, &input).await,
        Err(AdapterError::Protocol("categories"))
    );

    // The password write is taken but never answered: the apply times out
    // on the client's own deadline, and a retry finds the new password in
    // place and nothing left to do.
    let state = qbit_state(TEMPORARY);
    state.lock().unwrap().silent_password_once = true;
    let server = Server::start(qbit_handler(Arc::clone(&state))).await;
    let target = server.loopback();
    let input = ConfigureInput::new(&creds)
        .guided(Confirmation::of(target.clone()), Secret::new(TEMPORARY));
    let plan = qbit().plan(&http, &target, &input).await.unwrap();
    assert_eq!(names(&plan), ["webui.password"]);
    let e = qbit()
        .apply(&http, &target, &input, &plan)
        .await
        .unwrap_err();
    assert_eq!(e, AdapterError::Timeout);
    assert_eq!(state.lock().unwrap().password, GENERATED);
    assert!(
        qbit()
            .plan(&http, &target, &input)
            .await
            .unwrap()
            .changes()
            .is_empty()
    );
}
