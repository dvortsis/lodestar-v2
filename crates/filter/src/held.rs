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

    /// Inserts those of `hashes` the store still holds, after a batch that
    /// held them committed. `held_among` asks the store, per hash, while
    /// the write lock is held, so a release can't remove a hash between
    /// the answer and the insert (`2026-10-31-B-held-exact.md`). When the
    /// store can't answer, every hash goes in: over-strict, never loose.
    pub fn insert_held<E>(
        &self,
        hashes: &[[u8; 20]],
        held_among: impl FnOnce(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        if hashes.is_empty() {
            return Ok(());
        }
        let mut set = self.0.write().unwrap_or_else(|e| e.into_inner());
        match held_among(hashes) {
            Ok(held) => {
                debug_assert_eq!(held.len(), hashes.len());
                for (h, still) in hashes
                    .iter()
                    .zip(held.iter().chain(std::iter::repeat(&true)))
                {
                    if *still {
                        set.insert(*h);
                    }
                }
                Ok(())
            }
            Err(e) => {
                set.extend(hashes.iter().copied());
                Err(e)
            }
        }
    }

    /// Removes those of `hashes` the store no longer holds, after a
    /// release (or an expiry) committed. `held_among` is asked under the
    /// write lock, so a batch that held one again since stays held. When
    /// the store can't answer, none is removed: over-strict, never loose.
    pub fn remove_released<E>(
        &self,
        hashes: &[[u8; 20]],
        held_among: impl FnOnce(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        if hashes.is_empty() {
            return Ok(());
        }
        let mut set = self.0.write().unwrap_or_else(|e| e.into_inner());
        let held = held_among(hashes)?;
        debug_assert_eq!(held.len(), hashes.len());
        for (h, still) in hashes
            .iter()
            .zip(held.iter().chain(std::iter::repeat(&true)))
        {
            if !*still {
                set.remove(h);
            }
        }
        Ok(())
    }

    /// Every key, for comparing with the store's holds.
    pub fn snapshot(&self) -> HashSet<[u8; 20]> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn len(&self) -> usize {
        self.0.read().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_held_takes_only_what_the_store_holds() {
        let s = HeldSet::default();
        s.insert_held::<()>(&[[1; 20], [2; 20]], |_| Ok(vec![true, false]))
            .unwrap();
        assert!(s.contains(&[1; 20]));
        assert!(!s.contains(&[2; 20]));
    }

    #[test]
    fn a_store_error_holds_every_hash() {
        let s = HeldSet::default();
        assert!(s.insert_held(&[[1; 20], [2; 20]], |_| Err(())).is_err());
        assert_eq!(s.len(), 2);
        assert!(s.remove_released(&[[1; 20]], |_| Err(())).is_err());
        assert!(s.contains(&[1; 20]));
    }

    #[test]
    fn remove_released_keeps_what_the_store_holds_again() {
        let s = HeldSet::default();
        s.insert([1; 20]);
        s.insert([2; 20]);
        s.remove_released::<()>(&[[1; 20], [2; 20]], |_| Ok(vec![true, false]))
            .unwrap();
        assert!(s.contains(&[1; 20]));
        assert!(!s.contains(&[2; 20]));
    }
}
