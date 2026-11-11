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
use lodestar_companions::inputs::{CompanionId, PortNumber};
use lodestar_companions::manager::{Launch, Routes, Run, Supervised};
use lodestar_companions::supervise::egress::Route;
use lodestar_companions::supervise::{Launcher, Limits, Sandbox, State, Supervisor, Timings};
use lodestar_sources::egress::{DownReason, RouteKind, RouteState};
use tokio::sync::watch;

const WAIT: Duration = Duration::from_secs(10);

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
struct Fixed(Mutex<Option<Route>>);

impl Routes for Fixed {
    fn route<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Option<Route>> {
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
        let route = kind.map(|kind| Route { kind, state: rx });
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
