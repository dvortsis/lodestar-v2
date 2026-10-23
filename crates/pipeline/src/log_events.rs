//! The pipeline crate's log events (architecture §12.1). Nothing here
//! carries a tracker URL, which can hold a passkey: trackers are named by
//! their id in the store, and errors by their kind.

lodestar_log::catalogue! {
    "lodestar-pipeline";
    /// A scrape request to a tracker failed; the tracker backs off.
    SCRAPE_FAILED {
        id: "pipeline.scrape_failed",
        component: Pipeline,
        level: Debug,
        default_on: false,
        message: "Scrape request to a tracker failed",
        fields: { tracker: Label, error: Error },
    }
    /// The HTTP client couldn't be built: HTTP trackers aren't scraped this
    /// run, UDP ones still are.
    HTTP_TRACKERS_DISABLED {
        id: "pipeline.http_trackers_disabled",
        component: Pipeline,
        level: Error,
        message: "HTTP trackers disabled: the HTTP client couldn't be built",
        fields: { error: Error },
    }
    /// A configured tracker can't be scraped (no client for its scheme).
    TRACKER_SKIPPED {
        id: "pipeline.tracker_skipped",
        component: Pipeline,
        level: Warn,
        message: "Tracker can't be scraped; skipped",
        fields: { tracker: Label },
    }
    /// The hot columns' snapshot couldn't be read; they're rebuilt from
    /// SQLite instead, as after a crash.
    HOT_SNAPSHOT_UNREADABLE {
        id: "pipeline.hot_snapshot_unreadable",
        component: Pipeline,
        level: Warn,
        message: "Swarm stats snapshot unreadable; rebuilding from the database",
        fields: { error: Error },
    }
    /// The scrape scheduler couldn't read its lanes from the store; it
    /// tries again at the next refill.
    SCHEDULER_STORE_FAILED {
        id: "pipeline.scheduler_store_failed",
        component: Pipeline,
        level: Warn,
        message: "Scrape scheduler can't read the store; trying again",
        fields: { error: Error },
    }
}
