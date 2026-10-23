//! Search: the live indexer, started with the server.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Danger, Field, Preview, Section};
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
        Field::new("enabled")
            .file("search.enabled")
            .restart()
            .group("search")
            .preview()
            .danger(Danger::Disruptive),
        Field::new("heap_mib")
            .file("search.heap_mib")
            .restart()
            .range(16, 8192)
            .group("index")
            .advanced()
            .unit("mebibytes")
            .preview(),
    ];

    fn from_config(config: &Config) -> Self {
        Search {
            enabled: config.search.enabled,
            heap_mib: u32::try_from(config.search.heap_mib).unwrap_or(u32::MAX),
        }
    }

    fn preview(&self, field: &str) -> Preview {
        match field {
            "enabled" if !self.enabled => Preview::default().warn(
                "search_stops",
                "Search stops after the restart, until you turn it back on.",
            ),
            "heap_mib" => Preview::default().estimate("memory_mib", self.heap_mib),
            _ => Preview::default(),
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
