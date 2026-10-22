//! Search: the live indexer, started with the server.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Search {
    /// Keep the torrent index up to date (§8).
    pub enabled: bool,
    /// The index writer's heap, in MiB.
    pub heap_mib: u32,
}

impl Default for Search {
    fn default() -> Self {
        Search::from_config(&Config::default())
    }
}

impl Section for Search {
    const SECTION: &'static str = "search";
    const FIELDS: &'static [Field] = &[
        Field::new("enabled").file("search.enabled").restart(),
        Field::new("heap_mib")
            .file("search.heap_mib")
            .restart()
            .range(16, 8192),
    ];

    fn from_config(config: &Config) -> Self {
        Search {
            enabled: config.search.enabled,
            heap_mib: u32::try_from(config.search.heap_mib).unwrap_or(u32::MAX),
        }
    }

    fn apply(&self, field: &str, config: &mut Config) {
        match field {
            "enabled" => config.search.enabled = self.enabled,
            "heap_mib" => config.search.heap_mib = self.heap_mib as usize,
            _ => {}
        }
    }
}
