//! The pipeline crate's log events (architecture §12.1). Nothing here
//! carries a tracker URL, which can hold a passkey: trackers are named by
//! their id in the store, and errors by their kind. A fetch key is a
//! `Torrent`; a filter rule is named by its id, never by its terms.

use lodestar_log::Error as LogError;

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
    /// A remote worker's spool segment is damaged (WK-11's format): its
    /// header doesn't read or names another worker, its last write was
    /// torn, or damage stops it part-way. What can't be read is dropped
    /// and counted; the rest is replayed. Named by the segment's number, in
    /// decimal (its file name is that number in 16 hex digits), and the first
    /// damaged part found, never a byte of it. One line per segment, the
    /// first time it's read, and at most `spool::MAX_DAMAGE_LOGS` a spool.
    SPOOL_DAMAGED {
        id: "pipeline.spool_damaged",
        component: Workers,
        level: Warn,
        message: "A spool segment is damaged; what can't be read is dropped",
        fields: { segment: Label, part: Label },
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
    /// A source's saved priority isn't one; its default applies.
    PRIORITY_UNREADABLE {
        id: "pipeline.priority_unreadable",
        component: Pipeline,
        level: Warn,
        message: "Ignoring a saved source priority that isn't one",
        fields: { plugin: Label },
    }
    /// The crawler was paused or resumed (`state`).
    CRAWLER_TOGGLED {
        id: "pipeline.crawler_toggled",
        component: Pipeline,
        level: Info,
        message: "Crawler paused or resumed",
        fields: { state: Label },
    }
    /// A pause or intensity change applied, but Insights' record of it
    /// couldn't be saved (`what`).
    EVENT_UNRECORDED {
        id: "pipeline.event_unrecorded",
        component: Pipeline,
        level: Error,
        message: "Crawler change applied, but recording it for Insights failed",
        fields: { what: Label, error: Error },
    }
    /// A source was enabled or disabled. When a start switches one off,
    /// `reason` says why: `consent_text_unapproved` (this build can't show
    /// its class's consent dialog yet) or `consent_lapsed` (its class
    /// condition no longer holds, §18.7). A change made through the API
    /// carries none.
    SOURCE_STATE_CHANGED {
        id: "pipeline.source_state_changed",
        component: Pipeline,
        level: Info,
        message: "Source state changed",
        fields: { plugin: Label, state: Label, reason: Label },
    }
    /// The DHT node bound its sockets.
    DHT_STARTED {
        id: "pipeline.dht_started",
        component: Dht,
        level: Info,
        message: "DHT node started",
        fields: { sockets: Size },
    }
    /// A CS-1 entry's origin code is unknown; it's kept as imported.
    CS_ORIGIN_UNKNOWN {
        id: "pipeline.cs_origin_unknown",
        component: Filters,
        level: Warn,
        message: "Unknown CS-1 origin; kept as imported",
        fields: { origin: Size },
    }
    /// The pipeline starts with what's stored and queued.
    STARTING {
        id: "pipeline.starting",
        component: Pipeline,
        level: Info,
        message: "Pipeline starting",
        fields: { stored: Size, queued: Size },
    }
    /// A stored filter rule was skipped: `what` (`rule`, `override`,
    /// `exception`, `always_allow`, `class_rule`, `pack_override`, or
    /// `sp4_row` for a row of SP-4's signature set) and its id, never its
    /// terms. `reason` is `rows_full` for an SP-4 row dropped because
    /// memory held as many rows as it can, at start or after a flush.
    FILTER_RULE_SKIPPED {
        id: "pipeline.filter_rule_skipped",
        component: Filters,
        level: Warn,
        message: "A stored filter rule was skipped",
        fields: { what: Label, rule: Size, reason: Label },
    }
    /// A running source's egress route went down: its kill switch
    /// refuses its requests until the route is back.
    SOURCE_EGRESS_DOWN {
        id: "pipeline.source_egress_down",
        component: Pipeline,
        level: Warn,
        message: "Source egress down; its requests are refused",
        fields: { plugin: Label, reason: Label },
    }
    /// A running source's egress route is back up.
    SOURCE_EGRESS_UP {
        id: "pipeline.source_egress_up",
        component: Pipeline,
        level: Info,
        message: "Source egress back up",
        fields: { plugin: Label },
    }
    /// `HTTPS_PROXY` or `ALL_PROXY` is set, but this running source
    /// connects directly: source plugins never use those variables.
    SOURCE_PROXY_ENV_IGNORED {
        id: "pipeline.source_proxy_env_ignored",
        component: Pipeline,
        level: Warn,
        message: "Proxy variable ignored; this source connects directly",
        fields: { plugin: Label },
    }
    /// A source's saved egress, schedule or agents isn't readable; its
    /// default applies.
    SOURCE_SETTING_UNREADABLE {
        id: "pipeline.source_setting_unreadable",
        component: Pipeline,
        level: Warn,
        message: "Ignoring a saved source setting that isn't readable",
        fields: { plugin: Label, setting: Label },
    }
    /// A source isn't enabled, so it isn't started.
    SOURCE_NOT_STARTED {
        id: "pipeline.source_not_started",
        component: Pipeline,
        level: Info,
        message: "Source not started: it isn't enabled",
        fields: { plugin: Label, state: Label },
    }
    /// A source wasn't started: the companion it needs (`companion`, a
    /// catalogue id) isn't installed or detected, or its state isn't
    /// known yet (CMP-10). Logged once; it starts when the companion is
    /// there and healthy.
    SOURCE_NEEDS_COMPANION {
        id: "pipeline.source_needs_companion",
        component: Pipeline,
        level: Info,
        message: "Source waiting for a companion it needs",
        fields: { plugin: Label, companion: Label },
    }
    /// A source was paused, or not started: the companion it needs is
    /// failing its health check (CMP-10). Logged once.
    SOURCE_COMPANION_UNHEALTHY {
        id: "pipeline.source_companion_unhealthy",
        component: Pipeline,
        level: Warn,
        message: "Source held: the companion it needs is unhealthy",
        fields: { plugin: Label, companion: Label },
    }
    /// A source held for its companion started: the companion is there
    /// and healthy (CMP-10).
    SOURCE_COMPANION_READY {
        id: "pipeline.source_companion_ready",
        component: Pipeline,
        level: Info,
        message: "Source started: the companion it needs is ready",
        fields: { plugin: Label, companion: Label },
    }
    /// A running source stopped with an error.
    SOURCE_FAILED {
        id: "pipeline.source_failed",
        component: Pipeline,
        level: Error,
        message: "Source stopped with an error",
        fields: { plugin: Label, error: Error },
    }
    /// At shutdown, a source stopped with an error.
    SOURCE_STOP_FAILED {
        id: "pipeline.source_stop_failed",
        component: Pipeline,
        level: Warn,
        message: "Source stopped with an error at shutdown",
        fields: { plugin: Label, error: Error },
    }
    /// At shutdown, a source didn't stop in time and was aborted.
    SOURCE_STOP_TIMEOUT {
        id: "pipeline.source_stop_timeout",
        component: Pipeline,
        level: Warn,
        message: "Source didn't stop in time; aborting it",
        fields: { plugin: Label },
    }
    /// A key reached the worker without its lease (a bug; the funnel
    /// isn't counted for it). `stage`: `lost` or `fetched`.
    LEASE_MISSING {
        id: "pipeline.lease_missing",
        component: Pipeline,
        level: Warn,
        message: "A key without a lease",
        fields: { key: Hash, stage: Label },
    }
    /// Fetched metadata couldn't be prepared (malformed or refused).
    PREPARE_FAILED {
        id: "pipeline.prepare_failed",
        component: Pipeline,
        level: Debug,
        default_on: false,
        message: "Preparing fetched metadata failed",
        fields: { key: Hash, error: Error },
    }
    /// The WAL checkpoint at shutdown failed (SQLite checkpoints at the
    /// next open).
    CHECKPOINT_FAILED {
        id: "pipeline.checkpoint_failed",
        component: Pipeline,
        level: Warn,
        message: "WAL checkpoint at shutdown failed",
        fields: { error: Error },
    }
    /// A snapshot past its retention was deleted.
    SNAPSHOT_EXPIRED {
        id: "pipeline.snapshot_expired",
        component: Pipeline,
        level: Info,
        message: "Deleted a snapshot past its retention",
        fields: { what: Label, path: Path, days: Size },
    }
    /// The snapshot purge met problems (a file it couldn't delete or
    /// purge); it tries again at the next purge.
    SNAPSHOT_PURGE_PROBLEMS {
        id: "pipeline.snapshot_purge_problems",
        component: Pipeline,
        level: Warn,
        message: "The snapshot purge met problems; it tries again at the next purge",
        fields: { count: Size },
    }
    /// Purging expired quarantine and filter hits failed.
    FILTER_PURGE_FAILED {
        id: "pipeline.filter_purge_failed",
        component: Pipeline,
        level: Warn,
        message: "Purging filter state failed",
        fields: { error: Error },
    }
    /// A batch couldn't be written to the store.
    STORE_WRITE_FAILED {
        id: "pipeline.store_write_failed",
        component: Pipeline,
        level: Error,
        message: "Store write failed",
        fields: { error: Error },
    }
    /// The core re-judge (`crate::core_rejudge`) starts on this run, or
    /// resumes: its totals so far, counts only.
    CORE_REJUDGE_STARTED {
        id: "pipeline.core_rejudge_started",
        component: Pipeline,
        level: Info,
        message: "Re-judging stored torrents by today's filter core",
        fields: { judged: Size, held: Size, unreadable: Size },
    }
    /// The core re-judge reached the end: how many stored torrents were
    /// judged again, how many of those are now held, and how many of
    /// those couldn't be judged at all. Counts only.
    CORE_REJUDGE_DONE {
        id: "pipeline.core_rejudge_done",
        component: Pipeline,
        level: Info,
        message: "Re-judge of stored torrents finished",
        fields: { judged: Size, held: Size, unreadable: Size },
    }
    /// A core re-judge batch failed in the store: it's tried again from
    /// the same place after a backoff, then one torrent at a time. (A
    /// batch that mostly won't read is `core_rejudge_stalled` instead.)
    CORE_REJUDGE_FAILED {
        id: "pipeline.core_rejudge_failed",
        component: Pipeline,
        level: Error,
        message: "Re-judge of stored torrents failed; it will try again",
        fields: { error: Error },
    }
    /// The core re-judge's breaker stopped a batch: most of its stored
    /// torrents wouldn't read. Nothing in it was held; its range is kept to
    /// judge again, and the pass went on past it. The torrents the range
    /// covers, and the ranges kept now. Counts only.
    CORE_REJUDGE_STALLED {
        id: "pipeline.core_rejudge_stalled",
        component: Pipeline,
        level: Warn,
        message: "Re-judge skipped stored torrents that won't read; it will try them again",
        fields: { rows: Size, ranges: Size },
    }
    /// A range the core re-judge's breaker stopped was judged through and
    /// removed: the torrents it still counted (those it covered when it
    /// stopped, less any judged in earlier tries), and the ranges still
    /// kept. Counts only.
    CORE_REJUDGE_STALL_CLEARED {
        id: "pipeline.core_rejudge_stall_cleared",
        component: Pipeline,
        level: Info,
        message: "Re-judge judged stored torrents it had skipped",
        fields: { rows: Size, ranges: Size },
    }
    /// The background enrichment pass failed.
    ENRICHMENT_FAILED {
        id: "pipeline.enrichment_failed",
        component: Pipeline,
        level: Error,
        message: "Background enrichment failed",
        fields: { error: Error },
    }
    /// Rebuilding the have-filter failed.
    HAVE_REBUILD_FAILED {
        id: "pipeline.have_rebuild_failed",
        component: Pipeline,
        level: Error,
        message: "Rebuilding the have-filter failed",
        fields: { error: Error },
    }
    /// The fetch queue couldn't be saved.
    QUEUE_SAVE_FAILED {
        id: "pipeline.queue_save_failed",
        component: Pipeline,
        level: Error,
        message: "Saving the fetch queue failed",
        fields: { error: Error },
    }
}

/// A store error by its kind: its text can carry a path or a value.
pub fn store_error(e: &lodestar_store::Error) -> LogError<'static> {
    use lodestar_store::Error as E;
    LogError::kind(match e {
        E::Sqlite(_) => "sqlite",
        E::Codec(_) => "codec",
        E::Compression(_) => "compression",
        E::SchemaTooNew { .. } => "schema_too_new",
        E::Corrupt(_) => "corrupt",
        E::TooManySources => "too_many_sources",
        E::Invalid(_) => "invalid",
        E::NotFound(_) => "not_found",
        _ => "store",
    })
}

/// The pipeline's error by its kind.
pub fn error(e: &crate::Error) -> LogError<'static> {
    match e {
        crate::Error::Store(e) => store_error(e),
        crate::Error::Join(_) => LogError::kind("join"),
        crate::Error::Io(e) => LogError::io(e),
    }
}

/// A source state as a label.
pub fn state(s: lodestar_store::SourceState) -> &'static str {
    use lodestar_store::SourceState as S;
    match s {
        S::Enabled => "enabled",
        S::Paused => "paused",
        S::Disabled => "disabled",
        S::Purged => "purged",
    }
}

/// A filter loader's note (`rule U-12 skipped: it no longer parses`,
/// `rule U-3 fails closed: unreadable`) as
/// what was skipped, its id and why, so the note's text (which a later
/// loader could make name a term) is never logged. A pack override's id
/// (`MW-1`) isn't a number, so it logs as 0; the Insights exception
/// `filter_rows_unreadable` names the row.
pub fn filter_note(note: &str) -> (&'static str, u64, &'static str) {
    const WHAT: [(&str, &str); 7] = [
        ("pack override ", "pack_override"),
        ("classification rule ", "class_rule"),
        ("rule U-", "rule"),
        ("override O-", "override"),
        ("exception ", "exception"),
        ("always-allow ", "always_allow"),
        ("SP-4 signature row ", "sp4_row"),
    ];
    let Some((what, rest)) = WHAT
        .iter()
        .find_map(|(prefix, what)| note.strip_prefix(prefix).map(|rest| (*what, rest)))
    else {
        return ("other", 0, "other");
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let id = rest[..digits].parse().unwrap_or(0);
    // Checked first: a row that fails closed also says why.
    let reason = if rest.contains(" fails closed:") {
        "fails_closed"
    } else if rest.ends_with("it no longer parses") {
        "no_longer_parses"
    } else if rest.ends_with("unreadable") {
        "unreadable"
    } else if rest.ends_with(lodestar_filter::spam_sigs::ROWS_FULL) {
        "rows_full"
    } else {
        "other"
    };
    (what, id, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_notes_become_kinds_and_ids() {
        for (note, want) in [
            (
                "classification rule 7 skipped: it no longer parses",
                ("class_rule", 7, "no_longer_parses"),
            ),
            (
                "rule U-12 skipped: it no longer parses",
                ("rule", 12, "no_longer_parses"),
            ),
            (
                "override O-3 skipped: unreadable",
                ("override", 3, "unreadable"),
            ),
            (
                "exception 9 skipped: unreadable",
                ("exception", 9, "unreadable"),
            ),
            (
                "always-allow 4 skipped: it no longer parses",
                ("always_allow", 4, "no_longer_parses"),
            ),
            (
                "rule U-5 fails closed: its unless no longer parses",
                ("rule", 5, "fails_closed"),
            ),
            (
                "classification rule 2 fails closed: it no longer parses",
                ("class_rule", 2, "fails_closed"),
            ),
            (
                "pack override MW-1 fails closed: unreadable",
                ("pack_override", 0, "fails_closed"),
            ),
            (
                "SP-4 signature row 12 skipped: unreadable",
                ("sp4_row", 12, "unreadable"),
            ),
            (
                "SP-4 signature row 3 dropped: more rows than memory holds",
                ("sp4_row", 3, "rows_full"),
            ),
            ("rule about sometermhere", ("other", 0, "other")),
        ] {
            assert_eq!(filter_note(note), want, "{note}");
        }
        lodestar_log::check_ids(&[&CATALOGUE]).unwrap();
    }
}
