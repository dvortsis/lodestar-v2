//! The search crate's log events (architecture §12.1). Nothing here carries
//! a query, a title or a path: counts, ids and error types only. An index
//! version is its schema and tokenizer numbers; a path is a `Path`.

use lodestar_log::Error as LogError;

lodestar_log::catalogue! {
    "lodestar-search";
    /// The suggestions' title set was rebuilt from the store.
    SUGGEST_TITLES_REBUILT {
        id: "search.suggest_titles_rebuilt",
        component: Search,
        level: Debug,
        message: "Suggestions: title set rebuilt",
        fields: { titles: Label },
    }
    /// Rebuilding the title set failed; it's tried again after the interval.
    SUGGEST_TITLES_FAILED {
        id: "search.suggest_titles_failed",
        component: Search,
        level: Warn,
        message: "Suggestions: couldn't rebuild the title set; trying again later",
        fields: { error: Error },
    }
    /// A saved search failed in the index during percolation. Its error
    /// isn't recorded: Tantivy's can carry query terms.
    PERCOLATE_SEARCH_FAILED {
        id: "search.percolate_search_failed",
        component: Search,
        level: Warn,
        message: "A saved search failed in the index while matching new torrents",
        fields: { saved_search: Label },
    }
    /// Renaming an index folder failed; trying again (`attempt`).
    RENAME_RETRY {
        id: "search.rename_retry",
        component: Search,
        level: Warn,
        message: "Index: renaming a folder failed; trying again",
        fields: { path: Path, attempt: Label, error: Error },
    }
    /// The index is rebuilt for this build's version while the older one
    /// serves searches.
    REBUILD_STARTED {
        id: "search.rebuild_started",
        component: Search,
        level: Info,
        message: "Index: rebuilding while the older index serves",
        fields: { schema: Label, tokenizer: Label, serving_schema: Label, serving_tokenizer: Label },
    }
    /// The rebuild failed; the older index keeps serving.
    REBUILD_FAILED {
        id: "search.rebuild_failed",
        component: Search,
        level: Error,
        message: "Index: rebuilding failed",
        fields: { schema: Label, tokenizer: Label, error: Error },
    }
    /// A step of the rebuild failed; it's tried again.
    REBUILD_RETRY {
        id: "search.rebuild_retry",
        component: Search,
        level: Warn,
        message: "Index rebuild: a step failed; trying again",
        fields: { error: Error },
    }
    /// The rebuilt index was swapped in.
    SWAPPED {
        id: "search.swapped",
        component: Search,
        level: Info,
        message: "Index: swapped to the rebuilt index",
        fields: {},
    }
    /// Stopping the index (or the old indexer at a swap) failed.
    STOP_FAILED {
        id: "search.stop_failed",
        component: Search,
        level: Warn,
        message: "Index: stopping failed",
        fields: { what: Label, error: Error },
    }
    /// The old indexer couldn't be started again after a failed swap.
    OLD_INDEXER_RESTART_FAILED {
        id: "search.old_indexer_restart_failed",
        component: Search,
        level: Error,
        message: "Index: restarting the old indexer failed",
        fields: { error: Error },
    }
    /// An index version folder isn't served: `reason` `empty` (it holds
    /// no index) or `incompatible` (it lacks fields every search needs).
    VERSION_SKIPPED {
        id: "search.version_skipped",
        component: Search,
        level: Warn,
        message: "Index: a version folder isn't served",
        fields: { schema: Label, tokenizer: Label, reason: Label },
    }
    /// Opening an index version failed; trying again (`attempt`).
    OPEN_RETRY {
        id: "search.open_retry",
        component: Search,
        level: Warn,
        message: "Index: opening failed; trying again",
        fields: { schema: Label, tokenizer: Label, attempt: Label, error: Error },
    }
    /// The retired index was still open at the limit; it's deleted anyway.
    RETIRED_STILL_OPEN {
        id: "search.retired_still_open",
        component: Search,
        level: Warn,
        message: "Index: the retired index is still open; deleting it anyway",
        fields: {},
    }
    /// A cleanup pass over old index folders failed; it's tried again.
    CLEANUP_RETRY {
        id: "search.cleanup_retry",
        component: Search,
        level: Debug,
        message: "Index cleanup: a pass failed; trying again",
        fields: { error: Error },
    }
    /// Old index folders remain; they're deleted at the next start.
    CLEANUP_LEFT {
        id: "search.cleanup_left",
        component: Search,
        level: Warn,
        message: "Index: some old index folders remain; deleting them at the next start",
        fields: {},
    }
    /// The live indexer failed; it tries again after `backoff_ms`.
    INDEXER_RETRY {
        id: "search.indexer_retry",
        component: Search,
        level: Warn,
        message: "Indexer: a step failed; trying again",
        fields: { backoff_ms: Size, error: Error },
    }
    /// Counting the newly indexed torrents for Insights failed.
    INDEXER_COUNT_FAILED {
        id: "search.indexer_count_failed",
        component: Search,
        level: Warn,
        message: "Indexer: counting newly indexed torrents failed",
        fields: { error: Error },
    }
    /// The file index's part of a search failed; the search goes on
    /// without file matches.
    FILE_SEARCH_FAILED {
        id: "search.file_search_failed",
        component: Search,
        level: Error,
        message: "The file index's part of a search failed",
        fields: { error: Error },
    }
    /// A torrent's stored enrichment can't be decoded; it's indexed
    /// without it.
    ENRICHMENT_UNREADABLE {
        id: "search.enrichment_unreadable",
        component: Search,
        level: Debug,
        message: "A torrent's enrichment is unreadable; indexed without it",
        fields: { torrent_id: Size },
    }
}

/// The search crate's error by its kind: Tantivy's text can carry query
/// terms, and the lock errors name the index's folder.
pub fn error(e: &crate::Error) -> LogError<'static> {
    use crate::Error as E;
    match e {
        E::Tantivy(_) => LogError::kind("tantivy"),
        E::Store(_) => LogError::kind("store"),
        E::Io(e) => LogError::io(e),
        E::Corrupt(_) => LogError::kind("corrupt"),
        E::Locked(_) => LogError::kind("locked"),
        E::LockFailed { error, .. } => LogError::io(error),
        E::ReadOnly => LogError::kind("read_only"),
        E::Cancelled => LogError::kind("cancelled"),
        E::OutOfTime => LogError::kind("out_of_time"),
        E::LeaseExpired => LogError::kind("lease_expired"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ids_are_unique() {
        lodestar_log::check_ids(&[&super::CATALOGUE]).unwrap();
    }
}
