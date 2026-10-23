//! The binary's events: the required ones (§12.1: a log must be able to
//! explain itself, so these can't be turned off and carry no torrent
//! data), and those of its swept modules.
//!
//! Required events whose call sites wait for the follow-up sweep
//! (`app.stop` in `main.rs`, `auth.setup_token` in `services.rs`,
//! `app.restart` with B2c) are declared here already.

lodestar_log::catalogue! {
    "lodestar";

    // ── Required ──
    /// The process started, from `settings::log::init`.
    APP_START {
        id: "app.start",
        component: System,
        level: Info,
        required: true,
        message: "Lodestar started",
        fields: { version: Label },
    }
    /// The process is stopping (call site: the follow-up sweep, `main.rs`).
    APP_STOP {
        id: "app.stop",
        component: System,
        level: Info,
        required: true,
        message: "Lodestar stopped",
        fields: {},
    }
    /// A panic, from the hook `settings::log::init` installs. The panic's
    /// message can hold anything, so only where it happened is shown.
    APP_CRASH {
        id: "app.crash",
        component: System,
        level: Error,
        required: true,
        message: "Lodestar crashed",
        fields: { location: Label },
    }
    /// A restart asked for in the app (call site: B2c).
    APP_RESTART {
        id: "app.restart",
        component: System,
        level: Info,
        required: true,
        message: "Lodestar is restarting",
        fields: {},
    }
    /// The first-run setup token (§12; call site: the follow-up sweep,
    /// `services.rs`). The token works only until setup completes.
    AUTH_SETUP_TOKEN {
        id: "auth.setup_token",
        component: Auth,
        level: Warn,
        required: true,
        message: "First run: open Lodestar in a browser and enter setup token",
        fields: { token: SetupToken, address: Label },
    }
    /// One field of a section was saved: its name, never its value.
    SETTINGS_SAVED {
        id: "settings.saved",
        component: System,
        level: Info,
        required: true,
        message: "Setting saved",
        fields: { section: Label, field: Label, revision: Label },
    }
    /// A data kind or an opt-in turned on or off, by a save or because its
    /// time ran out.
    LOGGING_KIND_SWITCHED {
        id: "logging.kind_switched",
        component: System,
        level: Info,
        required: true,
        message: "Log data kind switched",
        fields: { kind: Label, state: Label, until: Label },
    }
    /// Writing the log file failed for lack of space: file logging stops,
    /// Lodestar keeps running.
    LOGGING_DISK_FULL {
        id: "logging.disk_full",
        component: System,
        level: Warn,
        required: true,
        message: "The disk is full: file logging stopped until Lodestar restarts; Lodestar keeps running",
        fields: {},
    }
    /// Rotating the log file failed: it keeps growing rather than losing
    /// lines.
    LOGGING_ROTATION_FAILED {
        id: "logging.rotation_failed",
        component: System,
        level: Warn,
        required: true,
        message: "Couldn't rotate the log file; still appending to it",
        fields: { error: Error },
    }

    // ── Logging ──
    /// The logging settings were applied.
    LOGGING_APPLIED {
        id: "logging.applied",
        component: System,
        level: Info,
        message: "Logging settings applied",
        fields: { verbosity: Label, data: Label },
    }
    /// A line couldn't be written to the log file (other than for space).
    LOGGING_FILE_ERROR {
        id: "logging.file_error",
        component: System,
        level: Warn,
        message: "Couldn't write the log file",
        fields: { error: Error },
    }
    /// The log file couldn't be opened.
    LOGGING_FILE_UNAVAILABLE {
        id: "logging.file_unavailable",
        component: System,
        level: Warn,
        message: "Couldn't open the log file; logging to the console only",
        fields: { path: Path, error: Error },
    }
    /// More start-up lines came than the file's start-up buffer holds.
    LOGGING_STARTUP_DROPPED {
        id: "logging.startup_dropped",
        component: System,
        level: Warn,
        message: "Start-up lines not kept in the log file",
        fields: { count: Label },
    }
    /// A rotated file couldn't be compressed or an old one deleted.
    LOGGING_CLEANUP_FAILED {
        id: "logging.cleanup_failed",
        component: System,
        level: Warn,
        message: "Couldn't compress or delete an old log file",
        fields: { error: Error },
    }
    /// The Libraries filter couldn't be applied.
    LOGGING_FILTER_NOT_APPLIED {
        id: "logging.filter_not_applied",
        component: System,
        level: Warn,
        message: "The Libraries log filter wasn't applied",
        fields: { error: Error },
    }

    // ── Settings ──
    /// A saved value was skipped at start (an older build's, a hand edit).
    SETTINGS_SAVED_IGNORED {
        id: "settings.saved_ignored",
        component: System,
        level: Warn,
        message: "Ignoring a saved setting",
        fields: { section: Label, field: Label, reason: Label },
    }
    /// A saved value shadows a different `config.toml` value.
    SETTINGS_OVERRIDES_FILE {
        id: "settings.overrides_file",
        component: System,
        level: Info,
        message: "A setting saved in the app overrides config.toml; set it to null in the app to reset",
        fields: { section: Label, field: Label },
    }
    /// A saved value for a field that's now read-only was removed.
    SETTINGS_RETIRED {
        id: "settings.retired",
        component: System,
        level: Info,
        message: "Removed a saved setting: it can only be set in config.toml or the environment now",
        fields: { section: Label, field: Label },
    }
    /// A section's saved revision wasn't a number.
    SETTINGS_REVISION_RESET {
        id: "settings.revision_reset",
        component: System,
        level: Warn,
        message: "A section's saved revision isn't a number; starting again",
        fields: { section: Label },
    }
    /// An earlier build's `general.log_level` moved to the `logging`
    /// section.
    SETTINGS_LOG_LEVEL_MOVED {
        id: "settings.log_level_moved",
        component: System,
        level: Info,
        message: "Moved the saved log level to the logging section",
        fields: {},
    }
    /// A new crawler intensity reached the running DHT node.
    SETTINGS_INTENSITY {
        id: "settings.intensity_applied",
        component: Dht,
        level: Info,
        message: "Crawler intensity applied",
        fields: { intensity: Label },
    }

    // ── Start-up checks ──
    /// `PUID`/`PGID` can't apply: not started as root.
    SYSTEM_IDS_IGNORED {
        id: "system.ids_ignored",
        component: System,
        level: Info,
        message: "PUID/PGID are ignored: not started as root, so the account can't change (with docker --user, the data directory must already be writable by that account)",
        fields: { uid: Label, gid: Label },
    }
    /// The data directory already belongs to the target account.
    SYSTEM_DATA_DIR_OWNED {
        id: "system.data_dir_owned",
        component: System,
        level: Info,
        message: "The data directory is owned by the target account",
        fields: { path: Path, uid: Label, gid: Label },
    }
    /// The data directory was handed to the target account.
    SYSTEM_DATA_DIR_CHOWNED {
        id: "system.data_dir_chowned",
        component: System,
        level: Info,
        message: "The data directory was owned by another account; changed it to the target account",
        fields: {
            path: Path,
            from_uid: Label,
            from_gid: Label,
            uid: Label,
            gid: Label,
            changed: Size,
            hard_links_left: Size,
            special_left: Size,
            too_deep_left: Size,
        },
    }
    /// Started as root and allowed to stay root.
    SYSTEM_ROOT {
        id: "system.running_as_root",
        component: System,
        level: Warn,
        message: "Running as root (LODESTAR_ALLOW_ROOT=1): privileges were not dropped",
        fields: { gid: Label },
    }
    /// Root was dropped.
    SYSTEM_DROPPED_ROOT {
        id: "system.dropped_root",
        component: System,
        level: Info,
        message: "dropped root: running as the target account with no supplementary groups",
        fields: { uid: Label, gid: Label },
    }
    /// The config file's data directory isn't the one PUID/PGID checked.
    SYSTEM_DATA_DIR_MISMATCH {
        id: "system.data_dir_mismatch",
        component: System,
        level: Warn,
        message: "The data directory from the config file isn't the one PUID/PGID ownership was checked on; set LODESTAR_DATA_DIR so both agree",
        fields: { path: Path, checked: Path },
    }
    /// The data directory is on Unraid's user share.
    SYSTEM_DATA_DIR_USER_SHARE {
        id: "system.data_dir_user_share",
        component: System,
        level: Warn,
        message: "The data folder is on Unraid's user share (/mnt/user, FUSE shfs). It's slow for the search index and risky for the database. Map the data folder to a pool path instead, such as /mnt/cache/appdata/lodestar, and keep only backups on the array.",
        fields: { path: Path },
    }
    /// The data directory is on another FUSE filesystem.
    SYSTEM_DATA_DIR_FUSE {
        id: "system.data_dir_fuse",
        component: System,
        level: Warn,
        message: "The data folder is on a FUSE filesystem. It's slow for the search index and may not give the database the locking it needs. Keep the data folder on a local disk, ideally an SSD.",
        fields: { path: Path },
    }
    /// The data directory is on a network filesystem.
    SYSTEM_DATA_DIR_NETWORK {
        id: "system.data_dir_network",
        component: System,
        level: Warn,
        message: "The data folder is on a network filesystem. Its file locking isn't reliable enough for the database, which can be damaged. Keep the data folder on a local disk, ideally an SSD, and send only backups to the share.",
        fields: { path: Path },
    }

    // ── Blocklists and the floor ──
    /// Floor entries removed from a file or upstream wait for the app.
    FILTERS_FLOOR_PENDING {
        id: "filters.floor_removals_pending",
        component: Filters,
        level: Warn,
        message: "Locked-floor entries were removed from their source; remove them in the app to unblock",
        fields: { count: Size, source: Label, path: Path, list: Label },
    }
    /// `floor = true` in the config for a list subscribed outside it.
    FILTERS_SUBSCRIPTION_FLOOR_IGNORED {
        id: "filters.subscription_floor_ignored",
        component: Filters,
        level: Warn,
        message: "A list is already subscribed outside the floor; floor = true in the config is ignored (mark it in the app)",
        fields: { url: Url },
    }
    /// A list's refresh failed; its status in the app says why.
    FILTERS_SUBSCRIPTION_REFRESH_FAILED {
        id: "filters.subscription_refresh_failed",
        component: Filters,
        level: Warn,
        message: "A blocklist refresh failed; the list's status in the app says why",
        fields: { list: Label, url: Url },
    }
    /// A list's new version was held back; its status in the app says why.
    FILTERS_SUBSCRIPTION_HELD {
        id: "filters.subscription_held",
        component: Filters,
        level: Warn,
        message: "A blocklist's new version was held back; the list's status in the app says why",
        fields: { list: Label, url: Url },
    }
    /// A refresh's result couldn't be recorded.
    FILTERS_SUBSCRIPTION_RECORD_FAILED {
        id: "filters.subscription_record_failed",
        component: Filters,
        level: Warn,
        message: "Recording a blocklist refresh failed",
        fields: { list: Label, error: Error },
    }
    /// The subscriptions couldn't be read.
    FILTERS_SUBSCRIPTIONS_READ_FAILED {
        id: "filters.subscriptions_read_failed",
        component: Filters,
        level: Warn,
        message: "Reading the blocklist subscriptions failed",
        fields: { error: Error },
    }
    /// Refreshed lists couldn't be applied.
    FILTERS_SUBSCRIPTIONS_APPLY_FAILED {
        id: "filters.subscriptions_apply_failed",
        component: Filters,
        level: Warn,
        message: "Applying the blocklist subscriptions failed",
        fields: { error: Error },
    }
}
