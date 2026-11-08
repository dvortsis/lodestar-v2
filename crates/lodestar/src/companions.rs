//! The companion manager in the binary (CMP-5; plan
//! 2026-11-06-v06-companions §4.3; decisions
//! `docs/decisions/2026-11-08-CMP-5.md`):
//!
//! - [`StoreRows`]: companion credential rows in `settings`, through the
//!   store, each write one committed transaction;
//! - [`host`]: the platform, and whether Lodestar runs in a container (Q17);
//! - [`start`]: at serve, journal recovery for every companion before
//!   anything else touches them, and the snapshot channel CMP-10 reads,
//!   with whether any download client is set up.
//!
//! Starting installed companions at serve, behind their own route (Q18's
//! `companions.<id>.egress`), is CMP-5g's wiring; until then [`Unwired`]
//! refuses every start, so nothing runs on a route nobody chose.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lodestar_companions::adapter::{AdapterError, BoxFuture, Credentials, Target};
use lodestar_companions::configure::InstallReceipt;
use lodestar_companions::credentials::{CredentialKey, Field, SealedRow};
use lodestar_companions::fetch::{self, Guarded};
use lodestar_companions::log_events::INSTALL_FAILED;
use lodestar_companions::manager::{
    ApiConfigure, Downloader, Host, Launch, Manager, Parts, RowError, Rows, Run, Snapshot,
};
use lodestar_companions::{Catalogue, CompanionId};
use lodestar_integrations::net::Route;
use lodestar_integrations::secret::{SealedSecret, SecretStore};
use lodestar_store::Store;
use tokio::sync::watch;

/// Companion credential rows in the store's `settings` table.
#[derive(Debug, Clone)]
pub struct StoreRows(pub Arc<Mutex<Store>>);

impl Rows for StoreRows {
    fn put(&self, row: &SealedRow) -> Result<(), RowError> {
        let mut s = self.0.lock().map_err(|_| RowError)?;
        s.update_settings_raw(&[(&row.key, row.value.as_bytes())], &[])
            .map_err(|_| RowError)
    }

    fn get(&self, key: &str) -> Result<Option<SealedSecret>, RowError> {
        let s = self.0.lock().map_err(|_| RowError)?;
        let rows = s.settings_with_prefix_raw(key).map_err(|_| RowError)?;
        match rows.into_iter().find(|(k, _)| k == key) {
            None => Ok(None),
            Some((_, Err(bytes))) => SealedSecret::from_bytes(bytes)
                .map(Some)
                .map_err(|_| RowError),
            Some((_, Ok(_))) => Err(RowError),
        }
    }

    fn forget(&self, id: &CompanionId) -> Result<(), RowError> {
        let keys: Vec<String> = Field::ALL
            .iter()
            .map(|f| CredentialKey::new(id.clone(), *f).row_key())
            .collect();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let mut s = self.0.lock().map_err(|_| RowError)?;
        s.update_settings_raw(&[], &keys).map_err(|_| RowError)
    }
}

/// This platform, and whether it's a container. Stricter than the About
/// screen's detection, which lets `LODESTAR_DEPLOYMENT` say otherwise:
/// here any sign of one refuses native installs (Q17).
pub fn host() -> Host {
    let hinted =
        std::env::var("LODESTAR_DEPLOYMENT").is_ok_and(|d| matches!(d.trim(), "docker" | "unraid"));
    Host::this(hinted || in_container(|p| std::fs::read(p).ok()))
}

/// Docker's `/.dockerenv`, Podman's `/run/.containerenv`, a container
/// cgroup (v1), or `container=` in PID 1's environment (LXC,
/// systemd-nspawn, Podman). On cgroup v2 the cgroup alone says nothing.
fn in_container(read: impl Fn(&str) -> Option<Vec<u8>>) -> bool {
    let has = |p: &str| read(p).is_some();
    let cgroup = read("/proc/1/cgroup").is_some_and(|c| {
        let c = String::from_utf8_lossy(&c);
        c.contains("/docker") || c.contains("/kubepods") || c.contains("/containerd")
    });
    let env = read("/proc/1/environ")
        .is_some_and(|e| e.split(|b| *b == 0).any(|v| v.starts_with(b"container=")));
    has("/.dockerenv") || has("/run/.containerenv") || cgroup || env
}

/// Refuses every start until CMP-5g hands in the supervisor and each
/// companion's route. Stopping is a no-op: nothing was started.
#[derive(Debug)]
pub struct Unwired;

impl Run for Unwired {
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

impl ApiConfigure for Unwired {
    fn configure<'a>(
        &'a self,
        _: &'a CompanionId,
        _: &'a Target,
        _: InstallReceipt,
        _: &'a dyn Credentials,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async { Err(AdapterError::Refused("not_wired")) })
    }
}

/// The manager over this data folder and store. Downloads go direct, as
/// the updater's do (CMP-1's decision); nothing is started ([`Unwired`]).
pub fn manager(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    catalogue: &'static Catalogue,
) -> Result<Manager, OpenError> {
    let fail = |code: &'static str, message: String| OpenError { code, message };
    let secrets =
        SecretStore::open_or_create(&data).map_err(|e| fail("secrets", format!("secrets: {e}")))?;
    let client = fetch::client(&Route::Direct).map_err(|e| fail(e.code(), e.code().to_owned()))?;
    let downloader =
        Downloader::new(Guarded::new(client).map_err(|e| fail(e.code(), e.code().to_owned()))?);
    Manager::open(Parts {
        data,
        host: host(),
        catalogue,
        downloader,
        secrets: Arc::new(secrets),
        rows: Arc::new(StoreRows(store)),
        run: Arc::new(Unwired),
        api: Arc::new(Unwired),
    })
    .map_err(|e| match e {
        lodestar_companions::manager::ManagerError::Refused(_) => fail(
            "busy",
            "another Lodestar process is managing companions".to_owned(),
        ),
        e => fail(e.code(), e.code().to_owned()),
    })
}

/// Why the manager couldn't be opened: a stable code for the log, and a
/// sentence for the CLI.
#[derive(Debug)]
pub struct OpenError {
    pub code: &'static str,
    pub message: String,
}

/// What serve keeps: the manager, and the snapshot channel.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "CMP-5g starts companions and hands the snapshot on"
)]
pub struct Companions {
    pub manager: Arc<Manager>,
    /// CMP-10's input. TODO(CMP-5g): hand it to the pipeline as
    /// `PipelineConfig::companions` once CMP-10 has merged.
    pub snapshot: watch::Receiver<Snapshot>,
}

/// At serve: recovers what a crash interrupted, publishes the installed
/// companions, and follows whether any download client is set up. A bad
/// catalogue turns companions off for the run (CMP-0); a failed recovery
/// is logged by code and leaves that companion as it is.
pub async fn start(
    data: PathBuf,
    store: Arc<Mutex<Store>>,
    mut integrations: watch::Receiver<crate::settings::integrations::Integrations>,
) -> Option<Companions> {
    let catalogue = lodestar_companions::builtin().ok()?;
    let manager = match manager(data, store, catalogue) {
        Ok(m) => Arc::new(m),
        Err(e) => {
            // Companions are off for this run.
            lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = "all",
                version = "",
                step = "open",
                reason = e.code
            );
            return None;
        }
    };
    for (id, r) in manager.recover().await {
        if let Err(e) = r {
            let companion = catalogue
                .get(id.as_str())
                .map_or("unknown", |e| e.id.as_str());
            lodestar_log::log_event!(
                INSTALL_FAILED,
                companion = companion,
                version = "",
                step = "recover",
                reason = e.code()
            );
        }
    }
    let m = manager.clone();
    m.set_download_clients(!integrations.borrow_and_update().clients.is_empty());
    tokio::spawn(async move {
        while integrations.changed().await.is_ok() {
            let any = !integrations.borrow_and_update().clients.is_empty();
            m.set_download_clients(any);
        }
    });
    let snapshot = manager.snapshot();
    Some(Companions { manager, snapshot })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_companions::credentials::generate;

    #[test]
    fn containers_are_recognised() {
        let only = |path: &'static str, body: &'static [u8]| {
            move |p: &str| (p == path).then(|| body.to_vec())
        };
        assert!(in_container(only("/.dockerenv", b"")));
        assert!(in_container(only("/run/.containerenv", b"")));
        assert!(in_container(only(
            "/proc/1/cgroup",
            b"12:pids:/docker/abc\n"
        )));
        assert!(in_container(only(
            "/proc/1/environ",
            b"PATH=/bin\0container=lxc\0"
        )));
        assert!(!in_container(only("/proc/1/cgroup", b"0::/\n")));
        assert!(!in_container(only("/proc/1/environ", b"PATH=/bin\0")));
    }

    #[test]
    fn rows_round_trip_and_forget_only_their_companion() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("db"), Default::default()).unwrap();
        let rows = StoreRows(Arc::new(Mutex::new(store)));
        let secrets = SecretStore::from_key(&[3u8; 32]);
        let qbt = CompanionId::parse("qbittorrent").unwrap();
        let prowlarr = CompanionId::parse("prowlarr").unwrap();
        let a = generate(
            &secrets,
            &CredentialKey::new(qbt.clone(), Field::WebUiPassword),
        )
        .unwrap();
        let b = generate(
            &secrets,
            &CredentialKey::new(prowlarr.clone(), Field::ApiKey),
        )
        .unwrap();
        rows.put(&a.row).unwrap();
        rows.put(&b.row).unwrap();
        assert_eq!(rows.get(&a.row.key).unwrap(), Some(a.row.value.clone()));
        rows.forget(&qbt).unwrap();
        assert_eq!(rows.get(&a.row.key).unwrap(), None);
        assert_eq!(rows.get(&b.row.key).unwrap(), Some(b.row.value));
    }
}
