//! The search crate's log events (architecture §12.1). Nothing here carries
//! a query, a title or a path: counts and error types only.

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
}
