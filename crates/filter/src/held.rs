//! Keys held back from fetching (§20.3).

use std::collections::HashSet;
use std::sync::RwLock;

/// Tombstoned and quarantined keys (§20.3): intake refuses them, so a
/// dropped torrent isn't fetched again and again.
/// Hashes checked against the store per hold of the write lock.
pub const CHECK_CHUNK: usize = 1_024;

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

    /// Inserts those of `hashes` the store still holds, after a batch that
    /// held them committed. `held_among` asks the store, per hash, while
    /// the write lock is held, so a release can't remove a hash between
    /// the answer and the insert (`2026-10-31-B-held-exact.md`). It's
    /// asked [`CHECK_CHUNK`] hashes at a time, the lock dropping between
    /// chunks, so intake never waits behind more: exactness is per hash.
    /// When the store can't answer, that chunk and every later one go
    /// in: over-strict, never loose.
    pub fn insert_held<E>(
        &self,
        hashes: &[[u8; 20]],
        mut held_among: impl FnMut(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        for (i, chunk) in hashes.chunks(CHECK_CHUNK).enumerate() {
            let mut set = self.0.write().unwrap_or_else(|e| e.into_inner());
            match held_among(chunk) {
                Ok(held) => {
                    debug_assert_eq!(held.len(), chunk.len());
                    for (h, still) in chunk
                        .iter()
                        .zip(held.iter().chain(std::iter::repeat(&true)))
                    {
                        if *still {
                            set.insert(*h);
                        }
                    }
                }
                Err(e) => {
                    set.extend(hashes[i * CHECK_CHUNK..].iter().copied());
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Removes those of `hashes` the store no longer holds, after a
    /// release (or an expiry) committed. `held_among` is asked under the
    /// write lock, so a batch that held one again since stays held; in
    /// chunks, as for [`insert_held`](Self::insert_held). When the store
    /// can't answer, that chunk and every later one stay: over-strict,
    /// never loose.
    pub fn remove_released<E>(
        &self,
        hashes: &[[u8; 20]],
        mut held_among: impl FnMut(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        for chunk in hashes.chunks(CHECK_CHUNK) {
            let mut set = self.0.write().unwrap_or_else(|e| e.into_inner());
            let held = held_among(chunk)?;
            debug_assert_eq!(held.len(), chunk.len());
            for (h, still) in chunk
                .iter()
                .zip(held.iter().chain(std::iter::repeat(&true)))
            {
                if !*still {
                    set.remove(h);
                }
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
    fn a_failing_chunk_holds_it_and_the_rest() {
        let s = HeldSet::default();
        let hashes: Vec<[u8; 20]> = (0..CHECK_CHUNK as u32 + 2)
            .map(|i| {
                let mut h = [0; 20];
                h[..4].copy_from_slice(&i.to_be_bytes());
                h
            })
            .collect();
        let mut calls = 0;
        let r = s.insert_held(&hashes, |hs| {
            calls += 1;
            if calls == 1 {
                Ok(vec![false; hs.len()])
            } else {
                Err(())
            }
        });
        assert!(r.is_err());
        assert_eq!(s.len(), 2, "only the failed second chunk");
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
