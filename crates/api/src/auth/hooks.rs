//! Test seams for the lock tests ([`super::lock_tests`]): a hook called
//! where a change takes the save lock and where it starts writing, so a
//! test can hold a save mid-way, and a switch that fails the directory
//! fsync. Unit tests only; nothing here exists in a release build.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Where a change has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Point {
    /// About to take the save lock (any password check is done).
    Locking,
    /// Holding the save lock, about to write `auth.json`.
    Saving,
}

type Hook = Arc<dyn Fn(Point) + Send + Sync>;

#[derive(Default)]
pub(super) struct Hooks {
    hook: Mutex<Option<Hook>>,
    /// Fail every directory fsync.
    pub(super) fail_dir_sync: AtomicBool,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

impl Hooks {
    /// Calls `hook` at every [`Point`], on the changing thread.
    pub(super) fn set(&self, hook: impl Fn(Point) + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Arc::new(hook));
    }

    pub(super) fn at(&self, point: Point) {
        // Cloned out, so a hook that blocks holds no lock here.
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }
}
