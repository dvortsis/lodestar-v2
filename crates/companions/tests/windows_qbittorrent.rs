//! CMP-11: qBittorrent on Windows (owner cq4). The catalogue entry and its
//! guide, the pinned-version gate over CMP-8b's adapter, the hand-over, the
//! Windows save paths with hostile inputs, and the password kept out of
//! logs, errors and `Debug`. The fake qBittorrent is an in-process
//! `GuardedHttp`, so nothing waits on a socket or a timer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use lodestar_companions::adapter::{
    AdapterError, BoxFuture, ConfigPlan, Configure, ConfigureInput, CredentialError, Credentials,
    GuardedHttp, Limits, Method, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::catalogue::{Arch, ArchiveKind, Catalogue, Entry, NotAvailable, Platform};
use lodestar_companions::configure::{
    self, Category, CategoryName, ConfigSink, Confirmation, DownloadClientEntry, Downloads,
    IndexerManagerEntry, SavePath, SinkError, WindowsPath, qbittorrent,
};
use lodestar_companions::inputs::{HostPath, InputError, PortNumber, Version};
use lodestar_companions::windows_qbittorrent::{
    DOWNLOAD_PAGE, Guide, Installer, Step, VERSION_UNSUPPORTED, WindowsQBittorrent,
};
use lodestar_companions::{CompanionId, builtin};
use lodestar_integrations::secret::Secret;
use serde_json::{Map, Value, json};

const SHA: &str = "4f1c2b0c7b8e5d6a9f0e1d2c3b4a59687766554433221100ffeeddccbbaa9988";
const PASTED: &str = "Pasted-WebUi-Pw-71";
const GENERATED: &str = "GeneratedPw_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

// ─── A catalogue with the Windows installer pinned ──────────────────────

/// The compiled-in catalogue with qBittorrent's Windows installer pinned
/// at `version`, as CMP-13's tool would write it (test-only release
/// folder).
fn pinned(version: &str) -> &'static Entry {
    with_windows_pin(Some(version))
}

/// The compiled-in catalogue with qBittorrent's Windows installer awaiting
/// its pin, whatever CMP-13 later pins in the shipped file.
fn unpinned() -> &'static Entry {
    with_windows_pin(None)
}

fn with_windows_pin(version: Option<&str>) -> &'static Entry {
    let mut v: Value =
        serde_json::from_str(include_str!("../data/catalogue.json")).expect("catalogue json");
    let q = &mut v["entries"][0];
    assert_eq!(q["id"], "qbittorrent");
    q["native"]["release"] = json!("https://github.com/qbittorrent/qBittorrent/releases/download/");
    for a in q["native"]["artifacts"].as_array_mut().unwrap() {
        if a["platform"] != "windows" {
            continue;
        }
        a["pin"] = match version {
            None => Value::Null,
            Some(version) => json!({
                "version": version,
                "url": format!("https://github.com/qbittorrent/qBittorrent/releases/download/release-{version}/qbittorrent_{version}_x64_setup.exe"),
                "size": 36_000_000,
                "sha256": SHA,
                "upstream_sig": {"kind": "none"}
            }),
        };
    }
    let cat = Catalogue::from_json(&v.to_string()).expect("test catalogue");
    let cat: &'static Catalogue = Box::leak(Box::new(cat));
    cat.get("qbittorrent").unwrap()
}

fn builtin_qbt() -> &'static Entry {
    builtin().unwrap().get("qbittorrent").unwrap()
}

/// The shipped Windows build is an installer, whether or not CMP-13 has
/// pinned it yet.
#[test]
fn the_catalogue_offers_an_installer() {
    let q = builtin_qbt();
    let a = q.artifact(Platform::Windows, Arch::X86_64).unwrap();
    assert_eq!(a.archive, ArchiveKind::Installer);
    assert!(a.executable.is_none());
    assert_eq!(
        Installer::of(unpinned()).unwrap_err(),
        NotAvailable::AwaitingPin
    );
    // Prowlarr's Windows build is a zip Lodestar installs; it has no guide.
    let p = builtin().unwrap().get("prowlarr").unwrap();
    assert_eq!(Installer::of(p).unwrap_err(), NotAvailable::NoPlatform);
    assert_eq!(Guide::of(p).unwrap_err(), NotAvailable::NoPlatform);
}

/// Unpinned: the official page and the steps, and no version or checksum
/// claimed.
#[test]
fn an_unpinned_guide_claims_no_checksum() {
    let g = Guide::of(unpinned()).unwrap();
    assert!(g.pinned.is_none());
    assert!(!g.steps().contains(&Step::CheckSha256));
    assert_eq!(g.text(Step::CheckSha256), None);
    let all: Vec<String> = g.steps().iter().filter_map(|s| g.text(*s)).collect();
    assert_eq!(all.len(), Step::ALL.len() - 1);
    assert!(all[0].contains(DOWNLOAD_PAGE) && all[0].contains("doesn't download or run"));
    assert!(
        all[0].contains("no checked qBittorrent installer yet"),
        "{}",
        all[0]
    );
    for t in &all {
        assert!(
            !t.contains("SHA") && !t.contains("hash") && !t.contains("Get-FileHash"),
            "{t}"
        );
    }
}

/// Pinned: the official page, the pinned version, file and SHA-256, the
/// loopback WebUI, and steps that tell you to run it yourself.
#[test]
fn the_guide_shows_the_pin_and_never_a_lodestar_download() {
    let g = Guide::of(pinned("5.0.3")).unwrap();
    assert_eq!(g.download_page, DOWNLOAD_PAGE);
    assert_eq!(g.download_page, "https://www.qbittorrent.org/download");
    let p = g.pinned.as_ref().unwrap();
    assert_eq!(p.version.as_str(), "5.0.3");
    assert_eq!(p.file_name, "qbittorrent_5.0.3_x64_setup.exe");
    assert_eq!(p.sha256, SHA);
    assert_eq!(g.webui(), "http://localhost:8080");
    assert_eq!(g.steps(), Step::ALL.to_vec());
    let all: Vec<String> = g.steps().iter().filter_map(|s| g.text(*s)).collect();
    assert!(all[0].contains(DOWNLOAD_PAGE) && all[0].contains("doesn't download or run"));
    assert!(
        all[1].contains(SHA) && all[1].contains(r"Get-FileHash .\qbittorrent_5.0.3_x64_setup.exe")
    );
    assert!(all[3].contains("127.0.0.1") && all[3].contains("8080"));
    assert!(all[4].contains("Bypass authentication for clients on localhost off"));
    assert!(all[5].contains("http://localhost:8080"));
    // No step links the file itself: the page is the only address shown.
    for t in &all {
        assert!(!t.contains("github.com"), "{t}");
    }
}

// ─── A fake qBittorrent 5.0 ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Seen {
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct State {
    version: &'static str,
    password: String,
    prefs: Map<String, Value>,
    sessions: u32,
    failed_logins: u32,
    seen: Vec<Seen>,
    /// The version to switch to once the plan is made (a qBittorrent
    /// updated between the plan and the apply).
    then: Option<&'static str>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<State>>);

fn fake(version: &'static str) -> Fake {
    let prefs = json!({
        "save_path": "C:/Users/me/Downloads/",
        "web_ui_address": "127.0.0.1",
        "current_network_interface": "",
        "current_interface_address": "",
        "bypass_local_auth": false
    });
    Fake(Arc::new(Mutex::new(State {
        version,
        password: PASTED.to_owned(),
        prefs: prefs.as_object().unwrap().clone(),
        sessions: 0,
        failed_logins: 0,
        seen: Vec::new(),
        then: None,
    })))
}

fn form_decode(b: &[u8]) -> HashMap<String, String> {
    let dec = |s: &str| {
        let s = s.replace('+', " ");
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() + 1 {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    };
    std::str::from_utf8(b)
        .unwrap()
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (dec(k), dec(v))
        })
        .collect()
}

impl Fake {
    fn answer(&self, req: &ProbeRequest<'_>) -> (u16, Vec<u8>, Option<String>) {
        let mut s = self.0.lock().unwrap();
        let path = req.path.as_str().to_owned();
        let headers: Vec<(String, String)> = req
            .headers
            .iter()
            .map(|(n, v)| (n.as_str().to_ascii_lowercase(), v.expose().to_owned()))
            .collect();
        let body = req.body.clone().unwrap_or_default();
        s.seen.push(Seen {
            method: req.method,
            path: path.clone(),
            headers: headers.clone(),
            body: body.clone(),
        });
        if req.method == Method::Post && path == "/api/v2/auth/login" {
            let f = form_decode(&body);
            if f.get("username").map(String::as_str)
                == Some(lodestar_companions::config_files::Username::default().as_str())
                && f.get("password") == Some(&s.password)
            {
                s.sessions += 1;
                return (200, b"Ok.".to_vec(), Some(format!("SID=sid{}", s.sessions)));
            }
            s.failed_logins += 1;
            return (200, b"Fails.".to_vec(), None);
        }
        let authed = headers
            .iter()
            .any(|(n, v)| n == "cookie" && v.starts_with("SID=sid"));
        if !authed {
            return (403, b"Forbidden".to_vec(), None);
        }
        match (req.method, path.as_str()) {
            (Method::Get, "/api/v2/app/version") => {
                (200, format!("v{}", s.version).into_bytes(), None)
            }
            (Method::Get, "/api/v2/app/preferences") => (
                200,
                Value::Object(s.prefs.clone()).to_string().into_bytes(),
                None,
            ),
            (Method::Get, "/api/v2/torrents/categories") => (200, b"{}".to_vec(), None),
            (Method::Post, "/api/v2/app/setPreferences") => {
                let f = form_decode(&body);
                let Ok(Value::Object(m)) = serde_json::from_str(&f["json"]) else {
                    return (400, Vec::new(), None);
                };
                for (k, v) in m {
                    if k == "web_ui_password" {
                        s.password = v.as_str().unwrap().to_owned();
                    } else {
                        s.prefs.insert(k, v);
                    }
                }
                (200, Vec::new(), None)
            }
            (Method::Post, "/api/v2/torrents/createCategory") => (200, Vec::new(), None),
            _ => (404, b"Not Found".to_vec(), None),
        }
    }

    fn writes(&self) -> Vec<Seen> {
        let s = self.0.lock().unwrap();
        s.seen
            .iter()
            .filter(|r| r.method == Method::Post && r.path != "/api/v2/auth/login")
            .cloned()
            .collect()
    }

    fn after_plan(&self) {
        let mut s = self.0.lock().unwrap();
        if let Some(v) = s.then.take() {
            s.version = v;
        }
    }
}

impl GuardedHttp for Fake {
    fn send<'a>(
        &'a self,
        request: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async move {
            let (status, body, cookie) = self.answer(&request);
            let r = ProbeResponse::new(status, body, &Limits::PROBE)?;
            match cookie {
                Some(c) if request.session_cookie.is_some() => {
                    r.with_session_cookie(Secret::new(c))
                }
                _ => Ok(r),
            }
        })
    }
}

// ─── Credentials, the sink and the log capture ──────────────────────────

/// The companion's credential as CMP-5g holds it: the generated password,
/// stored before the apply.
struct Creds(&'static str);

impl Credentials for Creds {
    fn get(&self, field: &str) -> Result<Option<Secret>, CredentialError> {
        Ok((field == "webui_password").then(|| Secret::new(self.0)))
    }
}

#[derive(Default)]
struct Sink(Mutex<Vec<DownloadClientEntry>>);

impl ConfigSink for Sink {
    fn download_client<'a>(
        &'a self,
        entry: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        self.0.lock().unwrap().push(entry);
        Box::pin(async { Ok(()) })
    }

    fn indexer_manager<'a>(
        &'a self,
        _entry: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async { Err(SinkError::Rejected) })
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

fn loopback() -> Target {
    Target::Loopback(PortNumber::new(8080).unwrap())
}

fn input<'a>(creds: &'a Creds, target: &Target) -> ConfigureInput<'a> {
    ConfigureInput::new(creds).guided(Confirmation::of(target.clone()), Secret::new(PASTED))
}

/// Plan, then apply, through the Windows gate.
async fn run(
    gate: &dyn Configure,
    http: &Fake,
    input: &ConfigureInput<'_>,
) -> Result<ConfigPlan, AdapterError> {
    let t = loopback();
    let plan = gate.plan(http, &t, input).await?;
    http.after_plan();
    gate.apply(http, &t, input, &plan).await?;
    Ok(plan)
}

// ─── The version gate ───────────────────────────────────────────────────

#[tokio::test]
async fn a_supported_version_is_configured_and_becomes_the_download_client() {
    let log = capture();
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let http = fake("5.0.4");
    let creds = Creds(GENERATED);
    let t = loopback();
    let input = input(&creds, &t);
    let plan = run(&gate, &http, &input).await.unwrap();
    assert!(plan.changes().contains(&qbittorrent::PASSWORD));
    assert_eq!(http.0.lock().unwrap().password, GENERATED);

    let sink = Sink::default();
    let id = CompanionId::parse("qbittorrent").unwrap();
    configure::hand_over(&sink, &id, &t, &input).await.unwrap();
    let entries = sink.0.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].companion, id);
    assert_eq!(entries[0].target, t);
    assert_eq!(entries[0].password.expose(), GENERATED);

    // Neither password reaches a log line, a URL, a header or `Debug`.
    let lines = log.0.lock().unwrap().join("\n");
    assert!(lines.contains("companions.config_handed_over"), "{lines}");
    for secret in [PASTED, GENERATED] {
        assert!(!lines.contains(secret));
        assert!(!format!("{gate:?} {:?} {:?}", entries[0], plan).contains(secret));
        for r in &http.0.lock().unwrap().seen {
            assert!(!r.path.contains(secret));
            assert!(!r.headers.iter().any(|(_, v)| v.contains(secret)));
            // Only in the sign-in form and the one password write.
            if String::from_utf8_lossy(&r.body).contains(secret) {
                assert!(
                    ["/api/v2/auth/login", "/api/v2/app/setPreferences"].contains(&r.path.as_str()),
                    "{}",
                    r.path
                );
            }
        }
    }
}

#[tokio::test]
async fn a_version_outside_the_pin_is_refused_before_anything_is_written() {
    for found in ["5.0.2", "4.6.7", "5.1.0", "6.0.0"] {
        let gate = WindowsQBittorrent::new(pinned("5.0.3"));
        let http = fake(found);
        let creds = Creds(GENERATED);
        let t = loopback();
        let err = gate.plan(&http, &t, &input(&creds, &t)).await.unwrap_err();
        assert_eq!(err, AdapterError::Refused(VERSION_UNSUPPORTED), "{found}");
        assert!(http.writes().is_empty(), "{found}");
        assert_eq!(http.0.lock().unwrap().password, PASTED);
        // The plain message names both versions, nothing qBittorrent sent,
        // and the gate keeps it for the screen.
        let msg = gate.explain(&Version::parse(found).unwrap()).unwrap();
        assert!(msg.contains(found) && msg.contains("5.0.3"), "{msg}");
        assert_eq!(gate.last_refusal().unwrap().to_string(), msg);
    }
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    assert_eq!(gate.explain(&Version::parse("5.0.9").unwrap()), None);
}

/// A qBittorrent updated between the plan and the apply is checked again,
/// before the password is written.
#[tokio::test]
async fn the_apply_checks_the_version_again() {
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let http = fake("5.0.3");
    http.0.lock().unwrap().then = Some("5.1.0");
    let creds = Creds(GENERATED);
    let t = loopback();
    let err = run(&gate, &http, &input(&creds, &t)).await.unwrap_err();
    assert_eq!(err, AdapterError::Refused(VERSION_UNSUPPORTED));
    assert!(http.writes().is_empty());
    assert_eq!(http.0.lock().unwrap().password, PASTED);
}

/// While the installer awaits its pin, the gate accepts exactly CMP-8b's
/// checked range (4.6.0 to below 5.3): inside it, configured; below it,
/// at 5.3 and past it, refused with nothing written (runner, option (D)).
#[tokio::test]
async fn an_unpinned_installer_accepts_the_checked_range_only() {
    let creds = Creds(GENERATED);
    let t = loopback();
    for ok in ["4.6.0", "5.0.3", "5.2.9"] {
        let gate = WindowsQBittorrent::new(unpinned());
        let http = fake(ok);
        run(&gate, &http, &input(&creds, &t)).await.unwrap();
        assert_eq!(http.0.lock().unwrap().password, GENERATED, "{ok}");
    }
    for refused in ["4.5.9", "5.3.0", "5.3", "6.0.0"] {
        let gate = WindowsQBittorrent::new(unpinned());
        let http = fake(refused);
        let err = gate.plan(&http, &t, &input(&creds, &t)).await.unwrap_err();
        assert_eq!(err, AdapterError::Refused(VERSION_UNSUPPORTED), "{refused}");
        assert!(http.writes().is_empty(), "{refused}");
        assert_eq!(http.0.lock().unwrap().password, PASTED);
        assert!(gate.last_refusal().is_some());
    }
}

#[tokio::test]
async fn an_unreadable_version_refuses() {
    let creds = Creds(GENERATED);
    let t = loopback();
    for gate in [
        WindowsQBittorrent::new(unpinned()),
        WindowsQBittorrent::new(pinned("5.0.3")),
    ] {
        let http = fake("5.0.3-beta");
        let err = gate.plan(&http, &t, &input(&creds, &t)).await.unwrap_err();
        assert_eq!(err, AdapterError::Protocol("version"));
        assert!(http.writes().is_empty());
    }
}

/// A wrong pasted password costs one refused sign-in, as without the gate.
#[tokio::test]
async fn a_wrong_password_costs_one_sign_in() {
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let http = fake("5.0.3");
    http.0.lock().unwrap().password = "something-else".into();
    let creds = Creds(PASTED);
    let t = loopback();
    let err = gate.plan(&http, &t, &input(&creds, &t)).await.unwrap_err();
    assert_eq!(err, AdapterError::Unauthorized);
    let s = http.0.lock().unwrap();
    let logins = s
        .seen
        .iter()
        .filter(|r| r.path == "/api/v2/auth/login")
        .count();
    assert_eq!(logins, 1);
    assert!(!format!("{err:?} {err}").contains(PASTED));
}

fn failed_logins(http: &Fake) -> u32 {
    http.0.lock().unwrap().failed_logins
}

/// The gate checks on CMP-8b's own session, so it never adds a failed
/// sign-in: a stored credential that isn't qBittorrent's yet costs one in
/// the plan, and a retried apply whose password was already replaced costs
/// one in the apply, as without the gate.
#[tokio::test]
async fn the_gate_adds_no_sign_in_of_its_own() {
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let http = fake("5.0.3");
    let creds = Creds(GENERATED);
    let t = loopback();
    let i = input(&creds, &t);
    let plan = gate.plan(&http, &t, &i).await.unwrap();
    assert_eq!(failed_logins(&http), 1);
    assert_eq!(gate.last_refusal(), None);
    // An earlier apply already wrote the generated password.
    http.0.lock().unwrap().password = GENERATED.into();
    let dyn_gate: &dyn Configure = &gate;
    dyn_gate.apply(&http, &t, &i, &plan).await.unwrap();
    assert_eq!(failed_logins(&http), 2);
    let version_reads = http
        .0
        .lock()
        .unwrap()
        .seen
        .iter()
        .filter(|r| r.path == "/api/v2/app/version" && r.headers.iter().any(|(n, _)| n == "cookie"))
        .count();
    assert_eq!(version_reads, 2, "checked in the plan and the apply");
}

/// The configurator registered for Windows is the gate: it refuses a
/// version no range accepts. On Windows that's what `configure::configurator` hands
/// out; elsewhere it's CMP-8b's adapter, which reads no version.
#[tokio::test]
async fn the_windows_configurator_is_the_gate() {
    let id = CompanionId::parse("qbittorrent").unwrap();
    let creds = Creds(GENERATED);
    let t = loopback();
    // A release no pin or checked range accepts, whatever CMP-13 pins.
    let c = lodestar_companions::windows_qbittorrent::configurator(&id).unwrap();
    let http = fake("99.0.0");
    let err = c.plan(&http, &t, &input(&creds, &t)).await.unwrap_err();
    assert_eq!(err, AdapterError::Refused(VERSION_UNSUPPORTED));
    let registered = configure::configurator(&id).unwrap();
    let http = fake("99.0.0");
    let got = registered.plan(&http, &t, &input(&creds, &t)).await;
    if cfg!(windows) {
        assert_eq!(got.unwrap_err(), AdapterError::Refused(VERSION_UNSUPPORTED));
    } else {
        assert!(got.is_ok(), "{got:?}");
    }
}

// ─── Windows save paths ─────────────────────────────────────────────────

#[test]
fn windows_paths_accept_drives_and_shares() {
    for ok in [
        r"D:\Downloads",
        r"C:\Users\me\Downloads",
        r"c:\Users\me\Downloads\lodestar",
        r"E:\Media\TV Shows",
        r"\\nas\downloads",
        r"\\nas.lan\media\complete",
        r"\\192.168.1.20\torrents",
        r"D:\a.b\c-d_e",
    ] {
        let p = WindowsPath::parse(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert_eq!(p.as_str(), ok);
    }
}

#[test]
fn windows_paths_refuse_hostile_inputs() {
    use InputError::*;
    let cases: &[(&str, InputError)] = &[
        // Traversal and empty folders.
        (r"C:\Downloads\..\Windows", Component),
        (r"C:\..", Component),
        (r"C:\Downloads\.", Component),
        (r"C:\Downloads\", Component),
        (r"C:\Down\\loads", Component),
        (r"\\nas\share\..\..\c$", Component),
        // Windows strips a trailing dot or space: `.. ` is `..`.
        (r"C:\Downloads\.. ", Character),
        (r"C:\Downloads\...", Character),
        (r"C:\Windows.", Character),
        (r"C:\ Downloads", Character),
        // Verbatim, device and NT namespaces.
        (r"\\?\C:\Downloads", Forbidden),
        (r"\\?\UNC\nas\share", Forbidden),
        (r"\\.\PhysicalDrive0", Forbidden),
        (r"\\.\pipe\x", Forbidden),
        (r"\??\C:\Downloads", Forbidden),
        // Device names, any case, with an extension or spaces.
        (r"C:\CON", Reserved),
        (r"C:\Downloads\nul", Reserved),
        (r"C:\Downloads\NUL.txt", Reserved),
        (r"C:\Downloads\aux .tar.gz", Reserved),
        (r"C:\Downloads\Com1", Reserved),
        (r"C:\Downloads\lpt9\x", Reserved),
        (r"\\nas\prn", Reserved),
        // Separators: forward or mixed.
        ("C:/Downloads", Character),
        (r"C:\Downloads/sub", Character),
        (r"\\nas/share", Character),
        ("//nas/share", Character),
        // Relative forms.
        (r"C:Downloads", NotAbsolute),
        ("C:", NotAbsolute),
        (r"\Downloads", NotAbsolute),
        ("Downloads", NotAbsolute),
        (r".\Downloads", NotAbsolute),
        // Streams, short names, admin shares, variables, wildcards, Unicode.
        (r"C:\Downloads:hidden", Character),
        (r"C:\PROGRA~1\x", Character),
        (r"\\nas\C$", Character),
        (r"\\nas\ADMIN$\x", Character),
        (r"C:\%APPDATA%\x", Character),
        (r"C:\Down*", Character),
        ("C:\\Downloads\u{ff0e}\u{ff0e}", Character),
        ("C:\\Down\u{0}loads", Character),
        (r#"C:\"x""#, Character),
        (r"\\[::1]\share", Character),
        (r"\\-nas\share", Character),
        // Roots and Windows' own folders.
        (r"C:\", Forbidden),
        (r"C:\Windows", Forbidden),
        (r"c:\WINDOWS\System32", Forbidden),
        (r"C:\Program Files\qBittorrent", Forbidden),
        (r"C:\ProgramData\Lodestar", Forbidden),
        (r"C:\ProgramData", Forbidden),
        (r"D:\System Volume Information", Forbidden),
        (r"C:\Users", Forbidden),
        (r"C:\Users\me", Forbidden),
        (r"C:\Users\me\AppData\Roaming\qBittorrent", Forbidden),
        (r"C:\Users\me\.ssh", Forbidden),
        // Junctions Windows ships into the refused folders.
        (r"C:\Users\All Users\Lodestar", Forbidden),
        (r"C:\Users\Default User\x", Forbidden),
        (r"C:\Documents and Settings\me\Downloads", Forbidden),
        (r"C:\Users\me\Application Data\x", Forbidden),
        (r"C:\Users\me\Local Settings\Temp", Forbidden),
        (r"C:\Users\me\Start Menu", Forbidden),
        (r"C:\Users\me\SendTo", Forbidden),
        // UNC: this machine, pipes and mailslots, and shared system roots.
        (r"\\localhost\Users\me\AppData\Roaming", Forbidden),
        (r"\\LOCALHOST\downloads", Forbidden),
        (r"\\127.0.0.1\downloads", Forbidden),
        (r"\\127.1\downloads", Forbidden),
        (r"\\2130706433\downloads", Forbidden),
        (r"\\0.0.0.0\downloads", Forbidden),
        (r"\\a.localhost\downloads", Forbidden),
        // Loopback in octal, hex and Windows' IPv6-literal names.
        (r"\\0177.0.0.1\share\x", Forbidden),
        (r"\\0x7f.0.0.1\share\x", Forbidden),
        (r"\\0x7f000001\share\x", Forbidden),
        (r"\\0--1.ipv6-literal.net\share\x", Forbidden),
        (r"\\192.168.001.20\share", Forbidden),
        (r"\\nas\Windows.old\x", Forbidden),
        (r"C:\Windows.old\Users\me", Forbidden),
        (r"\\nas\pipe\x", Forbidden),
        (r"\\nas\MailSlot\x", Forbidden),
        (r"\\nas\Users\me\AppData", Forbidden),
        (r"\\nas\Windows\System32", Forbidden),
        (r"\\nas\ProgramData\Lodestar", Forbidden),
        (r"\\nas\share\", Component),
        // UNC without a share.
        (r"\\nas", Component),
        (r"\\nas\", Component),
        (r"\\", Component),
        ("", Length),
    ];
    for (s, want) in cases {
        assert_eq!(WindowsPath::parse(s), Err(*want), "{s:?}");
    }
    let long = format!(r"D:\{}", "a".repeat(245));
    assert_eq!(WindowsPath::parse(&long), Err(Length));
    assert!(WindowsPath::parse(&long[..247]).is_ok());
}

#[test]
fn windows_downloads_take_no_linux_category_path() {
    let win = WindowsPath::parse(r"D:\Downloads").unwrap();
    let cat = |p: Option<HostPath>| Category {
        name: CategoryName::parse("lodestar").unwrap(),
        save_path: p,
    };
    let d = Downloads::new(win.clone(), vec![cat(None)]).unwrap();
    assert_eq!(d.save_path(), &SavePath::Windows(win.clone()));
    assert_eq!(
        Downloads::new(
            win,
            vec![cat(Some(HostPath::parse("/downloads/x").unwrap()))]
        ),
        Err(InputError::NotAbsolute)
    );
    // A Linux save path is as before.
    let linux = HostPath::parse("/downloads").unwrap();
    let d = Downloads::new(linux.clone(), vec![cat(Some(linux.clone()))]).unwrap();
    assert_eq!(d.save_path().as_str(), "/downloads");
}

/// The save path qBittorrent reports with `/` and another case is the same
/// folder; a different one is written in backslashes.
#[tokio::test]
async fn a_windows_save_path_is_compared_and_written_as_windows_does() {
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let creds = Creds(GENERATED);
    let t = loopback();
    let same = WindowsPath::parse(r"c:\users\ME\downloads").unwrap();
    let http = fake("5.0.3");
    let i = input(&creds, &t).downloads(Downloads::new(same, Vec::new()).unwrap());
    let plan = gate.plan(&http, &t, &i).await.unwrap();
    assert!(
        !plan
            .changes()
            .iter()
            .any(|k| k.as_str() == "downloads.save_path"),
        "{plan:?}"
    );

    let other = WindowsPath::parse(r"D:\Torrents").unwrap();
    let http = fake("5.0.3");
    let i = input(&creds, &t).downloads(Downloads::new(other, Vec::new()).unwrap());
    let plan = run(&gate, &http, &i).await.unwrap();
    assert!(
        plan.changes()
            .iter()
            .any(|k| k.as_str() == "downloads.save_path")
    );
    assert_eq!(
        http.0.lock().unwrap().prefs["save_path"],
        json!(r"D:\Torrents")
    );
}

#[test]
fn same_compares_as_windows_does() {
    let p = WindowsPath::parse(r"\\nas\Media\Done").unwrap();
    for have in [
        r"\\nas\media\done",
        "//nas/Media/Done/",
        r"\\NAS\Media\Done\",
    ] {
        assert!(p.same(have), "{have}");
    }
    assert!(!p.same(r"\\nas\Media"));
    assert!(!p.same(r"\\nas\Media\Done2"));
    let d = WindowsPath::parse(r"D:\Torrents").unwrap();
    assert!(d.same("d:/torrents/") && d.same(r"D:\Torrents\"));
    assert!(!d.same(r"D:\"));
}

proptest::proptest! {
    /// Whatever is typed, an accepted path has none of the refused shapes.
    #[test]
    fn an_accepted_path_is_never_a_refused_shape(
        s in proptest::prop_oneof![
            ".{0,80}",
            r"([A-Za-z]:|\\\\[a-z0-9.?-]{0,6})(\\[A-Za-z0-9 ._$~:/?-]{0,12}){0,5}",
        ]
    ) {
        if let Ok(p) = WindowsPath::parse(&s) {
            let t = p.as_str();
            proptest::prop_assert!(t.is_ascii() && !t.contains('/'));
            proptest::prop_assert!(!t.starts_with(r"\\?") && !t.starts_with(r"\\.") && !t.starts_with(r"\??"));
            proptest::prop_assert!(!t[2..].contains(':'));
            proptest::prop_assert!(p.same(t));
            let start = if t.starts_with(r"\\") { 2 } else { 3 };
            let folders: Vec<String> = t[start..].split('\\').map(str::to_ascii_lowercase).collect();
            for (i, f) in folders.iter().enumerate() {
                proptest::prop_assert!(!f.is_empty() && f != "." && f != "..");
                proptest::prop_assert!(!f.ends_with('.') && !f.ends_with(' ') && !f.starts_with(' '));
                let stem = f.split('.').next().unwrap().trim_end();
                proptest::prop_assert!(!["con", "prn", "aux", "nul"].contains(&stem));
                proptest::prop_assert!(!(stem.len() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) && stem.as_bytes()[3].is_ascii_digit()));
                if i == 0 && start == 2 {
                    proptest::prop_assert!(f != "localhost" && !f.starts_with("127."));
                }
            }
            let rooted = if start == 2 { &folders[1..] } else { &folders[..] };
            if let Some(first) = rooted.first() {
                proptest::prop_assert!(!["windows", "programdata", "program files", "documents and settings"].contains(&first.as_str()));
                if first == "users" {
                    proptest::prop_assert!(rooted.len() >= 3 && rooted[2] != "appdata" && rooted[1] != "all users");
                }
            }
        }
    }
}

/// The gated plan signs in exactly once, and reads the version on that
/// session: no sign-in of the gate's own.
#[tokio::test]
async fn the_gated_plan_signs_in_exactly_once() {
    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let http = fake("5.0.3");
    // CMP-5g's plan holds the pasted password as the credential too.
    let creds = Creds(PASTED);
    let t = loopback();
    gate.plan(&http, &t, &input(&creds, &t)).await.unwrap();
    let s = http.0.lock().unwrap();
    let logins = s
        .seen
        .iter()
        .filter(|r| r.path == "/api/v2/auth/login")
        .count();
    assert_eq!(logins, 1);
    assert_eq!(s.failed_logins, 0);
    let signed_in_version = s
        .seen
        .iter()
        .filter(|r| r.path == "/api/v2/app/version" && r.headers.iter().any(|(n, _)| n == "cookie"))
        .count();
    assert_eq!(signed_in_version, 1);
}

/// A save path's kind must match the adapter: a Linux path never reaches
/// Windows' qBittorrent, and a Windows path never reaches anyone else's.
/// Both are refused before anything is sent.
#[tokio::test]
async fn a_save_path_of_the_wrong_platform_is_refused() {
    let creds = Creds(PASTED);
    let t = loopback();
    let linux = Downloads::new(HostPath::parse("/downloads").unwrap(), Vec::new()).unwrap();
    let windows = Downloads::new(WindowsPath::parse(r"D:\Downloads").unwrap(), Vec::new()).unwrap();

    let gate = WindowsQBittorrent::new(pinned("5.0.3"));
    let http = fake("5.0.3");
    let err = gate
        .plan(&http, &t, &input(&creds, &t).downloads(linux.clone()))
        .await
        .unwrap_err();
    assert_eq!(err, AdapterError::Refused(configure::SAVE_PATH_PLATFORM));
    assert!(http.0.lock().unwrap().seen.is_empty());

    let id = CompanionId::parse("qbittorrent").unwrap();
    // CMP-8b's adapter without the gate, as every platform but Windows
    // gets it.
    let plain: Box<dyn Configure> = Box::new(qbittorrent::QBittorrent::new(id.clone()));
    let http = fake("5.0.3");
    let err = plain
        .plan(&http, &t, &input(&creds, &t).downloads(windows))
        .await
        .unwrap_err();
    assert_eq!(err, AdapterError::Refused(configure::SAVE_PATH_PLATFORM));
    assert!(http.0.lock().unwrap().seen.is_empty());

    // The matching kinds still plan.
    let http = fake("5.0.3");
    plain
        .plan(&http, &t, &input(&creds, &t).downloads(linux))
        .await
        .unwrap();
}

#[test]
fn the_windows_configurator_is_only_qbittorrents() {
    for other in ["prowlarr", "gluetun"] {
        let id = CompanionId::parse(other).unwrap();
        assert!(lodestar_companions::windows_qbittorrent::configurator(&id).is_none());
    }
}

/// The ungated adapter is for adapter tests only: production code gets a
/// configurator through `configure::configurator`, which applies the
/// Windows gate. Fails if the binary's or the API's sources name it.
#[test]
fn production_code_never_builds_the_ungated_adapter() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for d in ["lodestar/src", "api/src"] {
        walk(&root.join(d), &mut files);
    }
    assert!(
        files.len() > 20,
        "the scan found too few files: {}",
        files.len()
    );
    let mut hits = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (n, line) in text.lines().enumerate() {
            let names_it = line.contains("configure::qbittorrent::QBittorrent")
                || line.contains("qbittorrent::QBittorrent::new")
                || (line.contains("configure::qbittorrent::{") && line.contains("QBittorrent"));
            if names_it {
                hits.push(format!("{}:{}", f.display(), n + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the ungated adapter is built outside tests: {hits:?}"
    );
}
