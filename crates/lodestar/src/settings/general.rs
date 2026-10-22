//! General: the log filter (live) and the data directory (read-only).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

/// The longest log filter accepted.
const MAX_FILTER: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct General {
    /// Holds the database, the index and the DHT state. Set in
    /// `config.toml` or `LODESTAR_DATA_DIR`; never moved at runtime.
    pub data_dir: String,
    /// `error`, `warn`, `info`, `debug`, `trace` or a `tracing` filter
    /// (`info,lodestar_dht=debug`). Applies at once.
    pub log_level: String,
}

impl Default for General {
    fn default() -> Self {
        General::from_config(&Config::default())
    }
}

impl Section for General {
    const SECTION: &'static str = "general";
    const FIELDS: &'static [Field] = &[
        Field::new("log_level").file("log").env("LODESTAR_LOG"),
        Field::new("data_dir")
            .file("data_dir")
            .env("LODESTAR_DATA_DIR")
            .read_only()
            .restart(),
    ];

    fn from_config(config: &Config) -> Self {
        General {
            data_dir: config.data_dir.display().to_string(),
            log_level: config.log.clone(),
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        // `data_dir` is read-only, so never applied: nothing moves files
        // at runtime.
        if field == "log_level" {
            config.log = self.log_level.clone();
        }
    }

    fn check(&self, field: &str) -> Result<(), String> {
        if field == "log_level" {
            if self.log_level.len() > MAX_FILTER {
                return Err(format!("Must be at most {MAX_FILTER} characters."));
            }
            super::log::parse(&self.log_level)?;
        }
        Ok(())
    }
}
