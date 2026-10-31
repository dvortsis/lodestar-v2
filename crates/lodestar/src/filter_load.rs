//! The filter as the server builds it, shared with `lodestar import` so
//! an import never skips a rule source the crawler applies:
//! 1. the config's subscriptions synced into the store;
//! 2. the config's blocklist files synced into the store's floor;
//! 3. the subscribed lists (the floor-marked ones included) applied;
//! 4. the store's CS-1 entries, signatures, switches, packs, your rules
//!    and classification rules ([`lodestar_pipeline::load_filter`]): the
//!    server's pipeline runs this step at its start, the CLI right after
//!    [`sync_sources`].
//!
//! The seal key is read here too ([`read_seal_key`]); only the server
//! creates one.

use std::path::Path;

use lodestar_filter::Handle;
use lodestar_filter::lists::SharedIps;
use lodestar_store::Store;

use crate::config::{BlocklistFile, Subscriptions};

/// Steps 1 to 3 of the module docs, on a writable connection.
pub fn sync_sources(
    store: &mut Store,
    subscriptions: &Subscriptions,
    blocklist: &[BlocklistFile],
    filter: &Handle,
    ips: &SharedIps,
    now: i64,
) -> Result<(), String> {
    crate::subscriptions::sync_config(store, subscriptions)?;
    // Before the pipeline loads the floor from the store.
    crate::floor::sync_files(store, blocklist, now)?;
    crate::subscriptions::apply(store, filter, ips)
}

/// The key that seals quarantined names, if one was made: `None` when
/// the file doesn't exist; an error when it can't be read or isn't 32
/// bytes.
pub fn read_seal_key(path: &Path) -> Result<Option<[u8; 32]>, String> {
    match std::fs::read(path) {
        Ok(bytes) => bytes
            .try_into()
            .map(Some)
            .map_err(|_| format!("{}: not a 32-byte key", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}
