//! CMP-5g-3: the manager's real `Run`, `Supervised`, as serve wires it (and
//! #621's N3 and N5). A companion with no route isn't started; one on a
//! route it can't follow is refused (Q8); one on a route that's down is
//! held, and its health check fails at once instead of waiting the whole
//! deadline; nothing starts after shutdown. Nothing here spawns a process:
//! a held companion isn't started until its route is up.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_companions::adapter::{
    AdapterError, BoxFuture, GuardedHttp, ProbeRequest, ProbeResponse, Target,
};
use lodestar_companions::configure::Socks5Proxy;
use lodestar_companions::inputs::{CompanionId, PortNumber};
use lodestar_companions::manager::{Launch, Prelaunch, Routes, Run, Supervised, launch_env};
use lodestar_companions::supervise::egress::Route;
use lodestar_companions::supervise::{Launcher, Limits, Sandbox, State, Supervisor, Timings};
use lodestar_sources::egress::{DownReason, RouteKind, RouteState};
use tokio::sync::watch;

const WAIT: Duration = Duration::from_secs(10);

const DOTNET8: &str = r#"{"runtimeOptions":{"tfm":"net8.0","includedFrameworks":[{"name":"Microsoft.NETCore.App","version":"8.0.11"}]}}"#;

fn proxy() -> Socks5Proxy {
    Socks5Proxy::new("10.8.0.1:1080".parse().unwrap()).unwrap()
}

/// Never reached: nothing here runs.
struct NoHttp;

impl GuardedHttp for NoHttp {
    fn send<'a>(
        &'a self,
        _: ProbeRequest<'a>,
    ) -> BoxFuture<'a, Result<ProbeResponse, AdapterError>> {
        Box::pin(async { Err(AdapterError::Refused("test")) })
    }
}

/// One route for every companion, or none.
struct Fixed(Mutex<Option<Prelaunch>>);

impl Routes for Fixed {
    fn route<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Option<Prelaunch>> {
        let r = self.0.lock().unwrap().clone();
        Box::pin(async move { r })
    }
}

struct World {
    data: tempfile::TempDir,
    routes: Arc<Fixed>,
    /// The route's state, held so it stays readable.
    state: watch::Sender<RouteState>,
}

impl World {
    fn new(kind: Option<RouteKind>) -> World {
        let (state, rx) = watch::channel(RouteState::Down(DownReason::ProxyUnreachable));
        let route = kind.map(|kind| Prelaunch {
            route: Route { kind, state: rx },
            proxy: (kind == RouteKind::Socks5).then(proxy),
        });
        World {
            data: tempfile::tempdir().unwrap(),
            routes: Arc::new(Fixed(Mutex::new(route))),
            state,
        }
    }

    async fn supervised(&self) -> Supervised {
        let launcher = Launcher {
            exe: std::env::current_exe().unwrap(),
            args: Vec::new(),
        };
        let sandbox = Sandbox::new(Limits::default(), launcher).unwrap();
        let data = self.data.path().to_path_buf();
        let sup = Supervisor::new(data.clone(), sandbox.hook(), Timings::default())
            .await
            .unwrap();
        Supervised::new(
            data,
            sup,
            sandbox,
            Arc::new(NoHttp),
            self.routes.clone(),
            None,
        )
    }

    /// An installed version's program for `id`, as the manager launches it.
    fn launch(&self, id: &str, port: u16) -> Launch {
        let install = self.data.path().join("companions").join(id).join("1.0.0");
        std::fs::create_dir_all(&install).unwrap();
        let executable = install.join(id);
        std::fs::write(&executable, b"not run").unwrap();
        // A self-contained .NET 8 build, as Prowlarr ships.
        std::fs::write(install.join(format!("{id}.runtimeconfig.json")), DOTNET8).unwrap();
        Launch {
            id: CompanionId::parse(id).unwrap(),
            executable,
            install,
            args: Vec::new(),
            target: Target::Loopback(PortNumber::new(port).unwrap()),
            secrets: Vec::new(),
        }
    }
}

#[tokio::test]
async fn a_companion_without_a_route_is_not_started() {
    let w = World::new(None);
    let s = w.supervised().await;
    let l = w.launch("prowlarr", 9696);
    let id = l.id.clone();
    assert_eq!(s.start(l).await, Err("no_route"));
    assert!(s.status(&id).is_none());
    assert!(s.logs(&id).is_none());
}

/// Q8: qBittorrent can't follow a SOCKS5 route, so it never starts, and
/// never on the direct route instead.
#[tokio::test]
async fn a_route_it_cant_follow_is_refused() {
    let w = World::new(Some(RouteKind::Socks5));
    let s = w.supervised().await;
    let l = w.launch("qbittorrent", 8080);
    let id = l.id.clone();
    assert_eq!(s.start(l).await, Err("cant_follow_route"));
    assert!(s.status(&id).is_none());
}

/// #621 N3: a companion held for its route is supervised but not
/// running, and its health check says so at once rather than after
/// `HEALTH_DEADLINE` (120 s). Its logs stay readable after it stops.
#[tokio::test]
async fn a_held_companion_fails_its_health_check_at_once() {
    let w = World::new(Some(RouteKind::Socks5));
    let s = w.supervised().await;
    let l = w.launch("prowlarr", 9696);
    let (id, target) = (l.id.clone(), l.target.clone());
    s.start(l).await.unwrap();
    let status = s.status(&id).expect("not supervised");
    assert_eq!(status.borrow().state, State::Held);
    // It's launched on its proxy (X-cmp-egress-prelaunch).
    let env = s.launched_env(&id).unwrap();
    assert!(
        env.iter()
            .any(|(k, v)| k == "ALL_PROXY" && v.to_str() == Some("socks5://10.8.0.1:1080")),
        "{env:?}"
    );
    let healthy = tokio::time::timeout(WAIT, s.healthy(&id, &target)).await;
    assert_eq!(
        healthy,
        Ok(false),
        "the health check waited for a held companion"
    );
    assert_eq!(s.logs(&id), Some(Vec::new()));
    s.stop(&id).await;
    assert!(s.status(&id).is_none());
    assert_eq!(s.logs(&id), Some(Vec::new()));
    // Nothing reads its route any more (the gate's copy and the
    // supervisor's), so a relay would stop probing it. The test's own
    // copy is in `Fixed`.
    w.routes.0.lock().unwrap().take();
    let closed = tokio::time::timeout(WAIT, w.state.closed()).await;
    assert!(closed.is_ok(), "the route is still read after the stop");
}

/// Serve's shutdown stops what runs, and nothing starts after it.
#[tokio::test]
async fn nothing_starts_after_shutdown() {
    let w = World::new(Some(RouteKind::Socks5));
    let s = w.supervised().await;
    let l = w.launch("prowlarr", 9696);
    let id = l.id.clone();
    s.start(l).await.unwrap();
    tokio::time::timeout(WAIT, s.shutdown())
        .await
        .expect("shutdown wasn't bounded");
    let again = w.launch("prowlarr", 9696);
    assert_eq!(s.start(again).await, Err("shutting_down"));
    assert!(s.status(&id).is_none());
}

/// X-cmp-egress-prelaunch: a SOCKS5 route whose proxy isn't known can't be
/// put into the companion's launch, so it isn't started at all: never
/// "direct, then configured".
#[tokio::test]
async fn a_route_that_cant_be_launched_on_is_refused() {
    let w = World::new(Some(RouteKind::Socks5));
    if let Some(p) = w.routes.0.lock().unwrap().as_mut() {
        p.proxy = None;
    }
    let s = w.supervised().await;
    let l = w.launch("prowlarr", 9696);
    let id = l.id.clone();
    assert_eq!(s.start(l).await, Err("route_not_expressible"));
    assert!(s.status(&id).is_none());
}

/// The launch environment carries the route: on SOCKS5, every proxy
/// variable .NET reads, with the proxy resolving names, and only loopback
/// bypassed; direct carries nothing; a kind with no launch form refuses.
#[test]
fn the_launch_environment_carries_the_route() {
    let (_tx, rx) = watch::channel(RouteState::Up);
    let pre = |kind, proxy| Prelaunch {
        route: Route {
            kind,
            state: rx.clone(),
        },
        proxy,
    };
    let env = |p: &Prelaunch| {
        launch_env(p).map(|e| {
            e.into_iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.into_string().unwrap()))
                .collect::<Vec<_>>()
        })
    };
    let url = "socks5://10.8.0.1:1080";
    let loopback = "localhost,127.0.0.1,::1,[::1]";
    let mut want = vec![
        ("ALL_PROXY", url),
        ("HTTP_PROXY", url),
        ("HTTPS_PROXY", url),
    ];
    if cfg!(unix) {
        want.extend([
            ("all_proxy", url),
            ("http_proxy", url),
            ("https_proxy", url),
        ]);
        want.extend([("NO_PROXY", loopback), ("no_proxy", loopback)]);
    } else {
        want.push(("NO_PROXY", loopback));
    }
    let want: Vec<(String, String)> = want
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    assert_eq!(env(&pre(RouteKind::Socks5, Some(proxy()))), Some(want));
    let v6 = Socks5Proxy::new("[fd00::1]:1080".parse().unwrap()).unwrap();
    let got = env(&pre(RouteKind::Socks5, Some(v6))).unwrap();
    assert_eq!(got[0].1, "socks5://[fd00::1]:1080");
    // Direct adds none of them, even with a proxy known.
    assert_eq!(
        env(&pre(RouteKind::Direct, Some(proxy()))),
        Some(Vec::new())
    );
    assert_eq!(env(&pre(RouteKind::Socks5, None)), None);
    assert_eq!(env(&pre(RouteKind::Interface, None)), None);
    assert_eq!(env(&pre(RouteKind::BindAddress, None)), None);
}

/// A .NET older than 8 ignores a `socks5://` proxy variable and would go
/// direct, and a program that isn't .NET (no runtime config) isn't known
/// to read it: neither is started on a SOCKS5 route.
#[tokio::test]
async fn only_a_runtime_that_reads_the_proxy_variables_starts_on_socks5() {
    let w = World::new(Some(RouteKind::Socks5));
    let s = w.supervised().await;
    let l = w.launch("prowlarr", 9696);
    let config = l.install.join("prowlarr.runtimeconfig.json");
    std::fs::write(
        &config,
        r#"{"runtimeOptions":{"tfm":"net6.0","includedFrameworks":[{"name":"Microsoft.NETCore.App","version":"6.0.36"}]}}"#,
    )
    .unwrap();
    assert_eq!(s.start(l).await, Err("route_not_expressible"));
    let l = w.launch("prowlarr", 9696);
    std::fs::remove_file(l.install.join("prowlarr.runtimeconfig.json")).unwrap();
    assert_eq!(s.start(l).await, Err("route_not_expressible"));
    // (Direct needs no runtime check: `launch_env` adds nothing there, and
    // a direct start would spawn the program, which these tests don't.)
}

/// A route the companion can't follow at all keeps its own refusal, ahead
/// of whether it could be launched on.
#[tokio::test]
async fn cant_follow_comes_before_cant_launch() {
    let w = World::new(Some(RouteKind::Socks5));
    if let Some(p) = w.routes.0.lock().unwrap().as_mut() {
        p.proxy = None;
    }
    let s = w.supervised().await;
    let l = w.launch("qbittorrent", 8080);
    assert_eq!(s.start(l).await, Err("cant_follow_route"));
}

/// #679's review: the runtime config is never read through a link, and a
/// FIFO in its place is refused at once instead of blocking the start.
#[cfg(unix)]
#[tokio::test]
async fn a_linked_or_fifo_runtime_config_is_refused() {
    let w = World::new(Some(RouteKind::Socks5));
    let s = w.supervised().await;
    let l = w.launch("prowlarr", 9696);
    let config = l.install.join("prowlarr.runtimeconfig.json");
    let real = w.data.path().join("elsewhere.json");
    std::fs::rename(&config, &real).unwrap();
    std::os::unix::fs::symlink(&real, &config).unwrap();
    assert_eq!(s.start(l).await, Err("route_not_expressible"));

    let l = w.launch("prowlarr", 9696);
    let config = l.install.join("prowlarr.runtimeconfig.json");
    std::fs::remove_file(&config).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(&config)
        .status()
        .unwrap();
    assert!(made.success());
    let got = tokio::time::timeout(WAIT, s.start(l)).await;
    assert_eq!(
        got,
        Ok(Err("route_not_expressible")),
        "a FIFO blocked the start"
    );
}
