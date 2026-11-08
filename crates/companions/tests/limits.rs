//! Companion limits, sandbox and route gate against a real process
//! (CMP-4b).
//!
//! **The fake companion is this test binary,** as in `tests/supervise.rs`:
//! it runs itself filtered to [`fake_companion`] with `FAKE_MODE` set. On
//! Linux the launcher is this binary too, filtered to [`fake_launcher`],
//! which acts as Lodestar's launcher when the plan is in its environment.
//! Under Landlock the companion may only run what's in its install folder,
//! so the Linux tests copy this binary there.
//!
//! Every wait is a condition polled against a deadline of at least 10 s.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lodestar_companions::CompanionId;
use lodestar_companions::supervise::egress::{self, Follows, GateError, Route};
use lodestar_companions::supervise::{
    Companion, EnvName, LogRing, NoHook, SpawnSpec, State, Status, Supervisor, Timings,
};
use lodestar_sources::egress::{DownReason, RouteKind, RouteState};
use tokio::sync::watch;

const DEADLINE: Duration = Duration::from_secs(60);

/// The fake companion. Never returns when `FAKE_MODE` is set.
#[test]
fn fake_companion() {
    let Ok(mode) = std::env::var("FAKE_MODE") else {
        return;
    };
    let mut out = std::io::stdout();
    match mode.as_str() {
        "hang" => {
            let _ = writeln!(out, "READY");
        }
        #[cfg(target_os = "linux")]
        "limits" => {
            use rustix::process::{Resource, getrlimit};
            let show = |r: Option<u64>| r.map_or("unlimited".to_owned(), |v| v.to_string());
            let n = getrlimit(Resource::Nofile);
            let a = getrlimit(Resource::As);
            let c = getrlimit(Resource::Core);
            let _ = writeln!(
                out,
                "LIMITS nofile={}/{} as={} core={}",
                show(n.current),
                show(n.maximum),
                show(a.current),
                show(c.current)
            );
        }
        "probe" => {
            let data = PathBuf::from(std::env::var_os("FAKE_DATA").unwrap_or_default());
            let read = |p: PathBuf| match std::fs::read(p) {
                Ok(_) => "ok",
                Err(_) => "denied",
            };
            let _ = writeln!(out, "READ_DATA {}", read(data.join("secrets.key")));
            let _ = writeln!(
                out,
                "READ_RUN {}",
                read(data.join("companions").join("run").join("canary"))
            );
            let _ = writeln!(
                out,
                "READ_OTHER {}",
                read(data.join("companions").join("prowlarr").join("config.xml"))
            );
            let wrote = std::fs::write("probe.txt", b"x").is_ok();
            let _ = writeln!(out, "WRITE_HOME {}", if wrote { "ok" } else { "denied" });
        }
        _ => std::process::exit(64),
    }
    let _ = out.flush();
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// The launcher: with the plan in its environment, applies it and execs
/// the companion. Without it, does nothing.
#[test]
fn fake_launcher() {
    #[cfg(target_os = "linux")]
    if std::env::var_os(lodestar_companions::supervise::limits_linux::PLAN_ENV).is_some() {
        std::process::exit(lodestar_companions::supervise::limits_linux::run_launcher());
    }
}

fn id(s: &str) -> CompanionId {
    CompanionId::parse(s).unwrap_or_else(|e| panic!("{e:?}"))
}

fn args_for(test: &str) -> Vec<OsString> {
    [test, "--exact", "--nocapture", "--test-threads=1", "-q"]
        .map(OsString::from)
        .to_vec()
}

fn spec(data: &Path, name: &str, exe: PathBuf, env: &[(&str, &OsString)]) -> SpawnSpec {
    SpawnSpec {
        id: id(name),
        executable: exe,
        args: args_for("fake_companion"),
        env: env
            .iter()
            .map(|(k, v)| {
                (
                    EnvName::parse(k).unwrap_or_else(|e| panic!("{e}")),
                    (*v).clone(),
                )
            })
            .collect(),
        data_dir: data.to_owned(),
        allow_privileged: true,
    }
}

fn companion(spawn: SpawnSpec) -> Companion {
    Companion {
        spawn,
        depends_on: Vec::new(),
        probe: None,
        secrets: Vec::new(),
        stop: None,
    }
}

fn me() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|e| panic!("{e}"))
}

fn fast() -> Timings {
    Timings {
        backoff_start: Duration::from_millis(10),
        backoff_cap: Duration::from_millis(40),
        crash_window: Duration::from_secs(600),
        crash_limit: 2,
        health_every: Duration::from_millis(50),
        health_failures: 3,
        stop_deadline: Duration::from_millis(500),
    }
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

async fn wait_lines(ring: &LogRing, prefix: &str, count: usize) -> Vec<String> {
    let until = Instant::now() + DEADLINE;
    loop {
        let got: Vec<String> = ring
            .lines()
            .into_iter()
            .filter(|l| l.starts_with(prefix))
            .collect();
        if got.len() >= count {
            return got;
        }
        assert!(
            Instant::now() < until,
            "wanted {count} {prefix:?} lines; saw {:?}",
            ring.lines()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---- the route gate (every platform) -------------------------------

fn route(kind: RouteKind, state: RouteState) -> (watch::Sender<RouteState>, Route) {
    let (tx, rx) = watch::channel(state);
    (tx, Route { kind, state: rx })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_companion_runs_only_while_its_route_is_up_and_holds_are_not_crashes() {
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let hang = OsString::from("hang");
    let c = companion(spec(
        data.path(),
        "qbittorrent",
        me(),
        &[("FAKE_MODE", &hang)],
    ));
    let (tx, r) = route(RouteKind::Interface, RouteState::Unchecked);
    let gated = egress::add(&mut sup, c, Follows::of(&id("qbittorrent")), r)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        gated.decision,
        egress::Decision::Follow(RouteKind::Interface)
    );
    let q = id("qbittorrent");
    let mut st = sup.status(&q).unwrap_or_else(|| panic!("not supervised"));
    let ring = sup.logs(&q).unwrap_or_else(|| panic!("no ring"));
    // Not checked yet, then down: it never starts.
    assert_eq!(st.borrow().state, State::Held);
    let _ = tx.send(RouteState::Down(DownReason::InterfaceDown));
    wait_status(&mut st, "held", |s| s.state == State::Held).await;
    assert!(
        ring.lines().is_empty(),
        "started while down: {:?}",
        ring.lines()
    );
    // Up, down, up, ... with a crash limit of 2: never Needs attention.
    for round in 1..=4 {
        let _ = tx.send(RouteState::Up);
        wait_status(&mut st, "running", |s| s.state == State::Running).await;
        // One start per time the route came up, and none while it was down.
        assert_eq!(wait_lines(&ring, "READY", round).await.len(), round);
        let _ = tx.send(RouteState::Down(DownReason::InterfaceMissing));
        let s = wait_status(&mut st, "held again", |s| s.state == State::Held).await;
        assert_eq!(s.restarts, 0, "a hold counted as a crash");
    }
    assert_eq!(ring.lines().iter().filter(|l| *l == "READY").count(), 4);
    // Nothing reports the route any more: it stays held.
    let _ = tx.send(RouteState::Up);
    wait_status(&mut st, "running", |s| s.state == State::Running).await;
    drop(tx);
    let s = wait_status(&mut st, "held, route gone", |s| s.state == State::Held).await;
    assert_eq!(s.restarts, 0);
    drop(gated);
    sup.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_the_companion_cant_follow_never_starts_it() {
    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let hang = OsString::from("hang");
    for (name, kind) in [
        ("prowlarr", RouteKind::Interface),
        ("prowlarr", RouteKind::BindAddress),
        ("qbittorrent", RouteKind::Socks5),
    ] {
        let c = companion(spec(data.path(), name, me(), &[("FAKE_MODE", &hang)]));
        let (_tx, r) = route(kind, RouteState::Up);
        let got = egress::add(&mut sup, c, Follows::of(&id(name)), r);
        assert_eq!(got.err(), Some(GateError::CantFollow(kind)), "{name}");
        assert!(sup.status(&id(name)).is_none(), "{name} was added");
    }
    // Direct: it just runs.
    let c = companion(spec(data.path(), "prowlarr", me(), &[("FAKE_MODE", &hang)]));
    let (_tx, r) = route(RouteKind::Direct, RouteState::Up);
    let gated =
        egress::add(&mut sup, c, Follows::of(&id("prowlarr")), r).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(gated.decision, egress::Decision::Direct);
    let mut st = sup
        .status(&id("prowlarr"))
        .unwrap_or_else(|| panic!("not supervised"));
    wait_status(&mut st, "running", |s| s.state == State::Running).await;
    sup.shutdown().await;
}

/// A real `PluginEgress` from the factory: the loopback interface is up,
/// a missing one is down.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_factory_route_drives_the_gate() {
    use lodestar_sources::egress::{Egress, EgressFactory, Endpoints, NoCredentials};
    let factory = EgressFactory::new(Arc::new(NoCredentials));
    let up = factory
        .build(
            "companion:qbittorrent",
            &Egress::Interface("lo".into()),
            Endpoints::manifest(&[]),
        )
        .await;
    let down = factory
        .build(
            "companion:qbittorrent",
            &Egress::Interface("nosuchif0".into()),
            Endpoints::manifest(&[]),
        )
        .await;
    assert_eq!(up.check_now().await, RouteState::Up);
    assert!(matches!(down.check_now().await, RouteState::Down(_)));
    assert_eq!(Route::of(&up).kind, RouteKind::Interface);

    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let hang = OsString::from("hang");
    let follows = Follows::of(&id("qbittorrent"));
    let a = companion(spec(
        data.path(),
        "qbittorrent",
        me(),
        &[("FAKE_MODE", &hang)],
    ));
    let _ga = egress::add(&mut sup, a, follows, Route::of(&up)).unwrap_or_else(|e| panic!("{e}"));
    let mut st = sup
        .status(&id("qbittorrent"))
        .unwrap_or_else(|| panic!("none"));
    wait_status(&mut st, "running on lo", |s| s.state == State::Running).await;
    sup.shutdown().await;

    let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mut sup = Supervisor::new(data.path().to_owned(), Arc::new(NoHook), fast())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let b = companion(spec(
        data.path(),
        "qbittorrent",
        me(),
        &[("FAKE_MODE", &hang)],
    ));
    let _gb = egress::add(&mut sup, b, follows, Route::of(&down)).unwrap_or_else(|e| panic!("{e}"));
    let st = sup
        .status(&id("qbittorrent"))
        .unwrap_or_else(|| panic!("none"));
    assert_eq!(st.borrow().state, State::Held);
    sup.shutdown().await;
}

// ---- Linux: rlimits and Landlock -----------------------------------

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use lodestar_companions::supervise::limits::{Level, Mechanism};
    use lodestar_companions::supervise::{Confinement, Launcher, Limits, Sandbox};

    /// A data folder with secrets outside the companion's folder, and this
    /// binary installed as the companion.
    struct Setup {
        _tmp: tempfile::TempDir,
        data: PathBuf,
        install: PathBuf,
        exe: PathBuf,
    }

    fn setup(name: &str) -> Setup {
        let tmp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let data = tmp.path().join("data");
        let install = data.join("companions").join(name).join("1.0.0");
        let run = data.join("companions").join("run");
        let other = data.join("companions").join("prowlarr");
        for d in [&install, &run, &other] {
            std::fs::create_dir_all(d).unwrap_or_else(|e| panic!("{e}"));
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700));
        let write = |p: PathBuf| std::fs::write(p, b"secret").unwrap_or_else(|e| panic!("{e}"));
        write(data.join("secrets.key"));
        write(run.join("canary"));
        write(other.join("config.xml"));
        let exe = install.join("fake");
        std::fs::copy(me(), &exe).unwrap_or_else(|e| panic!("{e}"));
        Setup {
            _tmp: tmp,
            data,
            install,
            exe,
        }
    }

    fn sandbox(limits: Limits) -> Sandbox {
        Sandbox::new(
            limits,
            Launcher {
                exe: me(),
                args: args_for("fake_launcher"),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"))
    }

    async fn run_one(s: &Setup, sb: &Sandbox, mode: &str) -> (Supervisor, LogRing) {
        let mut sup = Supervisor::new(s.data.clone(), sb.hook(), fast())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let (mode, data) = (OsString::from(mode), s.data.clone().into_os_string());
        let raw = spec(
            &s.data,
            "qbittorrent",
            s.exe.clone(),
            &[("FAKE_MODE", &mode), ("FAKE_DATA", &data)],
        );
        let (wrapped, _) = sb
            .prepare(
                raw,
                &Confinement {
                    install: s.install.clone(),
                    downloads: None,
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
        sup.add(companion(wrapped))
            .unwrap_or_else(|e| panic!("{e}"));
        let ring = sup
            .logs(&id("qbittorrent"))
            .unwrap_or_else(|| panic!("no ring"));
        (sup, ring)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_companion_runs_with_its_rlimits() {
        let s = setup("qbittorrent");
        let sb = sandbox(Limits {
            open_files: 1000,
            memory: Some(64 << 30),
        });
        let (_, report) = sb
            .prepare(
                spec(&s.data, "qbittorrent", s.exe.clone(), &[]),
                &Confinement {
                    install: s.install.clone(),
                    downloads: None,
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.limits.open_files, Some(1000));
        assert_eq!(report.limits.memory, Some(64 << 30));
        // CI runs as root, where a cgroup is never touched.
        if rustix::process::geteuid().is_root() {
            assert_eq!(report.limits.mechanism, Mechanism::Rlimits);
            assert_eq!(report.limits.cgroup_skipped, Some("root"));
        }
        let (sup, ring) = run_one(&s, &sb, "limits").await;
        let line = wait_lines(&ring, "LIMITS", 1).await.remove(0);
        assert_eq!(
            line,
            format!("LIMITS nofile=1000/1000 as={} core=0", 64u64 << 30)
        );
        // The record names the companion, not the launcher.
        let rec = std::fs::read_to_string(
            s.data
                .join("companions")
                .join("run")
                .join("qbittorrent.json"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let canon = std::fs::canonicalize(&s.exe).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            rec.contains(&*canon.to_string_lossy()),
            "record {rec} doesn't name {}",
            canon.display()
        );
        sup.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn landlock_keeps_the_companion_out_of_lodestars_data() {
        let s = setup("qbittorrent");
        let sb = sandbox(Limits::default());
        let (_, report) = sb
            .prepare(
                spec(&s.data, "qbittorrent", s.exe.clone(), &[]),
                &Confinement {
                    install: s.install.clone(),
                    downloads: None,
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let (sup, ring) = run_one(&s, &sb, "probe").await;
        let lines = wait_lines(&ring, "WRITE_HOME", 1).await;
        assert_eq!(lines, ["WRITE_HOME ok"]);
        let all = ring.lines();
        match report.sandbox.level {
            Level::Full | Level::Partial => {
                for want in ["READ_DATA denied", "READ_RUN denied", "READ_OTHER denied"] {
                    assert!(all.iter().any(|l| l == want), "no {want:?} in {all:?}");
                }
            }
            Level::None => {
                // The kernel doesn't enforce it: the report says so.
                assert_eq!(report.sandbox.reason, "landlock_unavailable");
                assert!(all.iter().any(|l| l == "READ_DATA ok"), "{all:?}");
            }
        }
        sup.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_launcher_that_cant_launch_fails_the_start() {
        // A damaged plan, and a target outside `<data>/companions/` (this
        // binary where it was built): the launcher exits, the hook sees it
        // and the start fails, so nothing is recorded.
        for case in ["damaged plan", "target outside companions"] {
            let s = setup("qbittorrent");
            let sb = sandbox(Limits::default());
            let mut sup = Supervisor::new(s.data.clone(), sb.hook(), fast())
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let exe = if case == "damaged plan" {
                s.exe.clone()
            } else {
                me()
            };
            let hang = OsString::from("hang");
            let raw = spec(&s.data, "qbittorrent", exe, &[("FAKE_MODE", &hang)]);
            let (mut wrapped, _) = sb
                .prepare(
                    raw,
                    &Confinement {
                        install: s.install.clone(),
                        downloads: None,
                    },
                )
                .unwrap_or_else(|e| panic!("{e}"));
            if case == "damaged plan" {
                for (k, v) in &mut wrapped.env {
                    if k.as_str() == lodestar_companions::supervise::limits_linux::PLAN_ENV {
                        *v = OsString::from("garbage");
                    }
                }
            }
            sup.add(companion(wrapped))
                .unwrap_or_else(|e| panic!("{e}"));
            let mut st = sup
                .status(&id("qbittorrent"))
                .unwrap_or_else(|| panic!("none"));
            let got = wait_status(&mut st, case, |s| {
                matches!(s.state, State::NeedsAttention(_))
            })
            .await;
            assert!(got.restarts >= 2, "{case}");
            assert!(
                !s.data
                    .join("companions")
                    .join("run")
                    .join("qbittorrent.json")
                    .exists(),
                "{case}"
            );
            sup.shutdown().await;
        }
    }
}

// ---- Windows: the Job Object ----------------------------------------

#[cfg(windows)]
mod windows {
    use super::*;
    use lodestar_companions::supervise::limits::{Level, Mechanism};
    use lodestar_companions::supervise::{Confinement, Launcher, Limits, Sandbox, process};

    #[tokio::test(flavor = "multi_thread")]
    async fn companions_are_in_the_job_and_die_when_it_closes() {
        let data = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let install = data
            .path()
            .join("companions")
            .join("qbittorrent")
            .join("1.0.0");
        std::fs::create_dir_all(&install).unwrap_or_else(|e| panic!("{e}"));
        let sb = Sandbox::new(
            Limits::default(),
            Launcher {
                exe: me(),
                args: Vec::new(),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let hang = OsString::from("hang");
        let raw = spec(data.path(), "qbittorrent", me(), &[("FAKE_MODE", &hang)]);
        let (spec, report) = sb
            .prepare(
                raw,
                &Confinement {
                    install,
                    downloads: None,
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.limits.mechanism, Mechanism::JobObject);
        assert!(report.limits.dies_with_lodestar);
        assert_eq!(report.sandbox.level, Level::None);
        assert_eq!(report.sandbox.reason, "no_safe_wrapper");
        let hook = sb.hook();
        let started = tokio::task::spawn_blocking(move || process::spawn(&spec, hook.as_ref()))
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("{e}"));
        let mut child = started.child;
        assert!(
            sb.job_members().contains(&started.pid),
            "{} not in {:?}",
            started.pid,
            sb.job_members()
        );
        // Closing the job (dropping its last handle) kills it.
        drop(sb);
        let exit = tokio::time::timeout(DEADLINE, child.wait())
            .await
            .unwrap_or_else(|_| panic!("still running after the job closed"));
        assert!(exit.is_ok());
    }
}
