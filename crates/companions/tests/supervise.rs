//! The companion supervisor against a real process (CMP-4a).
//!
//! **The fake companion is this test binary.** It runs itself filtered to
//! [`fake_companion`] with `FAKE_MODE` in the (cleared) environment; that
//! test then acts out the mode and never returns. Without `FAKE_MODE` it
//! does nothing. So the fake is a real, cargo-built executable on every
//! platform, with no shell script.
//!
//! Every wait is a condition polled against a deadline of at least 10 s.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestar_companions::CompanionId;
use lodestar_companions::adapter::{
    AdapterError, BoxFuture, GuardedHttp, Health, HealthStatus, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::inputs::PortNumber;
use lodestar_companions::supervise::{
    Attention, Companion, EnvName, LogRing, NoHook, Probe, SpawnSpec, State, Status, Stop,
    Supervisor, Timings, pidfile, process,
};
use lodestar_integrations::secret::Secret;
use tokio::sync::watch;

/// Generous: on Windows each start also runs PowerShell, and a crash
/// loop is five starts.
const DEADLINE: Duration = Duration::from_secs(60);
const SECRET: &str = "Hunter2-Very-Secret-Value";

/// The fake companion's modes. Never returns when `FAKE_MODE` is set.
#[test]
fn fake_companion() {
    let Ok(mode) = std::env::var("FAKE_MODE") else {
        return;
    };
    let mut out = std::io::stdout();
    match mode.as_str() {
        "report" => {
            let mut names: Vec<String> = std::env::vars_os()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect();
            names.sort();
            let _ = writeln!(out, "ENV {}", names.join(","));
            let cwd = std::env::current_dir().unwrap_or_default();
            let _ = writeln!(out, "CWD {}", cwd.display());
            let mut buf = [0u8; 16];
            let n = std::io::Read::read(&mut std::io::stdin(), &mut buf);
            let _ = writeln!(out, "STDIN {n:?}");
            // A secret split across two writes, flushed between them.
            let (a, b) = SECRET.split_at(7);
            let _ = write!(out, "secret {a}");
            let _ = out.flush();
            std::thread::sleep(Duration::from_millis(50));
            let _ = writeln!(out, "{b} apikey=abc123");
            let _ = out.flush();
            hang()
        }
        "crash" => std::process::exit(3),
        // Starts a grandchild of its own, as FlareSolverr starts a browser.
        "parent" => {
            let child = std::process::Command::new(std::env::current_exe().unwrap_or_default())
                .args(fake_args())
                .env("FAKE_MODE", "hang")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap_or_else(|e| panic!("{e}"));
            let _ = writeln!(out, "CHILD {}", child.id());
            let _ = out.flush();
            // Waits on the grandchild forever (it hangs).
            let mut child = child;
            let _ = child.wait();
            hang()
        }
        "hang" => {
            let _ = writeln!(out, "READY");
            let _ = out.flush();
            hang()
        }
        #[cfg(unix)]
        "stubborn" => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap_or_else(|e| panic!("{e}"));
            rt.block_on(async {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .unwrap_or_else(|e| panic!("{e}"));
                let _ = writeln!(std::io::stdout(), "READY");
                let _ = std::io::stdout().flush();
                loop {
                    term.recv().await;
                    let _ = writeln!(std::io::stdout(), "IGNORED TERM");
                    let _ = std::io::stdout().flush();
                }
            })
        }
        // The work child: supervises one hanging companion.
        "work" => {
            let data = PathBuf::from(std::env::var_os("FAKE_DATA").unwrap_or_default());
            let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
            rt.block_on(async move {
                let mut sup = Supervisor::new(data.clone(), Arc::new(NoHook), Timings::default())
                    .await
                    .unwrap_or_else(|e| panic!("{e}"));
                let mut c = companion(&data, "qbittorrent", "hang", &[]);
                c.spawn.executable =
                    PathBuf::from(std::env::var_os("FAKE_EXE").unwrap_or_default());
                sup.add(c).unwrap_or_else(|e| panic!("{e}"));
                std::future::pending::<()>().await;
            });
            hang()
        }
        _ => std::process::exit(64),
    }
}

fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn id(s: &str) -> CompanionId {
    CompanionId::parse(s).unwrap_or_else(|e| panic!("{e:?}"))
}

/// The arguments that make this binary run [`fake_companion`] only.
fn fake_args() -> Vec<OsString> {
    [
        "fake_companion",
        "--exact",
        "--nocapture",
        "--test-threads=1",
        "-q",
    ]
    .map(OsString::from)
    .to_vec()
}

fn spec(data: &Path, name: &str, mode: &str, extra: &[(&str, &str)]) -> SpawnSpec {
    let mut env = vec![(
        EnvName::parse("FAKE_MODE").unwrap_or_else(|e| panic!("{e}")),
        OsString::from(mode),
    )];
    for (k, v) in extra {
        env.push((
            EnvName::parse(k).unwrap_or_else(|e| panic!("{e}")),
            (*v).into(),
        ));
    }
    SpawnSpec {
        id: id(name),
        executable: std::env::current_exe().unwrap_or_else(|e| panic!("{e}")),
        args: fake_args(),
        env,
        data_dir: data.to_owned(),
        allow_privileged: true,
    }
}

fn companion(data: &Path, name: &str, mode: &str, extra: &[(&str, &str)]) -> Companion {
    Companion {
        spawn: spec(data, name, mode, extra),
        depends_on: Vec::new(),
        probe: None,
        secrets: vec![Secret::new(SECRET)],
        stop: None,
    }
}

fn fast() -> Timings {
    Timings {
        backoff_start: Duration::from_millis(10),
        backoff_cap: Duration::from_millis(40),
        crash_window: Duration::from_secs(600),
        crash_limit: 5,
        health_every: Duration::from_millis(50),
        health_failures: 3,
        stop_deadline: Duration::from_millis(500),
    }
}

fn data_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"))
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

async fn wait_status(
    rx: &mut watch::Receiver<Status>,
    what: &str,
    f: impl FnMut(&Status) -> bool,
) -> Status {
    let got = match tokio::time::timeout(DEADLINE, rx.wait_for(f)).await {
        Ok(Ok(s)) => Some(*s),
        _ => None,
    };
    got.unwrap_or_else(|| panic!("waited for {what}; last saw {:?}", *rx.borrow()))
}

async fn wait_line(ring: &LogRing, prefix: &str) -> String {
    let until = Instant::now() + DEADLINE;
    loop {
        if let Some(l) = ring.lines().into_iter().find(|l| l.starts_with(prefix)) {
            return l;
        }
        assert!(
            Instant::now() < until,
            "no {prefix:?} line; saw {:?}",
            ring.lines()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_companion_gets_a_minimal_env_its_folder_and_no_stdin() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    sup.add(companion(
        data.path(),
        "prowlarr",
        "report",
        &[("X_TEMPLATE", SECRET)],
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    let ring = sup
        .logs(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no ring"));

    let mut env = wait_line(&ring, "ENV ").await;
    // macOS: CoreFoundation, which the fake companion (this test binary)
    // links through the TLS stack, sets `__CF_USER_TEXT_ENCODING` inside
    // the process at start-up when it's missing. It's added by the child
    // after exec, not passed through: the spawn clears the environment.
    if cfg!(target_os = "macos") {
        let names: Vec<&str> = env["ENV ".len()..]
            .split(',')
            .filter(|n| *n != "__CF_USER_TEXT_ENCODING")
            .collect();
        env = format!("ENV {}", names.join(","));
    }
    let mut want = vec!["FAKE_MODE", "X_TEMPLATE"];
    if cfg!(windows) {
        want.extend([
            "APPDATA",
            "PATH",
            "SystemRoot",
            "TEMP",
            "TMP",
            "USERPROFILE",
        ]);
    }
    want.sort();
    assert_eq!(env, format!("ENV {}", want.join(",")));

    let cwd = wait_line(&ring, "CWD ").await;
    let home = process::home_dir(data.path(), &id("prowlarr"));
    assert_eq!(canonical(Path::new(&cwd["CWD ".len()..])), canonical(&home));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&home)
            .map(|m| m.permissions().mode())
            .unwrap_or(0);
        assert_eq!(mode & 0o777, 0o700);
    }
    assert_eq!(wait_line(&ring, "STDIN ").await, "STDIN Ok(0)");

    let secret = wait_line(&ring, "secret ").await;
    assert_eq!(secret, "secret [redacted] apikey=[redacted]");
    assert!(!ring.lines().iter().any(|l| l.contains("Hunter2")));
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crashes_back_off_then_need_attention_until_restarted() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    sup.add(companion(data.path(), "prowlarr", "crash", &[]))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut rx = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    let s = wait_status(&mut rx, "needs attention", |s| {
        matches!(s.state, State::NeedsAttention(_))
    })
    .await;
    assert_eq!(s.state, State::NeedsAttention(Attention::CrashLoop));
    assert_eq!(s.restarts, 5);
    assert_eq!(s.last_exit, Some(3));
    // It stays there: nothing restarts it.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.changed())
            .await
            .is_err()
    );

    assert!(sup.restart(&id("prowlarr")));
    let s = wait_status(&mut rx, "a second crash loop", |s| {
        s.restarts == 10 && matches!(s.state, State::NeedsAttention(_))
    })
    .await;
    assert_eq!(s.state, State::NeedsAttention(Attention::CrashLoop));
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn crashes_outside_the_window_never_need_attention() {
    let data = data_dir();
    let t = Timings {
        crash_window: Duration::from_millis(1),
        ..fast()
    };
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), t)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    sup.add(companion(data.path(), "prowlarr", "crash", &[]))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut rx = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    let s = wait_status(&mut rx, "eight restarts", |s| {
        s.restarts >= 8 || matches!(s.state, State::NeedsAttention(_))
    })
    .await;
    assert!(!matches!(s.state, State::NeedsAttention(_)), "{s:?}");
    sup.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn what_a_companion_starts_is_stopped_with_it() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    sup.add(companion(data.path(), "flaresolverr", "parent", &[]))
        .unwrap_or_else(|e| panic!("{e}"));
    let ring = sup
        .logs(&id("flaresolverr"))
        .unwrap_or_else(|| panic!("no ring"));
    let line = wait_line(&ring, "CHILD ").await;
    let pid: u32 = line["CHILD ".len()..]
        .parse()
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(alive(pid));
    sup.shutdown().await;
    let until = Instant::now() + DEADLINE;
    while alive(pid) {
        assert!(Instant::now() < until, "the grandchild survived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Running, not a zombie or gone.
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rfind(')')
                .map(|i| s[i + 1..].trim_start().starts_with(['R', 'S', 'D']))
        })
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
#[test]
fn the_init_hook_kills_only_its_own_children() {
    let data = data_dir();
    records_dir(data.path());
    let mut child = unsupervised(data.path());
    pidfile::record(data.path(), &id("prowlarr"), child.id()).unwrap_or_else(|e| panic!("{e}"));
    // The test process isn't pid 1, so pid 1 isn't this child's parent.
    let done = pidfile::kill_adopted(data.path(), 1).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(done.killed, 0);
    assert!(
        matches!(child.try_wait(), Ok(None)),
        "a stranger's child was killed"
    );
    let me = std::process::id();
    let done = pidfile::kill_adopted(data.path(), me).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(done.killed, 1);
    assert!(wait_exit(&mut child));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_spawn_needs_attention_at_once() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mut c = companion(data.path(), "prowlarr", "hang", &[]);
    c.spawn.executable = data.path().join("missing");
    sup.add(c).unwrap_or_else(|e| panic!("{e}"));
    let mut rx = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    let s = wait_status(&mut rx, "needs attention", |s| {
        matches!(s.state, State::NeedsAttention(_))
    })
    .await;
    assert_eq!(
        s.state,
        State::NeedsAttention(Attention::Refused("bad_executable"))
    );
    assert_eq!(s.restarts, 0);
    sup.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn root_is_refused_unless_a_test_allows_it() {
    let data = data_dir();
    let mut s = spec(data.path(), "prowlarr", "hang", &[]);
    s.allow_privileged = false;
    let got = process::spawn(&s, &NoHook);
    if rustix::process::geteuid().is_root() {
        assert_eq!(got.err(), Some(process::SpawnError::Privileged));
    } else if let Ok(mut started) = got {
        let _ = started.child.kill().await;
    }
}

struct FakeHealth {
    id: CompanionId,
    healthy: bool,
    calls: AtomicU32,
}

impl Health for FakeHealth {
    fn companion(&self) -> &CompanionId {
        &self.id
    }
    fn check<'a>(&'a self, _: &'a dyn GuardedHttp, _: &'a Target) -> BoxFuture<'a, HealthStatus> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let s = if self.healthy {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        };
        Box::pin(async move { s })
    }
}

struct NoHttp;

impl GuardedHttp for NoHttp {
    fn send<'a>(
        &'a self,
        _: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async { Err(AdapterError::Timeout) })
    }
}

fn probe(health: Arc<FakeHealth>) -> Probe {
    Probe {
        health,
        http: Arc::new(NoHttp),
        target: Target::Loopback(PortNumber::new(9696).unwrap_or_else(|e| panic!("{e:?}"))),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn three_failed_health_checks_restart_it() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let health = Arc::new(FakeHealth {
        id: id("prowlarr"),
        healthy: false,
        calls: AtomicU32::new(0),
    });
    let mut c = companion(data.path(), "prowlarr", "hang", &[]);
    c.probe = Some(probe(health.clone()));
    sup.add(c).unwrap_or_else(|e| panic!("{e}"));
    let mut rx = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    wait_status(&mut rx, "unhealthy", |s| {
        matches!(s.state, State::Unhealthy { .. })
    })
    .await;
    let s = wait_status(&mut rx, "a health restart", |s| s.restarts >= 1).await;
    assert!(s.last_exit.is_some(), "it was stopped and reaped");
    assert!(health.calls.load(Ordering::SeqCst) >= 3);
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_companion_keeps_running() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let health = Arc::new(FakeHealth {
        id: id("prowlarr"),
        healthy: true,
        calls: AtomicU32::new(0),
    });
    let mut c = companion(data.path(), "prowlarr", "hang", &[]);
    c.probe = Some(probe(health.clone()));
    sup.add(c).unwrap_or_else(|e| panic!("{e}"));
    let until = Instant::now() + DEADLINE;
    while health.calls.load(Ordering::SeqCst) < 5 {
        assert!(Instant::now() < until, "too few health checks");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let rx = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    assert_eq!(rx.borrow().restarts, 0);
    assert_eq!(rx.borrow().state, State::Running);
    sup.shutdown().await;
}

struct Order(Arc<Mutex<Vec<&'static str>>>, &'static str);

impl Stop for Order {
    fn request(&self) -> BoxFuture<'_, ()> {
        if let Ok(mut v) = self.0.lock() {
            v.push(self.1);
        }
        Box::pin(async {})
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_in_reverse_dependency_order() {
    let data = data_dir();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mut flare = companion(data.path(), "flaresolverr", "hang", &[]);
    flare.stop = Some(Arc::new(Order(order.clone(), "flaresolverr")));
    let mut prowlarr = companion(data.path(), "prowlarr", "hang", &[]);
    prowlarr.depends_on = vec![id("flaresolverr")];
    prowlarr.stop = Some(Arc::new(Order(order.clone(), "prowlarr")));
    // Not before what it depends on.
    let early = companion(data.path(), "other", "hang", &[]);
    let mut early = early;
    early.depends_on = vec![id("nothing")];
    assert!(sup.add(early).is_err());
    sup.add(flare).unwrap_or_else(|e| panic!("{e}"));
    sup.add(prowlarr).unwrap_or_else(|e| panic!("{e}"));
    let mut a = sup
        .status(&id("flaresolverr"))
        .unwrap_or_else(|| panic!("no status"));
    let mut b = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    wait_status(&mut a, "running", |s| s.state == State::Running).await;
    wait_status(&mut b, "running", |s| s.state == State::Running).await;
    sup.shutdown().await;
    assert_eq!(
        *order.lock().unwrap_or_else(|e| e.into_inner()),
        ["prowlarr", "flaresolverr"]
    );
    assert_eq!(a.borrow().state, State::Stopped);
    assert_eq!(b.borrow().state, State::Stopped);
    assert!(
        pidfile::kill_orphans(data.path()).unwrap_or_default() == pidfile::Cleanup::default(),
        "records were left"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_companion_ignoring_sigterm_is_killed_after_the_deadline() {
    let data = data_dir();
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    sup.add(companion(data.path(), "prowlarr", "stubborn", &[]))
        .unwrap_or_else(|e| panic!("{e}"));
    let ring = sup
        .logs(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no ring"));
    let mut rx = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("no status"));
    wait_line(&ring, "READY").await;
    let began = Instant::now();
    sup.shutdown().await;
    let took = began.elapsed();
    assert!(
        took >= fast().stop_deadline,
        "killed before the deadline: {took:?}"
    );
    assert!(took < DEADLINE, "shutdown took {took:?}");
    let s = wait_status(&mut rx, "stopped", |s| s.state == State::Stopped).await;
    assert_eq!(s.last_exit, Some(128 + 9), "SIGKILL");
    assert!(ring.lines().iter().any(|l| l == "IGNORED TERM"));
}

/// Starts the fake directly (not supervised) and records it.
/// Starts the fake directly (not supervised), from a copy installed
/// under `<data>/companions/`, where a record may name it.
fn unsupervised(data: &Path) -> std::process::Child {
    let exe = install_fake(data);
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(fake_args())
        .env("FAKE_MODE", "hang")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    spawn_fresh(&mut cmd)
}

/// Copies this binary to `<data>/companions/fake/`, as an install would.
fn install_fake(data: &Path) -> PathBuf {
    let me = std::env::current_exe().unwrap_or_else(|e| panic!("{e}"));
    let dir = data.join("companions").join("fake");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let exe = dir.join(me.file_name().unwrap_or_default());
    if !exe.exists() {
        std::fs::copy(&me, &exe).unwrap_or_else(|e| panic!("{e}"));
    }
    canonical(&exe)
}

/// Spawns a just-written program. Another test thread's fork can briefly
/// hold the copy's write handle open (`ETXTBSY` on Linux): retry that,
/// against the deadline.
fn spawn_fresh(cmd: &mut std::process::Command) -> std::process::Child {
    let until = Instant::now() + DEADLINE;
    loop {
        match cmd.spawn() {
            Ok(c) => return c,
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy && Instant::now() < until =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn wait_exit(child: &mut std::process::Child) -> bool {
    let until = Instant::now() + DEADLINE;
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn records_dir(data: &Path) -> PathBuf {
    let dir = pidfile::run_dir(data);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    dir
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_matching_orphan_is_killed_at_start() {
    let data = data_dir();
    records_dir(data.path());
    let mut child = unsupervised(data.path());
    pidfile::record(data.path(), &id("prowlarr"), child.id()).unwrap_or_else(|e| panic!("{e}"));
    let done = pidfile::kill_orphans(data.path()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        done,
        pidfile::Cleanup {
            killed: 1,
            stale: 0,
            group: 0
        }
    );
    assert!(wait_exit(&mut child), "the orphan survived");
    assert_eq!(
        std::fs::read_dir(pidfile::run_dir(data.path()))
            .map(|d| d.count())
            .unwrap_or(9),
        0
    );
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn a_partial_match_is_never_killed() {
    let data = data_dir();
    let dir = records_dir(data.path());
    let mut child = unsupervised(data.path());
    let rec =
        pidfile::record(data.path(), &id("prowlarr"), child.id()).unwrap_or_else(|e| panic!("{e}"));
    let other_start = pidfile::Record {
        start: rec.start + 1,
        ..rec.clone()
    };
    let other_exe = pidfile::Record {
        exe: data.path().join("other"),
        ..rec.clone()
    };
    std::fs::write(
        dir.join("prowlarr.json"),
        serde_json::to_vec(&other_start).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        dir.join("qbittorrent.json"),
        serde_json::to_vec(&other_exe).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let done = pidfile::kill_orphans(data.path()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        done,
        pidfile::Cleanup {
            killed: 0,
            stale: 2,
            group: 0
        }
    );
    assert!(
        matches!(child.try_wait(), Ok(None)),
        "a reused pid was killed"
    );
    let _ = child.kill();
    let _ = child.wait();
}

/// The plan's required test: SIGKILL the work child, and no companion
/// survives the restart. This process stands in for the init: a
/// subreaper that adopts the companion and calls the same hook.
#[cfg(target_os = "linux")]
#[test]
fn no_companion_survives_a_killed_work_child() {
    use rustix::process::{Pid, WaitOptions, getpid, set_child_subreaper, waitpid};
    set_child_subreaper(Some(getpid())).unwrap_or_else(|e| panic!("{e}"));
    let data = data_dir();
    let mut work =
        std::process::Command::new(std::env::current_exe().unwrap_or_else(|e| panic!("{e}")))
            .args(fake_args())
            .env("FAKE_MODE", "work")
            .env("FAKE_DATA", data.path())
            .env("FAKE_EXE", install_fake(data.path()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{e}"));
    let record = pidfile::run_dir(data.path()).join("qbittorrent.json");
    let until = Instant::now() + DEADLINE;
    let rec: pidfile::Record = loop {
        let parsed = std::fs::read(&record)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        if let Some(r) = parsed {
            break r;
        }
        assert!(
            Instant::now() < until,
            "the work child never started its companion"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(Path::new(&format!("/proc/{}", rec.pid)).exists());

    let _ = work.kill();
    let _ = work.wait();
    // The init's hook, once its work child has died.
    let me = getpid().as_raw_nonzero().get() as u32;
    let until = Instant::now() + DEADLINE;
    let killed = loop {
        // The kernel reparents the companion as the work child exits.
        let done = pidfile::kill_adopted(data.path(), me).unwrap_or_else(|e| panic!("{e}"));
        if done.killed == 1 {
            break done;
        }
        assert!(
            Instant::now() < until,
            "the companion was never adopted: {done:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(killed.stale, 0);
    let pid = Pid::from_raw(rec.pid as i32).unwrap_or_else(|| panic!("pid"));
    let until = Instant::now() + DEADLINE;
    loop {
        if let Ok(Some((_, status))) = waitpid(Some(pid), WaitOptions::NOHANG) {
            assert_eq!(status.terminating_signal(), Some(9));
            break;
        }
        assert!(Instant::now() < until, "the companion wasn't killed");
        std::thread::sleep(Duration::from_millis(20));
    }
    // The restart: the next work child's start-up cleanup.
    let done = pidfile::kill_orphans(data.path()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        done,
        pidfile::Cleanup {
            killed: 0,
            stale: 1,
            group: 0
        }
    );
    assert!(
        !Path::new(&format!("/proc/{}", rec.pid)).exists(),
        "a companion survived"
    );
}
