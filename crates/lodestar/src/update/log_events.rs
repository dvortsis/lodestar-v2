//! The child's probation events (§12.1, component `System`), registered
//! in `logging::CATALOGUES` on every platform. The supervisor itself writes only to the
//! console (`supervisor:` lines).

lodestar_log::catalogue! {
    "lodestar-update";
    /// The store is being upgraded during an update's probation.
    UPDATE_MIGRATING {
        id: "update.migrating",
        component: System,
        level: Info,
        message: "Upgrading the store for the new version",
        fields: { from: Label, to: Label },
    }
    /// The upgrade couldn't be recorded (`migration.json`), so it isn't
    /// begun: without that record a rollback couldn't tell a migrated
    /// store from an untouched one. The version doesn't start.
    UPDATE_MIGRATION_UNRECORDED {
        id: "update.migration_unrecorded",
        component: System,
        level: Error,
        message: "Couldn't record the store upgrade; the new version won't start",
        fields: { error: Error },
    }
    /// The new version passed its self-test: the update is kept.
    UPDATE_PROBATION_PASSED {
        id: "update.probation_passed",
        component: System,
        level: Info,
        message: "The new version passed its self-test",
        fields: {},
    }
    /// The new version failed its self-test; the supervisor retries or
    /// rolls back.
    UPDATE_PROBATION_FAILED {
        id: "update.probation_failed",
        component: System,
        level: Error,
        message: "The new version failed its self-test",
        fields: { check: Label },
    }
}
