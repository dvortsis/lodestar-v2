//! General: the data directory (read-only). The log filter that was here
//! is the `logging` section's now ([`super::logging`]).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct General {
    /// Holds the database, the index and the DHT state. Set in
    /// `config.toml` or `LODESTAR_DATA_DIR`; never moved at runtime.
    pub data_dir: String,
}

impl Default for General {
    fn default() -> Self {
        General::from_config(&Config::default())
    }
}

impl Section for General {
    const SECTION: &'static str = "general";
    const FIELDS: &'static [Field] = &[Field::new("data_dir")
        .file("data_dir")
        .env("LODESTAR_DATA_DIR")
        .read_only()
        .restart()
        .group("data")
        .keywords()];

    fn from_config(config: &Config) -> Self {
        General {
            data_dir: config.data_dir.display().to_string(),
        }
    }

    fn apply(&self, _field: &str, _config: &mut Config) {
        // `data_dir` is read-only, so never applied: nothing moves files
        // at runtime.
    }
}
