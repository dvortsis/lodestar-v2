//! The companions crate's log events (architecture §12.1). Nothing here
//! carries a credential, a URL you entered or text a companion sent; later
//! CMP slices add their events below, one per line-edit.

lodestar_log::catalogue! {
    "lodestar-companions";
    /// The compiled-in catalogue didn't validate, so companion installs
    /// are off for this run. `rule` is the broken rule's code. Declared
    /// here with the crate; CMP-0's catalogue loader emits it.
    CATALOGUE_INVALID {
        id: "companions.catalogue_invalid",
        component: Companions,
        level: Error,
        required: true,
        message: "The companion catalogue is invalid; companion installs are off",
        fields: { rule: Label },
    }
    /// One setting a configuration plan listed was written to a companion
    /// through its API (CMP-8b). `setting` is the setting's name; its value
    /// is never logged.
    CONFIG_APPLIED {
        id: "companions.config_applied",
        component: Companions,
        level: Info,
        message: "A companion setting was configured",
        fields: { companion: Label, setting: Label },
    }
    /// Configuring a companion stopped at a step (CMP-8b). `setting` names
    /// the step (`plan`, a setting's name, `lodestar.settings`); `reason` is
    /// the code's own word for the failure, never the companion's answer.
    CONFIG_FAILED {
        id: "companions.config_failed",
        component: Companions,
        level: Warn,
        message: "Configuring a companion failed",
        fields: { companion: Label, setting: Label, reason: Label },
    }
    /// A configured companion was handed to Lodestar's own settings
    /// (CMP-8b): `entry` is `download_client` or `indexer_manager`.
    CONFIG_HANDED_OVER {
        id: "companions.config_handed_over",
        component: Companions,
        level: Info,
        message: "A configured companion was added to Lodestar's settings",
        fields: { companion: Label, entry: Label },
    }
}
