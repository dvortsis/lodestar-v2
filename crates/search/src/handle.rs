//! What the API and the status page hold to search: the index serving
//! now (taken once per request), the leases cursors read through and the
//! result cache.

use std::sync::Arc;

use crate::cache::ResultCache;
use crate::index::SearchIndex;
use crate::lease::Leases;
use crate::live::IndexStats;
use crate::rebuild::{RebuildProgress, Serving};

/// Cheap to clone. Built from a [`Serving`] (the binary) or from a
/// fixed [`SearchIndex`] (tests and tools).
#[derive(Debug, Clone)]
pub struct SearchHandle {
    source: Source,
    leases: Arc<Leases>,
    cache: Arc<ResultCache>,
}

#[derive(Debug, Clone)]
enum Source {
    Fixed(Arc<SearchIndex>),
    Serving(Arc<Serving>),
}

impl SearchHandle {
    pub fn serving(serving: Arc<Serving>) -> SearchHandle {
        SearchHandle {
            leases: serving.leases().clone(),
            cache: serving.cache().clone(),
            source: Source::Serving(serving),
        }
    }

    /// A fixed index with its own leases and a default cache.
    pub fn fixed(index: SearchIndex, leases: Leases) -> SearchHandle {
        SearchHandle::fixed_with(index, leases, ResultCache::default())
    }

    /// A fixed index with its own leases and `cache` (tests: bounds).
    pub fn fixed_with(index: SearchIndex, leases: Leases, cache: ResultCache) -> SearchHandle {
        SearchHandle {
            source: Source::Fixed(Arc::new(index)),
            leases: Arc::new(leases),
            cache: Arc::new(cache),
        }
    }

    /// The index to run the next query on. Take it once per request:
    /// after a swap (`rebuild.rs`) the next call returns the new index.
    pub fn index(&self) -> Arc<SearchIndex> {
        match &self.source {
            Source::Fixed(index) => index.clone(),
            Source::Serving(s) => s.index(),
        }
    }

    pub fn leases(&self) -> &Leases {
        &self.leases
    }

    /// First pages and facet responses of the snapshot serving now.
    pub fn cache(&self) -> &ResultCache {
        &self.cache
    }

    /// The rebuild under way, if any.
    pub fn progress(&self) -> Option<RebuildProgress> {
        match &self.source {
            Source::Fixed(_) => None,
            Source::Serving(s) => s.progress(),
        }
    }

    /// The live indexer's counters (none for a fixed index).
    pub fn stats(&self) -> Option<Arc<IndexStats>> {
        match &self.source {
            Source::Fixed(_) => None,
            Source::Serving(s) => Some(s.stats()),
        }
    }
}

impl From<SearchIndex> for SearchHandle {
    fn from(index: SearchIndex) -> SearchHandle {
        SearchHandle::fixed(index, Leases::default())
    }
}
