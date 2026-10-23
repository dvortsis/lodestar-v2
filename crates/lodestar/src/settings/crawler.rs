//! Crawler: the pause and the intensity (both live) and the metadata
//! fetch limits.
//!
//! A new intensity reaches the running DHT node at once (its query and
//! reply rates and its cap on outstanding queries). The fetch pool's
//! socket count, which follows the intensity when `max_sockets` is 0
//! (or unset in `config.toml`), takes it at the next start.
//!
//! `paused` is the crawler's pause (`POST /crawler/pause`, B14): saved
//! here so it survives a restart, applied to the running pipeline by
//! [`follow_control`], and never set in `config.toml` or the
//! environment.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::NodeHandle;
use lodestar_pipeline::Control;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::{Config, EncryptionName, IntensityName};

/// How hard the DHT crawler works (architecture §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Intensity {
    Low,
    Balanced,
    Aggressive,
}

/// Message stream encryption for metadata fetches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Encryption {
    Prefer,
    Require,
    Disable,
}

/// The intensity applies at once to the DHT; the fetch pool follows at
/// the next start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Crawler {
    // Undocumented here: a field's own description would wrap the
    // inlined enum in a `oneOf`.
    #[schema(inline)]
    pub intensity: Intensity,
    #[schema(inline)]
    pub encryption: Encryption,
    /// Concurrent metadata fetches; 0 follows the intensity (100, 400,
    /// 1000). Not null: in a patch, null resets a field.
    pub max_sockets: u32,
    /// Seconds before a metadata fetch gives up.
    pub timeout_secs: u64,
    /// The largest info dict fetched, in MiB.
    pub max_metadata_mib: u64,
    /// Harvesting and metadata fetching are paused; the DHT node still
    /// answers other nodes.
    pub paused: bool,
}

impl Default for Crawler {
    fn default() -> Self {
        Crawler::from_config(&Config::default())
    }
}

impl Section for Crawler {
    const SECTION: &'static str = "crawler";
    const FIELDS: &'static [Field] = &[
        Field::new("intensity")
            .file("dht.intensity")
            .env("LODESTAR_INTENSITY"),
        Field::new("encryption").file("fetch.encryption").restart(),
        Field::new("max_sockets")
            .file("fetch.max_sockets")
            .restart()
            .range(0, 4096),
        Field::new("timeout_secs")
            .file("fetch.timeout_secs")
            .restart()
            .range(1, 120),
        Field::new("max_metadata_mib")
            .file("fetch.max_metadata_mib")
            .restart()
            .range(1, 64),
        Field::new("paused"),
    ];

    fn from_config(config: &Config) -> Self {
        Crawler {
            intensity: match config.dht.intensity {
                IntensityName::Low => Intensity::Low,
                IntensityName::Balanced => Intensity::Balanced,
                IntensityName::Aggressive => Intensity::Aggressive,
            },
            encryption: match config.fetch.encryption {
                EncryptionName::Prefer => Encryption::Prefer,
                EncryptionName::Require => Encryption::Require,
                EncryptionName::Disable => Encryption::Disable,
            },
            max_sockets: config
                .fetch
                .max_sockets
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
            timeout_secs: config.fetch.timeout_secs,
            max_metadata_mib: config.fetch.max_metadata_mib as u64,
            // Only ever saved in the app.
            paused: false,
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        match field {
            "intensity" => config.dht.intensity = self.intensity.into(),
            "encryption" => {
                config.fetch.encryption = match self.encryption {
                    Encryption::Prefer => EncryptionName::Prefer,
                    Encryption::Require => EncryptionName::Require,
                    Encryption::Disable => EncryptionName::Disable,
                }
            }
            "max_sockets" => {
                config.fetch.max_sockets =
                    (self.max_sockets > 0).then_some(self.max_sockets as usize)
            }
            "timeout_secs" => config.fetch.timeout_secs = self.timeout_secs,
            "max_metadata_mib" => config.fetch.max_metadata_mib = self.max_metadata_mib as usize,
            // The services read it from the registry, not the config.
            "paused" => {}
            _ => {}
        }
    }
}

impl From<Intensity> for IntensityName {
    fn from(i: Intensity) -> IntensityName {
        match i {
            Intensity::Low => IntensityName::Low,
            Intensity::Balanced => IntensityName::Balanced,
            Intensity::Aggressive => IntensityName::Aggressive,
        }
    }
}

impl From<Intensity> for lodestar_dht::node::Intensity {
    fn from(i: Intensity) -> lodestar_dht::node::Intensity {
        IntensityName::from(i).into()
    }
}

/// How long to wait for the DHT node to start before trying again.
const NODE_RETRY: Duration = Duration::from_millis(500);

/// Hands every new intensity to the DHT node behind `node` (set once it
/// has started). Runs until the registry is dropped.
pub fn follow_intensity(
    mut rx: watch::Receiver<Crawler>,
    node: Arc<Mutex<Option<NodeHandle>>>,
) -> JoinHandle<()> {
    // The node starts with the value the receiver has now; read it
    // before the task first runs, so a change in between isn't missed.
    let mut applied = rx.borrow_and_update().intensity;
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            loop {
                let want = rx.borrow_and_update().intensity;
                if want == applied {
                    break;
                }
                let handle = node.lock().expect("dht handle lock").clone();
                match handle {
                    Some(h) if h.set_intensity(want.into()).await => {
                        tracing::info!("settings: crawler intensity is now {want:?}");
                        applied = want;
                    }
                    // Not started yet, or restarting.
                    _ => tokio::time::sleep(NODE_RETRY).await,
                }
            }
        }
    })
}

/// Hands every change of the pause to the running pipeline and records
/// pause, resume and intensity changes as events, however they were made
/// (`/crawler` or `PATCH /settings/crawler`). Each is applied and
/// recorded once: [`Control`] ignores a repeat. Runs until the registry
/// is dropped.
pub fn follow_control(mut rx: watch::Receiver<Crawler>, control: Control) -> JoinHandle<()> {
    rx.borrow_and_update();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            // The value as it is when the control's lock is taken, not
            // when the change was seen (see `Control::sync`).
            control
                .sync(|| {
                    let now = rx.borrow_and_update();
                    (now.paused, now.intensity.into())
                })
                .await;
        }
    })
}
