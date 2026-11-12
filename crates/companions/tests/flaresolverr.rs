//! CMP-12: FlareSolverr, opt-in only (owner §2.47, Q21). Against fake
//! servers on loopback, through the real `CompanionHttp`:
//! - off by default: not in a default install or detection, and Prowlarr
//!   gets no proxy;
//! - switched on, Lodestar's proxy is written once; switched off, it's
//!   removed, and a proxy you made is never touched;
//! - detection and health: recorded answers, a timeout, oversized and
//!   malformed answers, random bytes;
//! - the catalogue entry, and FlareSolverr's port kept off your network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use lodestar_companions::adapter::{
    AdapterError, BoxFuture, ConfigureInput, CredentialError, Credentials, Detect, EnteredUrl,
    GuardedHttp, Health, HealthStatus, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::catalogue::{Arch, Catalogue, Exposure, Need, NotAvailable, Platform};
use lodestar_companions::compose::{
    Bind, DataFolders, Paths, Publish, Route, ServiceInput, TemplateError, compose,
};
use lodestar_companions::configure::{
    Confirmation, InstallReceipt, NOT_CONFIRMED, Switch, pairing,
};
use lodestar_companions::detect::{self, CompanionHttp, Runtime};
use lodestar_companions::flaresolverr::{
    self, ADD, EGRESS_DIRECT, PROXY_NAME, PROXY_NAME_TAKEN, Pairing, REMOVE, SOLVER_NOT_LOCAL,
    SolverAddress, TAG, TAG_LABEL, UPDATE,
};
use lodestar_companions::inputs::{ContainerName, NetworkName, Pgid, PortNumber, Puid};
use lodestar_companions::unraid::unraid;
use lodestar_companions::{CompanionId, builtin};
use lodestar_integrations::secret::Secret;
use proptest::prelude::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ─── A local server ─────────────────────────────────────────────────────

enum Reply {
    Full(u16, Vec<u8>),
    /// Read the request and say nothing.
    Silent,
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
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

    /// The requests that changed something.
    fn writes(&self) -> Vec<(String, String)> {
        self.seen()
            .into_iter()
            .filter(|s| s.method != "GET")
            .map(|s| (s.method, s.path))
            .collect()
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
        Reply::Full(status, body) => {
            let mut bytes = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            bytes.extend_from_slice(&body);
            let _ = sock.write_all(&bytes).await;
        }
        Reply::Silent => {
            let _ = sock.read(&mut buf).await;
        }
    }
    let _ = sock.shutdown().await;
}

fn http() -> CompanionHttp {
    CompanionHttp::new().unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/flaresolverr")
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixtures().join(name)).unwrap()).unwrap()
}

fn fs_id() -> CompanionId {
    builtin().unwrap().get(flaresolverr::ID).unwrap().id.clone()
}

// ─── Secrets and the log capture ────────────────────────────────────────

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

struct Creds;

impl Credentials for Creds {
    fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError> {
        Ok((field == "api_key").then(|| Secret::new(API_KEY)))
    }
}

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

fn capture() -> &'static Capture {
    static SINK: OnceLock<Capture> = OnceLock::new();
    let sink = SINK.get_or_init(|| Capture(Mutex::new(Vec::new())));
    let _ = lodestar_log::set_sink(sink);
    for e in lodestar_companions::log_events::CATALOGUE.events {
        e.set_on(true);
    }
    sink
}

// ─── A fake Prowlarr ────────────────────────────────────────────────────

struct Prowlarr {
    tags: Vec<Value>,
    proxies: Vec<Value>,
    next_id: u64,
}

type Prow = Arc<Mutex<Prowlarr>>;

fn prowlarr_state(proxies: Value, tags: Value) -> Prow {
    Arc::new(Mutex::new(Prowlarr {
        tags: tags.as_array().unwrap().clone(),
        proxies: proxies.as_array().unwrap().clone(),
        next_id: 10,
    }))
}

/// Prowlarr as its own user has it: a FlareSolverr proxy and an HTTP
/// proxy of yours, and one tag.
fn yours() -> Prow {
    prowlarr_state(
        fixture_json("configure/indexerproxy.json"),
        fixture_json("configure/tag.json"),
    )
}

fn prowlarr_handler(state: Prow) -> Handler {
    Arc::new(move |s: &Seen| {
        let key = s.headers.iter().find(|(n, _)| n == "x-api-key");
        if key.map(|(_, v)| v.as_str()) != Some(API_KEY) {
            return Reply::Full(401, Vec::new());
        }
        let mut st = state.lock().unwrap();
        let body: Option<Value> = serde_json::from_slice(&s.body).ok();
        let out = |status, v: &Value| Reply::Full(status, v.to_string().into_bytes());
        match (s.method.as_str(), s.path.as_str()) {
            ("GET", "/api/v1/tag") => out(200, &Value::Array(st.tags.clone())),
            ("GET", "/api/v1/indexerproxy") => out(200, &Value::Array(st.proxies.clone())),
            ("POST", "/api/v1/tag") => {
                let label = body.unwrap()["label"]
                    .as_str()
                    .unwrap()
                    .to_ascii_lowercase();
                st.next_id += 1;
                let t = json!({"label": label, "id": st.next_id});
                st.tags.push(t.clone());
                out(201, &t)
            }
            ("POST", "/api/v1/indexerproxy") => {
                let mut p = body.unwrap();
                assert!(p.get("id").is_none());
                st.next_id += 1;
                p["id"] = json!(st.next_id);
                st.proxies.push(p.clone());
                out(201, &p)
            }
            (m, p) if p.starts_with("/api/v1/indexerproxy/") => {
                let id: u64 = p.rsplit('/').next().unwrap().parse().unwrap();
                let at = st.proxies.iter().position(|x| x["id"] == id);
                match (m, at) {
                    ("PUT", Some(i)) => {
                        let v = body.unwrap();
                        assert_eq!(v["id"], id);
                        st.proxies[i] = v.clone();
                        out(202, &v)
                    }
                    ("DELETE", Some(i)) => {
                        st.proxies.remove(i);
                        Reply::Full(200, Vec::new())
                    }
                    _ => Reply::Full(404, Vec::new()),
                }
            }
            _ => Reply::Full(404, Vec::new()),
        }
    })
}

fn solver() -> SolverAddress {
    SolverAddress::Loopback(PortNumber::new(8191).unwrap())
}

fn on() -> Switch {
    Switch::On(solver())
}

fn names(keys: &[lodestar_companions::adapter::SettingKey]) -> Vec<&'static str> {
    keys.iter().map(|k| k.as_str()).collect()
}

/// Plans and applies the pairing; returns the plan's keys.
async fn pair(
    server: &Server,
    switch: Switch,
    input: &ConfigureInput<'_>,
) -> Result<Vec<&'static str>, AdapterError> {
    let http = http();
    let target = server.loopback();
    let adapter = pairing(&fs_id(), switch).unwrap();
    let plan = adapter.plan(&http, &target, input).await?;
    let keys = names(plan.changes());
    adapter.apply(&http, &target, input, &plan).await?;
    Ok(keys)
}

fn installed(server: &Server) -> ConfigureInput<'static> {
    ConfigureInput::new(&Creds).installed(InstallReceipt::for_tests(server.loopback()))
}

fn ours(state: &Prow) -> Vec<Value> {
    state
        .lock()
        .unwrap()
        .proxies
        .iter()
        .filter(|p| p["name"] == PROXY_NAME)
        .cloned()
        .collect()
}

// ─── Off by default ─────────────────────────────────────────────────────

#[test]
fn off_by_default_in_the_catalogue_and_detection() {
    let c = builtin().unwrap();
    let e = c.get(flaresolverr::ID).unwrap();
    assert!(e.opt_in);
    assert_eq!(e.needed_for, vec![Need::ChallengeSolver]);
    // A default install set is the entries not opt-in: never FlareSolverr.
    let defaults: Vec<&str> = c
        .entries()
        .iter()
        .filter(|e| !e.opt_in)
        .map(|e| e.id.as_str())
        .collect();
    assert_eq!(defaults, ["qbittorrent", "prowlarr", "gluetun"]);
    // Nothing needed by a default role names it.
    for need in [Need::DownloadClient, Need::IndexerManager, Need::VpnRoute] {
        assert!(!e.needs(need));
    }
    // The default sweep never probes it.
    for rt in [Runtime::Native, Runtime::Container] {
        assert!(
            detect::auto_probes(c, rt)
                .iter()
                .all(|(id, _)| id.as_str() != flaresolverr::ID)
        );
    }
    assert_eq!(Switch::default(), Switch::Off);
}

#[tokio::test]
async fn switched_off_prowlarr_gets_no_proxy() {
    let state = yours();
    let server = Server::start(prowlarr_handler(Arc::clone(&state))).await;
    let before = state.lock().unwrap().proxies.clone();
    assert_eq!(
        pair(&server, Switch::default(), &installed(&server))
            .await
            .unwrap(),
        Vec::<&str>::new()
    );
    assert!(server.writes().is_empty());
    assert_eq!(state.lock().unwrap().proxies, before);
}

#[tokio::test]
async fn nothing_is_sent_without_consent() {
    let state = yours();
    let server = Server::start(prowlarr_handler(state)).await;
    let http = http();
    let target = server.loopback();
    let adapter = pairing(&fs_id(), on()).unwrap();
    let other = Target::Loopback(PortNumber::new(9697).unwrap());
    for input in [
        ConfigureInput::new(&Creds),
        ConfigureInput::new(&Creds).confirmed(Confirmation::of(other.clone())),
        ConfigureInput::new(&Creds).installed(InstallReceipt::for_tests(other)),
    ] {
        assert_eq!(
            adapter.plan(&http, &target, &input).await.unwrap_err(),
            AdapterError::Refused(NOT_CONFIRMED)
        );
    }
    assert!(server.seen().is_empty());
}

// ─── Switched on, then off ──────────────────────────────────────────────

#[tokio::test]
async fn switched_on_the_proxy_is_written_once_then_removed() {
    let log = capture();
    let state = yours();
    let server = Server::start(prowlarr_handler(Arc::clone(&state))).await;
    let input = installed(&server);
    let theirs: Vec<Value> = state.lock().unwrap().proxies.clone();

    let keys = pair(&server, on(), &input).await.unwrap();
    assert_eq!(keys, [TAG.as_str(), ADD.as_str()]);
    assert_eq!(
        server.writes(),
        [
            ("POST".to_owned(), "/api/v1/tag".to_owned()),
            ("POST".to_owned(), "/api/v1/indexerproxy".to_owned()),
        ]
    );
    let mine = ours(&state);
    assert_eq!(mine.len(), 1);
    let tag = state
        .lock()
        .unwrap()
        .tags
        .iter()
        .find(|t| t["label"] == TAG_LABEL)
        .map(|t| t["id"].clone())
        .unwrap();
    assert_eq!(mine[0]["implementation"], "FlareSolverr");
    assert_eq!(mine[0]["configContract"], "FlareSolverrSettings");
    assert_eq!(mine[0]["tags"], json!([tag]));
    assert_eq!(
        mine[0]["fields"][0],
        json!({"name": "host", "value": "http://127.0.0.1:8191/"})
    );
    assert_eq!(Pairing::notes(&on()), &[EGRESS_DIRECT]);

    // Again: an empty plan, and nothing written.
    let n = server.writes().len();
    assert!(pair(&server, on(), &input).await.unwrap().is_empty());
    assert_eq!(server.writes().len(), n);
    assert_eq!(ours(&state).len(), 1);

    // FlareSolverr moved: the same proxy is updated, not a second added.
    let moved = SolverAddress::from_target(&Target::Container(
        ContainerName::parse("flaresolverr").unwrap(),
        PortNumber::new(8191).unwrap(),
    ))
    .unwrap();
    assert_eq!(
        pair(&server, Switch::On(moved.clone()), &input)
            .await
            .unwrap(),
        [UPDATE.as_str()]
    );
    let mine = ours(&state);
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0]["fields"][0]["value"], "http://flaresolverr:8191/");
    assert!(
        pair(&server, Switch::On(moved), &input)
            .await
            .unwrap()
            .is_empty()
    );

    // Off: exactly one DELETE, of Lodestar's own proxy. Yours are as they
    // were; the tag stays.
    let our_id = ours(&state)[0]["id"].as_u64().unwrap();
    let n = server.writes().len();
    assert_eq!(
        pair(&server, Switch::Off, &input).await.unwrap(),
        [REMOVE.as_str()]
    );
    assert_eq!(
        server.writes()[n..],
        [(
            "DELETE".to_owned(),
            format!("/api/v1/indexerproxy/{our_id}")
        )]
    );
    assert!(ours(&state).is_empty());
    assert_eq!(state.lock().unwrap().proxies, theirs);
    assert!(
        state
            .lock()
            .unwrap()
            .tags
            .iter()
            .any(|t| t["label"] == TAG_LABEL)
    );
    let n = server.writes().len();
    assert!(pair(&server, Switch::Off, &input).await.unwrap().is_empty());
    assert_eq!(server.writes().len(), n);

    // No write but Lodestar's own proxy's, and the key only in its header.
    for s in server.seen() {
        assert!(!s.path.contains(API_KEY));
        if s.method == "PUT" || s.method == "DELETE" {
            let id: u64 = s.path.rsplit('/').next().unwrap().parse().unwrap();
            assert!(id > 2, "touched your proxy {id}");
        }
    }
    let lines = log.0.lock().unwrap();
    assert!(lines.iter().all(|l| !l.contains(API_KEY)));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("companions.config_applied") && l.contains("indexer_proxy.add"))
    );
}

#[tokio::test]
async fn a_proxy_you_made_is_never_touched() {
    // Yours with Lodestar's name but not its tag, and yours with its tag
    // but another name.
    for (name, tags) in [(PROXY_NAME, json!([1])), ("My solver", json!([11]))] {
        let state = prowlarr_state(
            json!([{
                "id": 5, "name": name, "implementation": "FlareSolverr",
                "configContract": "FlareSolverrSettings",
                "fields": [{"name": "host", "value": "http://localhost:8191/"},
                           {"name": "requestTimeout", "value": 60}],
                "tags": tags,
            }]),
            json!([{"label": "flaresolverr", "id": 1}, {"label": TAG_LABEL, "id": 11}]),
        );
        let server = Server::start(prowlarr_handler(Arc::clone(&state))).await;
        let input = installed(&server);
        let before = state.lock().unwrap().proxies.clone();
        // Off never removes it.
        assert!(pair(&server, Switch::Off, &input).await.unwrap().is_empty());
        if name == PROXY_NAME {
            // On refuses rather than adopt or rename it.
            assert_eq!(
                pair(&server, on(), &input).await.unwrap_err(),
                AdapterError::Refused(PROXY_NAME_TAKEN)
            );
            assert!(server.writes().is_empty());
        } else {
            // On adds Lodestar's own beside it.
            assert_eq!(pair(&server, on(), &input).await.unwrap(), [ADD.as_str()]);
            assert_eq!(
                server.writes(),
                [("POST".into(), "/api/v1/indexerproxy".into())]
            );
            assert!(pair(&server, Switch::Off, &input).await.is_ok());
        }
        assert_eq!(state.lock().unwrap().proxies, before);
    }
}

#[tokio::test]
async fn hostile_prowlarr_answers_are_refused() {
    for body in [
        b"not json".to_vec(),
        b"[{\"id\":\"1\"}]".to_vec(),
        vec![b'['; 100],
        format!("[{}]", vec!["{\"id\":1}"; 20_000].join(",")).into_bytes(),
    ] {
        let b = body.clone();
        let server = Server::start(Arc::new(move |s: &Seen| {
            if s.path == "/api/v1/tag" {
                Reply::Full(200, b"[]".to_vec())
            } else {
                Reply::Full(200, b.clone())
            }
        }))
        .await;
        let err = pair(&server, on(), &installed(&server)).await.unwrap_err();
        assert!(
            matches!(err, AdapterError::Protocol(_) | AdapterError::TooLarge),
            "{err:?}"
        );
        assert!(server.writes().is_empty());
    }
    // A refused key.
    let server = Server::start(Arc::new(|_: &Seen| Reply::Full(401, Vec::new()))).await;
    assert_eq!(
        pair(&server, on(), &installed(&server)).await.unwrap_err(),
        AdapterError::Unauthorized
    );
}

#[test]
fn flaresolverr_is_never_paired_at_an_entered_url() {
    let e = Target::Entered(EnteredUrl::parse("http://192.168.1.20:8191").unwrap());
    assert_eq!(
        SolverAddress::from_target(&e),
        Err(AdapterError::Refused(SOLVER_NOT_LOCAL))
    );
}

// ─── Detection and health ───────────────────────────────────────────────

/// Serves a recorded case: each path its recording.
async fn replay(case: &str) -> Server {
    let dir = fixtures().join(case);
    let mut by_path: HashMap<String, (u16, Vec<u8>)> = HashMap::new();
    for n in 1.. {
        let Ok(head) = std::fs::read_to_string(dir.join(format!("{n:03}.http"))) else {
            break;
        };
        let mut lines = head.lines();
        let path = lines
            .next()
            .unwrap()
            .strip_prefix("GET ")
            .unwrap()
            .to_owned();
        assert_eq!(lines.next(), Some(""));
        let status = lines
            .next()
            .unwrap()
            .strip_prefix("HTTP ")
            .unwrap()
            .parse()
            .unwrap();
        let body = std::fs::read(dir.join(format!("{n:03}.body"))).unwrap_or_default();
        by_path.insert(path, (status, body));
    }
    assert!(!by_path.is_empty(), "no fixture {case}");
    Server::start(Arc::new(move |s: &Seen| match by_path.get(&s.path) {
        Some((st, b)) => Reply::Full(*st, b.clone()),
        None => Reply::Full(599, Vec::new()),
    }))
    .await
}

fn adapters() -> (Box<dyn Detect>, Box<dyn Health>) {
    let id = fs_id();
    (detect::detector(&id).unwrap(), detect::health(&id).unwrap())
}

#[tokio::test]
async fn recorded_answers() {
    let (d, h) = adapters();
    let http = http();
    for (case, found, version, health) in [
        ("found", true, Some("3.3.21"), HealthStatus::Healthy),
        ("found_v2", true, Some("2.2.10"), HealthStatus::Healthy),
        ("unhealthy", true, Some("3.3.21"), HealthStatus::Unhealthy),
        ("wrong_service", false, None, HealthStatus::Unhealthy),
    ] {
        let server = replay(case).await;
        for target in [server.loopback(), server.entered()] {
            let got = d.detect(&http, &target).await.unwrap();
            assert_eq!(got.is_some(), found, "{case}");
            if let Some(g) = got {
                assert_eq!(g.target, target);
                assert_eq!(g.version.as_ref().map(|v| v.as_str()), version, "{case}");
            }
            assert_eq!(h.check(&http, &target).await, health, "{case}");
        }
        assert!(server.seen().iter().all(|s| {
            s.method == "GET"
                && (s.path == "/" || s.path == "/health")
                && s.headers
                    .iter()
                    .all(|(n, _)| n != "x-api-key" && n != "authorization")
        }));
    }
}

/// Records every request; nothing answers.
#[derive(Default)]
struct Recorder(Mutex<Vec<(Target, String)>>);

impl GuardedHttp for Recorder {
    fn send<'a>(
        &'a self,
        request: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        self.0
            .lock()
            .unwrap()
            .push((request.target.clone(), request.path.as_str().to_owned()));
        Box::pin(async { Err(AdapterError::Unreachable) })
    }
}

#[tokio::test]
async fn find_looks_only_at_the_fixed_targets() {
    let c = builtin().unwrap();
    let lo = Target::Loopback(PortNumber::new(8191).unwrap());
    let ct = Target::Container(
        ContainerName::parse("flaresolverr").unwrap(),
        PortNumber::new(8191).unwrap(),
    );
    for (rt, want) in [
        (Runtime::Native, vec![lo.clone()]),
        (Runtime::Container, vec![lo, ct]),
    ] {
        let rec = Recorder::default();
        assert!(flaresolverr::find(&rec, c, rt).await.is_none());
        let sent = rec.0.lock().unwrap().clone();
        assert_eq!(
            sent.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
            want
        );
        assert!(sent.iter().all(|(_, p)| p == "/"));
    }
}

#[tokio::test]
async fn timeouts_and_oversized_or_malformed_answers() {
    let (d, h) = adapters();
    let http = http();
    // Says nothing: one deadline (5 s), then unreachable.
    let silent = Server::start(Arc::new(|_: &Seen| Reply::Silent)).await;
    assert_eq!(
        d.detect(&http, &silent.entered()).await,
        Err(AdapterError::Timeout)
    );
    assert_eq!(d.detect(&http, &silent.loopback()).await, Ok(None));
    assert_eq!(
        h.check(&http, &silent.loopback()).await,
        HealthStatus::Unreachable
    );

    // Past the 64 KiB cap.
    let big = Server::start(Arc::new(|_: &Seen| {
        let mut b = br#"{"msg":"FlareSolverr is ready!","x":""#.to_vec();
        b.extend(std::iter::repeat_n(b'a', 70 * 1024));
        b.extend_from_slice(b"\"}");
        Reply::Full(200, b)
    }))
    .await;
    assert_eq!(
        d.detect(&http, &big.loopback()).await,
        Err(AdapterError::TooLarge)
    );
    assert_eq!(
        h.check(&http, &big.loopback()).await,
        HealthStatus::Unhealthy
    );

    for body in [
        &b"{\"msg\":\"FlareSolverr is ready!\""[..],
        b"{\"msg\":\"FlareSolverr is ready!\",}",
        b"\xff\xfe",
        b"{\"status\":\"ok\"",
    ] {
        let b = body.to_vec();
        let s = Server::start(Arc::new(move |_: &Seen| Reply::Full(200, b.clone()))).await;
        assert_eq!(d.detect(&http, &s.loopback()).await, Ok(None));
        assert_eq!(h.check(&http, &s.loopback()).await, HealthStatus::Unhealthy);
    }
}

proptest! {
    /// Random bytes: no panic, and no answer but the marker detects.
    #[test]
    fn random_answers_never_panic(body in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let found = flaresolverr::parse_index(&body).is_some();
        prop_assert!(!found || std::str::from_utf8(&body).is_ok_and(|s| s.contains("FlareSolverr is ready!")));
        let _ = flaresolverr::parse_health(&body);
    }
}

// ─── The catalogue entry and its port ───────────────────────────────────

#[test]
fn the_catalogue_entry_loads_and_waits_for_its_pin() {
    let c = builtin().unwrap();
    let e = c.get(flaresolverr::ID).unwrap();
    assert_eq!(e.name, "FlareSolverr");
    assert_eq!(e.licence.spdx, "MIT");
    let api = e.port("api").unwrap();
    assert_eq!(api.number.get(), 8191);
    assert_eq!(api.exposure, Exposure::Internal);
    assert_eq!(e.ports.len(), 1);
    assert_eq!(e.health.as_ref().unwrap().path.as_str(), "/health");
    assert_eq!(
        e.container.as_ref().unwrap().name().as_str(),
        "flaresolverr"
    );
    // TODO(CMP-13): pinned by its tool. Until then nothing installs it.
    assert!(e.awaiting_pin());
    assert!(matches!(
        e.installable(Platform::Linux, Arch::X86_64),
        Err(NotAvailable::AwaitingPin)
    ));
    assert!(matches!(e.renderable(), Err(NotAvailable::AwaitingPin)));
    // The loader refuses it as anything but opt-in.
    let raw = include_str!("../data/catalogue.json").replace(
        r#""needed_for": ["challenge_solver"],
      "opt_in": true"#,
        r#""needed_for": ["challenge_solver"],
      "opt_in": false"#,
    );
    assert_ne!(raw, include_str!("../data/catalogue.json"));
    assert!(Catalogue::from_json(&raw).is_err());
}

/// The built-in catalogue with FlareSolverr's image given a digest.
fn pinned() -> Catalogue {
    let mut v: Value = serde_json::from_str(include_str!("../data/catalogue.json")).unwrap();
    for e in v["entries"].as_array_mut().unwrap() {
        if e["container"]["digest"].is_null() {
            e["container"]["digest"] = json!(format!("sha256:{}", "ab".repeat(32)));
        }
    }
    Catalogue::from_json(&v.to_string()).unwrap()
}

fn service(publish: Vec<Publish>) -> ServiceInput {
    ServiceInput {
        puid: Puid::new(99).unwrap(),
        pgid: Pgid::new(100).unwrap(),
        network: NetworkName::parse("lodestar").unwrap(),
        route: Route::Direct,
        paths: Paths::default(),
        publish,
        data: DataFolders::none(),
        taken: Vec::new(),
    }
}

#[test]
fn its_port_stays_on_loopback_or_the_companion_network() {
    let c = pinned();
    let id = fs_id();
    let publish = |bind| Publish {
        port: lodestar_companions::catalogue::PortName::parse("api").unwrap(),
        bind,
        host_port: PortNumber::new(8191).unwrap(),
    };
    // By default nothing is published: it's on the companion network only.
    let doc = compose(&c, &id, &service(Vec::new())).unwrap();
    assert!(!doc.contains("\"ports\":"), "{doc}");
    assert!(!doc.contains("8191"), "{doc}");
    // On loopback, if you ask.
    let doc = compose(&c, &id, &service(vec![publish(Bind::Loopback)])).unwrap();
    assert!(doc.contains("\"host_ip\": \"127.0.0.1\""), "{doc}");
    // Never on your network, in either template.
    assert_eq!(
        compose(&c, &id, &service(vec![publish(Bind::Lan)])).unwrap_err(),
        TemplateError::LanRefused
    );
    assert_eq!(
        unraid(&c, &id, &service(vec![publish(Bind::Lan)])).unwrap_err(),
        TemplateError::LanRefused
    );
    let tpl = unraid(&c, &id, &service(vec![publish(Bind::Loopback)])).unwrap();
    assert!(
        tpl.iter()
            .any(|t| t.xml.contains("--publish=127.0.0.1:8191:8191/tcp"))
    );
    // A native launch is told to listen on loopback.
    assert_eq!(flaresolverr::NATIVE_BIND, ("HOST", "127.0.0.1"));
}
