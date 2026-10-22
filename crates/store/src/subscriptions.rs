//! Blocklist subscriptions (architecture §20.13): the lists, their fetch
//! state, and the parsed data. The meaning of `kind`, `format` and
//! `action` belongs to the filter engine.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSubscription<'a> {
    pub url: &'a str,
    pub name: &'a str,
    pub kind: u8,
    pub format: Option<&'a str>,
    pub action: u8,
    pub floor: bool,
    pub interval_secs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub kind: u8,
    pub format: Option<String>,
    pub action: u8,
    pub floor: bool,
    pub enabled: bool,
    pub interval_secs: i64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub checked_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub entries: i64,
    /// `new`, `ok`, `unchanged`, `held` (a version awaits a decision) or
    /// `error`.
    pub status: String,
    pub error: Option<String>,
}

/// What one refresh found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh<'a> {
    /// A new version, applied.
    Updated {
        data: &'a [u8],
        entries: i64,
        etag: Option<&'a str>,
        last_modified: Option<&'a str>,
    },
    /// A new version the sanity gates held back.
    Held {
        data: &'a [u8],
        entries: i64,
        etag: Option<&'a str>,
        last_modified: Option<&'a str>,
        why: &'a str,
    },
    /// The server said nothing changed.
    Unchanged,
    Failed(&'a str),
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Subscription> {
    Ok(Subscription {
        id: r.get(0)?,
        url: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        format: r.get(4)?,
        action: r.get(5)?,
        floor: r.get(6)?,
        enabled: r.get(7)?,
        interval_secs: r.get(8)?,
        etag: r.get(9)?,
        last_modified: r.get(10)?,
        checked_at: r.get(11)?,
        updated_at: r.get(12)?,
        entries: r.get(13)?,
        status: r.get(14)?,
        error: r.get(15)?,
    })
}

const COLUMNS: &str = "id, url, name, kind, format, action, floor, enabled, interval_secs, etag,
    last_modified, checked_at, updated_at, entries, status, error";

impl Store {
    /// Adds a subscription, or returns the id of the one with this URL.
    pub fn add_subscription(&mut self, s: &NewSubscription<'_>) -> Result<i64> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM subscriptions WHERE url = ?1",
                [s.url],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO subscriptions (url, name, kind, format, action, floor, interval_secs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                s.url,
                s.name,
                s.kind,
                s.format,
                s.action,
                s.floor,
                s.interval_secs
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {COLUMNS} FROM subscriptions ORDER BY id"))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn subscription(&self, id: i64) -> Result<Option<Subscription>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM subscriptions WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?)
    }

    /// Removes a subscription and its data. Returns whether it existed.
    pub fn remove_subscription(&mut self, id: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM subscription_data WHERE sub_id = ?1", [id])?;
        crate::floor::forget_list(&tx, id)?;
        let n = tx.execute("DELETE FROM subscriptions WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn set_subscription_enabled(&mut self, id: i64, enabled: bool) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE subscriptions SET enabled = ?2 WHERE id = ?1",
            params![id, enabled],
        )? > 0)
    }

    /// The applied (`pending = false`) or held-back version's data.
    pub fn subscription_data(&self, id: i64, pending: bool) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT data FROM subscription_data WHERE sub_id = ?1 AND pending = ?2",
                params![id, pending],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Records a refresh at `now`. For a list that's part of the floor,
    /// returns how many entries the new version dropped that now wait in
    /// its pending floor removals (still blocked, §2.7).
    pub fn record_refresh(&mut self, id: i64, now: i64, refresh: &Refresh<'_>) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut kept = 0;
        match *refresh {
            Refresh::Updated {
                data,
                entries,
                etag,
                last_modified,
            } => {
                let old = applied(&tx, id)?;
                kept = crate::floor::note_list_version(&tx, id, old.as_deref(), data)?;
                tx.execute("DELETE FROM subscription_data WHERE sub_id = ?1", [id])?;
                tx.execute(
                    "INSERT INTO subscription_data (sub_id, pending, entries, data)
                     VALUES (?1, 0, ?2, ?3)",
                    params![id, entries, data],
                )?;
                tx.execute(
                    "UPDATE subscriptions SET etag = ?2, last_modified = ?3, checked_at = ?4,
                         updated_at = ?4, entries = ?5, status = 'ok', error = NULL
                     WHERE id = ?1",
                    params![id, etag, last_modified, now, entries],
                )?;
            }
            Refresh::Held {
                data,
                entries,
                etag,
                last_modified,
                why,
            } => {
                tx.execute(
                    "INSERT OR REPLACE INTO subscription_data (sub_id, pending, entries, data)
                     VALUES (?1, 1, ?2, ?3)",
                    params![id, entries, data],
                )?;
                tx.execute(
                    "UPDATE subscriptions SET etag = ?2, last_modified = ?3, checked_at = ?4,
                         status = 'held', error = ?5
                     WHERE id = ?1",
                    params![id, etag, last_modified, now, why],
                )?;
            }
            Refresh::Unchanged => {
                tx.execute(
                    "UPDATE subscriptions SET checked_at = ?2,
                         status = CASE status WHEN 'held' THEN 'held' ELSE 'unchanged' END
                     WHERE id = ?1",
                    params![id, now],
                )?;
            }
            Refresh::Failed(why) => {
                tx.execute(
                    "UPDATE subscriptions SET checked_at = ?2, status = 'error', error = ?3
                     WHERE id = ?1",
                    params![id, now, why],
                )?;
            }
        }
        tx.commit()?;
        Ok(kept)
    }

    /// Applies (`accept`) or discards a held-back version. Returns whether
    /// there was one.
    pub fn resolve_held(&mut self, id: i64, accept: bool, now: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let held: Option<i64> = tx
            .query_row(
                "SELECT entries FROM subscription_data WHERE sub_id = ?1 AND pending = 1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(entries) = held else {
            return Ok(false);
        };
        if accept {
            let old = applied(&tx, id)?;
            let held: Vec<u8> = tx.query_row(
                "SELECT data FROM subscription_data WHERE sub_id = ?1 AND pending = 1",
                [id],
                |r| r.get(0),
            )?;
            crate::floor::note_list_version(&tx, id, old.as_deref(), &held)?;
            tx.execute(
                "DELETE FROM subscription_data WHERE sub_id = ?1 AND pending = 0",
                [id],
            )?;
            tx.execute(
                "UPDATE subscription_data SET pending = 0 WHERE sub_id = ?1",
                [id],
            )?;
            tx.execute(
                "UPDATE subscriptions SET entries = ?2, updated_at = ?3, status = 'ok',
                     error = NULL WHERE id = ?1",
                params![id, entries, now],
            )?;
        } else {
            tx.execute(
                "DELETE FROM subscription_data WHERE sub_id = ?1 AND pending = 1",
                [id],
            )?;
            tx.execute(
                "UPDATE subscriptions SET status = 'ok', error = NULL WHERE id = ?1",
                [id],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
}

fn applied(c: &rusqlite::Connection, id: i64) -> Result<Option<Vec<u8>>> {
    Ok(c.query_row(
        "SELECT data FROM subscription_data WHERE sub_id = ?1 AND pending = 0",
        [id],
        |r| r.get(0),
    )
    .optional()?)
}
