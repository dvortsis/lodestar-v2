//! Source provenance, the shared id sequence and the persisted fetch queue
//! (architecture §5, §18.5).

use lodestar_metainfo::InfoHash;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Error, Result, Store};

/// `torrents.sources` is a u64 bitset, so there are at most 64 sources.
/// Ids are never reused, even after a purge.
pub const MAX_SOURCES: u8 = 64;

/// A registered source plugin (`sources.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(u8);

impl SourceId {
    pub const fn new(id: u8) -> Option<SourceId> {
        if id < MAX_SOURCES {
            Some(SourceId(id))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    /// This source's bit in `torrents.sources`, as SQLite stores it.
    pub(crate) fn bit(self) -> i64 {
        (1u64 << self.0) as i64
    }

    pub(crate) fn from_sql(v: i64) -> Result<SourceId> {
        u8::try_from(v)
            .ok()
            .and_then(SourceId::new)
            .ok_or(Error::Corrupt("source id out of range"))
    }
}

/// A source's lifecycle state (§18.1). Values are persisted: append only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Enabled = 0,
    Paused = 1,
    /// Off, data kept.
    Disabled = 2,
    /// Off, and its exclusive items and claims removed.
    Purged = 3,
}

impl SourceState {
    fn from_sql(v: i64) -> Result<SourceState> {
        Ok(match v {
            0 => SourceState::Enabled,
            1 => SourceState::Paused,
            2 => SourceState::Disabled,
            3 => SourceState::Purged,
            _ => return Err(Error::Corrupt("unknown source state")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: SourceId,
    pub plugin: String,
    pub version: String,
    pub state: SourceState,
    pub added_at: i64,
}

/// A key waiting for metadata. Peers aren't kept: addresses are transient
/// (§18.7) and stale after a restart anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedKey {
    pub info_hash: InfoHash,
    pub source: SourceId,
    /// Discovery hour, for cohort attribution (§7).
    pub cohort: i64,
    pub strategy: u8,
    pub priority: u8,
    pub attempts: u16,
    /// Found only through a private source: never sent to public peer
    /// finding (§18.7, BEP 27).
    pub private_source: bool,
}

const QUEUE_PRIVATE: i64 = 1;

/// Takes the next id from the shared sequence. Call inside the writer's
/// transaction so a rollback returns it.
pub(crate) fn next_id(conn: &Connection) -> Result<i64> {
    Ok(conn
        .prepare_cached("UPDATE id_seq SET next = next + 1 RETURNING next - 1")?
        .query_row([], |r| r.get(0))?)
}

/// The bits of every source that's disabled or purged: what [`Store::write`]
/// and [`Store::promote`] never write (SRC-14b). One query over at most 64
/// rows; call it inside the write's transaction, so a purge (which
/// disables first) and the write can't interleave.
pub(crate) fn stopped_mask(conn: &Connection) -> Result<u64> {
    let mut stmt = conn.prepare_cached("SELECT id FROM sources WHERE state IN (?1, ?2)")?;
    let ids = stmt.query_map(
        params![SourceState::Disabled as i64, SourceState::Purged as i64],
        |r| r.get::<_, i64>(0),
    )?;
    let mut mask = 0u64;
    for id in ids {
        mask |= SourceId::from_sql(id?)?.bit() as u64;
    }
    Ok(mask)
}

/// Where a source's consent record lives in `settings`: this, then the
/// plugin's manifest id. Its text is the sources crate's to encode.
const CONSENT_KEY: &str = "sources.consent.";
/// Where a source's sealed credentials live in `settings`: this, the
/// plugin's manifest id, `/`, then the credential's name. The value is the
/// sealed blob; the key file that opens it is outside the database.
const SECRET_KEY: &str = "sources.secret.";

/// A source's own settings beyond its state and priority (§18.4), each
/// kept in `settings` under its prefix, then the plugin's manifest id.
/// Their text is the pipeline's to encode. A purge drops them with the
/// consent record and credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSetting {
    /// How its traffic leaves (direct, or a SOCKS5 proxy).
    Egress,
    /// The daily windows it may run in.
    Schedule,
    /// Its concurrency, in the plugin's own unit.
    Agents,
    /// The plugin's own settings, as its settings form saves them (JSON
    /// under its `config_schema`), read when it's built at start.
    Config,
}

impl SourceSetting {
    pub const ALL: [SourceSetting; 4] = [
        SourceSetting::Egress,
        SourceSetting::Schedule,
        SourceSetting::Agents,
        SourceSetting::Config,
    ];

    fn key(self, plugin: &str) -> String {
        let prefix = match self {
            SourceSetting::Egress => "sources.egress.",
            SourceSetting::Schedule => "sources.schedule.",
            SourceSetting::Agents => "sources.agents.",
            SourceSetting::Config => "sources.config.",
        };
        format!("{prefix}{plugin}")
    }
}

/// A purge that started and hasn't finished (SRC-14b): this, then the
/// plugin's manifest id; the value is when it started (Unix seconds). The
/// source stays disabled until [`Store::finish_source_purge`] removes it,
/// and a purge run again goes over everything from the start.
const PURGING_KEY: &str = "sources.purging.";
/// When a source was last purged (Unix seconds): this, then the plugin's
/// manifest id. The snapshot purge drops the source's records (consent,
/// credentials, settings) from every snapshot taken before it, so a
/// rollback can't bring them back (`snapshots.rs`). Kept until no
/// snapshot it could match is left (`JOURNAL_SECS`), and through a
/// rollback, which carries it into the restored store.
pub(crate) const PURGED_KEY: &str = "sources.purged.";

pub(crate) fn secret_prefix(plugin: &str) -> String {
    format!("{SECRET_KEY}{plugin}/")
}

fn plugin_of(conn: &Connection, id: SourceId) -> Result<String> {
    conn.query_row("SELECT plugin FROM sources WHERE id = ?1", [id.0], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or(Error::NotFound("source"))
}

/// The `settings` keys of a source's consent record and own settings: what
/// a purge drops besides its credentials (under [`secret_prefix`]).
pub(crate) fn record_keys(plugin: &str) -> Vec<String> {
    std::iter::once(format!("{CONSENT_KEY}{plugin}"))
        .chain(SourceSetting::ALL.iter().map(|s| s.key(plugin)))
        .collect()
}

/// Removes a source's consent record, credentials and own settings (on
/// purge, and from a snapshot taken before one). Generic over the
/// connection, so the snapshot purge applies it to a snapshot.
pub(crate) fn drop_records(conn: &Connection, plugin: &str) -> rusqlite::Result<()> {
    for key in record_keys(plugin) {
        conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    }
    let prefix = secret_prefix(plugin);
    conn.execute(
        "DELETE FROM settings WHERE substr(key, 1, ?2) = ?1",
        params![prefix, prefix.chars().count() as i64],
    )?;
    Ok(())
}

impl Store {
    /// Returns `plugin`'s id, registering it as enabled on first sight, and
    /// records the running version. For importers and tests: the
    /// pipeline registers plugins with [`Store::register_source_as`], so a
    /// plugin that needs consent is never enabled, even for a moment.
    pub fn register_source(&mut self, plugin: &str, version: &str, now: i64) -> Result<SourceId> {
        self.register_source_as(plugin, version, SourceState::Enabled, now)
    }

    /// Returns `plugin`'s id and records the running version. A plugin
    /// seen for the first time is registered in state `first`, in the
    /// same write, so a crash can't leave it in any other state (§18.7).
    /// A known plugin keeps its stored state.
    pub fn register_source_as(
        &mut self,
        plugin: &str,
        version: &str,
        first: SourceState,
        now: i64,
    ) -> Result<SourceId> {
        // `/` ends the plugin's part of a credential key: an id holding one
        // could reach another plugin's credentials.
        if plugin.is_empty() || plugin.contains('/') {
            return Err(Error::Invalid("source plugin id"));
        }
        let tx = crate::begin_write(&mut self.conn)?;
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM sources WHERE plugin = ?1", [plugin], |r| {
                r.get(0)
            })
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE sources SET version = ?2 WHERE id = ?1 AND version != ?2",
                    params![id, version],
                )?;
                id
            }
            None => {
                let id: i64 =
                    tx.query_row("SELECT coalesce(max(id) + 1, 0) FROM sources", [], |r| {
                        r.get(0)
                    })?;
                if id >= i64::from(MAX_SOURCES) {
                    return Err(Error::TooManySources);
                }
                tx.execute(
                    "INSERT INTO sources (id, plugin, version, state, added_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, plugin, version, first as i64, now],
                )?;
                id
            }
        };
        tx.commit()?;
        SourceId::from_sql(id)
    }

    /// Sets a source's state to anything but enabled. Purging also drops
    /// its consent record and its credentials, in the same transaction.
    /// Enabling goes only through [`Store::enable_source`], which the
    /// consent check (§18.7) sits in front of.
    pub fn set_source_state(&mut self, id: SourceId, state: SourceState) -> Result<()> {
        if state == SourceState::Enabled {
            return Err(Error::Invalid("enable a source with enable_source"));
        }
        let tx = crate::begin_write(&mut self.conn)?;
        tx.execute(
            "UPDATE sources SET state = ?2 WHERE id = ?1",
            params![id.0, state as i64],
        )?;
        if state == SourceState::Purged {
            let plugin = plugin_of(&tx, id)?;
            drop_records(&tx, &plugin)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Starts purging source `id` (SRC-14b): disables it, unless it's
    /// already purged, and records that a purge is under way, in one
    /// transaction. From here nothing it finds is written
    /// ([`Store::write`] refuses its fetches), and [`Store::purge_source`]
    /// may run. Starting again, after a crash or a cancel, is the same.
    pub fn begin_source_purge(&mut self, id: SourceId, now: i64) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        let plugin = plugin_of(&tx, id)?;
        tx.execute(
            "UPDATE sources SET state = ?2 WHERE id = ?1 AND state != ?3",
            params![
                id.0,
                SourceState::Disabled as i64,
                SourceState::Purged as i64
            ],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO NOTHING",
            params![format!("{PURGING_KEY}{plugin}"), now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Ends a purge that [`Store::purge_source`] has finished: sets the
    /// source Purged, drops its consent record, credentials and own
    /// settings, clears the purge under way and records when it ended,
    /// all in one transaction. A credential or setting written while the
    /// purge ran goes with them.
    pub fn finish_source_purge(&mut self, id: SourceId, now: i64) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        let plugin = plugin_of(&tx, id)?;
        tx.execute(
            "UPDATE sources SET state = ?2 WHERE id = ?1",
            params![id.0, SourceState::Purged as i64],
        )?;
        drop_records(&tx, &plugin)?;
        tx.execute(
            "DELETE FROM settings WHERE key = ?1",
            [format!("{PURGING_KEY}{plugin}")],
        )?;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![format!("{PURGED_KEY}{plugin}"), now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// When `plugin`'s unfinished purge started, if one is under way.
    pub fn source_purge_started(&self, plugin: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [format!("{PURGING_KEY}{plugin}")],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
    }

    /// Enables a source and, when given, stores its new consent record,
    /// in one transaction: the record and the state agree after a crash.
    ///
    /// Refused (`Error::Invalid`) while a purge is under way
    /// ([`Store::begin_source_purge`]): the purge finishes first.
    pub fn enable_source(&mut self, id: SourceId, consent: Option<&str>) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        let plugin = plugin_of(&tx, id)?;
        let purging = tx
            .query_row(
                "SELECT 1 FROM settings WHERE key = ?1",
                [format!("{PURGING_KEY}{plugin}")],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if purging {
            return Err(Error::Invalid("a purge of this source is under way"));
        }
        if let Some(record) = consent {
            tx.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![format!("{CONSENT_KEY}{plugin}"), record],
            )?;
        }
        tx.execute(
            "UPDATE sources SET state = ?2 WHERE id = ?1",
            params![id.0, SourceState::Enabled as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `plugin`'s consent record, as [`Store::enable_source`] stored it.
    pub fn source_consent(&self, plugin: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [format!("{CONSENT_KEY}{plugin}")],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    /// The names of the credentials stored for `plugin`, sorted.
    pub fn source_secret_names(&self, plugin: &str) -> Result<Vec<String>> {
        let prefix = secret_prefix(plugin);
        let mut stmt = self.conn.prepare_cached(
            "SELECT substr(key, ?2 + 1) FROM settings
             WHERE substr(key, 1, ?2) = ?1 ORDER BY key",
        )?;
        // `substr` counts characters, not bytes.
        let rows = stmt.query_map(params![prefix, prefix.chars().count() as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One sealed credential of `plugin`'s.
    pub fn source_secret(&self, plugin: &str, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [format!("{}{name}", secret_prefix(plugin))],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?)
    }

    /// Stores the sealed credentials in `set`, removes those named in
    /// `remove` and, when given, sets the source's state, all in one
    /// transaction (removing what an enabled source needs disables it in
    /// the same write).
    pub fn update_source_secrets(
        &mut self,
        id: SourceId,
        set: &[(&str, &[u8])],
        remove: &[&str],
        state: Option<SourceState>,
    ) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        let prefix = secret_prefix(&plugin_of(&tx, id)?);
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )?;
            for (name, sealed) in set {
                upsert.execute(params![format!("{prefix}{name}"), sealed])?;
            }
            let mut delete = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
            for name in remove {
                delete.execute([format!("{prefix}{name}")])?;
            }
        }
        if let Some(state) = state {
            tx.execute(
                "UPDATE sources SET state = ?2 WHERE id = ?1",
                params![id.0, state as i64],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One of `plugin`'s own settings, as [`Store::set_source_settings`]
    /// stored it; `None` when it keeps the plugin's default.
    pub fn source_setting(&self, plugin: &str, setting: SourceSetting) -> Result<Option<String>> {
        self.setting_text(&setting.key(plugin))
    }

    /// Sets (`Some`) or resets to the plugin's default (`None`) each of a
    /// source's settings in `changes`, in one transaction.
    pub fn set_source_settings(
        &mut self,
        id: SourceId,
        changes: &[(SourceSetting, Option<&str>)],
    ) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        let plugin = plugin_of(&tx, id)?;
        for (setting, value) in changes {
            match value {
                Some(v) => tx.execute(
                    "INSERT INTO settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![setting.key(&plugin), v],
                )?,
                None => tx.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    [setting.key(&plugin)],
                )?,
            };
        }
        tx.commit()?;
        Ok(())
    }

    pub fn sources(&self) -> Result<Vec<SourceRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, plugin, version, state, added_at FROM sources ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, plugin, version, state, added_at) = row?;
            Ok(SourceRow {
                id: SourceId::from_sql(id)?,
                plugin,
                version,
                state: SourceState::from_sql(state)?,
                added_at,
            })
        })
        .collect()
    }

    /// Replaces the persisted fetch queue (called on shutdown and
    /// periodically, so a crash loses little). Keys of a disabled or
    /// purged source are left out: the writer would refuse their fetches.
    pub fn save_fetch_queue(&mut self, keys: &[QueuedKey]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM fetch_queue", [])?;
        // A disabled or purged source's keys aren't saved (SRC-14b): a
        // pool snapshot taken before its purge dropped them, or a key
        // still on its way to the pool then, mustn't outlive the purge.
        let stopped = stopped_mask(&tx)?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO fetch_queue
                     (info_hash, source, cohort, strategy, priority, attempts, flags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (info_hash) DO NOTHING",
            )?;
            for k in keys.iter().filter(|k| stopped & k.source.bit() as u64 == 0) {
                let flags = if k.private_source { QUEUE_PRIVATE } else { 0 };
                stmt.execute(params![
                    &k.info_hash.0[..],
                    k.source.0,
                    k.cohort,
                    k.strategy,
                    k.priority,
                    k.attempts,
                    flags
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_fetch_queue(&self) -> Result<Vec<QueuedKey>> {
        let mut stmt = self.conn.prepare(
            "SELECT info_hash, source, cohort, strategy, priority, attempts, flags
             FROM fetch_queue ORDER BY priority DESC, cohort",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, u8>(3)?,
                r.get::<_, u8>(4)?,
                r.get::<_, u16>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (hash, source, cohort, strategy, priority, attempts, flags) = row?;
            Ok(QueuedKey {
                info_hash: InfoHash(
                    hash.try_into()
                        .map_err(|_| Error::Corrupt("fetch_queue hash length"))?,
                ),
                source: SourceId::from_sql(source)?,
                cohort,
                strategy,
                priority,
                attempts,
                private_source: flags & QUEUE_PRIVATE != 0,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreConfig;

    #[test]
    fn source_settings_set_reset_and_purge() {
        let mut s = Store::open_in_memory(StoreConfig::default()).unwrap();
        let a = s
            .register_source_as("feed", "1", SourceState::Disabled, 0)
            .unwrap();
        let b = s
            .register_source_as("feed2", "1", SourceState::Disabled, 0)
            .unwrap();
        s.set_source_settings(
            a,
            &[
                (SourceSetting::Egress, Some("e")),
                (SourceSetting::Schedule, Some("s")),
                (SourceSetting::Agents, Some("2")),
                (SourceSetting::Config, Some("{}")),
            ],
        )
        .unwrap();
        s.set_source_settings(b, &[(SourceSetting::Egress, Some("other"))])
            .unwrap();
        assert_eq!(
            s.source_setting("feed", SourceSetting::Egress).unwrap(),
            Some("e".into())
        );
        // `feed2` doesn't share `feed`'s key, though one id prefixes the other.
        assert_eq!(
            s.source_setting("feed2", SourceSetting::Egress).unwrap(),
            Some("other".into())
        );
        s.set_source_settings(a, &[(SourceSetting::Agents, None)])
            .unwrap();
        assert_eq!(
            s.source_setting("feed", SourceSetting::Agents).unwrap(),
            None
        );
        // A purge drops them, and only the purged source's.
        s.set_source_state(a, SourceState::Purged).unwrap();
        for setting in SourceSetting::ALL {
            assert_eq!(s.source_setting("feed", setting).unwrap(), None);
        }
        assert!(
            s.source_setting("feed2", SourceSetting::Egress)
                .unwrap()
                .is_some()
        );
        let unknown = SourceId::new(9).unwrap();
        assert!(matches!(
            s.set_source_settings(unknown, &[(SourceSetting::Egress, Some("e"))]),
            Err(Error::NotFound(_))
        ));
    }
}
