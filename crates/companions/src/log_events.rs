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
}
