//! CMP-5g-2's guided endpoints over the real store, secrets file,
//! detection and configuration: a fake qBittorrent on loopback answers
//! through the real `CompanionHttp`, and a recording sink takes the
//! hand-over. The egress refusals for an address you type, Prowlarr's
//! `handover_later`, the installed refusal, the rows each step writes,
//! the plan's rate limit, a mismatch restoring the temporary password,
//! forget, and detection running its probes at once under its cap.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_api::companions::CompanionsError;
use lodestar_api::companions_guided::{CompanionsGuidedApi, GuidedError};
use lodestar_api::dto::companions_guided::CompanionTarget;
use lodestar_companions::adapter::{
    AdapterError, BoxFuture, GuardedHttp, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::configure::{
    ConfigSink, DownloadClientEntry, IndexerManagerEntry, SinkError,
};
use lodestar_companions::credentials::{CredentialKey, Field};
use lodestar_companions::detect::{CompanionHttp, Runtime};
use lodestar_companions::guided::{self, StoredTarget};
use lodestar_companions::manager::Rows as _;
use lodestar_companions::{CompanionId, Version};
use lodestar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch as channel;

use super::{Added, Guided, PLAN_EVERY, checked_target, texts};
use crate::companions::StoreRows;

const TEMPORARY: &str = "Tq7tempPassDvX3kR";
/// The two markers detection looks for on qBittorrent's login page.
const LOGIN_PAGE: &str = r#"<form id="loginform"><img src="images/qbittorrent-tray.svg"></form>"#;
const PREFS: &str = r#"{"save_path":"/downloads/","web_ui_address":"*","current_network_interface":"","current_interface_address":"","bypass_local_auth":false}"#;

fn qbt() -> CompanionId {
    CompanionId::parse("qbittorrent").unwrap()
}

fn key() -> CredentialKey {
    CredentialKey::new(qbt(), Field::WebUiPassword)
}

// ─── A fake qBittorrent on loopback ─────────────────────────────────────

#[derive(Debug)]
struct QState {
    password: String,
    sessions: Vec<String>,
    logins: usize,
    /// Sign-ins refused: qBittorrent bans an address after five.
    failed: usize,
    /// Every sign-in is refused, as after a ban.
    banned: bool,
    /// Every settings write is refused.
    refuse_writes: bool,
}

type Qbit = Arc<Mutex<QState>>;

/// `application/x-www-form-urlencoded`, decoded.
fn form(body: &[u8]) -> HashMap<String, String> {
    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'+' => out.push(b' '),
                b'%' if i + 2 < b.len() => {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                    i += 2;
                }
                c => out.push(c),
            }
            i += 1;
        }
        String::from_utf8(out).unwrap()
    }
    std::str::from_utf8(body)
        .unwrap()
        .split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

/// `(status, set-cookie, body)` for a request, as qBittorrent 5.0 answers.
fn answer(
    s: &mut QState,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: &[u8],
) -> (u16, Option<String>, String) {
    if path == "/api/v2/app/version" {
        return (403, None, "Forbidden".into());
    }
    if method == "GET" && path == "/" {
        return (200, None, LOGIN_PAGE.into());
    }
    if method == "POST" && path == "/api/v2/auth/login" {
        s.logins += 1;
        let f = form(body);
        let ok = !s.banned
            && f.get("username").map(String::as_str) == Some("admin")
            && f.get("password") == Some(&s.password);
        if !ok {
            s.failed += 1;
            return (200, None, "Fails.".into());
        }
        let sid = format!("sid{}", s.sessions.len());
        s.sessions.push(sid.clone());
        return (
            200,
            Some(format!("SID={sid}; HttpOnly; path=/")),
            "Ok.".into(),
        );
    }
    let authed = cookie
        .and_then(|c| c.strip_prefix("SID="))
        .is_some_and(|sid| s.sessions.iter().any(|x| x == sid));
    if !authed {
        return (403, None, "Forbidden".into());
    }
    match (method, path) {
        ("GET", "/api/v2/app/preferences") => (200, None, PREFS.into()),
        ("GET", "/api/v2/torrents/categories") => (200, None, "{}".into()),
        ("POST", "/api/v2/app/setPreferences") if s.refuse_writes => (400, None, String::new()),
        ("POST", "/api/v2/app/setPreferences") => {
            let f = form(body);
            let m: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&f["json"]).unwrap();
            if let Some(p) = m.get("web_ui_password").and_then(|v| v.as_str()) {
                s.password = p.to_owned();
            }
            (200, None, String::new())
        }
        _ => (404, None, "Not Found".into()),
    }
}

async fn serve(mut sock: TcpStream, state: Qbit) {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let mut body = head.split_off(end);
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, path) = (
        first.next().unwrap_or("").to_owned(),
        first.next().unwrap_or("").to_owned(),
    );
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let header = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let length: usize = header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while body.len() < length {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
        }
    }
    let (status, cookie, out) = {
        let mut s = state.lock().unwrap();
        answer(&mut s, &method, &path, header("cookie").as_deref(), &body)
    };
    let mut reply = format!("HTTP/1.1 {status} X\r\n");
    if let Some(c) = cookie {
        reply.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    reply.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{out}",
        out.len()
    ));
    let _ = sock.write_all(reply.as_bytes()).await;
    let _ = sock.shutdown().await;
}

async fn qbittorrent(password: &str) -> (SocketAddr, Qbit) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(Mutex::new(QState {
        password: password.into(),
        sessions: Vec::new(),
        logins: 0,
        failed: 0,
        banned: false,
        refuse_writes: false,
    }));
    let s = state.clone();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(serve(sock, s.clone()));
        }
    });
    (addr, state)
}

// ─── The binary's half over a temporary data folder ─────────────────────

/// What the hand-over was given; `reject` refuses it, as a settings
/// save that fails; `park` holds the save in flight until told.
#[derive(Default)]
struct Recorder(
    Mutex<Vec<(String, StoredTarget, String, String)>>,
    std::sync::atomic::AtomicBool,
    Mutex<Option<Arc<(tokio::sync::Notify, tokio::sync::Notify)>>>,
);

impl ConfigSink for Recorder {
    fn download_client<'a>(
        &'a self,
        entry: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        if self.1.load(Ordering::SeqCst) {
            return Box::pin(async { Err(SinkError::Rejected) });
        }
        self.0.lock().unwrap().push((
            entry.companion.as_str().to_owned(),
            StoredTarget::of(&entry.target),
            entry.username.as_str().to_owned(),
            entry.password.expose().to_owned(),
        ));
        let park = self.2.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(p) = park {
                p.0.notify_one();
                p.1.notified().await;
            }
            Ok(())
        })
    }
    fn indexer_manager<'a>(
        &'a self,
        _: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async { Err(SinkError::Rejected) })
    }
}

struct Fixture {
    guided: Arc<Guided>,
    service: Arc<crate::companions::Service>,
    store: Arc<Mutex<Store>>,
    sink: Arc<Recorder>,
    added: channel::Receiver<Vec<Added>>,
    dir: tempfile::TempDir,
}

fn fixture_with(http: Arc<dyn GuardedHttp>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(
        Store::open(dir.path().join("db"), Default::default()).unwrap(),
    ));
    let service = crate::companions::test_service(dir.path(), store.clone(), false);
    let sink = Arc::new(Recorder::default());
    let (tx, added) = channel::channel(Vec::new());
    let guided = Arc::new(Guided::new(
        service.clone(),
        store.clone(),
        Runtime::Native,
        http,
        sink.clone(),
        tx,
    ));
    Fixture {
        guided,
        service,
        store,
        sink,
        added,
        dir,
    }
}

/// [`fixture`], with a native install that pauses once it holds the lock.
fn fixture_parked(park: Arc<(tokio::sync::Notify, tokio::sync::Notify)>) -> Fixture {
    let mut f = fixture();
    let parked = crate::companions::parked_service(&f.service, park);
    let (tx, added) = channel::channel(Vec::new());
    f.guided = Arc::new(Guided::new(
        parked.clone(),
        f.store.clone(),
        Runtime::Native,
        Arc::new(CompanionHttp::new().unwrap()),
        f.sink.clone(),
        tx,
    ));
    f.service = parked;
    f.added = added;
    f
}

fn fixture() -> Fixture {
    fixture_with(Arc::new(CompanionHttp::new().unwrap()))
}

impl Fixture {
    fn row(&self, key: &str) -> Option<String> {
        texts(&self.store)
            .unwrap()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    fn provenance(&self) -> Option<String> {
        self.row(&guided::provenance_key(&qbt(), Field::WebUiPassword).unwrap())
    }

    /// The stored credential, opened.
    fn credential(&self) -> Option<String> {
        let sealed = StoreRows(self.store.clone())
            .get(&key().row_key())
            .unwrap()?;
        let secrets = self.guided.secrets.clone();
        Some(
            lodestar_companions::credentials::open(&secrets, &key(), &sealed)
                .unwrap()
                .expose()
                .to_owned(),
        )
    }

    /// Lets the next plan through, as a minute passing would.
    fn a_minute_passes(&self) {
        *self.guided.last_plan.lock().unwrap() = None;
    }
}

fn url(addr: SocketAddr) -> CompanionTarget {
    CompanionTarget::Url {
        url: format!("http://{addr}"),
    }
}

fn refused(e: GuidedError) -> &'static str {
    match e {
        GuidedError::Companions(CompanionsError::Refused(code)) => code,
        GuidedError::Companions(CompanionsError::BadInput { code, .. }) => code,
        GuidedError::Companions(CompanionsError::Mismatch) => "confirmation_mismatch",
        GuidedError::Companions(CompanionsError::Unknown) => "unknown_companion",
        other => panic!("{other:?}"),
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────

/// An address you type follows the egress rules: `http`/`https` only, no
/// user name or password, query or fragment, and no link-local or cloud
/// metadata address. Loopback and the private LAN are allowed. Catalogue
/// targets take only the companion's own ports, and a container name only
/// inside a container.
#[test]
fn typed_addresses_follow_the_egress_rules() {
    let cat = lodestar_companions::builtin().unwrap();
    let e = cat.get("qbittorrent").unwrap();
    let check = |t: CompanionTarget| checked_target(e, Runtime::Native, t);
    let typed = |u: &str| check(CompanionTarget::Url { url: u.into() });
    for (u, code) in [
        ("http://169.254.169.254/", "address_refused"),
        ("http://169.254.10.20:8080/", "address_refused"),
        ("http://[fe80::1]:8080/", "address_refused"),
        ("http://[fd00:ec2::254]/", "address_refused"),
        ("http://0.0.0.0:8080/", "address_refused"),
        ("http://user:pw@127.0.0.1:8080/", "bad_template_input"),
        ("http://user@127.0.0.1:8080/", "bad_template_input"),
        ("http://127.0.0.1:8080/#frag", "bad_template_input"),
        ("http://127.0.0.1:8080/?q=1", "bad_template_input"),
        ("ftp://127.0.0.1/", "bad_template_input"),
        ("file:///etc/passwd", "bad_template_input"),
        ("127.0.0.1:8080", "bad_template_input"),
        ("", "bad_template_input"),
    ] {
        let got = typed(u).map_err(|e| refused(e.into()));
        assert_eq!(got, Err(code), "{u}");
    }
    for u in [
        "http://127.0.0.1:8080",
        "https://192.168.1.20:8080/",
        "http://10.0.0.5:8080",
        "http://[::1]:8080",
        "http://qbittorrent.lan:8080",
    ] {
        assert!(matches!(typed(u), Ok(Target::Entered(_))), "{u}");
    }
    assert!(matches!(
        check(CompanionTarget::Loopback { port: 8080 }),
        Ok(Target::Loopback(_))
    ));
    for t in [
        CompanionTarget::Loopback { port: 22 },
        CompanionTarget::Loopback { port: 9696 },
        // Not in a container, so no container name.
        CompanionTarget::Container { port: 8080 },
    ] {
        assert_eq!(
            check(t.clone()).map_err(|e| refused(e.into())),
            Err("bad_template_input"),
            "{t:?}"
        );
    }
}

/// A refused address is refused before anything is sent: no row, nothing
/// contacted.
#[tokio::test]
async fn a_refused_address_writes_nothing() {
    let f = fixture();
    for u in ["http://169.254.169.254/", "http://[fe80::1]/"] {
        let e = f
            .guided
            .set_target("qbittorrent".into(), CompanionTarget::Url { url: u.into() })
            .await
            .unwrap_err();
        assert_eq!(refused(e), "address_refused", "{u}");
    }
    assert_eq!(f.row(&guided::target_key(&qbt())), None);
}

/// Prowlarr's hand-over waits for `X-prowlarr-settings`, and Gluetun has
/// none; an unknown id is unknown.
#[tokio::test]
async fn only_qbittorrent_is_handed_over_in_this_version() {
    let f = fixture();
    for id in ["prowlarr", "gluetun"] {
        let g = &f.guided;
        let t = CompanionTarget::Loopback { port: 9696 };
        assert_eq!(
            refused(g.set_target(id.into(), t).await.unwrap_err()),
            "handover_later"
        );
        assert_eq!(
            refused(g.paste(id.into(), "x".into()).await.unwrap_err()),
            "handover_later"
        );
        assert_eq!(
            refused(g.plan(id.into()).await.unwrap_err()),
            "handover_later"
        );
        assert_eq!(
            refused(g.confirm(id.into(), vec![]).await.unwrap_err()),
            "handover_later"
        );
    }
    for id in ["nope", "Q", "../x"] {
        assert_eq!(
            refused(f.guided.plan(id.into()).await.unwrap_err()),
            "unknown_companion"
        );
    }
    assert!(texts(&f.store).unwrap().is_empty());
}

/// A companion Lodestar installed is managed on its own: none of the
/// guided steps, nor forget, touch it.
#[tokio::test]
async fn an_installed_companion_is_refused() {
    let f = fixture();
    let home = f.dir.path().join("companions/qbittorrent");
    std::fs::create_dir_all(&home).unwrap();
    lodestar_companions::state::save(
        &home,
        &lodestar_companions::state::Record {
            installed: Some(Version::parse("5.1.2").unwrap()),
            op: None,
        },
    )
    .unwrap();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    let t = CompanionTarget::Loopback { port: 8080 };
    assert_eq!(
        refused(g.set_target(id(), t).await.unwrap_err()),
        "installed"
    );
    assert_eq!(
        refused(g.paste(id(), TEMPORARY.into()).await.unwrap_err()),
        "installed"
    );
    assert_eq!(refused(g.plan(id()).await.unwrap_err()), "installed");
    assert_eq!(
        refused(g.confirm(id(), vec![]).await.unwrap_err()),
        "installed"
    );
    assert_eq!(refused(g.forget(id()).await.unwrap_err()), "installed");
    assert!(texts(&f.store).unwrap().is_empty());
}

/// The whole flow against a fake qBittorrent: add it at its address, paste
/// the temporary password (marked pasted), see the plan (at most one a
/// minute), confirm it (generated password stored and marked generated,
/// qBittorrent signed in with it, the client handed over as `admin`, the
/// record sent to the watch), then forget it.
#[tokio::test]
async fn the_guided_flow_hands_qbittorrent_over() {
    let (addr, q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();

    // Each step needs the one before.
    assert_eq!(
        refused(g.paste(id(), TEMPORARY.into()).await.unwrap_err()),
        "no_target"
    );
    assert_eq!(refused(g.plan(id()).await.unwrap_err()), "no_target");

    let found = g.set_target(id(), url(addr)).await.unwrap();
    assert_eq!(found.target, url(addr));
    assert_eq!(found.version, None, "403 without a session: no version");
    assert_eq!(
        f.row(&guided::target_key(&qbt())).as_deref(),
        Some(format!(r#"{{"kind":"url","url":"http://{addr}"}}"#).as_str())
    );
    // Not confirmed yet: no record, nothing watched.
    assert_eq!(f.row(&guided::origin_key(&qbt())), None);

    assert_eq!(
        refused(g.plan(id()).await.unwrap_err()),
        "no_temporary_password"
    );
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    assert_eq!(f.provenance().as_deref(), Some("pasted"));
    assert_eq!(f.credential().as_deref(), Some(TEMPORARY));

    let plan = g.plan(id()).await.unwrap();
    assert_eq!(plan, ["webui.password"]);
    assert_eq!(
        q.lock().unwrap().password,
        TEMPORARY,
        "a plan changes nothing"
    );
    match g.plan(id()).await.unwrap_err() {
        GuidedError::RateLimited { retry_after } => {
            assert!((1..=PLAN_EVERY.as_secs()).contains(&retry_after));
        }
        other => panic!("{other:?}"),
    }

    assert_eq!(
        q.lock().unwrap().failed,
        0,
        "a plan signs in with the temporary password only"
    );
    let c = g.confirm(id(), plan).await.unwrap();
    assert_eq!(c.id, "qbittorrent");
    let generated = f.credential().unwrap();
    assert_ne!(generated, TEMPORARY);
    assert_eq!(q.lock().unwrap().password, generated, "qBittorrent took it");
    assert_eq!(
        q.lock().unwrap().failed,
        0,
        "the flow costs no refused sign-in with the right password"
    );
    assert_eq!(f.provenance().as_deref(), Some("generated"));
    assert_eq!(
        f.row(&guided::origin_key(&qbt())).as_deref(),
        Some("confirmed")
    );
    let handed = f.sink.0.lock().unwrap().clone();
    assert_eq!(
        handed,
        [(
            "qbittorrent".to_owned(),
            StoredTarget::Url {
                url: format!("http://{addr}")
            },
            "admin".to_owned(),
            generated.clone()
        )]
    );
    let watched = f.added.borrow().clone();
    assert_eq!(watched.len(), 1);
    assert_eq!(watched[0].0, qbt());
    assert_eq!(
        StoredTarget::of(&watched[0].1),
        StoredTarget::Url {
            url: format!("http://{addr}")
        }
    );
    // Added: neither its target nor its credential changes in place, so
    // the generated password stays the one you can reveal, and nothing
    // plans or confirms again.
    f.a_minute_passes();
    for e in [
        g.set_target(id(), url(addr)).await.unwrap_err(),
        g.paste(id(), TEMPORARY.into()).await.unwrap_err(),
        g.plan(id()).await.unwrap_err(),
        g.confirm(id(), vec![]).await.unwrap_err(),
    ] {
        assert_eq!(refused(e), "already_added");
    }
    assert_eq!(f.credential(), Some(generated.clone()));
    assert_eq!(f.provenance().as_deref(), Some("generated"));
    let gone = g.forget(id()).await.unwrap();
    assert!(
        gone.credential_in_use,
        "the generated password was qBittorrent's"
    );
    for k in [
        "companions.qbittorrent.target",
        "companions.qbittorrent.origin",
        "companions.qbittorrent.provenance.webui_password",
        "companions.qbittorrent.secret.webui_password",
    ] {
        assert!(gone.deleted.iter().any(|g| g == k), "{k} in {gone:?}");
        assert_eq!(f.row(k), None, "{k}");
    }
    assert!(texts(&f.store).unwrap().is_empty());
    assert!(f.added.borrow().is_empty());
}

/// A confirmation that doesn't echo the plan is refused before qBittorrent
/// is signed in to, and uses the plan up; one whose re-plan fails puts the
/// temporary password back, marked pasted, so you can look again.
#[tokio::test]
async fn a_mismatch_or_failed_replan_keeps_the_temporary_password() {
    let (addr, q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();

    // No plan yet.
    assert_eq!(
        refused(
            g.confirm(id(), vec!["webui.password".into()])
                .await
                .unwrap_err()
        ),
        "confirmation_mismatch"
    );
    let plan = g.plan(id()).await.unwrap();
    let logins = q.lock().unwrap().logins;
    for wrong in [
        vec![],
        vec![
            "webui.password".to_owned(),
            "downloads.save_path".to_owned(),
        ],
        vec!["WEBUI.PASSWORD".to_owned()],
    ] {
        assert_eq!(
            refused(g.confirm(id(), wrong.clone()).await.unwrap_err()),
            "confirmation_mismatch",
            "{wrong:?}"
        );
    }
    // The plan was used up by the first wrong one.
    assert_eq!(
        refused(g.confirm(id(), plan.clone()).await.unwrap_err()),
        "confirmation_mismatch"
    );
    assert_eq!(q.lock().unwrap().logins, logins, "no sign-in spent");
    assert_eq!(f.provenance().as_deref(), Some("pasted"));
    assert_eq!(f.credential().as_deref(), Some(TEMPORARY));

    // A re-plan that can't sign in: nothing applied, the temporary password
    // restored.
    f.a_minute_passes();
    let plan = g.plan(id()).await.unwrap();
    q.lock().unwrap().banned = true;
    let e = g.confirm(id(), plan).await.unwrap_err();
    assert_eq!(refused(e), "companion_unauthorized");
    assert_eq!(f.provenance().as_deref(), Some("pasted"));
    assert_eq!(f.credential().as_deref(), Some(TEMPORARY));
    assert_eq!(q.lock().unwrap().password, TEMPORARY);
    assert_eq!(f.row(&guided::origin_key(&qbt())), None);
    assert!(f.sink.0.lock().unwrap().is_empty());
    assert!(f.added.borrow().is_empty());
}

/// Answers each first probe `403` once all `n` are in flight at once, and
/// the login page at once; counts requests.
struct Gate {
    barrier: tokio::sync::Barrier,
    seen: AtomicUsize,
    /// Never answers.
    silent: bool,
}

impl GuardedHttp for Gate {
    fn send<'a>(
        &'a self,
        req: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async move {
            self.seen.fetch_add(1, Ordering::SeqCst);
            if self.silent {
                std::future::pending::<()>().await;
            }
            if req.path.as_str() == "/" {
                return ProbeResponse::new(200, LOGIN_PAGE.as_bytes().to_vec(), &req.limits);
            }
            self.barrier.wait().await;
            ProbeResponse::new(403, b"Forbidden".to_vec(), &req.limits)
        })
    }
}

fn gate(silent: bool) -> Arc<Gate> {
    let cat = lodestar_companions::builtin().unwrap();
    let n = lodestar_companions::detect::auto_probes(cat, Runtime::Native).len();
    assert!(n > 1);
    Arc::new(Gate {
        barrier: tokio::sync::Barrier::new(n),
        seen: AtomicUsize::new(0),
        silent,
    })
}

/// Every probe is in flight at once: each answers only when all have been
/// sent, so probes run one after another would end at the cap with
/// nothing found. Each companion is listed once, at its first target.
#[tokio::test]
async fn detection_runs_its_probes_at_once() {
    let http = gate(false);
    let f = fixture_with(http.clone());
    let found = f.guided.detect().await.unwrap();
    let qbt: Vec<_> = found.iter().filter(|d| d.id == "qbittorrent").collect();
    assert_eq!(qbt.len(), 1, "{found:?}");
    assert_eq!(qbt[0].target, CompanionTarget::Loopback { port: 8080 });
    assert!(found.iter().all(|d| d.id != "prowlarr"), "{found:?}");
    // Nothing is written by detection.
    assert!(texts(&f.store).unwrap().is_empty());
}

/// Detection ends at its cap with what it has, and a second one while it
/// runs is `busy`.
#[tokio::test]
async fn detection_is_capped_and_one_at_a_time() {
    let http = gate(true);
    let mut f = fixture_with(http.clone());
    Arc::get_mut(&mut f.guided).unwrap().detect_cap = Duration::from_millis(200);
    let started = std::time::Instant::now();
    assert_eq!(f.guided.detect().await.unwrap(), []);
    assert!(started.elapsed() < Duration::from_secs(10));

    Arc::get_mut(&mut f.guided).unwrap().detect_cap = Duration::from_secs(600);
    let g = f.guided.clone();
    let first = tokio::spawn(async move { g.detect().await });
    let before = http.seen.load(Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while http.seen.load(Ordering::SeqCst) == before {
        assert!(
            std::time::Instant::now() < deadline,
            "the first detection started"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(refused(f.guided.detect().await.unwrap_err()), "busy");
    first.abort();
}

/// One guided change at a time: while one runs, every other is `busy`
/// and touches nothing.
#[tokio::test]
async fn guided_changes_run_one_at_a_time() {
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    let _held = g.changing.try_lock().unwrap();
    let t = CompanionTarget::Loopback { port: 8080 };
    for e in [
        g.set_target(id(), t).await.unwrap_err(),
        g.paste(id(), TEMPORARY.into()).await.unwrap_err(),
        g.plan(id()).await.unwrap_err(),
        g.confirm(id(), vec!["webui.password".into()])
            .await
            .unwrap_err(),
        g.forget(id()).await.unwrap_err(),
    ] {
        assert_eq!(refused(e), "busy");
    }
    assert!(texts(&f.store).unwrap().is_empty());
}

/// #679's review, Important 1: a native install and the guided steps take
/// one lock. While a guided step runs, an install is `busy` before it
/// checks or downloads anything; while an install runs (its checks,
/// hand-over and undo), every guided step is `busy` and writes nothing.
/// So neither writes the managed entry while the other is deciding.
#[tokio::test]
async fn a_native_install_and_the_guided_steps_exclude_each_other() {
    use lodestar_api::companions::CompanionsApi;
    let f = fixture();
    let g = &f.guided;
    let install = || {
        f.service
            .install("qbittorrent".into(), "5.1.2".into(), "ab".repeat(32))
    };
    let guided_step = g.changing.clone().try_lock_owned().unwrap();
    assert_eq!(install().await, Err(CompanionsError::Refused("busy")));
    // Only qBittorrent's install takes it: another companion's isn't held
    // up by a guided step (#679 follow-up's self-review I1).
    assert_eq!(
        f.service
            .install("prowlarr".into(), "1.0.0".into(), "ab".repeat(32))
            .await,
        Err(CompanionsError::Refused("awaiting_pin"))
    );
    drop(guided_step);
    // Free again: it goes on to the manager, which has no pin to install.
    assert_eq!(
        install().await,
        Err(CompanionsError::Refused("awaiting_pin"))
    );
    let installing = f.service.changes().try_lock_owned().unwrap();
    let id = || "qbittorrent".to_owned();
    let t = CompanionTarget::Loopback { port: 8080 };
    for e in [
        g.set_target(id(), t).await.unwrap_err(),
        g.paste(id(), TEMPORARY.into()).await.unwrap_err(),
        g.plan(id()).await.unwrap_err(),
        g.confirm(id(), vec!["webui.password".into()])
            .await
            .unwrap_err(),
        g.forget(id()).await.unwrap_err(),
    ] {
        assert_eq!(refused(e), "busy");
    }
    assert!(texts(&f.store).unwrap().is_empty());
    assert!(f.sink.0.lock().unwrap().is_empty());
    drop(installing);
}

/// #679 follow-up's self-review B1: the lock is held by the install's own
/// task, so a request dropped mid-install (a closed tab) doesn't let a
/// guided step in while the install, its hand-over or its undo still runs.
/// The install is parked, once it holds the lock, until the test lets it
/// go: a condition, not a sleep.
#[tokio::test]
async fn a_dropped_install_request_keeps_the_guided_steps_out() {
    use lodestar_api::companions::CompanionsApi;
    let park = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    let f = fixture_parked(park.clone());
    let g = &f.guided;
    let service = f.service.clone();
    let request = tokio::spawn(async move {
        service
            .install("qbittorrent".into(), "5.1.2".into(), "ab".repeat(32))
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), park.0.notified())
        .await
        .expect("the install never took the lock");
    // The request goes; the install it started doesn't.
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let t = CompanionTarget::Loopback { port: 8080 };
    let e = g.set_target("qbittorrent".into(), t).await.unwrap_err();
    assert_eq!(refused(e), "busy");
    // Once it ends, the lock is free again.
    park.1.notify_one();
    let free = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(held) = g.changing.clone().try_lock_owned() {
                return held;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(free.is_ok(), "the install never let the lock go");
}

/// #679 follow-up's self-review B2: a guided confirmation clears what a
/// crashed native install left pending, so the next start's release never
/// takes out the entry you added since.
#[tokio::test]
async fn a_guided_confirmation_clears_a_pending_release() {
    let (addr, _q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    let marker = f.dir.path().join("companions/.handed-over-qbittorrent");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::fs::write(&marker, b"").unwrap();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    let plan = g.plan(id()).await.unwrap();
    g.confirm(id(), plan).await.unwrap();
    assert!(!marker.exists(), "the marker would release your entry");
}

/// #692's review, Important 1: a confirmation whose request is dropped
/// while its hand-over is still saving keeps the lock until that save and
/// its record land, so a native install can't pass its check in between.
#[tokio::test]
async fn a_dropped_confirmation_keeps_the_lock_until_its_save_lands() {
    use lodestar_api::companions::CompanionsApi;
    let (addr, _q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = f.guided.clone();
    let id = || "qbittorrent".to_owned();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    let plan = g.plan(id()).await.unwrap();
    let park = Arc::new((tokio::sync::Notify::new(), tokio::sync::Notify::new()));
    *f.sink.2.lock().unwrap() = Some(park.clone());
    let request = tokio::spawn(async move { g.confirm(id(), plan).await });
    tokio::time::timeout(Duration::from_secs(10), park.0.notified())
        .await
        .expect("the hand-over never began");
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let install = || {
        f.service
            .install("qbittorrent".into(), "5.1.2".into(), "ab".repeat(32))
    };
    assert_eq!(install().await, Err(CompanionsError::Refused("busy")));
    park.1.notify_one();
    let origin = guided::origin_key(&qbt());
    let landed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(held) = f.service.changes().try_lock_owned() {
                return held;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(landed.is_ok(), "the confirmation never let the lock go");
    assert_eq!(f.row(&origin).as_deref(), Some("confirmed"));
    // And the watch says so, though nobody waited for the answer.
    assert_eq!(f.added.borrow().len(), 1);
    drop(landed);
    // Its entry is there now, so a native install is refused.
    assert_eq!(
        install().await,
        Err(CompanionsError::Refused("already_added"))
    );
}

/// Once the apply has started the generated password stays, marked
/// generated, whatever fails after; nothing is recorded or handed over
/// (#653's review, B1). A paste is refused then, so the password that may
/// already be qBittorrent's is never dropped, and a retry signs in with
/// it and finishes. If qBittorrent never took it, the retry is refused,
/// and forget says the credential was a generated one.
#[tokio::test]
async fn a_failed_hand_over_is_retried_with_the_generated_password() {
    let (addr, q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();

    // The hand-over fails after qBittorrent took the new password.
    let plan = g.plan(id()).await.unwrap();
    f.sink.1.store(true, Ordering::SeqCst);
    assert!(g.confirm(id(), plan).await.is_err());
    let generated = f.credential().unwrap();
    assert_ne!(generated, TEMPORARY);
    assert_eq!(
        q.lock().unwrap().password,
        generated,
        "applied before the hand-over"
    );
    assert_eq!(f.provenance().as_deref(), Some("generated"));
    assert_eq!(f.row(&guided::origin_key(&qbt())), None);
    assert!(f.added.borrow().is_empty());

    // Pasting the temporary password again would drop the only copy of the
    // live one: refused.
    assert_eq!(
        refused(g.paste(id(), TEMPORARY.into()).await.unwrap_err()),
        "generated_pending"
    );
    assert_eq!(f.credential(), Some(generated.clone()));
    // Nor may its address move: the retry signs in with the live password
    // (#653's review, B2).
    let (elsewhere, other) = qbittorrent(TEMPORARY).await;
    assert_eq!(
        refused(g.set_target(id(), url(elsewhere)).await.unwrap_err()),
        "generated_pending"
    );
    assert_eq!(
        f.row(&guided::applied_key(&qbt())),
        Some(format!(r#"{{"kind":"url","url":"http://{addr}"}}"#))
    );
    // Even a target row changed behind the endpoints is refused before any
    // sign-in there.
    let key = guided::target_key(&qbt());
    let moved = StoredTarget::Url {
        url: format!("http://{elsewhere}"),
    }
    .to_row();
    let saved = f.row(&key).unwrap();
    f.store
        .lock()
        .unwrap()
        .set_settings_text(&[(&key, &moved)])
        .unwrap();
    f.a_minute_passes();
    assert_eq!(refused(g.plan(id()).await.unwrap_err()), "target_changed");
    *f.guided.planned.lock().unwrap() = Some(vec![]);
    assert_eq!(
        refused(g.confirm(id(), vec![]).await.unwrap_err()),
        "target_changed"
    );
    assert_eq!(
        other.lock().unwrap().logins,
        0,
        "the other address got nothing"
    );
    f.store
        .lock()
        .unwrap()
        .set_settings_text(&[(&key, &saved)])
        .unwrap();

    // The retry signs in with the generated password and finishes.
    f.sink.1.store(false, Ordering::SeqCst);
    f.a_minute_passes();
    let failed = q.lock().unwrap().failed;
    let plan = g.plan(id()).await.unwrap();
    g.confirm(id(), plan).await.unwrap();
    assert_eq!(q.lock().unwrap().failed, failed, "no refused sign-in");
    assert_eq!(q.lock().unwrap().password, generated);
    assert_eq!(f.credential(), Some(generated.clone()));
    assert_eq!(
        f.row(&guided::origin_key(&qbt())).as_deref(),
        Some("confirmed")
    );
    let handed = f.sink.0.lock().unwrap().clone();
    assert_eq!(handed.len(), 1);
    assert_eq!(
        (handed[0].2.as_str(), handed[0].3.as_str()),
        ("admin", generated.as_str())
    );
    assert_eq!(f.added.borrow().len(), 1);
}

/// An apply qBittorrent refuses before it takes the new password: the
/// retry's sign-in is refused, paste stays refused, and forget answers
/// that the deleted credential was a generated one; adding it again then
/// works.
#[tokio::test]
async fn a_refused_apply_can_be_forgotten_and_added_again() {
    let (addr, q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    q.lock().unwrap().refuse_writes = true;
    let plan = g.plan(id()).await.unwrap();
    assert!(g.confirm(id(), plan).await.is_err());
    assert_eq!(
        q.lock().unwrap().password,
        TEMPORARY,
        "qBittorrent refused it"
    );
    assert_eq!(f.provenance().as_deref(), Some("generated"));
    assert_eq!(
        refused(g.paste(id(), TEMPORARY.into()).await.unwrap_err()),
        "generated_pending"
    );
    f.a_minute_passes();
    assert_eq!(
        refused(g.plan(id()).await.unwrap_err()),
        "companion_unauthorized"
    );

    let gone = g.forget(id()).await.unwrap();
    assert!(gone.credential_in_use);
    assert!(texts(&f.store).unwrap().is_empty());
    q.lock().unwrap().refuse_writes = false;
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    f.a_minute_passes();
    let plan = g.plan(id()).await.unwrap();
    g.confirm(id(), plan).await.unwrap();
    assert_eq!(q.lock().unwrap().password, f.credential().unwrap());
}

/// A wrong temporary password costs one refused sign-in per plan, and
/// after three in a row plans stop asking qBittorrent, before its ban at
/// five; forget resets the count (#653's review, L2).
#[tokio::test]
async fn wrong_passwords_stop_before_qbittorrents_ban() {
    let (addr, q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    g.set_target(id(), url(addr)).await.unwrap();
    for n in 1..=super::SIGN_IN_CAP {
        g.paste(id(), "not the password".into()).await.unwrap();
        f.a_minute_passes();
        assert_eq!(
            refused(g.plan(id()).await.unwrap_err()),
            "companion_unauthorized"
        );
        assert_eq!(
            q.lock().unwrap().failed as u32,
            n,
            "one refused sign-in a plan"
        );
    }
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    f.a_minute_passes();
    let logins = q.lock().unwrap().logins;
    assert_eq!(refused(g.plan(id()).await.unwrap_err()), "sign_in_refused");
    assert_eq!(q.lock().unwrap().logins, logins, "qBittorrent wasn't asked");

    g.forget(id()).await.unwrap();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();
    f.a_minute_passes();
    assert_eq!(g.plan(id()).await.unwrap(), ["webui.password"]);
}

/// While Lodestar installs qBittorrent itself, no guided step races it
/// (#653's review, L1).
#[tokio::test]
async fn a_guided_step_waits_for_an_install_under_way() {
    let f = fixture();
    let home = f.dir.path().join("companions/qbittorrent");
    std::fs::create_dir_all(&home).unwrap();
    lodestar_companions::state::save(
        &home,
        &lodestar_companions::state::Record {
            installed: None,
            op: Some(lodestar_companions::state::Op {
                kind: lodestar_companions::state::OpKind::Install,
                version: Some(Version::parse("5.1.2").unwrap()),
                step: lodestar_companions::state::Step::Begun,
            }),
        },
    )
    .unwrap();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    let t = CompanionTarget::Loopback { port: 8080 };
    for e in [
        g.set_target(id(), t).await.unwrap_err(),
        g.paste(id(), TEMPORARY.into()).await.unwrap_err(),
        g.plan(id()).await.unwrap_err(),
        g.confirm(id(), vec![]).await.unwrap_err(),
        g.forget(id()).await.unwrap_err(),
    ] {
        assert_eq!(refused(e), "busy");
    }
    assert!(texts(&f.store).unwrap().is_empty());
}

/// Each confirmation path checks its re-plan against what you confirmed,
/// not only against the last plan shown: a qBittorrent that changed
/// between the two is refused, and nothing is applied (#653's review,
/// L5). The last plan is set by hand here, as a qBittorrent that changed
/// in between would leave it.
#[tokio::test]
async fn a_replan_that_differs_from_the_confirmation_is_refused() {
    let (addr, q) = qbittorrent(TEMPORARY).await;
    let f = fixture();
    let g = &f.guided;
    let id = || "qbittorrent".to_owned();
    g.set_target(id(), url(addr)).await.unwrap();
    g.paste(id(), TEMPORARY.into()).await.unwrap();

    // The first confirmation: its re-plan is `webui.password`.
    let other = vec!["downloads.save_path".to_owned()];
    *g.planned.lock().unwrap() = Some(other.clone());
    assert_eq!(
        refused(g.confirm(id(), other.clone()).await.unwrap_err()),
        "confirmation_mismatch"
    );
    assert_eq!(f.provenance().as_deref(), Some("pasted"), "restored");
    assert_eq!(f.credential().as_deref(), Some(TEMPORARY));
    assert_eq!(f.row(&guided::applied_key(&qbt())), None);
    assert_eq!(q.lock().unwrap().password, TEMPORARY);

    // A retry: its re-plan is empty.
    let plan = g.plan(id()).await.unwrap();
    f.sink.1.store(true, Ordering::SeqCst);
    assert!(g.confirm(id(), plan).await.is_err());
    f.sink.1.store(false, Ordering::SeqCst);
    let pw = vec!["webui.password".to_owned()];
    *g.planned.lock().unwrap() = Some(pw.clone());
    assert_eq!(
        refused(g.confirm(id(), pw).await.unwrap_err()),
        "confirmation_mismatch"
    );
    assert_eq!(f.row(&guided::origin_key(&qbt())), None);
    assert!(f.sink.0.lock().unwrap().is_empty());
}
