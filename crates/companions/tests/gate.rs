//! CMP-G, the companion gate (architecture §18.9 "Companion tests", §18.10,
//! §12; plan `docs/plan/2026-11-06-v06-companions.md` CMP-G; decisions
//! `docs/decisions/2026-11-28-CMP-G.md`).
//!
//! One test per guarantee, each through the crate's public API as serve
//! wires it, and each failing if its guarantee broke:
//!
//! 1. a download that fails its checksum, or a signature that fails, is
//!    refused, and nothing is installed or run;
//! 2. managed companions bind to loopback, and a container's ports stay on
//!    the internal network unless you publish one;
//! 3. a companion's traffic follows its route, and with the VPN route
//!    chosen and down it isn't running (the kill switch);
//! 4. a companion that's missing, or whose route isn't set up, leaves its
//!    dependants waiting ("Needs setup") and is never started;
//! 5. a credential is sealed at rest and never reaches a log line, a
//!    companion's log ring, a journal, a compose file or a `Debug`;
//! 6. a companion left running by an earlier run is killed before anything
//!    starts, and an uninstall leaves nothing of it behind;
//! 7. the install smoke: a test companion from the test-only catalogue
//!    (`tests/data/test-catalogue/`) is installed, checked, run under the
//!    real supervisor and sandbox, and removed;
//! 8. a release build can't load any catalogue but the compiled-in one.
//!
//! **The test companion is this test binary,** packed as the artifact a
//! catalogue pin names (a `.tar.gz` on Linux, a `.zip` on Windows, as
//! Prowlarr ships). Run with `FAKE_MODE` set and filtered to
//! [`fake_companion`], it acts as a companion; on Linux the launcher is this
//! binary too ([`fake_launcher`]). The manager's own [`Run`] is a fake that
//! records the launch, because the manager starts the real program with
//! its real flags (`-nobrowser -data=…`), which a test binary can't take;
//! the tests then start the installed bytes themselves under the real
//! supervisor, sandbox and route gate.
//!
//! No network beyond loopback. Every wait is a condition with a deadline
//! of at least 10 s.

#![cfg(any(target_os = "linux", windows))]

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use lodestar_companions::adapter::{
    AdapterError, BoxFuture, Credentials, GuardedHttp, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::catalogue::{self, Catalogue, Exposure, Need, PortName, Sha256};
use lodestar_companions::compose::{
    self, Bind, DataFolders, Paths, Publish, Route as ComposeRoute, ServiceInput, TemplateError,
};
use lodestar_companions::config_files::{self, PasswordHash, Username};
use lodestar_companions::configure::{InstallReceipt, Socks5Proxy};
use lodestar_companions::credentials::{self, CredentialKey, Field, SealedRow};
use lodestar_companions::fetch::{self, Chunks, FetchError, Opened, Source};
use lodestar_companions::inputs::{NetworkName, Pgid, PortNumber, Puid};
use lodestar_companions::manager::{
    ApiConfigure, Downloader, Health, Host, InstallToken, Launch, Manager, ManagerError, Parts,
    Prelaunch, Routes, RowError, Rows, Run, Supervised, launch_env, reads_proxy_env,
};
use lodestar_companions::requires;
use lodestar_companions::supervise::egress::{self, Decision, Follows, GateError, Route};
use lodestar_companions::supervise::{
    Companion, Confinement, EnvName, Launcher, Limits, LogRing, NoHook, Sandbox, SpawnSpec, State,
    Status, Supervisor, Timings, pidfile,
};
use lodestar_companions::verify::{self, VerifyError};
use lodestar_companions::{CompanionId, Version};
use lodestar_integrations::secret::{SealedSecret, Secret, SecretStore};
use lodestar_sources::egress::{DownReason, RouteKind, RouteState};
use lodestar_update::testing::TestKey;
use reqwest::Url;
use sha2::Digest;
use tokio::sync::watch;

const DEADLINE: Duration = Duration::from_secs(60);

/// The test companion's version in the test catalogue.
const VERSION: &str = "1.0.0.1";

/// A SOCKS5 proxy on the VPN, as a companion's route names it. Nothing
/// listens there: the gate's companions never send anything.
const PROXY: &str = "10.8.0.1:1080";

/// The test catalogue: the shipped entries for qBittorrent, Prowlarr and
/// gluetun with test image digests, and no pins. The gate pins Prowlarr's
/// build for this platform to the test companion's archive at run time.
const TEST_CATALOGUE: &str = include_str!("data/test-catalogue/catalogue.json");

/// The download slot is per companion and per process: installs take
/// turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ─── The test companion ─────────────────────────────────────────────────

/// The test companion's modes. Never returns when `FAKE_MODE` is set.
#[test]
fn fake_companion() {
    let Ok(mode) = std::env::var("FAKE_MODE") else {
        return;
    };
    let mut out = std::io::stdout();
    match mode.as_str() {
        // Says which proxy it was launched with and, when `FAKE_CONFIG`
        // names its config file, the API key in it, as a companion that
        // logs its own key would. Then answers HTTP on
        // 127.0.0.1:`FAKE_PORT`.
        "serve" => {
            let proxy = std::env::var("ALL_PROXY").unwrap_or_else(|_| "none".into());
            let _ = writeln!(out, "PROXY {proxy}");
            if let Some(conf) = std::env::var_os("FAKE_CONFIG") {
                let text = std::fs::read_to_string(conf).unwrap_or_default();
                let key = text
                    .split("<ApiKey>")
                    .nth(1)
                    .and_then(|r| r.split("</ApiKey>").next())
                    .unwrap_or("unreadable");
                let _ = writeln!(out, "KEY {key}");
            }
            let _ = out.flush();
            let port: u16 = std::env::var("FAKE_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| panic!("FAKE_PORT"));
            let listener =
                std::net::TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| panic!("{e}"));
            let _ = writeln!(out, "READY");
            let _ = out.flush();
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { continue };
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
            hang()
        }
        "hang" => {
            let _ = writeln!(out, "READY");
            let _ = out.flush();
            hang()
        }
        // An earlier run's work child: supervises the installed companion
        // `FAKE_EXE`, until it's killed.
        "work" => {
            let data = PathBuf::from(std::env::var_os("FAKE_DATA").unwrap_or_default());
            let exe = PathBuf::from(std::env::var_os("FAKE_EXE").unwrap_or_default());
            let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
            rt.block_on(async move {
                let mut sup = Supervisor::new(data.clone(), Arc::new(NoHook), Timings::default())
                    .await
                    .unwrap_or_else(|e| panic!("{e}"));
                let spawn = spec(&data, exe, &[("FAKE_MODE", "hang".into())]);
                sup.add(companion(spawn, Vec::new()))
                    .unwrap_or_else(|e| panic!("{e}"));
                std::future::pending::<()>().await;
            });
            hang()
        }
        _ => std::process::exit(64),
    }
}

/// Lodestar's launcher, on Linux: with the sandbox's plan in its
/// environment, applies it and execs the companion. Otherwise nothing.
#[test]
fn fake_launcher() {
    #[cfg(target_os = "linux")]
    if std::env::var_os(lodestar_companions::supervise::limits_linux::PLAN_ENV).is_some() {
        std::process::exit(lodestar_companions::supervise::limits_linux::run_launcher());
    }
}

fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn me() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|e| panic!("{e}"))
}

fn args_for(test: &str) -> Vec<OsString> {
    [test, "--exact", "--nocapture", "--test-threads=1", "-q"]
        .map(OsString::from)
        .to_vec()
}

fn prowlarr() -> CompanionId {
    CompanionId::parse("prowlarr").unwrap_or_else(|e| panic!("{e:?}"))
}

fn qbt() -> CompanionId {
    CompanionId::parse("qbittorrent").unwrap_or_else(|e| panic!("{e:?}"))
}

/// The installed companion's spawn, as `Supervised` builds it, but with
/// this binary's test filter for arguments and `allow_privileged` (CI's
/// Windows runners are administrators; `Supervised` refuses that, and
/// `tests/supervise.rs` checks it).
fn spec(data: &Path, exe: PathBuf, env: &[(&str, OsString)]) -> SpawnSpec {
    SpawnSpec {
        id: prowlarr(),
        executable: exe,
        args: args_for("fake_companion"),
        env: env
            .iter()
            .map(|(k, v)| {
                (
                    EnvName::parse(k).unwrap_or_else(|e| panic!("{e}")),
                    v.clone(),
                )
            })
            .collect(),
        data_dir: data.to_owned(),
        allow_privileged: true,
    }
}

fn companion(spawn: SpawnSpec, secrets: Vec<Secret>) -> Companion {
    Companion {
        spawn,
        depends_on: Vec::new(),
        probe: None,
        secrets,
        stop: None,
    }
}

// ─── The test catalogue and the artifact ───────────────────────────────

/// The test companion's archive: this binary as `Prowlarr/Prowlarr(.exe)`
/// beside a .NET 8 `runtimeconfig.json` (so a SOCKS5 route can be launched
/// on), packed as Prowlarr ships for this platform. Built once.
fn artifact() -> &'static [u8] {
    static A: OnceLock<Vec<u8>> = OnceLock::new();
    A.get_or_init(|| {
        let exe = std::fs::read(me()).unwrap_or_else(|e| panic!("{e}"));
        let config = br#"{"runtimeOptions":{"tfm":"net8.0","includedFrameworks":[{"name":"Microsoft.NETCore.App","version":"8.0.11"}]}}"#;
        pack(&exe, config)
    })
}

#[cfg(target_os = "linux")]
fn pack(exe: &[u8], config: &[u8]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (name, body, mode) in [
        ("Prowlarr/Prowlarr", exe, 0o755),
        ("Prowlarr/Prowlarr.runtimeconfig.json", config, 0o644),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        tar.append_data(&mut h, name, body)
            .unwrap_or_else(|e| panic!("{e}"));
    }
    tar.into_inner()
        .and_then(|gz| gz.finish())
        .unwrap_or_else(|e| panic!("{e}"))
}

#[cfg(windows)]
fn pack(exe: &[u8], config: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    for (name, body) in [
        ("Prowlarr/Prowlarr.exe", exe),
        ("Prowlarr/Prowlarr.runtimeconfig.json", config),
    ] {
        zip.start_file(name, opts).unwrap_or_else(|e| panic!("{e}"));
        zip.write_all(body).unwrap_or_else(|e| panic!("{e}"));
    }
    zip.finish().unwrap_or_else(|e| panic!("{e}")).into_inner()
}

/// Where the pin says the artifact is, under Prowlarr's release folder.
fn artifact_url() -> String {
    let file = if cfg!(windows) {
        "windows-core-x64.zip"
    } else if cfg!(target_arch = "aarch64") {
        "linux-core-arm64.tar.gz"
    } else {
        "linux-core-x64.tar.gz"
    };
    format!(
        "https://github.com/Prowlarr/Prowlarr/releases/download/v{VERSION}/Prowlarr.master.{VERSION}.{file}"
    )
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha_of(b: &[u8]) -> Sha256 {
    Sha256::parse_hex(&hex(&sha2::Sha256::digest(b))).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The test catalogue's text, as JSON.
fn template() -> serde_json::Value {
    serde_json::from_str(TEST_CATALOGUE).unwrap_or_else(|e| panic!("{e}"))
}

/// The test catalogue with Prowlarr's build for this platform pinned to
/// [`artifact`].
fn test_catalogue() -> &'static Catalogue {
    static C: OnceLock<&'static Catalogue> = OnceLock::new();
    C.get_or_init(|| {
        let c = Catalogue::from_json(&test_text()).unwrap_or_else(|e| panic!("{e:?}"));
        &*Box::leak(Box::new(c))
    })
}

fn platform_name(p: catalogue::Platform) -> &'static str {
    match p {
        catalogue::Platform::Linux => "linux",
        _ => "windows",
    }
}

fn token() -> InstallToken {
    InstallToken {
        id: prowlarr(),
        version: Version::parse(VERSION).unwrap_or_else(|e| panic!("{e:?}")),
        sha256: sha_of(artifact()),
    }
}

// ─── The manager's world ────────────────────────────────────────────────

/// Serves `body` at the artifact's URL, in 1 MiB chunks; nothing else.
struct Net {
    body: Arc<Vec<u8>>,
    asked: AtomicUsize,
}

impl Net {
    fn serving(body: Vec<u8>) -> Arc<Net> {
        Arc::new(Net {
            body: Arc::new(body),
            asked: AtomicUsize::new(0),
        })
    }
}

struct Body(Arc<Vec<u8>>, usize);

impl Chunks for Body {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>> {
        let end = (self.1 + (1 << 20)).min(self.0.len());
        let chunk = (self.1 < end).then(|| self.0[self.1..end].to_vec());
        self.1 = end;
        Box::pin(async move { Ok(chunk) })
    }
}

impl Source for Net {
    fn open<'a>(&'a self, u: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let found = u.as_str() == artifact_url();
        let body = self.body.clone();
        Box::pin(async move {
            Ok(Opened {
                status: if found { 200 } else { 404 },
                location: None,
                body: Box::new(Body(if found { body } else { Arc::default() }, 0)),
            })
        })
    }
}

/// Credential rows in `settings`.
#[derive(Default)]
struct Memory(Mutex<HashMap<String, SealedSecret>>);

impl Rows for Memory {
    fn put(&self, row: &SealedRow) -> Result<(), RowError> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(row.key.clone(), row.value.clone());
        Ok(())
    }
    fn get(&self, key: &str) -> Result<Option<SealedSecret>, RowError> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned())
    }
    fn forget(&self, id: &CompanionId) -> Result<(), RowError> {
        let prefix = format!("companions.{id}.secret.");
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|k, _| !k.starts_with(&prefix));
        Ok(())
    }
}

impl Memory {
    fn rows(&self) -> Vec<(String, SealedSecret)> {
        let mut r: Vec<_> = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        r.sort_by(|a, b| a.0.cmp(&b.0));
        r
    }
}

/// What the manager launched, kept for the test to run itself.
struct Launched {
    executable: PathBuf,
    install: PathBuf,
    args: Vec<OsString>,
    target: Target,
    secrets: Vec<Secret>,
}

/// The manager's `Run`: records the launch and stands in for the program
/// (see the module docs). `refuse` makes every start fail with that code,
/// as `Supervised` refuses a companion with no route.
#[derive(Default)]
struct Recorder {
    launched: Mutex<Option<Launched>>,
    starts: AtomicUsize,
    refuse: Mutex<Option<&'static str>>,
    status: Mutex<Option<watch::Sender<Status>>>,
}

impl Recorder {
    fn launched(&self) -> Launched {
        self.launched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| panic!("nothing was launched"))
    }
}

impl Run for Recorder {
    fn start(&self, l: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        if let Some(code) = *self.refuse.lock().unwrap_or_else(|e| e.into_inner()) {
            return Box::pin(async move { Err(code) });
        }
        self.starts.fetch_add(1, Ordering::SeqCst);
        let up = Status {
            state: State::Running,
            restarts: 0,
            last_exit: None,
            run: 0,
            up: true,
            pid: None,
        };
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = Some(watch::channel(up).0);
        *self.launched.lock().unwrap_or_else(|e| e.into_inner()) = Some(Launched {
            executable: l.executable,
            install: l.install,
            args: l.args,
            target: l.target,
            secrets: l.secrets,
        });
        Box::pin(async { Ok(()) })
    }
    fn healthy<'a>(&'a self, _: &'a CompanionId, _: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
    fn stop<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, ()> {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).take();
        Box::pin(async {})
    }
    fn status(&self, _: &CompanionId) -> Option<watch::Receiver<Status>> {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.subscribe())
    }
    fn ready_run(&self, id: &CompanionId) -> Option<u64> {
        self.status(id).map(|s| s.borrow().run)
    }
    fn recheck<'a>(&'a self, _: &'a CompanionId, _: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
    fn refusal(&self, _: &CompanionId) -> Option<&'static str> {
        None
    }
}

/// CMP-8b's configuration, which needs the companion's own API: accepted.
#[derive(Default)]
struct Api {
    configured: AtomicBool,
}

impl ApiConfigure for Api {
    fn configure<'a>(
        &'a self,
        _: &'a CompanionId,
        _: &'a Target,
        _: InstallReceipt,
        _: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        self.configured.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn hands_over(&self, _: &CompanionId) -> bool {
        false
    }
}

struct World {
    data: tempfile::TempDir,
    secrets: Arc<SecretStore>,
    rows: Arc<Memory>,
    run: Arc<Recorder>,
    api: Arc<Api>,
}

impl World {
    fn new() -> World {
        World {
            data: tempfile::tempdir().unwrap_or_else(|e| panic!("{e}")),
            secrets: Arc::new(SecretStore::from_key(&[7u8; 32])),
            rows: Arc::default(),
            run: Arc::default(),
            api: Arc::default(),
        }
    }

    fn data(&self) -> &Path {
        self.data.path()
    }

    fn manager(&self, net: Arc<Net>) -> Manager {
        Manager::open(Parts {
            data: self.data().to_path_buf(),
            host: Host::this(false),
            catalogue: test_catalogue(),
            downloader: Downloader::any(net),
            secrets: self.secrets.clone(),
            rows: self.rows.clone(),
            run: self.run.clone(),
            api: self.api.clone(),
        })
        .unwrap_or_else(|e| panic!("{e:?}"))
    }

    /// `<data>/companions/prowlarr`.
    fn folder(&self) -> PathBuf {
        self.data().join("companions").join("prowlarr")
    }

    /// The API key the install generated, opened from its row.
    fn api_key(&self) -> Secret {
        let key = CredentialKey::new(prowlarr(), Field::ApiKey);
        let sealed = self
            .rows
            .get(&key.row_key())
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("no API key row"));
        credentials::open(&self.secrets, &key, &sealed).unwrap_or_else(|e| panic!("{e:?}"))
    }

    /// Installs the test companion, as you'd confirm it.
    async fn install(&self) -> (Manager, Launched) {
        let m = self.manager(Net::serving(artifact().to_vec()));
        let got = m.install(&token()).await;
        assert_eq!(
            got.as_ref().map(Version::as_str),
            Ok(VERSION),
            "the install failed"
        );
        (m, self.run.launched())
    }
}

/// Every file and folder under `dir`, relative, sorted.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            out.push(
                p.strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            if p.is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

// ─── Running the installed program ──────────────────────────────────────

/// The real sandbox: on Linux the launcher is this binary.
fn sandbox() -> Sandbox {
    Sandbox::new(
        Limits::default(),
        Launcher {
            exe: me(),
            args: args_for("fake_launcher"),
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Whether something answers HTTP on 127.0.0.1:`port`.
fn answers(port: u16) -> bool {
    let Ok(mut s) = std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], port).into(),
        Duration::from_secs(2),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(b"GET /ping HTTP/1.1\r\nHost: x\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 64];
    matches!(s.read(&mut buf), Ok(n) if buf[..n].starts_with(b"HTTP/1.1 200"))
}

/// Waits for `f`, polled against [`DEADLINE`].
async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + DEADLINE;
    while !f() {
        assert!(Instant::now() < end, "waited {DEADLINE:?} for {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_state(rx: &mut watch::Receiver<Status>, want: State) {
    let ok = matches!(
        tokio::time::timeout(DEADLINE, rx.wait_for(|s| s.state == want)).await,
        Ok(Ok(_))
    );
    assert!(ok, "waited for {want:?}; last saw {:?}", *rx.borrow());
}

async fn line(ring: &LogRing, prefix: &str) -> String {
    let mut found = None;
    until(&format!("a {prefix:?} line"), || {
        found = ring.lines().into_iter().find(|l| l.starts_with(prefix));
        found.is_some()
    })
    .await;
    found.unwrap_or_default()
}

/// The pid its record names, once the supervisor has written it.
fn recorded_pid(data: &Path) -> Option<u32> {
    let rec = std::fs::read(pidfile::run_dir(data).join("prowlarr.json")).ok()?;
    let rec: pidfile::Record = serde_json::from_slice(&rec).ok()?;
    Some(rec.pid)
}

/// Starts the installed program `l` under the real supervisor, sandbox
/// and route gate, on `route`, launched with what `launch_env` gives that
/// route (as `Supervised::start` does), answering on `port`.
async fn run_installed(
    data: &Path,
    l: &Launched,
    route: Route,
    proxy: Option<Socks5Proxy>,
    port: u16,
    extra: &[(&str, OsString)],
) -> (Supervisor, Sandbox, Result<egress::Gated, GateError>) {
    let sb = sandbox();
    let mut sup = Supervisor::new(data.to_owned(), sb.hook(), Timings::default())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let pre = Prelaunch {
        route: route.clone(),
        proxy,
    };
    let mut env: Vec<(String, OsString)> = launch_env(&pre)
        .unwrap_or_else(|| panic!("the route has no launch form"))
        .into_iter()
        .map(|(k, v)| (k.as_str().to_owned(), v))
        .collect();
    env.push(("FAKE_MODE".into(), "serve".into()));
    env.push(("FAKE_PORT".into(), port.to_string().into()));
    env.extend(extra.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
    let env: Vec<(&str, OsString)> = env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let (spawn, _) = sb
        .prepare(
            spec(data, l.executable.clone(), &env),
            &Confinement {
                install: l.install.clone(),
                downloads: None,
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let c = companion(spawn, l.secrets.clone());
    let gated = egress::add(&mut sup, c, Follows::of(&prowlarr()), route);
    (sup, sb, gated)
}

fn route(kind: RouteKind, state: RouteState) -> (watch::Sender<RouteState>, Route) {
    let (tx, rx) = watch::channel(state);
    (tx, Route { kind, state: rx })
}

fn proxy() -> Socks5Proxy {
    Socks5Proxy::new(PROXY.parse().unwrap_or_else(|e| panic!("{e}")))
        .unwrap_or_else(|e| panic!("{e:?}"))
}

// ─── Captured log events ────────────────────────────────────────────────

/// Every log event this process emits, rendered with every kind shown.
struct Capture(Mutex<Vec<String>>);

impl lodestar_log::Sink for Capture {
    fn record(&self, r: &lodestar_log::Record<'_>) {
        let line = format!(
            "{} {}",
            r.event.id,
            r.text(lodestar_log::Mask(u32::MAX), None)
        );
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(line);
    }
}

fn captured() -> &'static Capture {
    static C: OnceLock<&'static Capture> = OnceLock::new();
    C.get_or_init(|| {
        let c: &'static Capture = Box::leak(Box::new(Capture(Mutex::new(Vec::new()))));
        lodestar_log::set_sink(c).unwrap_or_else(|e| panic!("{e}"));
        // Every event of this crate on, whatever its default.
        for e in lodestar_companions::log_events::CATALOGUE.events {
            e.set_on(true);
        }
        c
    })
}

// ─── 1. Checksum and signature refusal ──────────────────────────────────

/// A download whose bytes aren't the pin's (one byte changed, cut short,
/// one byte more) is refused at the fetch: nothing is extracted, no
/// credential is made, no file is left, nothing is started, and the
/// snapshot dependants read stays empty. A confirmation naming other
/// bytes is refused before anything is downloaded.
#[tokio::test(flavor = "multi_thread")]
async fn a_download_that_fails_its_checksum_is_refused_and_nothing_runs() {
    let _g = SERIAL.lock().await;
    let good = artifact().to_vec();
    let mut flipped = good.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x01;
    let mut longer = good.clone();
    longer.push(0);
    for (what, body) in [
        ("one byte changed", flipped),
        ("cut short", good[..good.len() - 1].to_vec()),
        ("one byte more", longer),
    ] {
        let w = World::new();
        let m = w.manager(Net::serving(body));
        let got = m.install(&token()).await;
        assert!(
            matches!(
                got,
                Err(ManagerError::Step { code, .. })
                    if code == FetchError::Mismatch.code() || code == FetchError::TooLarge.code()
            ),
            "{what}: {got:?}"
        );
        assert_eq!(w.run.starts.load(Ordering::SeqCst), 0, "{what}: it ran");
        assert!(w.rows.rows().is_empty(), "{what}: a credential was made");
        assert!(
            !w.folder().join(VERSION).exists(),
            "{what}: something was installed"
        );
        assert!(
            tree(&fetch::folder(w.data())).is_empty(),
            "{what}: the download was left behind"
        );
        assert!(m.snapshot().borrow().companions.is_empty(), "{what}");
    }
    // Bytes other than the ones you were shown.
    let w = World::new();
    let net = Net::serving(artifact().to_vec());
    let m = w.manager(net.clone());
    let mut wrong = token();
    wrong.sha256 = sha_of(b"other bytes");
    assert!(m.install(&wrong).await.is_err());
    assert_eq!(
        net.asked.load(Ordering::SeqCst),
        0,
        "downloaded unconfirmed"
    );
    assert_eq!(w.run.starts.load(Ordering::SeqCst), 0);
}

/// Upstream signatures (owner cq2): the pin is the run-time gate for every
/// artifact, and the only upstream signature checked at run time,
/// minisign, verifies with the pinned key only. A catalogue can't claim a
/// signature kind the binary doesn't check, and an entry that breaks a
/// rule (a host off the list, a malformed hash) is refused whole.
#[tokio::test]
async fn a_signature_that_fails_is_refused() {
    let _g = SERIAL.lock().await;
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let cat = test_catalogue();
    let entry = cat.get("prowlarr").unwrap_or_else(|| panic!("no entry"));
    let (platform, arch) = Host::this(false)
        .platform
        .unwrap_or_else(|| panic!("platform"));
    let pin = entry
        .artifact(platform, arch)
        .and_then(|a| a.pin().ok())
        .unwrap_or_else(|| panic!("not pinned"));
    let net = Net::serving(artifact().to_vec());
    let mut file = fetch::fetch_from(&*net, data.path(), entry, pin, &mut |_| {})
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
    let key = TestKey::new(1);
    assert_eq!(
        verify::minisign(&mut file, &key.public(), &key.sign(artifact())),
        Ok(())
    );
    let mut other = artifact()[..4096].to_vec();
    other[0] ^= 1;
    assert_eq!(
        verify::minisign(&mut file, &key.public(), &key.sign(&other)),
        Err(VerifyError::BadSignature)
    );
    assert_eq!(
        verify::minisign(&mut file, &TestKey::new(2).public(), &key.sign(artifact())),
        Err(VerifyError::WrongKey)
    );
    assert_eq!(
        verify::minisign(&mut file, &key.public(), "not a signature"),
        Err(VerifyError::SignatureFile)
    );
    file.discard().unwrap_or_else(|e| panic!("{e}"));

    // Every pin in the shipped catalogue is gated by its hash alone or was
    // checked when it was made; nothing claims a check this build skips.
    let shipped = lodestar_companions::builtin().unwrap_or_else(|e| panic!("{e:?}"));
    for e in shipped.entries() {
        for p in [catalogue::Platform::Linux, catalogue::Platform::Windows] {
            for a in [catalogue::Arch::X86_64, catalogue::Arch::Aarch64] {
                if let Some(pin) = e.artifact(p, a).and_then(|x| x.pin().ok()) {
                    assert_eq!(verify::upstream(pin), verify::Upstream::PinOnly);
                }
            }
        }
    }
    // Entries that break a rule are refused, and with them the catalogue.
    let edit = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = serde_json::from_str::<serde_json::Value>(&test_text()).unwrap_or_default();
        for e in v["entries"].as_array_mut().into_iter().flatten() {
            if e["id"] == "prowlarr" {
                f(e);
            }
        }
        Catalogue::from_json(&v.to_string())
    };
    fn pinned(e: &mut serde_json::Value) -> &mut serde_json::Value {
        let arts = e["native"]["artifacts"]
            .as_array_mut()
            .unwrap_or_else(|| panic!("arts"));
        arts.iter_mut()
            .find(|a| !a["pin"].is_null())
            .map(|a| &mut a["pin"])
            .unwrap_or_else(|| panic!("no pin"))
    }
    assert!(edit(&|_| {}).is_ok(), "the unedited catalogue must load");
    assert!(
        edit(&|e| pinned(e)["upstream_sig"] = serde_json::json!({"kind": "minisign", "key": "x"}))
            .is_err(),
        "a signature kind the build doesn't check"
    );
    assert!(
        edit(&|e| pinned(e)["upstream_sig"] = serde_json::json!({"kind": "none", "extra": 1}))
            .is_err(),
        "an unknown field"
    );
    assert!(
        edit(&|e| pinned(e)["sha256"] = serde_json::json!("00")).is_err(),
        "a malformed hash"
    );
    assert!(
        edit(&|e| {
            pinned(e)["url"] =
                serde_json::json!(artifact_url().replace("github.com", "evil.example"))
        })
        .is_err(),
        "a URL off the entry's hosts"
    );
    assert!(
        edit(&|e| e["hosts"] = serde_json::json!(["github.com", "evil.example"])).is_err(),
        "a host off the download list"
    );
}

/// The pinned test catalogue's text.
fn test_text() -> String {
    let mut v = template();
    let a = artifact();
    let (platform, arch) = Host::this(false)
        .platform
        .unwrap_or_else(|| panic!("platform"));
    let arch = if matches!(arch, catalogue::Arch::Aarch64) {
        "aarch64"
    } else {
        "x86_64"
    };
    for e in v["entries"].as_array_mut().into_iter().flatten() {
        if e["id"] != "prowlarr" {
            continue;
        }
        for art in e["native"]["artifacts"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            if art["platform"] == platform_name(platform) && art["arch"] == arch {
                art["pin"] = serde_json::json!({
                    "version": VERSION, "url": artifact_url(), "size": a.len(),
                    "sha256": hex(&sha2::Sha256::digest(a)), "upstream_sig": {"kind": "none"},
                });
            }
        }
    }
    v.to_string()
}

// ─── 2. Localhost binding ───────────────────────────────────────────────

fn port_name(s: &str) -> PortName {
    PortName::parse(s).unwrap_or_else(|e| panic!("{e:?}"))
}

/// A container service for `id`, with the folders the golden tests use.
fn service(route: ComposeRoute, publish: Vec<Publish>) -> ServiceInput {
    let path = |s: &str| {
        lodestar_companions::inputs::HostPath::parse(s).unwrap_or_else(|e| panic!("{e:?}"))
    };
    ServiceInput {
        puid: Puid::new(99).unwrap_or_else(|e| panic!("{e:?}")),
        pgid: Pgid::new(100).unwrap_or_else(|e| panic!("{e:?}")),
        network: NetworkName::parse("lodestar").unwrap_or_else(|e| panic!("{e:?}")),
        route,
        paths: Paths {
            config: Some(path("/mnt/user/appdata/companion")),
            downloads: None,
        },
        publish,
        data: DataFolders::new("/mnt/cache/appdata/lodestar", None, None)
            .unwrap_or_else(|e| panic!("{e:?}")),
        taken: Vec::new(),
        gluetun_paths: None,
    }
}

fn publish(port: &str, bind: Bind, host_port: u16) -> Publish {
    Publish {
        port: port_name(port),
        bind,
        host_port: PortNumber::new(host_port).unwrap_or_else(|e| panic!("{e:?}")),
    }
}

/// A managed companion is reached on 127.0.0.1 at its catalogue port, and
/// its config file binds it there before its first start, over whatever
/// an existing file said. In a container every port stays on the
/// internal network unless you publish it: on 127.0.0.1, or on every
/// address only when you ask for that. Only a download client has a peer
/// port, and a peer port is never put on loopback.
#[tokio::test(flavor = "multi_thread")]
async fn managed_companions_bind_to_loopback() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let (_m, l) = w.install().await;
    let webui = test_catalogue()
        .get("prowlarr")
        .and_then(|e| e.port("webui"))
        .map(|p| p.number)
        .unwrap_or_else(|| panic!("no webui port"));
    assert_eq!(l.target, Target::Loopback(webui));
    let home = l
        .args
        .iter()
        .find_map(|a| a.to_str().and_then(|s| s.strip_prefix("-data=")))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("no -data= in {:?}", l.args));
    let conf = std::fs::read_to_string(home.join("config.xml")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(conf.matches("<BindAddress>").count(), 1, "{conf}");
    assert!(
        conf.contains("<BindAddress>127.0.0.1</BindAddress>"),
        "{conf}"
    );

    // Over files that bound every address.
    let key = Secret::new("0123456789abcdef0123456789abcdef");
    let p = config_files::prowlarr_config(
        Some("<Config><BindAddress>*</BindAddress><Port>9696</Port></Config>"),
        &key,
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(p.expose().contains("<BindAddress>127.0.0.1</BindAddress>"));
    assert!(!p.expose().contains("<BindAddress>*"));
    let q = config_files::qbittorrent_conf(
        Some("[Preferences]\nWebUI\\Address=0.0.0.0\nWebUI\\Port=8080\n"),
        &Username::parse("lodestar").unwrap_or_else(|e| panic!("{e:?}")),
        &PasswordHash::new(&Secret::new("pw")).unwrap_or_else(|e| panic!("{e:?}")),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    let binds: Vec<&str> = q
        .expose()
        .lines()
        .filter(|l| l.contains("WebUI\\Address"))
        .collect();
    assert_eq!(binds, ["WebUI\\Address=127.0.0.1"], "{}", q.expose());
    assert_eq!(config_files::BIND_ADDRESS, "127.0.0.1");

    // Only a download client has a published peer port, in both
    // catalogues.
    let shipped = lodestar_companions::builtin().unwrap_or_else(|e| panic!("{e:?}"));
    for c in [shipped, test_catalogue()] {
        for e in c.entries() {
            for p in &e.ports {
                if p.exposure == Exposure::P2p {
                    assert!(
                        e.needs(Need::DownloadClient),
                        "{} {}",
                        e.id,
                        p.name.as_str()
                    );
                }
            }
        }
    }

    // Containers.
    let cat = test_catalogue();
    let text = |id: &CompanionId, input: &ServiceInput| {
        compose::compose(cat, id, input).unwrap_or_else(|e| panic!("{e:?}"))
    };
    let internal = text(&prowlarr(), &service(ComposeRoute::Direct, Vec::new()));
    assert!(!internal.contains("\"ports\""), "{internal}");
    let local = text(
        &prowlarr(),
        &service(
            ComposeRoute::Direct,
            vec![publish("webui", Bind::Loopback, 9696)],
        ),
    );
    assert_eq!(
        local.matches("\"host_ip\": \"127.0.0.1\"").count(),
        1,
        "{local}"
    );
    assert_eq!(local.matches("\"published\"").count(), 1, "{local}");
    let lan = text(
        &prowlarr(),
        &service(
            ComposeRoute::Direct,
            vec![publish("webui", Bind::Lan, 9696)],
        ),
    );
    assert!(!lan.contains("\"host_ip\": \"127.0.0.1\""), "{lan}");
    assert_eq!(lan.matches("\"published\"").count(), 1, "{lan}");
    assert_eq!(
        compose::compose(
            cat,
            &qbt(),
            &ServiceInput {
                paths: Paths {
                    config: service(ComposeRoute::Direct, Vec::new()).paths.config,
                    downloads: Some(
                        lodestar_companions::inputs::HostPath::parse("/mnt/user/downloads")
                            .unwrap_or_else(|e| panic!("{e:?}")),
                    ),
                },
                ..service(
                    ComposeRoute::Direct,
                    vec![publish("p2p", Bind::Loopback, 6881)]
                )
            },
        )
        .err(),
        Some(TemplateError::P2pLoopback)
    );
    assert_eq!(
        compose::compose(
            cat,
            &prowlarr(),
            &service(ComposeRoute::Direct, vec![publish("p2p", Bind::Lan, 6881)]),
        )
        .err(),
        Some(TemplateError::UnknownPort),
        "a port the entry doesn't have"
    );
}

// ─── 3. Egress and the kill switch ──────────────────────────────────────

/// Natively a companion is launched on its route, never on another: direct
/// adds nothing; SOCKS5 (the VPN's proxy) puts every proxy variable in its
/// launch; a route kind with no launch form, or one the companion can't
/// follow, is refused, never run direct instead. In a container the VPN
/// path is gluetun: all of the companion's traffic goes through it, and
/// gluetun's post-rules keep it in the tunnel when the tunnel drops.
#[test]
fn a_companion_follows_its_route_and_no_other() {
    let (_tx, rx) = watch::channel(RouteState::Up);
    let pre = |kind, proxy| Prelaunch {
        route: Route {
            kind,
            state: rx.clone(),
        },
        proxy,
    };
    let names = |p: &Prelaunch| {
        launch_env(p).map(|e| {
            e.into_iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_string_lossy().into_owned()))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(
        names(&pre(RouteKind::Direct, Some(proxy()))),
        Some(Vec::new())
    );
    let socks = names(&pre(RouteKind::Socks5, Some(proxy()))).unwrap_or_default();
    let url = format!("socks5://{PROXY}");
    for var in ["ALL_PROXY", "HTTP_PROXY", "HTTPS_PROXY"] {
        assert!(
            socks.contains(&(var.to_owned(), url.clone())),
            "{var}: {socks:?}"
        );
    }
    let bypass: Vec<&str> = socks
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("no_proxy"))
        .map(|(_, v)| v.as_str())
        .collect();
    assert!(!bypass.is_empty());
    for v in bypass {
        assert!(
            v.split(',')
                .all(|h| ["localhost", "127.0.0.1", "::1", "[::1]"].contains(&h)),
            "only loopback bypasses the proxy: {v}"
        );
    }
    // A SOCKS5 route without its proxy, and routes with no launch form.
    for (kind, proxy) in [
        (RouteKind::Socks5, None),
        (RouteKind::Interface, None),
        (RouteKind::BindAddress, None),
    ] {
        assert_eq!(names(&pre(kind, proxy)), None, "{kind:?}");
    }
    // Who can follow what; never direct instead.
    assert_eq!(
        egress::decide(Follows::of(&prowlarr()), RouteKind::Socks5),
        Ok(Decision::Follow(RouteKind::Socks5))
    );
    assert_eq!(
        egress::decide(Follows::of(&qbt()), RouteKind::Socks5),
        Err(GateError::CantFollow(RouteKind::Socks5))
    );
    assert_eq!(
        egress::decide(Follows::of(&prowlarr()), RouteKind::Direct),
        Ok(Decision::Direct)
    );
    // Containers.
    let cat = test_catalogue();
    let vpn = compose::compose(
        cat,
        &prowlarr(),
        &service(ComposeRoute::Gluetun, Vec::new()),
    )
    .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        vpn.contains("\"network_mode\": \"service:gluetun\""),
        "{vpn}"
    );
    assert!(vpn.contains("\"gluetun-post-rules\""), "{vpn}");
    let direct = compose::compose(cat, &prowlarr(), &service(ComposeRoute::Direct, Vec::new()))
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(!direct.contains("network_mode"), "{direct}");
}

/// The kill switch, against the installed program: on the VPN's SOCKS5
/// route it isn't started while the route is down, starts on the proxy
/// once it's up, is stopped the moment it goes down, and is held when its
/// route can't be read any more.
#[tokio::test(flavor = "multi_thread")]
async fn the_kill_switch_stops_a_companion_whose_vpn_route_is_down() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let (_m, l) = w.install().await;
    assert!(
        reads_proxy_env(&l.executable),
        "the test companion takes no proxy"
    );
    let port = free_port();
    let (tx, r) = route(
        RouteKind::Socks5,
        RouteState::Down(DownReason::ProxyUnreachable),
    );
    let (sup, _sb, gated) = run_installed(w.data(), &l, r, Some(proxy()), port, &[]).await;
    let gated = gated.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(gated.decision, Decision::Follow(RouteKind::Socks5));
    let id = prowlarr();
    let mut st = sup.status(&id).unwrap_or_else(|| panic!("not supervised"));
    assert_eq!(st.borrow().state, State::Held);
    assert_eq!(recorded_pid(w.data()), None, "started with the tunnel down");
    assert!(!answers(port));

    let _ = tx.send(RouteState::Up);
    wait_state(&mut st, State::Running).await;
    until("it answers", || answers(port)).await;
    let ring = sup.logs(&id).unwrap_or_else(|| panic!("no logs"));
    assert_eq!(
        line(&ring, "PROXY ").await,
        format!("PROXY socks5://{PROXY}")
    );
    assert!(recorded_pid(w.data()).is_some());

    let _ = tx.send(RouteState::Down(DownReason::ProxyUnreachable));
    wait_state(&mut st, State::Held).await;
    until("it stopped", || {
        !answers(port) && recorded_pid(w.data()).is_none()
    })
    .await;

    // Up again, then nothing to read the route from: down.
    let _ = tx.send(RouteState::Up);
    wait_state(&mut st, State::Running).await;
    until("it answers again", || answers(port)).await;
    drop(tx);
    wait_state(&mut st, State::Held).await;
    until("it stopped again", || !answers(port)).await;
    drop(gated);
    sup.shutdown().await;
}

// ─── 4. Needs setup ─────────────────────────────────────────────────────

/// Whether a requirement is met by what the manager's snapshot holds: a
/// healthy companion that meets it. The pipeline's own reading of the
/// snapshot (`crates/pipeline/src/requires.rs`, its `tests/requires.rs`)
/// holds the plugin while this is false.
fn met(req: &requires::Requirement, snap: &lodestar_companions::manager::Snapshot) -> bool {
    req.companions(Some(test_catalogue())).iter().any(|id| {
        snap.companions
            .iter()
            .any(|p| p.id.as_str() == *id && p.health == Health::Healthy)
    })
}

struct NoHttp;

impl GuardedHttp for NoHttp {
    fn send<'a>(
        &'a self,
        _: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async { Err(AdapterError::Refused("gate")) })
    }
}

struct OneRoute(Option<Prelaunch>);

impl Routes for OneRoute {
    fn route<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Option<Prelaunch>> {
        let r = self.0.clone();
        Box::pin(async move { r })
    }
}

/// The Prowlarr source needs Prowlarr. While it's missing the requirement
/// is unmet and Prowlarr is the one suggested ("Needs Prowlarr · Set
/// up"). A companion whose route isn't set up is never started, by the
/// real runner, and an install that can't start it leaves nothing, so the
/// source still waits. Installed and healthy it's met; removed, unmet
/// again.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_or_unset_companion_needs_setup_and_is_not_started() {
    let _g = SERIAL.lock().await;
    let req = requires::requirement("prowlarr").unwrap_or_else(|| panic!("no requirement"));
    assert_eq!(req.suggest, "prowlarr");
    let w = World::new();
    let m = w.manager(Net::serving(artifact().to_vec()));
    assert!(!met(&req, &m.snapshot().borrow()));

    // The real runner, with no route and with a SOCKS5 route whose proxy
    // isn't known: refused before anything is spawned.
    let launched = || Launch {
        id: prowlarr(),
        executable: w.folder().join(VERSION).join("Prowlarr").join("Prowlarr"),
        install: w.folder().join(VERSION),
        args: Vec::new(),
        target: Target::Loopback(PortNumber::new(free_port()).unwrap_or_else(|e| panic!("{e:?}"))),
        secrets: Vec::new(),
    };
    let (_tx, rx) = watch::channel(RouteState::Up);
    for (routes, code) in [
        (OneRoute(None), "no_route"),
        (
            OneRoute(Some(Prelaunch {
                route: Route {
                    kind: RouteKind::Socks5,
                    state: rx.clone(),
                },
                proxy: None,
            })),
            "route_not_expressible",
        ),
    ] {
        let sb = sandbox();
        let sup = Supervisor::new(w.data().to_owned(), sb.hook(), Timings::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let s = Supervised::new(
            w.data().to_owned(),
            sup,
            sb,
            Arc::new(NoHttp),
            Arc::new(routes),
            None,
        );
        assert_eq!(s.start(launched()).await, Err(code));
        assert!(s.status(&prowlarr()).is_none(), "{code}: supervised");
        assert_eq!(recorded_pid(w.data()), None, "{code}: spawned");
    }

    // An install whose start is refused that way is undone.
    *w.run.refuse.lock().unwrap_or_else(|e| e.into_inner()) = Some("no_route");
    let got = m.install(&token()).await;
    assert!(
        matches!(
            got,
            Err(ManagerError::Step {
                code: "no_route",
                ..
            })
        ),
        "{got:?}"
    );
    assert!(!w.folder().join(VERSION).exists(), "left installed");
    assert!(w.rows.rows().is_empty(), "left its credential");
    assert!(!met(&req, &m.snapshot().borrow()));

    *w.run.refuse.lock().unwrap_or_else(|e| e.into_inner()) = None;
    m.install(&token())
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(met(&req, &m.snapshot().borrow()));
    m.remove(&prowlarr())
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(!met(&req, &m.snapshot().borrow()));
}

// ─── 5. Credentials ─────────────────────────────────────────────────────

fn holds(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// The API key the install generated is sealed in its `settings` row,
/// under its own context, and appears in plain text only in the
/// companion's own config file (0600 on Unix), which the companion needs.
/// It never reaches a log event, the journal or any other file, the
/// companion's log ring (when the companion prints it), a compose file,
/// or the `Debug` of anything the API reads.
#[tokio::test(flavor = "multi_thread")]
async fn credentials_are_sealed_and_never_shown() {
    let _g = SERIAL.lock().await;
    let log = captured();
    let w = World::new();
    let (m, l) = w.install().await;
    let key = w.api_key();
    let k = key.expose().to_owned();
    assert_eq!(k.len(), 32);
    assert!(k.bytes().all(|b| b.is_ascii_hexdigit()));

    // Sealed, and only under its own row's context.
    let rows = w.rows.rows();
    let own = CredentialKey::new(prowlarr(), Field::ApiKey);
    assert_eq!(
        rows.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        [own.row_key().as_str()]
    );
    for (name, sealed) in &rows {
        assert!(
            !holds(sealed.as_bytes(), k.as_bytes()),
            "{name} holds it in clear"
        );
        assert!(
            credentials::open(
                &w.secrets,
                &CredentialKey::new(qbt(), Field::WebUiPassword),
                sealed
            )
            .is_err(),
            "{name} opens as another companion's credential"
        );
    }

    // On disk: only the companion's own config file.
    let install = w.folder().join(VERSION);
    let mut found = Vec::new();
    for rel in tree(w.data()) {
        let p = w.data().join(&rel);
        if !p.is_file() || p.starts_with(&install) {
            // The installed program is the archive's bytes, made before
            // the key existed.
            continue;
        }
        if holds(&std::fs::read(&p).unwrap_or_default(), k.as_bytes()) {
            found.push(rel);
        }
    }
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].ends_with("/config.xml"), "{found:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(w.data().join(&found[0]))
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0);
        assert_eq!(mode, 0o600, "config.xml is {mode:o}");
    }

    // The log events the install wrote.
    let lines = log.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert!(
        lines.iter().any(|l| l.starts_with("companions.installed")),
        "the capture saw no install: {lines:?}"
    );
    for l in &lines {
        assert!(!l.contains(&k), "a log event carries it: {l}");
    }

    // What the API and the CLI read.
    for shown in [
        format!("{:?}", m.removal(&prowlarr())),
        format!("{:?}", *m.snapshot().borrow()),
        format!("{:?}", l.secrets),
        format!("{:?}", w.rows.rows()),
        compose::compose(
            test_catalogue(),
            &prowlarr(),
            &service(ComposeRoute::Gluetun, Vec::new()),
        )
        .unwrap_or_else(|e| panic!("{e:?}")),
    ] {
        assert!(!shown.contains(&k), "{shown}");
    }

    // The companion prints its own key: its log ring never holds it.
    let conf = found[0].clone();
    let port = free_port();
    let (_tx, r) = route(RouteKind::Direct, RouteState::Up);
    let (sup, _sb, gated) = run_installed(
        w.data(),
        &l,
        r,
        None,
        port,
        &[("FAKE_CONFIG", w.data().join(&conf).into_os_string())],
    )
    .await;
    let gated = gated.unwrap_or_else(|e| panic!("{e}"));
    let ring = sup.logs(&prowlarr()).unwrap_or_else(|| panic!("no logs"));
    let shown = line(&ring, "KEY ").await;
    assert!(!shown.contains(&k), "{shown}");
    assert_ne!(
        shown, "KEY unreadable",
        "the companion couldn't read its config"
    );
    assert_ne!(shown, "KEY ", "the companion printed nothing");
    for l in ring.lines() {
        assert!(!l.contains(&k), "{l}");
    }
    drop(gated);
    sup.shutdown().await;
}

// ─── 6. Orphans and uninstall ───────────────────────────────────────────

/// Whether `pid` is still the program recorded for it.
fn alive(pid: u32, exe: &Path) -> bool {
    pidfile::identity(pid).is_some_and(|(_, e)| {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
        canon(&e) == canon(exe)
    })
}

/// An earlier run's work child is killed outright, which leaves its
/// companion running. The next start's supervisor kills it before
/// anything starts. Then an uninstall removes exactly what it listed, and
/// nothing of the companion is left under the data folder.
#[tokio::test(flavor = "multi_thread")]
async fn an_orphan_is_killed_at_start_and_an_uninstall_leaves_nothing() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let (m, l) = w.install().await;
    let mut work = std::process::Command::new(me())
        .args(args_for("fake_companion"))
        .env("FAKE_MODE", "work")
        .env("FAKE_DATA", w.data())
        .env("FAKE_EXE", &l.executable)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    let mut pid = None;
    until("the earlier run's companion", || {
        pid = recorded_pid(w.data()).filter(|p| alive(*p, &l.executable));
        pid.is_some()
    })
    .await;
    let pid = pid.unwrap_or_default();
    let _ = work.kill();
    let _ = work.wait();
    assert!(
        alive(pid, &l.executable),
        "the companion died with its parent"
    );

    let sup = Supervisor::new(w.data().to_owned(), Arc::new(NoHook), Timings::default())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(sup.orphans.killed, 1, "{:?}", sup.orphans);
    until("the orphan is gone", || !alive(pid, &l.executable)).await;
    assert_eq!(recorded_pid(w.data()), None);
    sup.shutdown().await;

    let removal = m.removal(&prowlarr()).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(removal.folder, w.folder());
    // Every credential field it could have, whichever it has.
    assert!(
        removal
            .rows
            .contains(&CredentialKey::new(prowlarr(), Field::ApiKey).row_key())
    );
    assert!(
        removal
            .rows
            .iter()
            .all(|r| r.starts_with("companions.prowlarr.secret.")),
        "{:?}",
        removal.rows
    );
    assert_eq!(
        m.remove(&prowlarr())
            .await
            .unwrap_or_else(|e| panic!("{e:?}")),
        removal
    );
    assert!(!w.folder().exists());
    assert!(w.rows.rows().is_empty());
    assert!(m.snapshot().borrow().companions.is_empty());
    let left: Vec<String> = tree(w.data())
        .into_iter()
        .filter(|p| p.to_ascii_lowercase().contains("prowlarr"))
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

// ─── 7. Install smoke ───────────────────────────────────────────────────

/// The install smoke, on every platform CI runs: the offer shows what
/// will be installed; the install puts the archive's program, byte for
/// byte, in its version folder with a clean journal; the installed
/// program runs under the real supervisor and sandbox on the direct
/// route and answers on 127.0.0.1; and the removal takes it all away.
#[tokio::test(flavor = "multi_thread")]
async fn install_smoke_installs_checks_runs_and_removes_the_test_companion() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(Net::serving(artifact().to_vec()));
    let offer = m.offer(&prowlarr()).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(offer.version.as_str(), VERSION);
    assert_eq!(offer.sha256, sha_of(artifact()));
    assert_eq!(offer.size, artifact().len() as u64);
    assert_eq!(offer.licence, "GPL-3.0-only");
    assert_eq!(offer.replaces, None);
    m.install(&token())
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
    let l = w.run.launched();

    // Checked: what runs is the archive's program, in its version folder.
    assert_eq!(l.install, w.folder().join(VERSION));
    assert!(l.executable.starts_with(&l.install));
    let installed = std::fs::read(&l.executable).unwrap_or_else(|e| panic!("{e}"));
    let original = std::fs::read(me()).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        installed == original,
        "the installed program isn't the archive's"
    );
    let rec = lodestar_companions::state::load(&w.folder())
        .unwrap_or_else(|e| panic!("{e:?}"))
        .unwrap_or_else(|| panic!("no journal"));
    assert_eq!(rec.op, None, "the journal isn't clean");
    assert_eq!(rec.installed.as_ref().map(Version::as_str), Some(VERSION));
    assert!(w.api.configured.load(Ordering::SeqCst));
    let snap = m.snapshot().borrow().clone();
    assert_eq!(snap.companions.len(), 1);
    assert_eq!(snap.companions[0].health, Health::Healthy);
    assert!(
        tree(&fetch::folder(w.data())).is_empty(),
        "the download was left"
    );

    // Run: the installed bytes, under the real supervisor and sandbox.
    let port = free_port();
    let (_tx, r) = route(RouteKind::Direct, RouteState::Up);
    let (sup, _sb, gated) = run_installed(w.data(), &l, r, None, port, &[]).await;
    let gated = gated.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(gated.decision, Decision::Direct);
    let mut st = sup
        .status(&prowlarr())
        .unwrap_or_else(|| panic!("not supervised"));
    wait_state(&mut st, State::Running).await;
    until("it answers on 127.0.0.1", || answers(port)).await;
    let ring = sup.logs(&prowlarr()).unwrap_or_else(|| panic!("no logs"));
    assert_eq!(line(&ring, "PROXY ").await, "PROXY none");
    drop(gated);
    sup.shutdown().await;
    until("it stopped", || !answers(port)).await;

    // Removed.
    m.remove(&prowlarr())
        .await
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(!w.folder().exists());
    assert!(w.rows.rows().is_empty());
    assert!(m.snapshot().borrow().companions.is_empty());
}

// ─── 8. No catalogue override in a release ──────────────────────────────

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Whether the item starting `at` in `src` carries the test-only cfg.
fn test_only(src: &str, item: &str) -> bool {
    let Some(at) = src.find(item) else {
        panic!("{item} is gone; update the gate");
    };
    // The lines above the item's own line.
    let above = &src[..src[..at].rfind('\n').unwrap_or(0)];
    above
        .lines()
        .rev()
        .take_while(|l| {
            let t = l.trim_start();
            t.starts_with("#[") || t.starts_with("///")
        })
        .any(|l| l.trim() == "#[cfg(any(test, feature = \"testing\"))]")
}

/// A release build reads only the compiled-in catalogue: the only
/// constructor without the test-only `testing` feature is `builtin`, over
/// `include_str!`; nothing in the catalogue module reads a file or the
/// environment; the other ways in (another catalogue, a download from
/// anything but the guarded client, the fault points) are test-only; no
/// crate's normal dependency or feature turns `testing` on; and the
/// release workflow builds the binary with only its `web` feature.
#[test]
fn a_release_build_has_no_catalogue_override() {
    let cat = source("crates/companions/src/catalogue.rs");
    assert!(cat.contains(r#"const BUILTIN_JSON: &str = include_str!("../data/catalogue.json");"#));
    assert!(test_only(&cat, "pub fn from_json("));
    for read in [
        "std::env",
        "env::var",
        "fs::read",
        "File::open",
        "OpenOptions",
        "include_bytes!",
    ] {
        assert!(!cat.contains(read), "catalogue.rs uses {read}");
    }
    let ctors: Vec<&str> = cat
        .lines()
        .filter(|l| l.trim_start().starts_with("pub fn") && l.contains("Catalogue"))
        .map(str::trim)
        .collect();
    assert_eq!(
        ctors,
        [
            "pub fn builtin() -> Result<&'static Catalogue, &'static CatalogueError> {",
            "pub fn from_json(json: &str) -> Result<Catalogue, CatalogueError> {",
        ],
        "a new way to make a catalogue: gate it"
    );
    let manager = source("crates/companions/src/manager.rs");
    assert!(test_only(&manager, "pub fn any("));
    assert!(test_only(&manager, "pub enum Fault {"));
    let fetch = source("crates/companions/src/fetch.rs");
    assert!(test_only(&fetch, "pub async fn fetch_from("));
    assert!(test_only(&fetch, "pub async fn fetch_limited("));

    // No shipped dependency or feature turns `testing` on.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root())
        .output()
        .unwrap_or_else(|e| panic!("cargo metadata: {e}"));
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}"));
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str())
        .collect();
    let mut checked = 0;
    for p in meta["packages"].as_array().into_iter().flatten() {
        if !members.contains(&p["id"].as_str().unwrap_or_default()) {
            continue;
        }
        let name = p["name"].as_str().unwrap_or_default();
        for d in p["dependencies"].as_array().into_iter().flatten() {
            if d["name"] != "lodestar-companions" || d["kind"] == "dev" {
                continue;
            }
            checked += 1;
            let features: Vec<&str> = d["features"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str())
                .collect();
            assert!(!features.contains(&"testing"), "{name} turns it on");
        }
        for (feature, turns_on) in p["features"].as_object().into_iter().flatten() {
            for t in turns_on.as_array().into_iter().flatten() {
                let t = t.as_str().unwrap_or_default();
                assert!(
                    !(t.starts_with("lodestar-companions") && t.ends_with("/testing")),
                    "{name}'s feature {feature} turns it on"
                );
            }
        }
    }
    assert!(
        checked >= 2,
        "found only {checked} crates using lodestar-companions"
    );

    let release = source(".github/workflows/release.yml");
    assert!(!release.contains("--all-features"));
    for l in release.lines().filter(|l| l.contains("-p lodestar ")) {
        assert!(
            l.contains("--no-default-features --features web") && !l.contains("testing"),
            "release build line: {l}"
        );
    }
    // Nothing shipped knows about the test catalogue.
    for dir in std::fs::read_dir(root().join("crates"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let src = dir.path().join("src");
        for rel in tree(&src) {
            let text = std::fs::read_to_string(src.join(&rel)).unwrap_or_default();
            assert!(!text.contains("test-catalogue"), "{}/{rel}", src.display());
        }
    }
}
