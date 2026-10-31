//! Keys held back from fetching (§20.3).

use std::collections::{HashMap, HashSet};
use std::sync::{RwLock, RwLockWriteGuard};

/// Hashes checked against the store per hold of the write lock.
pub const CHECK_CHUNK: usize = 1_024;

/// Tombstoned and quarantined keys (§20.3): intake refuses them, so a
/// dropped torrent isn't fetched again and again. Hashes a batch is about
/// to hold are refused too, from before its commit until its reconcile
/// (`2026-10-31-B-held-precommit.md`).
#[derive(Debug, Default)]
pub struct HeldSet(RwLock<Inner>);

#[derive(Debug, Default)]
struct Inner {
    held: HashSet<[u8; 20]>,
    /// Marked by [`HeldSet::pend`], per in-flight batch that marked them.
    pending: HashMap<[u8; 20], u32>,
}

impl Inner {
    fn unpend(&mut self, h: &[u8; 20]) {
        if let Some(n) = self.pending.get_mut(h) {
            *n -= 1;
            if *n == 0 {
                self.pending.remove(h);
            }
        }
    }
}

/// Hashes a batch marked before its store commit. Refused by intake until
/// [`settle`](Self::settle) reconciles them with the store. Dropped
/// unsettled (an error or a panic between the mark and the reconcile),
/// they move into the held set: refused until a release reconciles them
/// or the next start, never dropped.
#[must_use = "settle it after the commit"]
#[derive(Debug)]
pub struct Pending<'a> {
    set: &'a HeldSet,
    hashes: Vec<[u8; 20]>,
}

impl Pending<'_> {
    /// After the commit (or its rollback): inserts those of the marked
    /// hashes the store holds, and clears the marks, a chunk at a time
    /// under the write lock, as [`HeldSet::insert_held`]. When the store
    /// can't answer, that chunk and every later one go in.
    pub fn settle<E>(
        mut self,
        held_among: impl FnMut(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        let hashes = std::mem::take(&mut self.hashes);
        self.set.reconcile(&hashes, true, held_among)
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if self.hashes.is_empty() {
            return;
        }
        let mut set = self.set.write();
        for h in std::mem::take(&mut self.hashes) {
            set.unpend(&h);
            set.held.insert(h);
        }
    }
}

impl HeldSet {
    fn write(&self) -> RwLockWriteGuard<'_, Inner> {
        self.0.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Held, or marked by a batch that hasn't reconciled yet.
    pub fn contains(&self, hash: &[u8; 20]) -> bool {
        let set = self.0.read().unwrap_or_else(|e| e.into_inner());
        set.held.contains(hash) || set.pending.contains_key(hash)
    }

    pub fn insert(&self, hash: [u8; 20]) {
        self.write().held.insert(hash);
    }

    /// Marks `hashes`, which a batch is about to hold, before its store
    /// commit: intake refuses them from now on, and a release reconciling
    /// meanwhile can't make them fetchable. Settle the result after the
    /// commit, whether it held them or rolled back.
    pub fn pend(&self, hashes: &[[u8; 20]]) -> Pending<'_> {
        let mut set = self.write();
        for h in hashes {
            *set.pending.entry(*h).or_default() += 1;
        }
        Pending {
            set: self,
            hashes: hashes.to_vec(),
        }
    }

    /// Hashes marked pending and not yet settled, for the tests.
    pub fn pending_len(&self) -> usize {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
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
        held_among: impl FnMut(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        self.reconcile(hashes, false, held_among)
    }

    /// [`insert_held`](Self::insert_held), clearing one pending mark per
    /// hash as it's reconciled when `unpend`.
    fn reconcile<E>(
        &self,
        hashes: &[[u8; 20]],
        unpend: bool,
        mut held_among: impl FnMut(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        for (i, chunk) in hashes.chunks(CHECK_CHUNK).enumerate() {
            let mut set = self.write();
            match held_among(chunk) {
                Ok(held) => {
                    debug_assert_eq!(held.len(), chunk.len());
                    for (h, still) in chunk
                        .iter()
                        .zip(held.iter().chain(std::iter::repeat(&true)))
                    {
                        if *still {
                            set.held.insert(*h);
                        }
                        if unpend {
                            set.unpend(h);
                        }
                    }
                }
                Err(e) => {
                    for h in &hashes[i * CHECK_CHUNK..] {
                        set.held.insert(*h);
                        if unpend {
                            set.unpend(h);
                        }
                    }
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
    /// never loose. A pending mark is never removed: a batch that marked
    /// a hash keeps it refused until its own reconcile reads the store.
    pub fn remove_released<E>(
        &self,
        hashes: &[[u8; 20]],
        mut held_among: impl FnMut(&[[u8; 20]]) -> Result<Vec<bool>, E>,
    ) -> Result<(), E> {
        for chunk in hashes.chunks(CHECK_CHUNK) {
            let mut set = self.write();
            let held = held_among(chunk)?;
            debug_assert_eq!(held.len(), chunk.len());
            for (h, still) in chunk
                .iter()
                .zip(held.iter().chain(std::iter::repeat(&true)))
            {
                if !*still {
                    set.held.remove(h);
                }
            }
        }
        Ok(())
    }

    /// Every held key (not the pending marks), for comparing with the
    /// store's holds.
    pub fn snapshot(&self) -> HashSet<[u8; 20]> {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .held
            .clone()
    }

    pub fn len(&self) -> usize {
        self.0.read().unwrap_or_else(|e| e.into_inner()).held.len()
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

    /// Pre-mark, a release's M, the commit, the reconcile: refused at
    /// every step, held at the end because the store holds it.
    #[test]
    fn a_pending_hash_is_refused_until_its_reconcile() {
        let s = HeldSet::default();
        s.insert([1; 20]);
        let p = s.pend(&[[1; 20]]);
        assert!(s.contains(&[1; 20]));
        s.remove_released::<()>(&[[1; 20]], |c| Ok(vec![false; c.len()]))
            .unwrap();
        assert!(s.contains(&[1; 20]), "M can't make it fetchable");
        p.settle::<()>(|c| Ok(vec![true; c.len()])).unwrap();
        assert!(s.contains(&[1; 20]));
        assert_eq!((s.len(), s.pending_len()), (1, 0));
    }

    /// A rollback: the store holds nothing, so nothing stays refused.
    #[test]
    fn a_rolled_back_mark_clears() {
        let s = HeldSet::default();
        let p = s.pend(&[[1; 20]]);
        p.settle::<()>(|c| Ok(vec![false; c.len()])).unwrap();
        assert!(!s.contains(&[1; 20]));
        assert!(s.is_empty() && s.pending_len() == 0);
    }

    /// Two batches marking one hash: the first reconcile leaves the
    /// second's mark in place.
    #[test]
    fn marks_are_counted_per_batch() {
        let s = HeldSet::default();
        let a = s.pend(&[[1; 20]]);
        let b = s.pend(&[[1; 20]]);
        a.settle::<()>(|c| Ok(vec![false; c.len()])).unwrap();
        assert!(s.contains(&[1; 20]), "b is still in flight");
        b.settle::<()>(|c| Ok(vec![false; c.len()])).unwrap();
        assert!(!s.contains(&[1; 20]));
    }

    /// An error between the mark and the reconcile, or a failing
    /// reconcile, fails closed: held, and no mark left behind.
    #[test]
    fn an_unsettled_or_failed_mark_stays_refused() {
        let s = HeldSet::default();
        drop(s.pend(&[[1; 20]]));
        assert!(s.contains(&[1; 20]) && s.snapshot().contains(&[1; 20]));
        assert!(s.pend(&[[2; 20]]).settle(|_| Err(())).is_err());
        assert!(s.snapshot().contains(&[2; 20]));
        assert_eq!(s.pending_len(), 0);
    }
}
