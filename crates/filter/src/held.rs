//! Keys held back from fetching (§20.3).

use std::collections::HashSet;
use std::sync::RwLock;

/// Tombstoned and quarantined keys (§20.3): intake refuses them, so a
/// dropped torrent isn't fetched again and again.
#[derive(Debug, Default)]
pub struct HeldSet(RwLock<HashSet<[u8; 20]>>);

impl HeldSet {
    pub fn contains(&self, hash: &[u8; 20]) -> bool {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(hash)
    }

    pub fn insert(&self, hash: [u8; 20]) {
        self.0
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(hash);
    }

    pub fn remove(&self, hash: &[u8; 20]) {
        self.0
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(hash);
    }

    pub fn len(&self) -> usize {
        self.0.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
