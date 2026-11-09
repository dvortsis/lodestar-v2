//! Self-updates (architecture §12.2), on Unix:
//!
//! - **Part 2:** `lodestar serve` as the supervisor of an overlay child,
//!   probation with journaled rollback, and the child's side of it.
//!   Nothing stages an overlay outside tests yet, so the supervisor runs
//!   the image's own code as its child.
//! - **Part 3 of 5:** the pure pieces the update engine is built on: the
//!   window, the policy, the trust file and the signed check's fetch.
//! - **Part 4 of 5:** the [`engine`] in `serve --child` (the scheduled and
//!   manual checks, the policy over what they found), `/updates`, the
//!   `updates` SSE topic and `/system`'s Updates row; [`unavailable`]
//!   answers for a fixture and Windows. Part 5 installs.
//!
//! `<data>/bin`'s files and who writes them are in [`layout`]; the exit
//! codes and the request file in [`control`]; probation's accounting in
//! [`pending`]; the rollback steps in [`journal`].
//!
//! **Windows** (PK-win-upd-1): [`windows::check`] checks and notifies
//! with the same [`window`], [`policy`], [`check`] request and
//! [`trust_file`] (over a Windows [`layout`]); installing waits for
//! PK-win-upd-2 and PK-win-portable-upd, so Install and Roll back answer
//! `not_built_yet`. Off Unix only those, [`log_events`] and
//! [`unavailable`] (a fixture) compile.

pub mod check;
#[cfg(unix)]
pub mod control;
// Shared with Windows (the MSI and the portable build).
#[cfg_attr(not(unix), allow(dead_code))]
pub mod download;
#[cfg(unix)]
pub mod engine;
#[cfg(unix)]
pub mod history;
#[cfg(unix)]
pub mod install;
#[cfg(unix)]
pub mod journal;
// Off Unix the overlay and supervisor halves aren't used.
#[cfg_attr(not(unix), allow(dead_code))]
pub mod layout;
pub mod log_events;
#[cfg(unix)]
pub mod overlay;
#[cfg(unix)]
pub mod pending;
// Off Unix Install now's rules (`install_now`) aren't used yet.
#[cfg_attr(not(unix), allow(dead_code))]
pub mod policy;
#[cfg(unix)]
pub mod probation;
#[cfg(unix)]
pub mod rollback_cache;
#[cfg(unix)]
pub mod supervisor;
#[cfg(unix)]
pub mod testing;
pub mod trust_file;
pub mod unavailable;
pub mod window;
// Compiled and tested everywhere; started only on Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod windows;

#[cfg(unix)]
use lodestar_update::semver::Version;
#[cfg(unix)]
use lodestar_update::{Embedded, PublicKey};

/// This build's version (`LODESTAR_BUILD_VERSION`), or the debug-only
/// pretend one.
#[cfg(unix)]
pub fn build_version() -> Version {
    testing::version()
        .and_then(layout::parse_version)
        .or_else(|| Version::parse(env!("LODESTAR_BUILD_VERSION")).ok())
        .unwrap_or_else(|| Version::new(0, 0, 0))
}

/// The keys overlays are verified with: the embedded ones, or the
/// debug-only test root.
#[cfg(unix)]
pub fn trust_root() -> Result<Embedded, lodestar_update::Error> {
    let Some(t) = testing::trust_root() else {
        return Embedded::builtin();
    };
    let key = |k: &Option<String>| k.as_deref().map(PublicKey::from_base64).transpose();
    Ok(Embedded {
        recovery: PublicKey::from_base64(&t.recovery)?,
        release: key(&t.release)?,
        nightly: key(&t.nightly)?,
        freshness: key(&t.freshness)?,
        statements: t.statements.clone(),
    })
}

/// The update engine as the server starts it: what `/updates` answers
/// with, `/system`'s Updates row, and the scheduler to stop.
pub struct Started {
    pub api: std::sync::Arc<dyn lodestar_api::updates::UpdatesApi>,
    pub row: Option<crate::system::UpdatesRow>,
    pub task: Option<(
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    )>,
}

/// Starts the engine, except for a fixture (served offline: nothing is
/// checked). On Windows it's the [`windows::check`] notifier, which
/// installs nothing yet. `busy` names what keeps an install waiting (a
/// rebuild under way).
pub fn start(
    config: &crate::config::Config,
    settings: &crate::settings::Registry,
    events: &std::sync::Arc<lodestar_api::events::EventHub>,
    busy: Box<dyn Fn() -> Option<&'static str> + Send + Sync>,
) -> Started {
    // The build's own version: a nightly keeps its suffix.
    let version = env!("LODESTAR_BUILD_VERSION").to_string();
    let unavailable = |why: lodestar_api::updates::UpdatesUnavailable| Started {
        api: std::sync::Arc::new(unavailable::Unavailable {
            why,
            version: version.clone(),
        }),
        row: None,
        task: None,
    };
    if crate::fixture::active() {
        return unavailable(lodestar_api::updates::UpdatesUnavailable::Fixture);
    }
    #[cfg(windows)]
    {
        use self::windows::check as win;
        let _ = busy;
        let Ok(fetch) = check::Guarded::new() else {
            return unavailable(lodestar_api::updates::UpdatesUnavailable::Network);
        };
        let ctx = win::Context {
            data_dir: config.data_dir.clone(),
            running: lodestar_update::semver::Version::parse(&version)
                .unwrap_or_else(|_| lodestar_update::semver::Version::new(0, 0, 0)),
            repository: env!("CARGO_PKG_REPOSITORY").to_string(),
            dev: crate::system::info::channel() == lodestar_api::system::Channel::Dev,
            kind: win::detect(&win::signals(&config.data_dir)),
            target: lodestar_update::Target::from_triple(env!("LODESTAR_BUILD_TARGET")),
            checked_data: crate::windows::service::same_dir(
                &config.data_dir,
                &crate::windows::service::default_data_dir(std::env::var("ProgramData").ok()),
            ),
        };
        let notifier = win::Notifier::new(
            ctx,
            settings.subscribe(),
            events.clone(),
            std::sync::Arc::new(fetch),
        );
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let task = notifier.spawn(stopped);
        let row_notifier = notifier.clone();
        let row: crate::system::UpdatesRow = std::sync::Arc::new(move || row_notifier.summary());
        Started {
            api: std::sync::Arc::new(notifier),
            row: Some(row),
            task: Some((stop, task)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (config, settings, events, busy);
        unavailable(lodestar_api::updates::UpdatesUnavailable::WindowsHelper)
    }
    #[cfg(unix)]
    {
        let (Ok(fetch), Ok(source)) = (check::Guarded::new(), download::Guarded::new()) else {
            return unavailable(lodestar_api::updates::UpdatesUnavailable::Network);
        };
        let child = probation::current();
        let hint = child.and_then(|c| c.hint.as_ref());
        let running = child
            .and_then(|c| layout::parse_version(&c.version))
            .unwrap_or_else(build_version);
        let ctx = engine::Context {
            data_dir: config.data_dir.clone(),
            running,
            image: hint
                .and_then(|h| layout::parse_version(&h.image_version))
                .unwrap_or_else(build_version),
            supervised: child.is_some_and(|c| c.supervised),
            notices: hint.map(|h| h.notices.clone()).unwrap_or_default(),
            repository: env!("CARGO_PKG_REPOSITORY").to_string(),
            db: config.db_path(),
            dev: crate::system::info::channel() == lodestar_api::system::Channel::Dev,
        };
        let engine = engine::Engine::new(
            ctx,
            settings.subscribe(),
            events.clone(),
            std::sync::Arc::new(fetch),
            engine::Installer::new(std::sync::Arc::new(source), busy),
        );
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let task = engine.spawn(stopped);
        let row_engine = engine.clone();
        let row: crate::system::UpdatesRow = std::sync::Arc::new(move || row_engine.summary());
        Started {
            api: std::sync::Arc::new(engine),
            row: Some(row),
            task: Some((stop, task)),
        }
    }
}
