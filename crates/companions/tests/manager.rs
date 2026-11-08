//! CMP-5's manager: an install is one journalled state machine, so a crash
//! at any step leaves the old version or nothing once recovery has run;
//! updates keep the previous version until the new one passes health;
//! uninstall removes exactly the companion's folder and credentials; and
//! every refusal is typed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_companions::adapter::{AdapterError, BoxFuture, Credentials, Target};
use lodestar_companions::catalogue::{Arch, Catalogue, Platform, Sha256};
use lodestar_companions::configure::InstallReceipt;
use lodestar_companions::credentials::SealedRow;
use lodestar_companions::fetch::{Chunks, FetchError, Opened, Source};
use lodestar_companions::layout::Install;
use lodestar_companions::manager::{
    ApiConfigure, Downloader, Fault, Health, Host, InstallToken, Launch, Manager, ManagerError,
    Parts, Refusal, RowError, Rows, Run,
};
use lodestar_companions::state::{self, Step};
use lodestar_companions::{CompanionId, Version};
use lodestar_integrations::secret::{SealedSecret, SecretStore};
use reqwest::Url;
use serde_json::json;
use sha2::Digest;

/// The download slot is per companion and per process: tests take turns.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const LINUX: Host = Host {
    platform: Some((Platform::Linux, Arch::X86_64)),
    in_container: false,
};

fn url(v: &str) -> String {
    format!(
        "https://github.com/userdocs/qbittorrent-nox-static/releases/download/release-{v}/x86_64-qbittorrent-nox"
    )
}

fn body(v: &str) -> Vec<u8> {
    format!("#!/bin/false\nqbittorrent {v}\n").into_bytes()
}

fn sha(b: &[u8]) -> String {
    sha2::Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// qBittorrent pinned to `v` on Linux, with its Windows installer; and
/// Prowlarr, not pinned.
fn catalogue(v: &str) -> &'static Catalogue {
    let b = body(v);
    let j = json!({
        "format": 1,
        "entries": [{
            "id": "qbittorrent",
            "name": "qBittorrent",
            "licence": {"spdx": "GPL-2.0-or-later", "url": "https://github.com/qbittorrent/qBittorrent/blob/master/COPYING"},
            "homepage": "https://www.qbittorrent.org/",
            "needed_for": ["download_client"],
            "opt_in": false,
            "hosts": ["github.com"],
            "native": {"release": "https://github.com/userdocs/qbittorrent-nox-static/releases/download/", "artifacts": [
                {"platform": "linux", "arch": "x86_64", "archive": "binary", "executable": null,
                 "pin": {"version": v, "url": url(v), "size": b.len(), "sha256": sha(&b),
                         "upstream_sig": {"kind": "none"}}},
                {"platform": "windows", "arch": "x86_64", "archive": "installer", "executable": null,
                 "pin": {"version": v, "url": format!("https://github.com/userdocs/qbittorrent-nox-static/releases/download/release-{v}/qbittorrent_setup.exe"),
                         "size": b.len(), "sha256": sha(&b), "upstream_sig": {"kind": "none"}}}
            ]},
            "container": null,
            "ports": [{"name": "webui", "port": 8080, "proto": "tcp", "exposure": "internal"}],
            "health": {"port": "webui", "path": "/api/v2/app/version"},
            "templates": ["qbittorrent_conf"]
        }, {
            "id": "prowlarr",
            "name": "Prowlarr",
            "licence": {"spdx": "GPL-3.0-only", "url": "https://github.com/Prowlarr/Prowlarr/blob/develop/LICENSE.md"},
            "homepage": "https://prowlarr.com/",
            "needed_for": ["indexer_manager"],
            "opt_in": false,
            "hosts": ["github.com"],
            "native": {"release": "https://github.com/Prowlarr/Prowlarr/releases/download/", "artifacts": [
                {"platform": "linux", "arch": "x86_64", "archive": "tar_gz", "executable": "Prowlarr/Prowlarr", "pin": null}
            ]},
            "container": null,
            "ports": [{"name": "webui", "port": 9696, "proto": "tcp", "exposure": "internal"}],
            "health": {"port": "webui", "path": "/ping"},
            "templates": ["prowlarr_config_xml"]
        }]
    });
    Box::leak(Box::new(Catalogue::from_json(&j.to_string()).unwrap()))
}

/// Serves every version's body at its pinned URL.
struct Net;

struct Once(Option<Vec<u8>>);

impl Chunks for Once {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, FetchError>> {
        let c = self.0.take();
        Box::pin(async move { Ok(c) })
    }
}

impl Source for Net {
    fn open<'a>(&'a self, u: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        let v = u
            .path()
            .split("release-")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or("")
            .to_owned();
        Box::pin(async move {
            Ok(Opened {
                status: 200,
                location: None,
                body: Box::new(Once(Some(body(&v)))),
            })
        })
    }
}

#[derive(Default)]
struct Memory(Mutex<HashMap<String, SealedSecret>>);

impl Rows for Memory {
    fn put(&self, row: &SealedRow) -> Result<(), RowError> {
        self.0
            .lock()
            .unwrap()
            .insert(row.key.clone(), row.value.clone());
        Ok(())
    }
    fn get(&self, key: &str) -> Result<Option<SealedSecret>, RowError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn forget(&self, id: &CompanionId) -> Result<(), RowError> {
        let prefix = format!("companions.{id}.secret.");
        self.0
            .lock()
            .unwrap()
            .retain(|k, _| !k.starts_with(&prefix));
        Ok(())
    }
}

impl Memory {
    fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self.0.lock().unwrap().keys().cloned().collect();
        k.sort();
        k
    }
}

/// Records starts and stops; health as set.
#[derive(Default)]
struct FakeRun {
    started: Mutex<Vec<String>>,
    stops: AtomicUsize,
    sick: AtomicBool,
}

impl Run for FakeRun {
    fn start(&self, l: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        assert!(l.executable.is_file(), "started a program that isn't there");
        assert!(l.executable.starts_with(&l.install));
        let version = l
            .install
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        self.started.lock().unwrap().push(version);
        Box::pin(async { Ok(()) })
    }
    fn healthy<'a>(&'a self, _: &'a CompanionId, _: &'a Target) -> BoxFuture<'a, bool> {
        let ok = !self.sick.load(Ordering::SeqCst);
        Box::pin(async move { ok })
    }
    fn stop<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, ()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}

/// Counts configurations, checking the credential opens.
#[derive(Default)]
struct FakeApi {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl ApiConfigure for FakeApi {
    fn configure<'a>(
        &'a self,
        _: &'a CompanionId,
        target: &'a Target,
        receipt: InstallReceipt,
        credentials: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        assert_eq!(receipt, InstallReceipt::for_tests(target.clone()));
        let pw = credentials.get("webui_password").unwrap().unwrap();
        assert_eq!(pw.expose().len(), 43);
        self.calls.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail.load(Ordering::SeqCst);
        Box::pin(async move {
            if fail {
                Err(AdapterError::Refused("test"))
            } else {
                Ok(())
            }
        })
    }
}

struct World {
    data: tempfile::TempDir,
    secrets: Arc<SecretStore>,
    rows: Arc<Memory>,
    run: Arc<FakeRun>,
    api: Arc<FakeApi>,
}

impl World {
    fn new() -> World {
        World {
            data: tempfile::tempdir().unwrap(),
            secrets: Arc::new(SecretStore::from_key(&[9u8; 32])),
            rows: Arc::default(),
            run: Arc::default(),
            api: Arc::default(),
        }
    }

    fn manager(&self, cat: &'static Catalogue, host: Host) -> Manager {
        Manager::open(Parts {
            data: self.data.path().to_path_buf(),
            host,
            catalogue: cat,
            downloader: Downloader::any(Arc::new(Net)),
            secrets: self.secrets.clone(),
            rows: self.rows.clone(),
            run: self.run.clone(),
            api: self.api.clone(),
        })
        .unwrap()
    }

    fn folder(&self) -> std::path::PathBuf {
        self.data.path().join("companions").join("qbittorrent")
    }

    /// The installed version on disk (pointer current, previous), and
    /// what the journal says, with nothing in progress.
    fn on_disk(&self) -> Option<(String, Option<String>)> {
        let id = qbt();
        let i = Install::existing(self.data.path(), &id).unwrap()?;
        let rec = state::load(i.root())
            .unwrap()
            .expect("a folder without a record");
        assert_eq!(rec.op, None, "the journal isn't clean");
        let p = i.pointer().unwrap().expect("installed without a pointer");
        assert_eq!(
            rec.installed.as_ref().map(Version::as_str),
            Some(p.current.as_str())
        );
        let mut versions: Vec<String> = std::fs::read_dir(i.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(char::is_numeric))
            .collect();
        versions.sort();
        let mut named = vec![p.current.as_str().to_owned()];
        named.extend(p.previous.iter().map(|v| v.as_str().to_owned()));
        named.sort();
        assert_eq!(versions, named, "version folders the pointer doesn't name");
        Some((
            p.current.as_str().to_owned(),
            p.previous.map(|v| v.as_str().to_owned()),
        ))
    }
}

fn refused<T>(r: Refusal) -> Result<T, ManagerError> {
    Err(ManagerError::Refused(r))
}

fn qbt() -> CompanionId {
    CompanionId::parse("qbittorrent").unwrap()
}

fn token(v: &str) -> InstallToken {
    InstallToken {
        id: qbt(),
        version: Version::parse(v).unwrap(),
        sha256: Sha256::parse_hex(&sha(&body(v))).unwrap(),
    }
}

#[tokio::test]
async fn a_first_install_runs_every_step() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    let offer = m.offer(&qbt()).unwrap();
    assert_eq!(offer.version.as_str(), "5.0.3");
    assert_eq!(offer.sha256, token("5.0.3").sha256);
    assert_eq!(offer.replaces, None);
    assert_eq!(m.install(&token("5.0.3")).await.unwrap().as_str(), "5.0.3");
    assert_eq!(w.on_disk(), Some(("5.0.3".into(), None)));
    assert_eq!(
        w.rows.keys(),
        ["companions.qbittorrent.secret.webui_password"]
    );
    let conf = w.folder().join("home/qBittorrent/config/qBittorrent.conf");
    assert!(conf.is_file());
    assert_eq!(*w.run.started.lock().unwrap(), ["5.0.3"]);
    assert_eq!(w.api.calls.load(Ordering::SeqCst), 1);
    let snap = m.snapshot().borrow().clone();
    assert_eq!(snap.companions.len(), 1);
    assert_eq!(snap.companions[0].health, Health::Healthy);
    // Installing it again is refused.
    assert_eq!(
        m.install(&token("5.0.3")).await,
        Err(ManagerError::Refused(Refusal::Current))
    );
}

#[tokio::test]
async fn refusals_are_typed_and_change_nothing() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let mut wrong = token("5.0.3");
    wrong.sha256 = Sha256::parse_hex(&sha(b"other")).unwrap();
    let inside = Host {
        in_container: true,
        ..LINUX
    };
    assert_eq!(
        w.manager(cat, inside).install(&token("5.0.3")).await,
        refused(Refusal::InContainer)
    );
    let nowhere = Host {
        platform: None,
        in_container: false,
    };
    assert_eq!(
        w.manager(cat, nowhere).install(&token("5.0.3")).await,
        refused(Refusal::UnsupportedPlatform)
    );
    let windows = Host {
        platform: Some((Platform::Windows, Arch::X86_64)),
        in_container: false,
    };
    assert_eq!(
        w.manager(cat, windows).offer(&qbt()),
        refused(Refusal::GuidedOnly)
    );
    let m = w.manager(cat, LINUX);
    assert_eq!(m.install(&wrong).await, refused(Refusal::Confirmation));
    let mut other_version = token("5.0.3");
    other_version.version = Version::parse("5.0.4").unwrap();
    assert_eq!(
        m.install(&other_version).await,
        refused(Refusal::Confirmation)
    );
    let prowlarr = CompanionId::parse("prowlarr").unwrap();
    assert!(matches!(
        m.offer(&prowlarr),
        Err(ManagerError::Refused(Refusal::NotAvailable(_)))
    ));
    // A detected companion's credential is never overwritten.
    let key = lodestar_companions::credentials::CredentialKey::new(
        qbt(),
        lodestar_companions::credentials::Field::WebUiPassword,
    );
    let row = lodestar_companions::credentials::generate(&w.secrets, &key)
        .unwrap()
        .row;
    w.rows.put(&row).unwrap();
    assert_eq!(m.install(&token("5.0.3")).await, refused(Refusal::Present));
    assert_eq!(w.rows.keys(), [key.row_key()]);
    assert!(
        !w.data
            .path()
            .join("companions")
            .join("qbittorrent")
            .exists()
    );
    assert!(w.run.started.lock().unwrap().is_empty());
}

const INSTALL_FAULTS: &[Fault] = &[
    Fault::Before(Step::Staged),
    Fault::After(Step::Staged),
    Fault::Before(Step::Credentials),
    Fault::After(Step::Credentials),
    Fault::Before(Step::Files),
    Fault::After(Step::Files),
    Fault::Before(Step::Committed),
    Fault::After(Step::Committed),
    Fault::Before(Step::Healthy),
    Fault::After(Step::Healthy),
    Fault::Before(Step::Configured),
    Fault::After(Step::Configured),
];

#[tokio::test]
async fn a_crash_at_any_install_step_leaves_nothing() {
    let _g = SERIAL.lock().await;
    let cat = catalogue("5.0.3");
    for f in INSTALL_FAULTS {
        let w = World::new();
        let m = w.manager(cat, LINUX);
        m.crash_at(Some(*f));
        assert_eq!(
            m.install(&token("5.0.3")).await,
            Err(ManagerError::Crashed),
            "{f:?}"
        );
        // Restart: a new manager recovers before anything else.
        drop(m);
        let m = w.manager(cat, LINUX);
        let got = m.recover().await;
        assert!(
            got.iter().all(|(_, r)| matches!(r, Ok(None))),
            "{f:?}: {got:?}"
        );
        assert_eq!(w.on_disk(), None, "{f:?}");
        assert!(!w.folder().exists(), "{f:?}");
        assert!(w.rows.keys().is_empty(), "{f:?}");
        assert!(m.snapshot().borrow().companions.is_empty());
        // And installing works afterwards.
        assert!(m.install(&token("5.0.3")).await.is_ok(), "{f:?}");
    }
    // Past the commit point it's rolled forward.
    let w = World::new();
    let m = w.manager(cat, LINUX);
    m.crash_at(Some(Fault::Prune));
    assert_eq!(m.install(&token("5.0.3")).await, Err(ManagerError::Crashed));
    drop(m);
    let got = w.manager(cat, LINUX).recover().await;
    assert!(
        got.iter()
            .any(|(_, r)| matches!(r, Ok(Some(v)) if v.as_str() == "5.0.3"))
    );
    assert_eq!(w.on_disk(), Some(("5.0.3".into(), None)));
}

#[tokio::test]
async fn an_update_keeps_the_previous_version_until_it_is_healthy() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    w.manager(catalogue("5.0.2"), LINUX)
        .install(&token("5.0.2"))
        .await
        .unwrap();
    let rows = w.rows.keys();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    assert_eq!(m.offer(&qbt()).unwrap().replaces.unwrap().as_str(), "5.0.2");
    m.install(&token("5.0.3")).await.unwrap();
    assert_eq!(w.on_disk(), Some(("5.0.3".into(), None)));
    // The credential and config were kept.
    assert_eq!(w.rows.keys(), rows);
    assert_eq!(*w.run.started.lock().unwrap(), ["5.0.2", "5.0.3"]);
}

#[tokio::test]
async fn a_failed_update_rolls_back_and_restarts_the_previous_version() {
    let _g = SERIAL.lock().await;
    for fail_api in [false, true] {
        let w = World::new();
        w.manager(catalogue("5.0.2"), LINUX)
            .install(&token("5.0.2"))
            .await
            .unwrap();
        let m = w.manager(catalogue("5.0.3"), LINUX);
        if fail_api {
            w.api.fail.store(true, Ordering::SeqCst);
        } else {
            w.run.sick.store(true, Ordering::SeqCst);
        }
        let e = m.install(&token("5.0.3")).await.unwrap_err();
        assert!(matches!(e, ManagerError::Step { .. }), "{e:?}");
        assert_eq!(w.on_disk(), Some(("5.0.2".into(), None)));
        assert_eq!(*w.run.started.lock().unwrap(), ["5.0.2", "5.0.3", "5.0.2"]);
        assert_eq!(w.rows.keys().len(), 1);
    }
}

#[tokio::test]
async fn a_crash_at_any_update_step_leaves_the_old_version() {
    let _g = SERIAL.lock().await;
    let faults = INSTALL_FAULTS.iter().filter(|f| {
        !matches!(
            f,
            Fault::Before(Step::Credentials | Step::Files)
                | Fault::After(Step::Credentials | Step::Files)
        )
    });
    for f in faults {
        let w = World::new();
        w.manager(catalogue("5.0.2"), LINUX)
            .install(&token("5.0.2"))
            .await
            .unwrap();
        let m = w.manager(catalogue("5.0.3"), LINUX);
        m.crash_at(Some(*f));
        assert_eq!(
            m.install(&token("5.0.3")).await,
            Err(ManagerError::Crashed),
            "{f:?}"
        );
        drop(m);
        let got = w.manager(catalogue("5.0.3"), LINUX).recover().await;
        assert!(
            got.iter()
                .any(|(_, r)| matches!(r, Ok(Some(v)) if v.as_str() == "5.0.2")),
            "{f:?}: {got:?}"
        );
        assert_eq!(w.on_disk(), Some(("5.0.2".into(), None)), "{f:?}");
        assert_eq!(w.rows.keys().len(), 1);
    }
}

#[tokio::test]
async fn uninstall_removes_exactly_the_folder_and_credentials() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let m = w.manager(cat, LINUX);
    assert_eq!(
        m.remove(&qbt()).await,
        Err(ManagerError::Refused(Refusal::NotInstalled))
    );
    m.install(&token("5.0.3")).await.unwrap();
    let keep = [
        w.data
            .path()
            .join("companions")
            .join("prowlarr")
            .join("mine"),
        w.data.path().join("companions").join("run").join("x"),
        w.data.path().join("keep.txt"),
    ];
    for k in &keep {
        std::fs::create_dir_all(k.parent().unwrap()).unwrap();
        std::fs::write(k, b"x").unwrap();
    }
    let other = "companions.prowlarr.secret.api_key".to_owned();
    w.rows.0.lock().unwrap().insert(
        other.clone(),
        SealedSecret::from_bytes(
            w.secrets
                .seal("x", &lodestar_integrations::secret::Secret::new("y"))
                .unwrap()
                .as_bytes()
                .to_vec(),
        )
        .unwrap(),
    );
    let plan = m.removal(&qbt()).unwrap();
    assert_eq!(plan.folder, std::fs::canonicalize(w.folder()).unwrap());
    assert!(
        plan.rows
            .contains(&"companions.qbittorrent.secret.webui_password".to_owned())
    );
    assert_eq!(m.remove(&qbt()).await.unwrap(), plan);
    assert!(!w.folder().exists());
    assert_eq!(w.rows.keys(), [other]);
    for k in &keep {
        assert!(k.is_file(), "{}", k.display());
    }
    assert!(m.snapshot().borrow().companions.is_empty());

    // A removal a crash interrupted is finished at start-up.
    let w = World::new();
    let m = w.manager(cat, LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    for f in [Fault::Before(Step::Stopped), Fault::Before(Step::Forgotten)] {
        m.crash_at(Some(f));
        assert_eq!(m.remove(&qbt()).await, Err(ManagerError::Crashed));
        assert!(w.folder().exists());
    }
    drop(m);
    let got = w.manager(cat, LINUX).recover().await;
    assert!(got.iter().all(|(_, r)| matches!(r, Ok(None))), "{got:?}");
    assert!(!w.folder().exists());
    assert!(w.rows.keys().is_empty());
}

#[tokio::test]
async fn recovery_leaves_unknown_folders_alone() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let stray = w.data.path().join("companions").join("someone-else");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("file"), b"x").unwrap();
    let got = w.manager(catalogue("5.0.3"), LINUX).recover().await;
    assert!(got.iter().all(|(_, r)| matches!(r, Ok(None))));
    assert!(stray.join("file").is_file());
    assert!(!Path::new(&w.folder()).exists());
}

/// #608's follow-up: the route the gate reads turns down (or closes,
/// which the gate reads as down) when its monitor stops, instead of
/// keeping a stale `Up`.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_monitor_turns_the_route_down() {
    use lodestar_sources::egress::{Egress, EgressFactory, Endpoints, NoCredentials, RouteState};
    use std::time::Duration;
    let factory = EgressFactory::new(Arc::new(NoCredentials));
    let up = factory
        .build(
            "companion:qbittorrent",
            &Egress::Interface("lo".into()),
            Endpoints::manifest(&[]),
        )
        .await;
    let (mut route, task) =
        lodestar_companions::manager::monitored(Arc::new(up), Duration::from_millis(20));
    let deadline = Duration::from_secs(10);
    tokio::time::timeout(deadline, route.state.wait_for(|s| *s == RouteState::Up))
        .await
        .expect("never up")
        .expect("closed while up");
    task.abort();
    let down = tokio::time::timeout(deadline, route.state.wait_for(|s| *s != RouteState::Up)).await;
    match down.expect("still up after the monitor stopped") {
        Ok(s) => assert!(matches!(*s, RouteState::Down(_))),
        Err(_closed) => {}
    }
}

/// One journal writer per data folder: a second manager (another process,
/// serve or the CLI) is refused while the first holds the lock.
#[tokio::test]
async fn a_second_manager_is_refused_while_one_runs() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let m = w.manager(cat, LINUX);
    let second = Manager::open(Parts {
        data: w.data.path().to_path_buf(),
        host: LINUX,
        catalogue: cat,
        downloader: Downloader::any(Arc::new(Net)),
        secrets: w.secrets.clone(),
        rows: w.rows.clone(),
        run: w.run.clone(),
        api: w.api.clone(),
    });
    assert!(matches!(second, Err(ManagerError::Refused(Refusal::Busy))));
    drop(m);
    drop(w.manager(cat, LINUX));
}

/// Bytes that aren't the pin's are never staged or started, and nothing
/// is left behind.
#[tokio::test]
async fn a_download_that_fails_verification_leaves_nothing() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    // The pin says 5.0.3's bytes; the source serves 5.0.4's at that URL.
    let pinned = catalogue("5.0.3");
    struct Wrong;
    impl Source for Wrong {
        fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
            Box::pin(async move {
                Ok(Opened {
                    status: 200,
                    location: None,
                    body: Box::new(Once(Some(body("5.0.4")))),
                })
            })
        }
    }
    let m = Manager::open(Parts {
        data: w.data.path().to_path_buf(),
        host: LINUX,
        catalogue: pinned,
        downloader: Downloader::any(Arc::new(Wrong)),
        secrets: w.secrets.clone(),
        rows: w.rows.clone(),
        run: w.run.clone(),
        api: w.api.clone(),
    })
    .unwrap();
    let e = m.install(&token("5.0.3")).await.unwrap_err();
    assert!(
        matches!(e, ManagerError::Step { step: "fetch", .. }),
        "{e:?}"
    );
    assert!(!w.folder().exists());
    assert!(w.rows.keys().is_empty());
    assert!(w.run.started.lock().unwrap().is_empty());
}

/// #608's follow-up: a link where a config folder belongs refuses the
/// install, the link's target is untouched, and the install is undone.
#[cfg(unix)]
#[tokio::test]
async fn a_linked_config_folder_is_refused() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let outside = tempfile::tempdir().unwrap();
    let home = w.folder().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(outside.path(), home.join("qBittorrent")).unwrap();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    let e = m.install(&token("5.0.3")).await.unwrap_err();
    assert_eq!(
        e,
        ManagerError::Step {
            step: "files",
            code: "config_path_link"
        }
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(!w.folder().exists());
    assert!(w.rows.keys().is_empty());
}

/// A folder with a pointer but no record isn't ours: recovery and install
/// both leave it alone.
#[tokio::test]
async fn a_folder_without_a_record_is_left_alone() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    drop(
        w.manager(cat, LINUX)
            .install(&token("5.0.3"))
            .await
            .unwrap(),
    );
    std::fs::remove_file(w.folder().join("state.json")).unwrap();
    let m = w.manager(cat, LINUX);
    let got = m.recover().await;
    assert!(got.iter().any(|(_, r)| r.is_err()));
    let e = m.install(&token("5.0.3")).await.unwrap_err();
    assert_eq!(e.code(), "no_record");
    assert!(w.folder().join("current").is_file());
    assert_eq!(w.rows.keys().len(), 1);
}
