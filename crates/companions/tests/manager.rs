//! CMP-5's manager: an install is one journalled state machine, so a crash
//! at any step leaves the old version or nothing once recovery has run;
//! updates keep the previous version until the new one passes health;
//! uninstall removes exactly the companion's folder and credentials; and
//! every refusal is typed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lodestar_companions::adapter::ConfigureInput;
use lodestar_companions::adapter::{AdapterError, BoxFuture, Credentials, Target};
use lodestar_companions::catalogue::{Arch, Catalogue, NotAvailable, Platform, Sha256};
use lodestar_companions::configure::{
    self, ConfigSink, DownloadClientEntry, IndexerManagerEntry, InstallReceipt, SinkError,
};
use lodestar_companions::credentials::SealedRow;
use lodestar_companions::fetch::{Chunks, FetchError, Opened, Source};
use lodestar_companions::layout::Install;
use lodestar_companions::manager::{
    ApiConfigure, Downloader, Fault, Health, Host, InstallToken, Launch, Manager, ManagerError,
    Parts, Refusal, RowError, Rows, Run,
};
use lodestar_companions::state::{self, Step};
use lodestar_companions::supervise::{State, Status};
use lodestar_companions::{CompanionId, Version};
use lodestar_integrations::secret::{SealedSecret, SecretStore};
use reqwest::Url;
use serde_json::json;
use sha2::Digest;
use tokio::sync::watch;

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

/// The compiled-in catalogue's text with every pin and digest taken out.
fn shipped_unpinned() -> &'static Catalogue {
    let mut v: serde_json::Value =
        serde_json::from_str(include_str!("../data/catalogue.json")).unwrap();
    for e in v["entries"].as_array_mut().unwrap() {
        // `get_mut`, not indexing: indexing a `null` would make it an object.
        if let Some(arts) = e
            .get_mut("native")
            .and_then(|n| n.get_mut("artifacts"))
            .and_then(|a| a.as_array_mut())
        {
            for a in arts {
                a["pin"] = serde_json::Value::Null;
            }
        }
        if e["container"].is_object() {
            e["container"]["digest"] = serde_json::Value::Null;
        }
    }
    Box::leak(Box::new(Catalogue::from_json(&v.to_string()).unwrap()))
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
    /// When set, `start` says so on `parked` and waits for `release`:
    /// an install held mid-step, for the operation guard's tests.
    park: AtomicBool,
    parked: tokio::sync::Notify,
    release: tokio::sync::Notify,
    /// The supervisor's state for the companion started last; dropped at
    /// its stop, as the supervisor's is.
    status: Mutex<Option<watch::Sender<Status>>>,
    /// Health checks made.
    checks: AtomicUsize,
}

impl FakeRun {
    /// The current run's state sender.
    fn run(&self) -> watch::Sender<Status> {
        self.status
            .lock()
            .unwrap()
            .clone()
            .expect("nothing running")
    }
}

fn status(state: State) -> Status {
    Status {
        state,
        restarts: 0,
        last_exit: None,
        run: 0,
    }
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
        {
            // As the supervisor: one run at a time.
            let mut st = self.status.lock().unwrap();
            if st.is_some() {
                return Box::pin(async { Err("duplicate") });
            }
            *st = Some(watch::channel(status(State::Running)).0);
        }
        self.started.lock().unwrap().push(version);
        let park = self.park.load(Ordering::SeqCst);
        Box::pin(async move {
            if park {
                self.parked.notify_one();
                self.release.notified().await;
            }
            Ok(())
        })
    }
    fn healthy<'a>(&'a self, _: &'a CompanionId, _: &'a Target) -> BoxFuture<'a, bool> {
        let ok = !self.sick.load(Ordering::SeqCst);
        self.checks.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { ok })
    }
    fn stop<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, ()> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        self.status.lock().unwrap().take();
        Box::pin(async {})
    }
    fn status(&self, _: &CompanionId) -> Option<watch::Receiver<Status>> {
        self.status.lock().unwrap().as_ref().map(|s| s.subscribe())
    }
}

/// Counts configurations, checking the credential opens.
#[derive(Default)]
struct FakeApi {
    calls: AtomicUsize,
    fail: AtomicBool,
    /// Never answers.
    hang: AtomicBool,
    /// What CMP-8b's hand-over saved, through [`Saved`].
    sink: Saved,
    /// Refuses the hand-over, or the release.
    refuse_hand_over: AtomicBool,
    refuse_release: AtomicBool,
    releases: AtomicUsize,
    /// Refuses the check before an install, as a first install over an
    /// entry that isn't Lodestar's is.
    refuse_pre_check: AtomicBool,
    /// Each check's `fresh`.
    asked: Mutex<Vec<bool>>,
    /// The hand-over waits for `hand_over_gate`, as a slow store's save
    /// does; `hand_over_dropped` says one was given up on before it
    /// answered.
    hang_hand_over: AtomicBool,
    hand_over_gate: tokio::sync::Notify,
    hand_over_dropped: AtomicBool,
    /// Says nothing is handed over, as for a companion this build doesn't.
    no_hand_over: AtomicBool,
    /// The release waits for `release_gate`, as a slow store's save does;
    /// `release_dropped` says one was given up on before it answered.
    hold_release: AtomicBool,
    release_gate: tokio::sync::Notify,
    release_dropped: AtomicBool,
}

/// Sets its flag when dropped before it's marked done.
struct Settles<'a>(&'a AtomicBool, bool);

impl Drop for Settles<'_> {
    fn drop(&mut self) {
        if !self.1 {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

/// Lodestar's settings, as the hand-over writes them: each password it
/// saved for the managed download client.
#[derive(Default)]
struct Saved(Mutex<Vec<String>>);

impl Saved {
    fn passwords(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl ConfigSink for Saved {
    fn download_client<'a>(
        &'a self,
        e: DownloadClientEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        self.0.lock().unwrap().push(e.password.expose().to_owned());
        Box::pin(async { Ok(()) })
    }
    fn indexer_manager<'a>(
        &'a self,
        _: IndexerManagerEntry,
    ) -> BoxFuture<'a, Result<(), SinkError>> {
        Box::pin(async { Err(SinkError::Unavailable) })
    }
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
        let hang = self.hang.load(Ordering::SeqCst);
        Box::pin(async move {
            if hang {
                std::future::pending::<()>().await;
            }
            if fail {
                Err(AdapterError::Refused("test"))
            } else {
                Ok(())
            }
        })
    }

    /// CMP-8b's own hand-over, on the receipt the manager minted.
    fn hand_over<'a>(
        &'a self,
        id: &'a CompanionId,
        target: &'a Target,
        receipt: InstallReceipt,
        credentials: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async move {
            if self.refuse_hand_over.load(Ordering::SeqCst) {
                return Err(AdapterError::Refused("sink_refused"));
            }
            if self.no_hand_over.load(Ordering::SeqCst) {
                return Ok(());
            }
            if self.hang_hand_over.load(Ordering::SeqCst) {
                let mut settles = Settles(&self.hand_over_dropped, false);
                self.hand_over_gate.notified().await;
                settles.1 = true;
            }
            let input = ConfigureInput::new(credentials).installed(receipt);
            configure::hand_over(&self.sink, id, target, &input).await
        })
    }

    fn can_hand_over<'a>(
        &'a self,
        _: &'a CompanionId,
        fresh: bool,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        self.asked.lock().unwrap().push(fresh);
        let refuse = self.refuse_pre_check.load(Ordering::SeqCst);
        Box::pin(async move {
            if refuse {
                Err(AdapterError::Refused("managed_client_in_use"))
            } else {
                Ok(())
            }
        })
    }

    fn hands_over(&self, id: &CompanionId) -> bool {
        *id == qbt() && !self.no_hand_over.load(Ordering::SeqCst)
    }

    fn release<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, Result<(), AdapterError>> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        let refuse = self.refuse_release.load(Ordering::SeqCst);
        let hold = self.hold_release.load(Ordering::SeqCst);
        Box::pin(async move {
            let mut settles = Settles(&self.release_dropped, false);
            if hold {
                self.release_gate.notified().await;
            }
            settles.1 = true;
            if refuse {
                Err(AdapterError::Refused("sink_refused"))
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

    fn parts(&self, cat: &'static Catalogue, host: Host) -> Parts {
        Parts {
            data: self.data.path().to_path_buf(),
            host,
            catalogue: cat,
            downloader: Downloader::any(Arc::new(Net)),
            secrets: self.secrets.clone(),
            rows: self.rows.clone(),
            run: self.run.clone(),
            api: self.api.clone(),
        }
    }

    fn manager(&self, cat: &'static Catalogue, host: Host) -> Manager {
        Manager::open(self.parts(cat, host)).unwrap()
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
    // The shipped catalogue with every pin taken out, whatever CMP-13
    // pins later: its Windows installer is still guided, never
    // `awaiting_pin` (#705's review, I3).
    let shipped = shipped_unpinned();
    assert_eq!(
        w.manager(shipped, windows).offer(&qbt()),
        refused(Refusal::GuidedOnly)
    );
    assert_eq!(
        w.manager(shipped, windows).install(&token("5.0.3")).await,
        refused(Refusal::GuidedOnly)
    );
    // A build Lodestar installs still answers `awaiting_pin` while it
    // waits: here the Linux binary.
    assert_eq!(
        w.manager(shipped, LINUX).offer(&qbt()),
        refused(Refusal::NotAvailable(NotAvailable::AwaitingPin))
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
    /// Serves the body after planting a link where the config folder
    /// goes: the companion's home, between `begun` and `files`.
    struct Planting(std::path::PathBuf, std::path::PathBuf);
    impl Source for Planting {
        fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
            std::fs::create_dir_all(&self.0).unwrap();
            std::os::unix::fs::symlink(&self.1, self.0.join("qBittorrent")).unwrap();
            Box::pin(async move {
                Ok(Opened {
                    status: 200,
                    location: None,
                    body: Box::new(Once(Some(body("5.0.3")))),
                })
            })
        }
    }
    let w = World::new();
    let outside = tempfile::tempdir().unwrap();
    let home = w.folder().join("home");
    let m = Manager::open(Parts {
        downloader: Downloader::any(Arc::new(Planting(home, outside.path().to_path_buf()))),
        ..w.parts(catalogue("5.0.3"), LINUX)
    })
    .unwrap();
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
    assert!(
        got.iter()
            .any(|(_, r)| matches!(r, Err(e) if e.code() == "not_ours"))
    );
    assert_eq!(
        m.install(&token("5.0.3")).await,
        refused(Refusal::FolderInUse)
    );
    assert!(w.folder().join("current").is_file());
    assert_eq!(w.rows.keys().len(), 1);
}

/// #621's review (B1): foreign files in a folder named for a catalogue id
/// (a compose companion's host folder) survive recovery and an install
/// attempt; an empty folder, all a crash before `begun` can leave, is
/// tidied away.
#[tokio::test]
async fn foreign_content_survives_recovery_and_install() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let file = w.folder().join("downloads").join("user-file.iso");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"mine").unwrap();
    let m = w.manager(cat, LINUX);
    let got = m.recover().await;
    assert!(
        got.iter()
            .any(|(_, r)| matches!(r, Err(e) if e.code() == "not_ours"))
    );
    assert_eq!(
        m.install(&token("5.0.3")).await,
        refused(Refusal::FolderInUse)
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"mine");
    assert!(w.run.started.lock().unwrap().is_empty());

    // An empty folder is what a crash before `begun` leaves.
    std::fs::remove_dir_all(w.folder()).unwrap();
    std::fs::create_dir_all(w.folder()).unwrap();
    let got = m.recover().await;
    assert!(got.iter().all(|(_, r)| matches!(r, Ok(None))), "{got:?}");
    assert!(!w.folder().exists());
}

/// Inside a container nothing is native (Q17): recovery looks at nothing,
/// not even a folder it would otherwise tidy.
#[tokio::test]
async fn recovery_does_nothing_inside_a_container() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    std::fs::create_dir_all(w.folder()).unwrap();
    let inside = Host {
        in_container: true,
        ..LINUX
    };
    assert!(
        w.manager(catalogue("5.0.3"), inside)
            .recover()
            .await
            .is_empty()
    );
    assert!(w.folder().exists());
}

/// A crashed removal keeps its journal to the end, so recovery always
/// knows the folder is being removed.
#[tokio::test]
async fn a_removal_keeps_its_journal_until_last() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let m = w.manager(cat, LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    m.crash_at(Some(Fault::Before(Step::Forgotten)));
    assert_eq!(m.remove(&qbt()).await, Err(ManagerError::Crashed));
    // The journal says what's going on, and recovery finishes it.
    let rec = state::load(&w.folder()).unwrap().unwrap();
    assert_eq!(rec.op.unwrap().step, Step::Stopped);
    drop(m);
    let got = w.manager(cat, LINUX).recover().await;
    assert!(got.iter().all(|(_, r)| matches!(r, Ok(None))), "{got:?}");
    assert!(!w.folder().exists());
}

/// #621's review (B2): the in-process guard. While an install is held
/// mid-step, a second install, a removal and recovery of the same
/// companion are refused as busy; once it finishes, they're not.
#[tokio::test(flavor = "multi_thread")]
async fn an_operation_in_progress_refuses_others_on_the_same_companion() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let m = Arc::new(w.manager(cat, LINUX));
    assert!(!m.busy(&qbt()));
    w.run.park.store(true, Ordering::SeqCst);
    let held = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.3")).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), w.run.parked.notified())
        .await
        .expect("the install never reached its start");
    // `busy` sees the slot the install holds, as a route change asks.
    assert!(m.busy(&qbt()));
    let busy = refused::<Version>(Refusal::Busy);
    assert_eq!(m.install(&token("5.0.3")).await, busy);
    assert_eq!(m.remove(&qbt()).await, refused(Refusal::Busy));
    let got = m.recover().await;
    assert!(
        got.iter()
            .any(|(id, r)| id == &qbt() && r == &refused(Refusal::Busy))
    );
    w.run.park.store(false, Ordering::SeqCst);
    w.run.release.notify_one();
    let done = tokio::time::timeout(std::time::Duration::from_secs(10), held)
        .await
        .expect("the install didn't finish")
        .unwrap();
    assert_eq!(done.unwrap().as_str(), "5.0.3");
    assert!(!m.busy(&qbt()));
    assert!(m.remove(&qbt()).await.is_ok());
}

/// #621's re-review (B1): a first save that failed before its rename (a
/// crash, a full disk) leaves only the journal's temporary file. Recovery
/// takes that folder as Lodestar's own, an install goes ahead over it,
/// and with anything else beside it the folder is still not ours.
#[tokio::test]
async fn a_leftover_temporary_journal_never_blocks_the_companion() {
    let _g = SERIAL.lock().await;
    let cat = catalogue("5.0.3");
    let leave = |w: &World| {
        std::fs::create_dir_all(w.folder()).unwrap();
        std::fs::write(w.folder().join(state::STATE_TMP), b"{\"for").unwrap();
    };

    let w = World::new();
    leave(&w);
    let got = w.manager(cat, LINUX).recover().await;
    assert!(got.iter().all(|(_, r)| matches!(r, Ok(None))), "{got:?}");
    assert!(!w.folder().exists());

    let w = World::new();
    leave(&w);
    let m = w.manager(cat, LINUX);
    assert_eq!(m.install(&token("5.0.3")).await.unwrap().as_str(), "5.0.3");
    assert_eq!(w.on_disk(), Some(("5.0.3".into(), None)));

    let w = World::new();
    leave(&w);
    std::fs::write(w.folder().join("theirs.iso"), b"mine").unwrap();
    let m = w.manager(cat, LINUX);
    let got = m.recover().await;
    assert!(
        got.iter()
            .any(|(_, r)| matches!(r, Err(e) if e.code() == "not_ours"))
    );
    assert_eq!(
        m.install(&token("5.0.3")).await,
        refused(Refusal::FolderInUse)
    );
    assert_eq!(
        std::fs::read(w.folder().join("theirs.iso")).unwrap(),
        b"mine"
    );
}

/// A dropped install future doesn't stop the install part-way: it runs to
/// completion in its own task, holding its slot until then, and the
/// companion is free afterwards (#621's re-review, N2).
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_install_still_runs_to_completion() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = Arc::new(w.manager(catalogue("5.0.3"), LINUX));
    w.run.park.store(true, Ordering::SeqCst);
    let caller = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.3")).await }
    });
    let deadline = std::time::Duration::from_secs(10);
    tokio::time::timeout(deadline, w.run.parked.notified())
        .await
        .expect("the install never reached its start");
    // The caller gives up; the install keeps its slot.
    caller.abort();
    let _ = caller.await;
    assert_eq!(m.remove(&qbt()).await, refused(Refusal::Busy));
    w.run.park.store(false, Ordering::SeqCst);
    w.run.release.notify_one();
    // Once it has finished (and only then) the slot is free: the install
    // completed, so the removal finds it installed.
    let start = std::time::Instant::now();
    let removed = loop {
        match m.remove(&qbt()).await {
            Err(ManagerError::Refused(Refusal::Busy)) => {
                assert!(
                    start.elapsed() < deadline,
                    "the dropped install never finished"
                );
                tokio::task::yield_now().await;
            }
            other => break other,
        }
    };
    assert!(removed.is_ok(), "{removed:?}");
    assert_eq!(*w.run.started.lock().unwrap(), ["5.0.3"]);
}

/// #621's delta review (N1): a failed first install's rollback deletes
/// only what Lodestar put in the folder. A file someone else wrote there
/// mid-install survives, and the folder stays because of it, while the
/// staged version, the journal and the credential go.
#[tokio::test]
async fn a_rolled_back_install_spares_a_file_written_meanwhile() {
    let _g = SERIAL.lock().await;
    /// Serves the body after writing a third party's file into the
    /// companion's folder, between `begun` and the failed health check.
    struct Writing(std::path::PathBuf);
    impl Source for Writing {
        fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
            std::fs::write(&self.0, b"mine").unwrap();
            Box::pin(async move {
                Ok(Opened {
                    status: 200,
                    location: None,
                    body: Box::new(Once(Some(body("5.0.3")))),
                })
            })
        }
    }
    let w = World::new();
    let theirs = w.folder().join("theirs.iso");
    w.run.sick.store(true, Ordering::SeqCst);
    let m = Manager::open(Parts {
        downloader: Downloader::any(Arc::new(Writing(theirs.clone()))),
        ..w.parts(catalogue("5.0.3"), LINUX)
    })
    .unwrap();
    let e = m.install(&token("5.0.3")).await.unwrap_err();
    assert!(
        matches!(e, ManagerError::Step { step: "health", .. }),
        "{e:?}"
    );
    assert_eq!(std::fs::read(&theirs).unwrap(), b"mine");
    let left: Vec<String> = std::fs::read_dir(w.folder())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["theirs.iso"]);
    assert!(w.rows.keys().is_empty());
}

/// #621's delta review (N3): a directory named like the journal's
/// temporary file isn't a leftover of Lodestar's.
#[tokio::test]
async fn a_directory_named_like_the_temporary_journal_is_not_ours() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    std::fs::create_dir_all(w.folder().join(state::STATE_TMP)).unwrap();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    assert_eq!(
        m.install(&token("5.0.3")).await,
        refused(Refusal::FolderInUse)
    );
    assert!(w.folder().join(state::STATE_TMP).is_dir());
}

/// A `Run` that can't start anything: serve's `Unwired` until CMP-5g.
struct NotWired;

impl Run for NotWired {
    fn ready(&self) -> Result<(), &'static str> {
        Err("not_wired")
    }
    fn start(&self, _: Launch) -> BoxFuture<'_, Result<(), &'static str>> {
        Box::pin(async { Err("not_wired") })
    }
    fn healthy<'a>(&'a self, _: &'a CompanionId, _: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn stop<'a>(&'a self, _: &'a CompanionId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// [`Net`], counting what's asked for.
#[derive(Default)]
struct Counting(AtomicUsize);

impl Source for Counting {
    fn open<'a>(&'a self, u: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Net.open(u)
    }
}

/// CMP-6: an install confirmed through the API, while the `Run` can't
/// start anything, is refused with `not_wired` before anything is
/// fetched or written, and leaves no journal behind.
#[tokio::test]
async fn an_unready_run_refuses_before_anything_is_fetched() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let fetched = Arc::new(Counting::default());
    let m = Manager::open(Parts {
        downloader: Downloader::any(fetched.clone()),
        run: Arc::new(NotWired),
        ..w.parts(catalogue("5.0.3"), LINUX)
    })
    .unwrap();
    assert_eq!(
        m.install(&token("5.0.3")).await,
        Err(ManagerError::Step {
            step: "start",
            code: "not_wired"
        })
    );
    assert_eq!(fetched.0.load(Ordering::SeqCst), 0);
    assert!(!w.folder().exists());
    assert!(w.rows.keys().is_empty());
    // A wrong confirmation is refused before even that.
    let mut wrong = token("5.0.3");
    wrong.sha256 = Sha256::parse_hex(&"ab".repeat(32)).unwrap();
    assert_eq!(m.install(&wrong).await, refused(Refusal::Confirmation));
    assert_eq!(fetched.0.load(Ordering::SeqCst), 0);
}

const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn health(m: &Manager) -> Option<Health> {
    m.snapshot()
        .borrow()
        .companions
        .iter()
        .find(|c| c.id == qbt())
        .map(|c| c.health)
}

async fn wait_health(m: &Manager, want: Health) {
    let mut rx = m.snapshot();
    let got = tokio::time::timeout(
        WAIT,
        rx.wait_for(|s| {
            s.companions
                .iter()
                .any(|c| c.id == qbt() && c.health == want)
        }),
    )
    .await;
    assert!(got.is_ok(), "never {want:?}: {:?}", health(m));
}

/// CMP-5g-3: serve's start and a restart each start and configure the
/// installed version; the snapshot then follows the supervisor (held for
/// its route is unhealthy, running again is healthy), and a run that
/// ended stops publishing, so an old run can't overwrite a new one.
#[tokio::test]
async fn an_installed_companion_starts_restarts_and_follows_its_supervisor() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    // Already running: serve's start leaves it as it is.
    m.start(&qbt()).await.unwrap();
    assert_eq!(*w.run.started.lock().unwrap(), ["5.0.3"]);
    // As at a fresh serve: nothing runs yet.
    w.run.status.lock().unwrap().take();
    m.start(&qbt()).await.unwrap();
    assert_eq!(*w.run.started.lock().unwrap(), ["5.0.3", "5.0.3"]);
    assert_eq!(w.api.calls.load(Ordering::SeqCst), 2);
    assert_eq!(health(&m), Some(Health::Healthy));
    w.run.run().send_replace(status(State::Held));
    wait_health(&m, Health::Unhealthy).await;
    w.run.run().send_replace(status(State::Running));
    wait_health(&m, Health::Healthy).await;

    let old = w.run.run();
    let stops = w.run.stops.load(Ordering::SeqCst);
    m.restart(&qbt()).await.unwrap().finished().await.unwrap();
    assert_eq!(w.run.stops.load(Ordering::SeqCst), stops + 1);
    assert_eq!(w.run.started.lock().unwrap().len(), 3);
    assert_eq!(w.api.calls.load(Ordering::SeqCst), 3);
    // The old run's last word is ignored, and its follower ends.
    old.send_replace(status(State::Stopped));
    let ended = tokio::time::timeout(WAIT, old.closed()).await;
    assert!(ended.is_ok(), "the old run's follower kept going");
    assert_eq!(health(&m), Some(Health::Healthy));
    w.run.run().send_replace(status(State::BackingOff {
        delay: std::time::Duration::from_secs(1),
    }));
    wait_health(&m, Health::Unhealthy).await;
}

/// A failed health check after a restart shows unhealthy, and the
/// companion is still followed: one held for its route shows healthy once
/// it runs.
#[tokio::test]
async fn a_restart_that_fails_its_health_is_still_followed() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    w.run.sick.store(true, Ordering::SeqCst);
    let got = m.restart(&qbt()).await.unwrap().finished().await;
    assert!(
        matches!(got, Err(ManagerError::Step { step: "health", .. })),
        "{got:?}"
    );
    assert_eq!(health(&m), Some(Health::Unhealthy));
    w.run.sick.store(false, Ordering::SeqCst);
    w.run.run().send_replace(status(State::Starting));
    w.run.run().send_replace(status(State::Running));
    wait_health(&m, Health::Healthy).await;
}

/// Start, restart and regenerate are for a companion Lodestar installed,
/// one operation at a time: refused during an install (`busy`), and for
/// one it didn't install or inside a container.
#[tokio::test]
async fn live_operations_are_refused_during_an_install_and_when_not_installed() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let m = Arc::new(w.manager(cat, LINUX));
    assert_eq!(m.start(&qbt()).await, refused(Refusal::NotInstalled));
    assert_eq!(
        m.restart(&qbt()).await.err(),
        Some(ManagerError::Refused(Refusal::NotInstalled))
    );
    assert_eq!(
        m.regenerate(&qbt()).await.err(),
        Some(ManagerError::Refused(Refusal::NotInstalled))
    );
    w.run.park.store(true, Ordering::SeqCst);
    let held = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.3")).await }
    });
    tokio::time::timeout(WAIT, w.run.parked.notified())
        .await
        .expect("the install never reached its start");
    let busy = Some(ManagerError::Refused(Refusal::Busy));
    assert_eq!(m.start(&qbt()).await.err(), busy);
    assert_eq!(m.restart(&qbt()).await.err(), busy);
    assert_eq!(m.regenerate(&qbt()).await.err(), busy);
    w.run.park.store(false, Ordering::SeqCst);
    w.run.release.notify_one();
    tokio::time::timeout(WAIT, held)
        .await
        .expect("the install didn't finish")
        .unwrap()
        .unwrap();
    // A restart under way holds the slot too.
    w.run.park.store(true, Ordering::SeqCst);
    let pending = m.restart(&qbt()).await.unwrap();
    tokio::time::timeout(WAIT, w.run.parked.notified())
        .await
        .expect("the restart never reached its start");
    assert_eq!(m.regenerate(&qbt()).await.err(), busy);
    assert_eq!(m.remove(&qbt()).await, refused(Refusal::Busy));
    w.run.park.store(false, Ordering::SeqCst);
    w.run.release.notify_one();
    tokio::time::timeout(WAIT, pending.finished())
        .await
        .expect("the restart didn't finish")
        .unwrap();
    drop(m);
    let docker = Host {
        in_container: true,
        ..LINUX
    };
    let m = w.manager(cat, docker);
    assert_eq!(
        m.restart(&qbt()).await.err(),
        Some(ManagerError::Refused(Refusal::NotInstalled))
    );
}

fn conf(w: &World) -> std::path::PathBuf {
    w.folder().join("home/qBittorrent/config/qBittorrent.conf")
}

const ROW: &str = "companions.qbittorrent.secret.webui_password";

/// Regenerate rotates the credential into its row and the companion's own
/// file, keeping the file's other lines, then starts and configures it
/// with the new one.
#[tokio::test]
async fn regenerate_rotates_the_row_and_the_file_together() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    let mut text = std::fs::read_to_string(conf(&w)).unwrap();
    text.push_str("\n[Mine]\nkept=yes\n");
    std::fs::write(conf(&w), &text).unwrap();
    let row = w.rows.get(ROW).unwrap().unwrap();
    m.regenerate(&qbt())
        .await
        .unwrap()
        .finished()
        .await
        .unwrap();
    let after = std::fs::read_to_string(conf(&w)).unwrap();
    assert_ne!(w.rows.get(ROW).unwrap().unwrap(), row);
    let hash = |t: &str| {
        t.lines()
            .find(|l| l.starts_with("WebUI\\Password_PBKDF2"))
            .map(str::to_owned)
    };
    assert!(hash(&after).is_some());
    assert_ne!(hash(&after), hash(&text));
    assert!(after.contains("kept=yes"));
    assert_eq!(w.run.started.lock().unwrap().len(), 2);
    assert_eq!(w.api.calls.load(Ordering::SeqCst), 2);
    assert_eq!(health(&m), Some(Health::Healthy));
}

/// A rotation whose file can't be written puts the old row back, so the
/// row and the file still agree. The start after it writes the file too,
/// so it fails the same way and nothing runs on a file Lodestar couldn't
/// write.
#[tokio::test]
async fn a_regenerate_that_cant_write_the_file_keeps_the_old_credential() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    let row = w.rows.get(ROW).unwrap().unwrap();
    std::fs::remove_file(conf(&w)).unwrap();
    std::fs::create_dir(conf(&w)).unwrap();
    let got = m.regenerate(&qbt()).await.unwrap().finished().await;
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "files",
            code: "write"
        })
    );
    assert_eq!(w.rows.get(ROW).unwrap().unwrap(), row);
    assert_eq!(w.run.started.lock().unwrap().len(), 1);
    assert_eq!(health(&m), Some(Health::Unhealthy));
}

/// A journal that still holds an operation (one a crash interrupted,
/// before recovery) refuses a restart: only recovery may touch it.
#[tokio::test]
async fn a_journal_mid_operation_refuses_a_restart() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    let mut rec = state::load(&w.folder()).unwrap().unwrap();
    rec.op = Some(state::Op {
        kind: state::OpKind::Update,
        version: Some(Version::parse("5.0.3").unwrap()),
        step: Step::Staged,
    });
    state::save(&w.folder(), &rec).unwrap();
    let busy = Some(ManagerError::Refused(Refusal::Busy));
    assert_eq!(m.restart(&qbt()).await.err(), busy);
    assert_eq!(m.regenerate(&qbt()).await.err(), busy);
    // A removal too, before it releases anything (#679's review,
    // Important 3).
    assert_eq!(m.remove(&qbt()).await, refused(Refusal::Busy));
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 0);
    assert!(w.folder().exists());
}

async fn wait_count(n: &AtomicUsize, at_least: usize) {
    let got = tokio::time::timeout(WAIT, async {
        while n.load(Ordering::SeqCst) < at_least {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(got.is_ok(), "never reached {at_least}");
}

/// Running again after a hold is healthy only once a health check passes:
/// the supervisor says running as soon as it has spawned it.
#[tokio::test]
async fn running_again_is_healthy_only_after_a_check() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    w.run.run().send_replace(status(State::Held));
    wait_health(&m, Health::Unhealthy).await;
    w.run.sick.store(true, Ordering::SeqCst);
    let checks = w.run.checks.load(Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    wait_count(&w.run.checks, checks + 1).await;
    assert_eq!(health(&m), Some(Health::Unhealthy));
    w.run.sick.store(false, Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    wait_health(&m, Health::Healthy).await;
}

/// #608's re-assert holds for a start that failed its health check (held
/// for a route that was down): it's configured once it runs, before it
/// shows healthy, and stopped if that fails (Q8).
#[tokio::test]
async fn a_start_held_for_its_route_is_configured_once_it_runs() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    w.run.sick.store(true, Ordering::SeqCst);
    let calls = w.api.calls.load(Ordering::SeqCst);
    assert!(m.restart(&qbt()).await.unwrap().finished().await.is_err());
    assert_eq!(
        w.api.calls.load(Ordering::SeqCst),
        calls,
        "configured without health"
    );
    w.run.sick.store(false, Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    wait_health(&m, Health::Healthy).await;
    assert_eq!(w.api.calls.load(Ordering::SeqCst), calls + 1);

    // A configuration that fails stops it.
    w.run.sick.store(true, Ordering::SeqCst);
    assert!(m.restart(&qbt()).await.unwrap().finished().await.is_err());
    w.api.fail.store(true, Ordering::SeqCst);
    w.run.sick.store(false, Ordering::SeqCst);
    let stops = w.run.stops.load(Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    wait_count(&w.run.stops, stops + 1).await;
    assert_eq!(health(&m), Some(Health::Unhealthy));
    assert!(w.run.status.lock().unwrap().is_none());
}

/// A crash between a regenerate's row and its file heals at the next
/// start: the stored credential is written into the file again.
#[tokio::test]
async fn a_start_writes_the_stored_credential_into_the_file_again() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    let good = std::fs::read_to_string(conf(&w)).unwrap();
    let line = |t: &str| {
        t.lines()
            .find(|l| l.starts_with("WebUI\\Password_PBKDF2"))
            .map(str::to_owned)
            .unwrap()
    };
    let stale = good.replace(
        &line(&good),
        "WebUI\\Password_PBKDF2=\"@ByteArray(AAAA:BBBB)\"",
    );
    std::fs::write(conf(&w), &stale).unwrap();
    m.restart(&qbt()).await.unwrap().finished().await.unwrap();
    let now = std::fs::read_to_string(conf(&w)).unwrap();
    assert_ne!(line(&now), line(&stale));
}

/// Parks every download until released, then refuses it.
#[derive(Default)]
struct Gate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl Source for Gate {
    fn open<'a>(&'a self, _: &'a Url) -> BoxFuture<'a, Result<Opened, FetchError>> {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(Opened {
                status: 500,
                location: None,
                body: Box::new(Once(None)),
            })
        })
    }
}

/// #660's L1: a companion that has to be configured once it runs, while
/// an operation holds its slot (an update still downloading), waits for
/// the slot instead of giving up and running unconfigured. Here the
/// update fails before it touches the companion, which then is
/// configured and shows healthy.
#[tokio::test]
async fn a_companion_waiting_for_the_slot_is_configured_when_it_frees() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let first = w.manager(catalogue("5.0.3"), LINUX);
    first.install(&token("5.0.3")).await.unwrap();
    drop(first);
    // A fresh serve, with an update pinned.
    w.run.status.lock().unwrap().take();
    let gate = Arc::new(Gate::default());
    let mut parts = w.parts(catalogue("5.0.4"), LINUX);
    parts.downloader = Downloader::any(gate.clone());
    let m = Arc::new(Manager::open(parts).unwrap());
    w.run.sick.store(true, Ordering::SeqCst);
    assert!(m.start(&qbt()).await.is_err());
    let calls = w.api.calls.load(Ordering::SeqCst);
    let update = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.4")).await }
    });
    tokio::time::timeout(WAIT, gate.entered.notified())
        .await
        .expect("the update never began its download");
    // It runs now, while the update holds the slot.
    w.run.sick.store(false, Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    gate.release.notify_one();
    let failed = tokio::time::timeout(WAIT, update).await.unwrap().unwrap();
    assert!(failed.is_err(), "{failed:?}");
    wait_health(&m, Health::Healthy).await;
    assert_eq!(w.api.calls.load(Ordering::SeqCst), calls + 1);
}

/// #660's L1, past the deadline: a slot still held when it ends leaves
/// the companion stopped and unhealthy (Q8), never running unconfigured.
#[tokio::test]
async fn a_slot_held_past_the_deadline_stops_the_companion() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let first = w.manager(catalogue("5.0.3"), LINUX);
    first.install(&token("5.0.3")).await.unwrap();
    drop(first);
    w.run.status.lock().unwrap().take();
    let gate = Arc::new(Gate::default());
    let mut parts = w.parts(catalogue("5.0.4"), LINUX);
    parts.downloader = Downloader::any(gate.clone());
    let m = Arc::new(Manager::open(parts).unwrap());
    w.run.sick.store(true, Ordering::SeqCst);
    assert!(m.start(&qbt()).await.is_err());
    // Shortened only now: the start writes the credential's hash, which
    // can take longer than this on a slow runner (#672's review).
    m.set_live_deadline(std::time::Duration::from_secs(1));
    let calls = w.api.calls.load(Ordering::SeqCst);
    let update = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.4")).await }
    });
    tokio::time::timeout(WAIT, gate.entered.notified())
        .await
        .expect("the update never began its download");
    let stops = w.run.stops.load(Ordering::SeqCst);
    w.run.sick.store(false, Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    wait_count(&w.run.stops, stops + 1).await;
    assert_eq!(health(&m), Some(Health::Unhealthy));
    assert_eq!(w.api.calls.load(Ordering::SeqCst), calls);
    gate.release.notify_one();
    let _ = tokio::time::timeout(WAIT, update).await.unwrap();
}

/// A configuration that never answers is bounded by the deadline
/// (`timed_out`), and the companion is stopped: here the follower's, once
/// a companion held at its restart runs.
#[tokio::test]
async fn a_configuration_that_never_answers_times_out_and_stops_it() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    // A restart that fails its health check (held, say), so it's left for
    // its follower to configure once it runs. Under the full deadline: the
    // start writes the credential's hash, which can take longer than the
    // short one on a slow runner (#672's review).
    w.run.sick.store(true, Ordering::SeqCst);
    assert!(m.restart(&qbt()).await.unwrap().finished().await.is_err());
    assert_eq!(health(&m), Some(Health::Unhealthy));
    // Now only the configuration is bounded by the short deadline.
    m.set_live_deadline(std::time::Duration::from_secs(1));
    w.api.hang.store(true, Ordering::SeqCst);
    w.run.sick.store(false, Ordering::SeqCst);
    let stops = w.run.stops.load(Ordering::SeqCst);
    w.run.run().send_replace(status(State::Running));
    wait_count(&w.run.stops, stops + 1).await;
    assert_eq!(health(&m), Some(Health::Unhealthy));
    assert!(w.run.status.lock().unwrap().is_none());
}

/// The password the companion's row holds now, opened.
fn stored_password(w: &World) -> String {
    let key = lodestar_companions::credentials::CredentialKey::new(
        qbt(),
        lodestar_companions::credentials::Field::WebUiPassword,
    );
    let sealed = w.rows.get(&key.row_key()).unwrap().unwrap();
    lodestar_companions::credentials::open(&w.secrets, &key, &sealed)
        .unwrap()
        .expose()
        .to_owned()
}

/// X-cmp-native-handover: an install hands qBittorrent over as the
/// managed download client, with the credential it generated and stored,
/// before it counts as installed; a restart hands it over again.
#[tokio::test]
async fn an_install_hands_qbittorrent_over_with_its_stored_password() {
    let _g = SERIAL.lock().await;
    let lines = capture();
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    assert_eq!(w.api.sink.passwords(), [stored_password(&w)]);
    m.restart(&qbt()).await.unwrap().finished().await.unwrap();
    assert_eq!(w.api.sink.passwords().len(), 2);
    assert_eq!(w.api.sink.passwords()[1], stored_password(&w));
    // The hand-over is logged, by name only.
    let lines = lines.0.lock().unwrap().clone();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("companions.config_handed_over")),
        "{lines:?}"
    );
    for l in &lines {
        assert!(!l.contains(&stored_password(&w)), "{l}");
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

/// Every companion event, as logged in this test binary.
fn capture() -> &'static Capture {
    static SINK: std::sync::OnceLock<Capture> = std::sync::OnceLock::new();
    let sink = SINK.get_or_init(|| Capture(Mutex::new(Vec::new())));
    let _ = lodestar_log::set_sink(sink);
    for e in lodestar_companions::log_events::CATALOGUE.events {
        e.set_on(true);
    }
    sink
}

/// A hand-over that fails fails the install, which is undone as any
/// failed step is: nothing is installed, no credential row is left, and
/// what a hand-over may have saved is released. It's never reported as
/// installed.
#[tokio::test]
async fn a_failed_hand_over_fails_and_undoes_the_install() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    w.api.refuse_hand_over.store(true, Ordering::SeqCst);
    let got = m.install(&token("5.0.3")).await;
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "handover",
            code: "sink_refused"
        })
    );
    assert_eq!(w.on_disk(), None);
    assert!(w.rows.keys().is_empty());
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 1);
    assert!(health(&m).is_none());
    // Tried again once the settings take it: installed and handed over.
    w.api.refuse_hand_over.store(false, Ordering::SeqCst);
    m.install(&token("5.0.3")).await.unwrap();
    assert_eq!(w.api.sink.passwords(), [stored_password(&w)]);
}

/// Regenerate hands the new credential over in the same step, so
/// Integrations holds what the companion's file and row hold. One whose
/// hand-over fails says so; a restart tries again.
#[tokio::test]
async fn regenerate_hands_the_new_credential_over() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    m.install(&token("5.0.3")).await.unwrap();
    let old = stored_password(&w);
    m.regenerate(&qbt())
        .await
        .unwrap()
        .finished()
        .await
        .unwrap();
    let new = stored_password(&w);
    assert_ne!(new, old);
    assert_eq!(w.api.sink.passwords().last(), Some(&new));
    w.api.refuse_hand_over.store(true, Ordering::SeqCst);
    let got = m.regenerate(&qbt()).await.unwrap().finished().await;
    assert!(
        matches!(
            got,
            Err(ManagerError::Step {
                step: "handover",
                ..
            })
        ),
        "{got:?}"
    );
    w.api.refuse_hand_over.store(false, Ordering::SeqCst);
    m.restart(&qbt()).await.unwrap().finished().await.unwrap();
    assert_eq!(w.api.sink.passwords().last(), Some(&stored_password(&w)));
}

/// A removal takes the managed entry out first; one the settings refuse
/// leaves everything installed, to try again.
#[tokio::test]
async fn a_removal_releases_the_managed_entry_first() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    // Nothing installed: nothing is released.
    assert_eq!(m.remove(&qbt()).await, refused(Refusal::NotInstalled));
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 0);
    m.install(&token("5.0.3")).await.unwrap();
    w.api.refuse_release.store(true, Ordering::SeqCst);
    let got = m.remove(&qbt()).await;
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "release",
            code: "sink_refused"
        })
    );
    assert!(w.on_disk().is_some());
    assert_eq!(w.rows.keys(), [ROW]);
    w.api.refuse_release.store(false, Ordering::SeqCst);
    m.remove(&qbt()).await.unwrap();
    assert_eq!(w.on_disk(), None);
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 2);
}

/// A first install that fails before its hand-over leaves Lodestar's
/// settings as they are: an entry there isn't this install's to take out.
#[tokio::test]
async fn an_install_that_fails_before_its_hand_over_releases_nothing() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    w.api.fail.store(true, Ordering::SeqCst);
    assert!(m.install(&token("5.0.3")).await.is_err());
    assert_eq!(w.on_disk(), None);
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 0);
    assert!(w.api.sink.passwords().is_empty());
}

/// An install whose hand-over can't happen is refused before anything is
/// downloaded or started, and only a first install is asked about an
/// entry already there.
#[tokio::test]
async fn an_install_that_cant_be_handed_over_is_refused_before_it_begins() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let m = w.manager(catalogue("5.0.3"), LINUX);
    w.api.refuse_pre_check.store(true, Ordering::SeqCst);
    let got = m.install(&token("5.0.3")).await;
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "handover",
            code: "managed_client_in_use"
        })
    );
    assert!(!w.folder().exists());
    assert!(w.run.started.lock().unwrap().is_empty());
    assert_eq!(*w.api.asked.lock().unwrap(), [true]);
}

/// #679's I2: a crash between a first install's hand-over and its
/// `configured` step leaves a marker. Recovery undoes the install, and
/// once the settings can be reached the managed entry it may have saved is
/// released and the marker goes. An install that finishes leaves none.
#[tokio::test]
async fn a_crash_after_the_hand_over_is_released_once_the_settings_are_there() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let marker = w.data.path().join("companions/.handed-over-qbittorrent");
    let cat = catalogue("5.0.3");
    let m = w.manager(cat, LINUX);
    m.crash_at(Some(Fault::After(Step::Configured)));
    assert_eq!(m.install(&token("5.0.3")).await, Err(ManagerError::Crashed));
    assert_eq!(w.api.sink.passwords().len(), 1);
    assert!(marker.is_file());
    drop(m);
    let m = w.manager(cat, LINUX);
    let got = m.recover().await;
    assert!(got.iter().all(|(_, r)| matches!(r, Ok(None))), "{got:?}");
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 0);
    // The sink is wired: released, and the marker goes.
    m.release_pending().await;
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 1);
    assert!(!marker.exists());
    // Nothing left to release a second time.
    m.release_pending().await;
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 1);
    // A release that fails keeps the marker for the next start.
    m.crash_at(Some(Fault::After(Step::Configured)));
    assert_eq!(m.install(&token("5.0.3")).await, Err(ManagerError::Crashed));
    drop(m);
    let m = w.manager(cat, LINUX);
    m.recover().await;
    w.api.refuse_release.store(true, Ordering::SeqCst);
    m.release_pending().await;
    assert!(marker.is_file());
    w.api.refuse_release.store(false, Ordering::SeqCst);
    drop(m);
    let m = w.manager(cat, LINUX);
    m.recover().await;
    m.release_pending().await;
    assert!(!marker.exists());
    // A first install that finishes leaves none.
    m.install(&token("5.0.3")).await.unwrap();
    assert!(!marker.exists());
}

/// A pending release is skipped once the companion is installed again
/// before the sink is wired: its entry is that install's now, and it stays.
#[tokio::test]
async fn a_pending_release_spares_a_companion_installed_since() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let cat = catalogue("5.0.3");
    let m = w.manager(cat, LINUX);
    m.crash_at(Some(Fault::After(Step::Configured)));
    assert_eq!(m.install(&token("5.0.3")).await, Err(ManagerError::Crashed));
    drop(m);
    let m = w.manager(cat, LINUX);
    m.recover().await;
    m.install(&token("5.0.3")).await.unwrap();
    m.release_pending().await;
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 0);
    assert!(w.on_disk().is_some());
}

/// A live undo removes the marker once it has released, and keeps it when
/// the release fails, so the next start finishes it. A marker that can't
/// be written fails the install before anything is handed over.
#[tokio::test]
async fn a_live_undo_keeps_the_marker_only_when_its_release_fails() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let marker = w.data.path().join("companions/.handed-over-qbittorrent");
    let m = w.manager(catalogue("5.0.3"), LINUX);
    w.api.refuse_hand_over.store(true, Ordering::SeqCst);
    assert!(m.install(&token("5.0.3")).await.is_err());
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 1);
    assert!(!marker.exists());
    w.api.refuse_release.store(true, Ordering::SeqCst);
    assert!(m.install(&token("5.0.3")).await.is_err());
    assert!(marker.is_file(), "the next start couldn't release it");
    w.api.refuse_release.store(false, Ordering::SeqCst);
    std::fs::remove_file(&marker).unwrap();
    // A marker that can't be written: nothing is handed over.
    w.api.refuse_hand_over.store(false, Ordering::SeqCst);
    std::fs::create_dir(&marker).unwrap();
    let got = m.install(&token("5.0.3")).await;
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "handover",
            code: "marker"
        })
    );
    assert!(w.api.sink.passwords().is_empty());
    assert_eq!(w.on_disk(), None);
}

/// #692's review, Important 2 and N1: a hand-over past its bound (three
/// of the live deadline, shortened here) is logged there and then waited
/// for, holding the slot, since it may still save. Once it has settled the
/// install fails `timed_out` and its undo releases what it saved, so no
/// marker is left and nothing lands later.
#[tokio::test]
async fn a_hand_over_past_its_bound_is_waited_for_then_undone() {
    let _g = SERIAL.lock().await;
    let lines = capture();
    let w = World::new();
    let marker = w.data.path().join("companions/.handed-over-qbittorrent");
    let m = Arc::new(w.manager(catalogue("5.0.3"), LINUX));
    m.set_live_deadline(std::time::Duration::from_millis(200));
    w.api.hang_hand_over.store(true, Ordering::SeqCst);
    let timed_out = |l: &String| {
        l.contains("companions.install_failed") && l.contains("handover") && l.contains("timed_out")
    };
    let seen = lines
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|l| timed_out(l))
        .count();
    let install = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.3")).await }
    });
    let logged = tokio::time::timeout(WAIT, async {
        while lines
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|l| timed_out(l))
            .count()
            == seen
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(logged.is_ok(), "the hand-over never passed its bound");
    assert!(!w.api.hand_over_dropped.load(Ordering::SeqCst));
    assert!(!install.is_finished());
    assert!(marker.is_file());
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 0);
    w.api.hand_over_gate.notify_one();
    let got = tokio::time::timeout(WAIT, install).await.unwrap().unwrap();
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "handover",
            code: "timed_out"
        })
    );
    assert!(!w.api.hand_over_dropped.load(Ordering::SeqCst));
    assert_eq!(w.api.releases.load(Ordering::SeqCst), 1);
    assert!(!marker.exists());
    assert_eq!(w.on_disk(), None);
    // Logged once, at the bound.
    let n = lines
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|l| timed_out(l))
        .count();
    assert_eq!(n, seen + 1);
}

/// #692's review, Important 3: a pending release that passes its deadline
/// is still waited for, under the slot (and serve's guided lock), so
/// nothing else writes the entry while it may still save; it keeps its
/// marker, and the next start releases what's left.
#[tokio::test]
async fn a_release_past_its_deadline_is_waited_for_and_keeps_its_marker() {
    let _g = SERIAL.lock().await;
    let lines = capture();
    let w = World::new();
    let marker = w.data.path().join("companions/.handed-over-qbittorrent");
    let cat = catalogue("5.0.3");
    let m = w.manager(cat, LINUX);
    m.crash_at(Some(Fault::After(Step::Configured)));
    assert_eq!(m.install(&token("5.0.3")).await, Err(ManagerError::Crashed));
    drop(m);
    let m = Arc::new(w.manager(cat, LINUX));
    m.recover().await;
    m.set_live_deadline(std::time::Duration::from_millis(200));
    w.api.hold_release.store(true, Ordering::SeqCst);
    let timed_out = |l: &String| {
        l.contains("companions.install_failed") && l.contains("release") && l.contains("timed_out")
    };
    let seen = lines
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|l| timed_out(l))
        .count();
    let pending = tokio::spawn({
        let m = m.clone();
        async move { m.release_pending().await }
    });
    // Past the deadline: logged, and still waited for, not dropped.
    let logged = tokio::time::timeout(WAIT, async {
        while lines
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|l| timed_out(l))
            .count()
            == seen
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(logged.is_ok(), "the release never timed out");
    assert!(!w.api.release_dropped.load(Ordering::SeqCst));
    assert!(!pending.is_finished());
    // Its slot is still held: nothing else can change the companion.
    assert_eq!(
        m.install(&token("5.0.3")).await,
        Err(ManagerError::Refused(Refusal::Busy))
    );
    w.api.release_gate.notify_one();
    tokio::time::timeout(WAIT, pending).await.unwrap().unwrap();
    assert!(!w.api.release_dropped.load(Ordering::SeqCst));
    assert!(marker.is_file(), "a late release kept no marker");
    // Logged once, at the bound, not again when it finally answers
    // (#692's review, N1).
    let n = lines
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|l| timed_out(l))
        .count();
    assert_eq!(n, seen + 1);
    // The next start releases what's left, and the marker goes.
    w.api.hold_release.store(false, Ordering::SeqCst);
    drop(m);
    let m = w.manager(cat, LINUX);
    m.recover().await;
    m.release_pending().await;
    assert!(!marker.exists());
}

/// Lines the capture holds that match `f`.
fn count(lines: &Capture, f: impl Fn(&String) -> bool) -> usize {
    lines.0.lock().unwrap().iter().filter(|l| f(l)).count()
}

/// Waits, with a deadline, until `f` matches more lines than `seen`.
async fn logged_past(lines: &Capture, seen: usize, f: impl Fn(&String) -> bool) {
    let got = tokio::time::timeout(WAIT, async {
        while count(lines, &f) == seen {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(got.is_ok(), "nothing was logged");
}

/// #692's review, N1: an undone first install whose release passes its
/// deadline is logged once, at the bound (`release`, `timed_out`), and not
/// again as `rollback_failed`.
#[tokio::test]
async fn an_undo_whose_release_passes_its_deadline_is_logged_once() {
    let _g = SERIAL.lock().await;
    let lines = capture();
    let w = World::new();
    let m = Arc::new(w.manager(catalogue("5.0.3"), LINUX));
    m.set_live_deadline(std::time::Duration::from_millis(200));
    // The hand-over is tried and refused, so the undo releases it, slowly.
    w.api.refuse_hand_over.store(true, Ordering::SeqCst);
    w.api.hold_release.store(true, Ordering::SeqCst);
    let at_bound = |l: &String| {
        l.contains("companions.install_failed") && l.contains("release") && l.contains("timed_out")
    };
    let rollback =
        |l: &String| l.contains("companions.install_failed") && l.contains("rollback_failed");
    let (seen, seen_rb) = (count(lines, at_bound), count(lines, rollback));
    let install = tokio::spawn({
        let m = m.clone();
        async move { m.install(&token("5.0.3")).await }
    });
    logged_past(lines, seen, at_bound).await;
    w.api.release_gate.notify_one();
    let got = tokio::time::timeout(WAIT, install).await.unwrap().unwrap();
    assert!(got.is_err(), "{got:?}");
    assert_eq!(count(lines, at_bound), seen + 1);
    assert_eq!(count(lines, rollback), seen_rb);
}

/// #692's review, N1: a restart whose hand-over passes its bound is logged
/// once, at the bound (`handover`, `timed_out`), and not again under the
/// restart's own step.
#[tokio::test]
async fn a_restart_whose_hand_over_passes_its_bound_is_logged_once() {
    let _g = SERIAL.lock().await;
    let lines = capture();
    let w = World::new();
    let m = Arc::new(w.manager(catalogue("5.0.3"), LINUX));
    m.install(&token("5.0.3")).await.unwrap();
    m.set_live_deadline(std::time::Duration::from_millis(200));
    w.api.hang_hand_over.store(true, Ordering::SeqCst);
    let at_bound = |l: &String| {
        l.contains("companions.install_failed") && l.contains("handover") && l.contains("timed_out")
    };
    let as_restart = |l: &String| {
        l.contains("companions.install_failed") && l.contains("restart") && l.contains("timed_out")
    };
    let (seen, seen_restart) = (count(lines, at_bound), count(lines, as_restart));
    let restart = tokio::spawn({
        let m = m.clone();
        async move { m.restart(&qbt()).await.unwrap().finished().await }
    });
    logged_past(lines, seen, at_bound).await;
    w.api.hand_over_gate.notify_one();
    let got = tokio::time::timeout(WAIT, restart).await.unwrap().unwrap();
    assert_eq!(
        got,
        Err(ManagerError::Step {
            step: "handover",
            code: "timed_out"
        })
    );
    assert_eq!(count(lines, at_bound), seen + 1);
    assert_eq!(count(lines, as_restart), seen_restart);
}

/// A companion this build doesn't hand over writes no marker: nothing it
/// saved needs releasing.
#[tokio::test]
async fn only_a_companion_that_is_handed_over_is_marked() {
    let _g = SERIAL.lock().await;
    let w = World::new();
    let marker = w.data.path().join("companions/.handed-over-qbittorrent");
    let m = w.manager(catalogue("5.0.3"), LINUX);
    w.api.no_hand_over.store(true, Ordering::SeqCst);
    m.crash_at(Some(Fault::After(Step::Configured)));
    assert_eq!(m.install(&token("5.0.3")).await, Err(ManagerError::Crashed));
    assert!(!marker.exists());
}
