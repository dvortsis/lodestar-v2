//! The engine the pipeline holds (§20.4): the rule set at a version, and
//! a handle that swaps in a new rule set without stopping anything.
//!
//! Prepare stamps its verdict with the version it used; commit trusts a
//! verdict from the built-in worker at the current version and judges
//! again otherwise.

use std::sync::{Arc, RwLock};

use crate::core::{Core, Hit, Item};

#[derive(Debug, Clone)]
pub struct Engine {
    pub core: Core,
    pub version: u64,
}

impl Engine {
    pub fn new(core: Core) -> Engine {
        Engine { core, version: 1 }
    }

    /// The decision for one torrent: the first hit of a switched-on rule.
    pub fn judge(&self, item: &Item) -> Option<Hit> {
        self.core.verdict(item)
    }
}

/// The current engine, shared by prepare, commit and the API.
#[derive(Debug)]
pub struct Handle(RwLock<Arc<Engine>>);

impl Handle {
    pub fn new(engine: Engine) -> Handle {
        Handle(RwLock::new(Arc::new(engine)))
    }

    pub fn current(&self) -> Arc<Engine> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Changes the rule set: a copy is edited and swapped in under the
    /// next version, so judgments in flight finish on the old one.
    pub fn update<T>(&self, f: impl FnOnce(&mut Core) -> T) -> T {
        let mut slot = self.0.write().unwrap_or_else(|e| e.into_inner());
        let mut next = Engine::clone(&slot);
        let out = f(&mut next.core);
        next.version += 1;
        *slot = Arc::new(next);
        out
    }
}
