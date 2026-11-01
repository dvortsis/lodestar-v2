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
    /// A signed check found a release newer than the running version.
    UPDATE_AVAILABLE {
        id: "update.available",
        component: System,
        level: Info,
        message: "An update is available",
        fields: {},
    }
    /// The signed update check failed (the code says why; never a URL).
    UPDATE_CHECK_FAILED {
        id: "update.check_failed",
        component: System,
        level: Warn,
        message: "The update check failed",
        fields: { error: Label },
    }
    /// An install failed before the switch (the code says why; never a
    /// URL). The running version carries on.
    UPDATE_INSTALL_FAILED {
        id: "update.install_failed",
        component: System,
        level: Warn,
        message: "The update couldn't be installed",
        fields: { error: Label },
    }
    /// An updater state file (`updater.json`, `skipped`, `history.json`)
    /// couldn't be saved durably, so the change wasn't acknowledged
    /// (DUR-26). `file` is its name, never a path.
    UPDATE_STATE_UNSAVED {
        id: "update.state_unsaved",
        component: System,
        level: Warn,
        message: "An update setting couldn't be saved",
        fields: { file: Label, error: Error },
    }
    /// `updater.json` was there but couldn't be read at start, so any
    /// pause in it is unknown: automatic installs are held, and nothing
    /// writes over it until a pause or resume is saved (DUR-26).
    UPDATE_STATE_UNREADABLE {
        id: "update.state_unreadable",
        component: System,
        level: Warn,
        message: "The update settings couldn't be read; automatic updates are held",
        fields: { error: Error },
    }
    /// The filesystem under `<data>/bin` can't fsync a folder (some FUSE
    /// mounts, such as Unraid's `/mnt/user`): updater changes are saved by
    /// rename without it. Logged once per run, without a path.
    UPDATE_DIR_SYNC_UNSUPPORTED {
        id: "update.dir_sync_unsupported",
        component: System,
        level: Warn,
        message: "The update folder's filesystem can't flush folders; saving without it",
        fields: {},
    }
    /// An entry in `<data>/bin` couldn't be read while the update check
    /// gathered the installed versions' trust; it's skipped.
    UPDATE_OVERLAY_UNREADABLE {
        id: "update.overlay_unreadable",
        component: System,
        level: Warn,
        message: "An entry in the update folder couldn't be read; skipped",
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
