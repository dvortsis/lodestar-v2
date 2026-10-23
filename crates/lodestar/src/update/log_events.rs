//! The child's probation events (§12.1, component `System`). The
//! supervisor itself writes only to the console (`supervisor:` lines).
//! Registering this catalogue in `logging::CATALOGUES` is B-log2's hub
//! line; until then the events still reach the outputs.

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
