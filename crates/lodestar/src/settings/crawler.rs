//! Crawler: the intensity (live) and the metadata fetch limits.
//!
//! A new intensity reaches the running DHT node at once (its query and
//! reply rates and its cap on outstanding queries). The fetch pool's
//! socket count, which follows the intensity when `max_sockets` is
//! unset, takes it at the next start.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lodestar_dht::node::NodeHandle;
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
    /// Concurrent metadata fetches; null follows the intensity (100,
    /// 400, 1000).
    pub max_sockets: Option<u32>,
    /// Seconds before a metadata fetch gives up.
    pub timeout_secs: u64,
    /// The largest info dict fetched, in MiB.
    pub max_metadata_mib: u64,
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
            .range(1, 4096),
        Field::new("timeout_secs")
            .file("fetch.timeout_secs")
            .restart()
            .range(1, 120),
        Field::new("max_metadata_mib")
            .file("fetch.max_metadata_mib")
            .restart()
            .range(1, 64),
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
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
            timeout_secs: config.fetch.timeout_secs,
            max_metadata_mib: config.fetch.max_metadata_mib as u64,
        }
    }

    fn apply(&self, config: &mut Config) {
        config.dht.intensity = self.intensity.into();
        config.fetch.encryption = match self.encryption {
            Encryption::Prefer => EncryptionName::Prefer,
            Encryption::Require => EncryptionName::Require,
            Encryption::Disable => EncryptionName::Disable,
        };
        config.fetch.max_sockets = self.max_sockets.map(|n| n as usize);
        config.fetch.timeout_secs = self.timeout_secs;
        config.fetch.max_metadata_mib = self.max_metadata_mib as usize;
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
