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

/// Where a source's consent record lives in `settings`: this, then the
/// plugin's manifest id. Its text is the sources crate's to encode.
const CONSENT_KEY: &str = "sources.consent.";
/// Where a source's sealed credentials live in `settings`: this, the
/// plugin's manifest id, `/`, then the credential's name. The value is the
/// sealed blob; the key file that opens it is outside the database.
const SECRET_KEY: &str = "sources.secret.";

fn secret_prefix(plugin: &str) -> String {
    format!("{SECRET_KEY}{plugin}/")
}

fn plugin_of(conn: &Connection, id: SourceId) -> Result<String> {
    conn.query_row("SELECT plugin FROM sources WHERE id = ?1", [id.0], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or(Error::NotFound("source"))
}

/// Removes a source's consent record and credentials (on purge).
fn drop_records(conn: &Connection, plugin: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        [format!("{CONSENT_KEY}{plugin}")],
    )?;
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

    /// Sets a source's state. Purging also drops its consent record and
    /// its credentials, in the same transaction.
    pub fn set_source_state(&mut self, id: SourceId, state: SourceState) -> Result<()> {
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

    /// Enables a source and, when given, stores its new consent record,
    /// in one transaction: the record and the state agree after a crash.
    pub fn enable_source(&mut self, id: SourceId, consent: Option<&str>) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        let plugin = plugin_of(&tx, id)?;
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
    /// periodically, so a crash loses little).
    pub fn save_fetch_queue(&mut self, keys: &[QueuedKey]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM fetch_queue", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO fetch_queue
                     (info_hash, source, cohort, strategy, priority, attempts, flags)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (info_hash) DO NOTHING",
            )?;
            for k in keys {
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
