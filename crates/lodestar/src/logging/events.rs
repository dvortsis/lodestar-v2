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
        fields: { clean: Label },
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
        message: "restart requested: shutting down, then starting again",
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
    /// The current file reached the folder's total cap because rotation
    /// keeps failing: lines stop until a rotation succeeds.
    LOGGING_OVER_CAP {
        id: "logging.over_cap",
        component: System,
        level: Warn,
        message: "The log file reached the folder's total cap; file logging pauses until it rotates",
        fields: {},
    }
    /// The logs folder isn't private (another account's on Unix; one
    /// Lodestar didn't make on Windows), and it's left as it is.
    LOGGING_FOLDER_SHARED {
        id: "logging.folder_shared",
        component: System,
        level: Warn,
        message: "The logs folder wasn't made private by Lodestar and is left as it is; other users may read the log files",
        fields: { path: Path },
    }
    /// A rotated file couldn't be compressed or an old one deleted.
    LOGGING_CLEANUP_FAILED {
        id: "logging.cleanup_failed",
        component: System,
        level: Warn,
        message: "Couldn't compress or delete an old log file",
        fields: { error: Error },
    }
    /// Log files were deleted from the Logging screen: how many and how
    /// much, never which.
    LOGGING_FILES_DELETED {
        id: "logging.files_deleted",
        component: System,
        level: Info,
        message: "Log files deleted",
        fields: { count: Size, bytes: Size },
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
    /// The restart proof's witness directory (`LODESTAR_DHT_WITNESS_DIR`,
    /// CR-0d) at start-up as root, handed to the target account
    /// (`changed` entries).
    SYSTEM_WITNESS_DIR {
        id: "system.witness_dir",
        component: System,
        level: Info,
        message: "The restart witness directory was handed to the target account",
        fields: { path: Path, uid: Label, gid: Label, changed: Size },
    }
    /// CR-0d §1.5: the restart witness directory wasn't handed over (a
    /// link, not a directory, `/`, other files in it), so with the restart
    /// check on the run has no witness and every restart waits out the
    /// sampling delay. The image sets the variable either way.
    SYSTEM_WITNESS_DIR_LEFT {
        id: "system.witness_dir_left",
        component: System,
        level: Warn,
        message: "The restart witness directory was left as it is: if the restart check is on, restarts wait out the sampling delay",
        fields: { path: Path, error: Error },
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
    /// The HTTP client for blocklist refreshes couldn't be built, so no
    /// list is refreshed this run.
    FILTERS_SUBSCRIPTIONS_CLIENT_FAILED {
        id: "filters.subscriptions_client_failed",
        component: Filters,
        level: Error,
        message: "Blocklists can't be refreshed: the HTTP client couldn't be set up",
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
    /// A saved search's query can't be read by this build; it's skipped
    /// until it's saved again (once per search per run of Lodestar).
    ALERTS_SEARCH_UNREADABLE {
        id: "alerts.search_unreadable",
        component: Search,
        level: Warn,
        message: "A saved search has a query this build can't read; skipped",
        fields: { saved_search: Label },
    }
    /// New alerts were recorded but their live event wasn't sent.
    ALERTS_ANNOUNCE_FAILED {
        id: "alerts.announce_failed",
        component: Search,
        level: Warn,
        message: "Announcing new alerts failed; they're recorded and listed",
        fields: {},
    }
    /// One percolator run: counts only, never a query.
    ALERTS_RUN {
        id: "alerts.run",
        component: Search,
        level: Debug,
        message: "Percolator run",
        fields: { searches: Label, through: Label, alerts: Label, unreadable: Label, more: Label },
    }
    /// New alerts were found.
    ALERTS_FOUND {
        id: "alerts.found",
        component: Search,
        level: Info,
        message: "New alerts",
        fields: { alerts: Label, searches: Label, events: Label, took_ms: Label },
    }
    /// Old alerts were pruned.
    ALERTS_PRUNED {
        id: "alerts.pruned",
        component: Search,
        level: Info,
        message: "Pruned old alerts",
        fields: { alerts: Label },
    }
    /// Pruning old alerts failed.
    ALERTS_PRUNE_FAILED {
        id: "alerts.prune_failed",
        component: Search,
        level: Warn,
        message: "Pruning old alerts failed",
        fields: { error: Error },
    }
    /// The index's watermark couldn't be read.
    ALERTS_WATERMARK_FAILED {
        id: "alerts.watermark_failed",
        component: Search,
        level: Warn,
        message: "Reading the index watermark failed",
        fields: { error: Error },
    }
    /// A percolator run failed or panicked; it's retried.
    ALERTS_RUN_FAILED {
        id: "alerts.run_failed",
        component: Search,
        level: Warn,
        message: "A percolator run failed; retrying",
        fields: { panicked: Label, error: Error },
    }
    /// The pending address change couldn't be read; none applies.
    ADDRESS_PENDING_UNREADABLE {
        id: "address.pending_unreadable",
        component: System,
        level: Warn,
        message: "address: the pending address change couldn't be read; it's ignored",
        fields: { error: Error },
    }
    /// The pending address change couldn't be stored: it won't survive a
    /// restart.
    ADDRESS_SAVE_FAILED {
        id: "address.save_failed",
        component: System,
        level: Error,
        message: "address: storing the pending address change failed; it won't survive a restart",
        fields: { error: Error },
    }
    /// `LODESTAR_HTTP_BIND` sets the address; the pending change waits.
    ADDRESS_ENV_HOLDS {
        id: "address.env_holds",
        component: System,
        level: Info,
        message: "address: LODESTAR_HTTP_BIND sets the address; the pending change waits until it's removed",
        fields: {},
    }
    /// The pending change no longer applies and is dropped.
    ADDRESS_CHANGE_DROPPED {
        id: "address.change_dropped",
        component: System,
        level: Info,
        message: "address: dropping the pending address change",
        fields: { reason: Label },
    }
    /// Nobody confirmed the new address within `minutes`: going back.
    ADDRESS_REVERTING {
        id: "address.reverting",
        component: System,
        level: Warn,
        message: "address: going back to the previous address; nobody confirmed the new one in time",
        fields: { address: Label, previous: Label, minutes: Label },
    }
    /// The admin asked to undo an address change waiting for
    /// confirmation: going back.
    ADDRESS_REVERT_ASKED {
        id: "address.revert_asked",
        component: System,
        level: Info,
        message: "address: going back to the previous address, as asked",
        fields: { address: Label, previous: Label },
    }
    /// The new address couldn't be bound: going back.
    ADDRESS_BIND_FAILED {
        id: "address.bind_failed",
        component: System,
        level: Error,
        message: "address: binding the new address failed; going back",
        fields: { address: Label, previous: Label, error: Error },
    }
    /// The previous address couldn't be saved back; the next start tries
    /// again.
    ADDRESS_REVERT_SAVE_FAILED {
        id: "address.revert_save_failed",
        component: System,
        level: Error,
        message: "address: saving the previous address failed; the next start tries again",
        fields: { error: Error },
    }
    /// Serving on a new address that has to be confirmed in the UI.
    ADDRESS_CONFIRM_NEEDED {
        id: "address.confirm_needed",
        component: System,
        level: Warn,
        message: "address: now serving at a new address; open it and choose \"Keep this address\" in time, or Lodestar goes back",
        fields: { address: Label, minutes: Label, previous: Label },
    }
    /// Serving on the previous address, which couldn't be saved back.
    ADDRESS_SERVING_FALLBACK {
        id: "address.serving_fallback",
        component: System,
        level: Warn,
        message: "address: serving on the previous address although another is saved; the next start tries again",
        fields: { address: Label, saved: Label },
    }
    /// The new address was kept.
    ADDRESS_KEPT {
        id: "address.kept",
        component: System,
        level: Info,
        message: "address: kept the new address",
        fields: { address: Label },
    }
    /// A saved change was undone before the restart that would apply it.
    ADDRESS_CHANGE_UNDONE {
        id: "address.change_undone",
        component: System,
        level: Info,
        message: "address: the address change was undone before a restart",
        fields: {},
    }
    /// The previous address applies only at the next restart.
    ADDRESS_APPLIES_AT_RESTART {
        id: "address.applies_at_restart",
        component: System,
        level: Warn,
        message: "address: the previous address applies when Lodestar is next restarted",
        fields: { address: Label },
    }
    /// Going back failed; a restart goes back before binding.
    ADDRESS_REVERT_FAILED {
        id: "address.revert_failed",
        component: System,
        level: Error,
        message: "address: going back failed",
        fields: { error: Error },
    }
    /// The process replaces itself with the binary at `path` (a restart
    /// on Unix).
    APP_REEXEC {
        id: "app.reexec",
        component: System,
        level: Info,
        message: "Restarting: starting the binary again",
        fields: { path: Path },
    }
    /// Windows leaves too few dynamic ports for the fetch pool. To fix
    /// it, run `netsh int <family> set dynamicport tcp start=<fix_start>
    /// num=<fix_count>` as administrator (no fix: 0), or lower the
    /// crawler's intensity or max_sockets.
    WINDOWS_PORTS_LOW {
        id: "windows.ports_low",
        component: System,
        level: Warn,
        message: "Windows has too few ports left for outgoing connections; fetches may fail and other programs may lose network access. To fix it, run `netsh int <family> set dynamicport tcp start=<fix_start> num=<fix_count>` in a Command Prompt opened as administrator, then check `netsh int <family> show excludedportrange protocol=tcp` (Hyper-V and WinNAT reserve ports); or lower the crawler's intensity or max_sockets. fix_count=0: no range is large enough, so lower them",
        fields: { family: Label, available: Size, excluded: Size, needed: Size, max_sockets: Size, fix_start: Size, fix_count: Size },
    }
    /// The dynamic port range is wide enough.
    WINDOWS_PORTS_OK {
        id: "windows.ports_ok",
        component: System,
        level: Debug,
        message: "Windows' dynamic port range leaves enough ports",
        fields: { family: Label, start: Size, end: Size, available: Size },
    }
    /// The dynamic port range couldn't be read.
    WINDOWS_PORTS_UNREADABLE {
        id: "windows.ports_unreadable",
        component: System,
        level: Debug,
        message: "Windows' dynamic port range couldn't be read",
        fields: { family: Label },
    }
    /// `LODESTAR_PRIORITY` isn't one Lodestar knows; below normal is kept.
    WINDOWS_PRIORITY_UNKNOWN {
        id: "windows.priority_unknown",
        component: System,
        level: Warn,
        message: "LODESTAR_PRIORITY is unknown (use below_normal or normal); keeping below normal",
        fields: {},
    }
    /// The process runs at below-normal priority.
    WINDOWS_PRIORITY_LOWERED {
        id: "windows.priority_lowered",
        component: System,
        level: Info,
        message: "Running at below-normal priority",
        fields: {},
    }
    /// The process priority couldn't be lowered.
    WINDOWS_PRIORITY_FAILED {
        id: "windows.priority_failed",
        component: System,
        level: Warn,
        message: "The process priority couldn't be lowered",
        fields: { error: Error },
    }
    /// The Windows service starts, with its data folder.
    SERVICE_STARTING {
        id: "service.starting",
        component: System,
        level: Info,
        message: "service: starting",
        fields: { path: Path },
    }
    /// The Windows service was asked to stop.
    SERVICE_STOPPING {
        id: "service.stopping",
        component: System,
        level: Info,
        message: "service: stopping",
        fields: {},
    }
    /// The Windows service stopped.
    SERVICE_STOPPED {
        id: "service.stopped",
        component: System,
        level: Info,
        message: "service: stopped",
        fields: {},
    }
    /// CR-0d §1.5: the service's restart witness folder couldn't be made
    /// private, so the DHT runs without one and suppresses after every
    /// restart (`no_witness`).
    SERVICE_WITNESS_REFUSED {
        id: "service.witness_refused",
        component: System,
        level: Warn,
        message: "service: no restart witness folder, so restarts wait out the sampling delay",
        fields: { path: Path, error: Error },
    }
    /// The Windows service stopped on an error.
    SERVICE_FAILED {
        id: "service.failed",
        component: System,
        level: Error,
        message: "service: stopped on an error",
        fields: { error: Error },
    }
    /// `serve --fixture`: test data, not a real install.
    FIXTURE_MODE {
        id: "fixture.mode",
        component: System,
        level: Warn,
        message: "fixture mode: test data in this folder, not a real install",
        fields: { path: Path },
    }
    /// The fixture's 24 h and 7 d numbers no longer match its manifest.
    FIXTURE_STALE {
        id: "fixture.stale",
        component: System,
        level: Warn,
        message: "this fixture was built more than six hours ago: its 24 h and 7 d numbers and cohort finality no longer match fixture.json; delete the folder to rebuild it",
        fields: {},
    }
    FIXTURE_BUILDING {
        id: "fixture.building",
        component: System,
        level: Info,
        message: "building the fixture",
        fields: { path: Path, anchor: Label },
    }
    FIXTURE_BUILT {
        id: "fixture.built",
        component: System,
        level: Info,
        message: "fixture built",
        fields: { torrents: Size, visible: Size, manifest: Path },
    }
    FIXTURE_SERVING {
        id: "fixture.serving",
        component: System,
        level: Info,
        message: "serving the fixture",
        fields: { path: Path },
    }
    /// Claimed with the password from the environment, never shown.
    FIXTURE_CLAIMED {
        id: "fixture.claimed",
        component: System,
        level: Info,
        message: "claimed with LODESTAR_FIXTURE_PASSWORD",
        fields: {},
    }
    FIXTURE_PASSWORD_IGNORED {
        id: "fixture.password_ignored",
        component: System,
        level: Info,
        message: "LODESTAR_FIXTURE_PASSWORD ignored: this fixture is claimed already; sign in with its password",
        fields: {},
    }
    /// Served beyond this computer.
    FIXTURE_PUBLIC {
        id: "fixture.public",
        component: System,
        level: Warn,
        message: "the fixture is served beyond this computer; it's test data, but anyone who can reach the address can try to sign in",
        fields: { address: Label },
    }
    /// A temporary fixture's folder couldn't be removed.
    FIXTURE_CLEANUP_FAILED {
        id: "fixture.cleanup_failed",
        component: System,
        level: Warn,
        message: "the temporary fixture's folder couldn't be removed",
        fields: { path: Path, error: Error },
    }
    /// The fixture couldn't be built or served.
    FIXTURE_FAILED {
        id: "fixture.failed",
        component: System,
        level: Error,
        message: "the fixture couldn't be built or served",
        fields: { error: Error },
    }
    /// The Insights recorder stopped; Insights stops updating until a
    /// restart.
    INSIGHTS_RECORDER_STOPPED {
        id: "insights.recorder_stopped",
        component: Search,
        level: Error,
        message: "insights: the recorder stopped",
        fields: { error: Error },
    }
    /// One of the recorder's steps failed; the next tick tries again.
    /// `step` names it; the error shows its kind only.
    INSIGHTS_STEP_FAILED {
        id: "insights.step_failed",
        component: Search,
        level: Warn,
        message: "insights: a recorder step failed; the next tick tries again",
        fields: { step: Label, error: Error },
    }
    /// Metrics with no aggregation kind: their hourly rows are kept.
    INSIGHTS_UNKNOWN_METRICS {
        id: "insights.unknown_metrics",
        component: Search,
        level: Warn,
        message: "insights: some metrics have no aggregation kind; their hourly rows are kept",
        fields: { count: Size },
    }
    /// The daily compaction folded hourly rows up to a day.
    INSIGHTS_COMPACTED {
        id: "insights.compacted",
        component: Search,
        level: Debug,
        message: "insights: compacted",
        fields: { day: Label, rows: Size },
    }
    /// The rollups were recounted after an unclean stop.
    INSIGHTS_REPAIRED {
        id: "insights.repaired",
        component: Search,
        level: Info,
        message: "insights: repaired the rollups after an unclean stop",
        fields: {},
    }
    /// The swarm-stats columns are ready: from last run's snapshot, or
    /// rebuilt from the store after a crash.
    APP_HOT_COLUMNS {
        id: "app.hot_columns",
        component: System,
        level: Info,
        message: "Swarm-stats columns ready",
        fields: { torrents: Size, ms: Size, from: Label },
    }
    /// Serving: the UI's address and the data folder.
    APP_SERVING {
        id: "app.serving",
        component: System,
        level: Info,
        message: "UI ready; status page on /status",
        fields: { address: Label, path: Path },
    }
    /// Shutting down: saving the queue and the DHT state.
    APP_STOPPING {
        id: "app.stopping",
        component: System,
        level: Info,
        message: "shutting down: saving the queue and DHT state",
        fields: {},
    }
    /// The swarm-stats snapshot couldn't be saved; the next start rebuilds
    /// it from the store.
    APP_HOT_SAVE_FAILED {
        id: "app.hot_save_failed",
        component: System,
        level: Error,
        message: "Saving the swarm-stats snapshot failed; the next start rebuilds it",
        fields: { error: Error },
    }
    /// A client kept the graceful shutdown waiting (a download or export
    /// held open without reading): its connections are dropped after
    /// `waited_s` seconds so the queue, the DHT state and the index are
    /// still saved.
    APP_SHUTDOWN_FORCED {
        id: "app.shutdown_forced",
        component: System,
        level: Warn,
        message: "Open connections kept the shutdown waiting; closing them",
        fields: { waited_s: Size },
    }
    /// A command (`serve`, `import-bitmagnet`) ended on an error. Its
    /// text, scrubbed of URL credentials, goes to stderr for the person
    /// who ran it; the line shows the command only.
    APP_COMMAND_FAILED {
        id: "app.command_failed",
        component: System,
        level: Error,
        message: "The command failed",
        fields: { command: Label, error: Error },
    }
    /// A bitmagnet import's progress, every 100,000 torrents read.
    APP_IMPORT_PROGRESS {
        id: "app.import_progress",
        component: Sources,
        level: Info,
        message: "Importing from bitmagnet",
        fields: { read: Size, new: Size, per_second: Size },
    }
    /// Every service stopped in order: the queue, the DHT state and the
    /// index are saved.
    APP_STOPPED_CLEANLY {
        id: "app.stopped_cleanly",
        component: System,
        level: Info,
        message: "stopped cleanly: the queue, the DHT state and the index are saved",
        fields: {},
    }
}
