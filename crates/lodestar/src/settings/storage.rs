//! Storage: the store's options, read when it opens.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Field, Section};
use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// SQLite memory-mapped I/O per connection, in MiB (0 turns it off).
    pub mmap_mib: u64,
    /// Keep each info dict minus `pieces` (§3).
    pub archive_raw: bool,
}

impl Default for Storage {
    fn default() -> Self {
        Storage::from_config(&Config::default())
    }
}

impl Section for Storage {
    const SECTION: &'static str = "storage";
    const FIELDS: &'static [Field] = &[
        Field::new("mmap_mib")
            .file("store.mmap_mib")
            .restart()
            .range(0, 65_536),
        Field::new("archive_raw")
            .file("store.archive_raw")
            .restart(),
    ];

    fn from_config(config: &Config) -> Self {
        Storage {
            mmap_mib: config.store.mmap_mib,
            archive_raw: config.store.archive_raw,
        }
    }

    fn apply(&self, config: &mut Config) {
        config.store.mmap_mib = self.mmap_mib;
        config.store.archive_raw = self.archive_raw;
    }
}
