//! The log filter, reloadable while the server runs: `main` installs
//! the subscriber through [`init`], and [`follow`] applies each saved
//! change to General's `log_level`.

use std::sync::OnceLock;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, reload};

use super::general::General;

/// The installed subscriber's filter.
static FILTER: OnceLock<reload::Handle<EnvFilter, Registry>> = OnceLock::new();

/// `filter` as a `tracing` filter, or why it isn't one.
pub fn parse(filter: &str) -> Result<EnvFilter, String> {
    EnvFilter::try_new(filter).map_err(|e| format!("Not a log filter: {e}."))
}

/// Installs the process's subscriber (stderr) with `filter`, falling
/// back to `info` if it doesn't parse.
pub fn init(filter: &str) {
    let filter = parse(filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let (layer, handle) = reload::Layer::new(filter);
    tracing_subscriber::registry()
        .with(layer)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
    let _ = FILTER.set(handle);
}

/// Replaces the filter behind `handle`.
pub fn reload<S>(handle: &reload::Handle<EnvFilter, S>, filter: &str) -> Result<(), String> {
    let filter = parse(filter)?;
    handle.reload(filter).map_err(|e| e.to_string())
}

/// Applies the current `log_level` through `apply` (the installed
/// subscriber's, see [`follow`]), since `main` started with the file's,
/// and then each new one.
pub fn follow_with(
    mut rx: watch::Receiver<General>,
    mut apply: impl FnMut(&str) -> Result<(), String> + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut applied: Option<String> = None;
        loop {
            let want = rx.borrow_and_update().log_level.clone();
            if applied.as_ref() != Some(&want) {
                match apply(&want) {
                    Ok(()) if applied.is_some() => {
                        tracing::info!("settings: log filter is now {want:?}")
                    }
                    Ok(()) => {}
                    Err(e) => tracing::warn!("settings: log filter {want:?} not applied: {e}"),
                }
                applied = Some(want);
            }
            if rx.changed().await.is_err() {
                break;
            }
        }
    })
}

/// [`follow_with`] the subscriber [`init`] installed; without one
/// (tests, tools) changes are only saved.
pub fn follow(rx: watch::Receiver<General>) -> JoinHandle<()> {
    follow_with(rx, |filter| match FILTER.get() {
        Some(handle) => reload(handle, filter),
        None => Ok(()),
    })
}
