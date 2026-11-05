//! Your rules, pack switches, the CSX-5 override list, exceptions and
//! *Always allow* (architecture §20.7), as plain rows. Their meaning
//! belongs to the filter engine; queries are kept as query text and
//! parsed by it.
//!
//! The pack versions this install applied (RP-0) are kept beside them in
//! `settings` rows ([`PackVersionRows`]): read with the rule set, so every
//! engine load sees them, but never written by [`Store::replace_rule_set`],
//! so Undo never changes them.
//!
//! Pack-rule overrides (RP-2) are `settings` rows too, one per overridden
//! rule ([`PACK_OVERRIDE_PREFIX`]), and part of the rule set: Undo
//! snapshots and restores them with everything else.

use rusqlite::types::ValueRef;
use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleRow {
    pub id: i64,
    pub query: String,
    pub action: u8,
    pub enabled: bool,
    pub shadow_until: Option<i64>,
    pub unless_query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRow {
    pub id: i64,
    /// 0 exact torrents, 1 keyword, 2 query.
    pub kind: u8,
    pub value: Vec<u8>,
    /// 0 override, 1 neutralize.
    pub strength: u8,
    pub scope: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub shadow_until: Option<i64>,
    pub hits: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionRow {
    pub id: i64,
    pub rule: String,
    /// 0 exact torrents, 2 query, 3 payload signature.
    pub kind: u8,
    pub value: Vec<u8>,
}

/// The two identical `settings` rows recording, per pack rule, the
/// version this install applied (`lodestar_filter`'s `packs::upgrade`).
pub const PACKS_APPLIED_KEYS: [&str; 2] = ["filter.packs.applied", "filter.packs.applied.2"];
/// The durable marker that pack versions were seeded.
pub const PACKS_SEEDED_KEY: &str = "filter.packs.seeded";

/// The `settings` key prefix of a pack-rule override: one row per rule,
/// `filter.pack_override:MW-1`. The value's meaning is the filter
/// engine's (`packs::overrides`).
pub const PACK_OVERRIDE_PREFIX: &str = "filter.pack_override:";

/// One pack-rule override row: `(pack rule id, value as stored)`, the
/// value's text, or its bytes when it doesn't read as text.
pub type PackOverrideRow = (String, std::result::Result<String, Vec<u8>>);

/// A `settings` row as stored: `None` when there's no row, `Ok` with its
/// text, or `Err` with the raw bytes of a value that doesn't read as text
/// (a blob, a number, bytes that aren't UTF-8). A row that exists counts
/// as state, readable or not. Plain types, so the filter engine reads it
/// without naming this module's types.
pub type StoredText = Option<std::result::Result<String, Vec<u8>>>;

/// The pack-version rows as stored. Their meaning is the filter engine's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackVersionRows {
    /// [`PACKS_APPLIED_KEYS`], in that order.
    pub applied: [StoredText; 2],
    /// [`PACKS_SEEDED_KEY`].
    pub seeded: StoredText,
}

impl PackVersionRows {
    /// Whether any of the rows exists, readable or not.
    pub fn any(&self) -> bool {
        self.applied.iter().any(Option::is_some) || self.seeded.is_some()
    }
}

/// What [`Store::seed_pack_versions`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PackSeed {
    /// The rule set had rows before seeding (an upgraded install, or one
    /// whose pack-version rows were all lost): reported, never silent.
    pub rule_set_nonempty: bool,
    /// The copied rule's switch was copied to the new one.
    pub switch_copied: bool,
    /// Exceptions copied from the copied rule to the new one.
    pub exceptions_copied: usize,
}

/// Everything the rule layers load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSet {
    pub rules: Vec<RuleRow>,
    /// `(pack name or pack rule id, enabled)`.
    pub pack_switches: Vec<(String, bool)>,
    pub overrides: Vec<OverrideRow>,
    pub exceptions: Vec<ExceptionRow>,
    /// `(id, query)`.
    pub allow: Vec<(i64, String)>,
    /// The pack-version rows. Not part of what Undo restores.
    pub pack_versions: PackVersionRows,
    /// Pack-rule overrides: `(pack rule id, value as stored)`, by id. A
    /// value that doesn't read as text is kept as its bytes, so it stays
    /// unreadable (and fails closed) across an Undo.
    pub pack_overrides: Vec<PackOverrideRow>,
}

/// A `settings` row's value ([`StoredText`]).
fn stored_text(conn: &rusqlite::Connection, key: &str) -> Result<StoredText> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            Ok(text_or_bytes(r.get_ref(0)?))
        })
        .optional()?)
}

/// A stored value as [`StoredText`] reads it.
pub(crate) fn text_or_bytes(v: ValueRef<'_>) -> std::result::Result<String, Vec<u8>> {
    match v {
        ValueRef::Text(b) => {
            std::str::from_utf8(b).map_or_else(|_| Err(b.to_vec()), |t| Ok(t.to_owned()))
        }
        ValueRef::Blob(b) => Err(b.to_vec()),
        ValueRef::Null => Err(Vec::new()),
        ValueRef::Integer(n) => Err(n.to_string().into_bytes()),
        ValueRef::Real(x) => Err(x.to_string().into_bytes()),
    }
}

/// Every pack-rule override row, by key.
fn pack_override_rows(conn: &rusqlite::Connection) -> Result<Vec<PackOverrideRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT key, value FROM settings WHERE substr(key, 1, ?1) = ?2 ORDER BY key",
    )?;
    let rows = stmt.query_map(
        params![PACK_OVERRIDE_PREFIX.len() as i64, PACK_OVERRIDE_PREFIX],
        |r| {
            let key: String = r.get(0)?;
            Ok((
                key[PACK_OVERRIDE_PREFIX.len()..].to_owned(),
                text_or_bytes(r.get_ref(1)?),
            ))
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Writes one pack-rule override row as stored: text as text, bytes as a
/// blob.
fn put_pack_override(
    conn: &rusqlite::Connection,
    rule: &str,
    value: &std::result::Result<String, Vec<u8>>,
) -> Result<()> {
    let key = format!("{PACK_OVERRIDE_PREFIX}{rule}");
    match value {
        Ok(t) => upsert_setting(conn, &key, t),
        Err(b) => {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, b],
            )?;
            Ok(())
        }
    }
}

fn pack_version_rows(conn: &rusqlite::Connection) -> Result<PackVersionRows> {
    Ok(PackVersionRows {
        applied: [
            stored_text(conn, PACKS_APPLIED_KEYS[0])?,
            stored_text(conn, PACKS_APPLIED_KEYS[1])?,
        ],
        seeded: stored_text(conn, PACKS_SEEDED_KEY)?,
    })
}

fn upsert_setting(conn: &rusqlite::Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

impl Store {
    pub fn rule_set(&self) -> Result<RuleSet> {
        let c = &self.conn;
        let mut set = RuleSet::default();
        let mut stmt = c.prepare(
            "SELECT id, query, action, enabled, shadow_until, unless_query FROM filter_rules
             ORDER BY position, id",
        )?;
        for r in stmt.query_map([], |r| {
            Ok(RuleRow {
                id: r.get(0)?,
                query: r.get(1)?,
                action: r.get(2)?,
                enabled: r.get(3)?,
                shadow_until: r.get(4)?,
                unless_query: r.get(5)?,
            })
        })? {
            set.rules.push(r?);
        }
        let mut stmt = c.prepare("SELECT id, enabled FROM pack_switches ORDER BY id")?;
        for r in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            set.pack_switches.push(r?);
        }
        let mut stmt = c.prepare(
            "SELECT id, kind, value, strength, scope, created_at, expires_at, shadow_until, hits
             FROM override_list ORDER BY id",
        )?;
        for r in stmt.query_map([], |r| {
            Ok(OverrideRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                value: r.get(2)?,
                strength: r.get(3)?,
                scope: r.get(4)?,
                created_at: r.get(5)?,
                expires_at: r.get(6)?,
                shadow_until: r.get(7)?,
                hits: r.get(8)?,
            })
        })? {
            set.overrides.push(r?);
        }
        let mut stmt =
            c.prepare("SELECT id, rule, kind, value FROM filter_exceptions ORDER BY id")?;
        for r in stmt.query_map([], |r| {
            Ok(ExceptionRow {
                id: r.get(0)?,
                rule: r.get(1)?,
                kind: r.get(2)?,
                value: r.get(3)?,
            })
        })? {
            set.exceptions.push(r?);
        }
        let mut stmt = c.prepare("SELECT id, query FROM filter_allow ORDER BY id")?;
        for r in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            set.allow.push(r?);
        }
        set.pack_versions = pack_version_rows(c)?;
        set.pack_overrides = pack_override_rows(c)?;
        Ok(set)
    }

    /// Writes (`Some`) or removes (`None`) one pack rule's override row.
    /// The value is the filter engine's encoding; it holds no author, time
    /// or reason.
    pub fn set_pack_override(&mut self, rule: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(v) => put_pack_override(&self.conn, rule, &Ok(v.to_owned())),
            None => {
                self.conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    [format!("{PACK_OVERRIDE_PREFIX}{rule}")],
                )?;
                Ok(())
            }
        }
    }

    /// The pack-version rows alone.
    pub fn pack_version_rows(&self) -> Result<PackVersionRows> {
        pack_version_rows(&self.conn)
    }

    /// Seeds the pack versions once, in one transaction, only when none
    /// of the pack-version rows exists (readable or not): both applied
    /// rows as `applied`, the marker, a copy of `copy_from`'s stored
    /// switch as `copy_to`'s (when `copy_from` has one and `copy_to`
    /// doesn't), and a copy of every exception to `copy_from` for
    /// `copy_to` (one with the same match already there isn't copied
    /// again). The marker's value is `1`, or `marker_with_rules` when the
    /// rule set already had rows, so such a seeding is never silent.
    /// Returns `None` when any row exists: nothing is written.
    pub fn seed_pack_versions(
        &mut self,
        applied: &str,
        copy_from: &str,
        copy_to: &str,
        marker_with_rules: &str,
    ) -> Result<Option<PackSeed>> {
        let tx = crate::begin_write(&mut self.conn)?;
        if pack_version_rows(&tx)?.any() {
            return Ok(None);
        }
        let mut seed = PackSeed::default();
        for table in [
            "filter_rules",
            "pack_switches",
            "override_list",
            "filter_exceptions",
            "filter_allow",
        ] {
            let any: bool =
                tx.query_row(&format!("SELECT EXISTS (SELECT 1 FROM {table})"), [], |r| {
                    r.get(0)
                })?;
            seed.rule_set_nonempty |= any;
        }
        for key in PACKS_APPLIED_KEYS {
            upsert_setting(&tx, key, applied)?;
        }
        let from: Option<bool> = tx
            .query_row(
                "SELECT enabled FROM pack_switches WHERE id = ?1",
                [copy_from],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(on) = from {
            seed.switch_copied = tx.execute(
                "INSERT OR IGNORE INTO pack_switches (id, enabled) VALUES (?1, ?2)",
                params![copy_to, on],
            )? > 0;
        }
        seed.exceptions_copied = tx.execute(
            "INSERT INTO filter_exceptions (rule, kind, value)
             SELECT ?2, e.kind, e.value FROM filter_exceptions e
             WHERE e.rule = ?1 AND NOT EXISTS (
                 SELECT 1 FROM filter_exceptions f
                 WHERE f.rule = ?2 AND f.kind = e.kind AND f.value = e.value)
             ORDER BY e.id",
            params![copy_from, copy_to],
        )?;
        let marker = if seed.rule_set_nonempty {
            marker_with_rules
        } else {
            "1"
        };
        upsert_setting(&tx, PACKS_SEEDED_KEY, marker)?;
        tx.commit()?;
        Ok(Some(seed))
    }

    /// Writes the pack-version rows in one transaction: each applied row
    /// that's `Some`, the marker when it's missing (one there is kept), and with `switch` a pack switch too, so
    /// a pack turned on and the versions it records land together.
    pub fn write_pack_versions(
        &mut self,
        applied: [Option<&str>; 2],
        switch: Option<(&str, bool)>,
    ) -> Result<()> {
        let tx = crate::begin_write(&mut self.conn)?;
        for (key, text) in PACKS_APPLIED_KEYS.iter().zip(applied) {
            if let Some(text) = text {
                upsert_setting(&tx, key, text)?;
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, '1')",
            [PACKS_SEEDED_KEY],
        )?;
        if let Some((id, on)) = switch {
            tx.execute(
                "INSERT OR REPLACE INTO pack_switches (id, enabled) VALUES (?1, ?2)",
                params![id, on],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Applies this build's pack defaults once, in one transaction (RP-DEF,
    /// `docs/decisions/2026-11-05-RP-DEF.md`); the meaning is the filter
    /// engine's (`packs::upgrade::ensure_seeded`).
    /// - **`on`, a fresh store:** only while [`crate::PACKS_FRESH_KEY`] is
    ///   there. Each of `packs` without a switch row gets an on row, `applied`
    ///   (when given) is written to both applied rows with the seeded marker,
    ///   and the fresh marker is consumed.
    /// - **Not `on`, an existing store:** only while neither marker is
    ///   there. Each of `packs` without a switch row gets an explicit off
    ///   row, so nothing flips.
    ///
    /// Either way [`crate::PACKS_DEFAULTS_KEY`] is set. A row already there
    /// is never changed: it's a choice. Returns whether it wrote anything.
    pub fn apply_pack_defaults(
        &mut self,
        on: bool,
        packs: &[&str],
        applied: Option<[&str; 2]>,
    ) -> Result<bool> {
        let tx = crate::begin_write(&mut self.conn)?;
        let has = |key: &str| -> Result<bool> {
            Ok(tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM settings WHERE key = ?1)",
                [key],
                |r| r.get(0),
            )?)
        };
        let fresh = has(crate::PACKS_FRESH_KEY)?;
        let ready = if on {
            fresh
        } else {
            !fresh && !has(crate::PACKS_DEFAULTS_KEY)?
        };
        if !ready {
            return Ok(false);
        }
        for id in packs {
            tx.execute(
                "INSERT OR IGNORE INTO pack_switches (id, enabled) VALUES (?1, ?2)",
                params![id, on],
            )?;
        }
        if let Some(applied) = applied {
            for (key, text) in PACKS_APPLIED_KEYS.iter().zip(applied) {
                upsert_setting(&tx, key, text)?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO settings (key, value) VALUES (?1, '1')",
                [PACKS_SEEDED_KEY],
            )?;
        }
        upsert_setting(&tx, crate::PACKS_DEFAULTS_KEY, "1")?;
        tx.execute(
            "DELETE FROM settings WHERE key = ?1",
            [crate::PACKS_FRESH_KEY],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Adds a rule at the end of the list.
    pub fn add_rule(
        &mut self,
        query: &str,
        action: u8,
        shadow_until: Option<i64>,
        unless_query: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO filter_rules (position, query, action, shadow_until, unless_query)
             VALUES ((SELECT coalesce(max(position), 0) + 1 FROM filter_rules), ?1, ?2, ?3, ?4)",
            params![query, action, shadow_until, unless_query],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_rule(&mut self, r: &RuleRow) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE filter_rules SET query = ?2, action = ?3, enabled = ?4, shadow_until = ?5,
                 unless_query = ?6 WHERE id = ?1",
            params![
                r.id,
                r.query,
                r.action,
                r.enabled,
                r.shadow_until,
                r.unless_query
            ],
        )? > 0)
    }

    pub fn remove_rule(&mut self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM filter_rules WHERE id = ?1", [id])?
            > 0)
    }

    pub fn set_pack_switch(&mut self, id: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO pack_switches (id, enabled) VALUES (?1, ?2)",
            params![id, enabled],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_override(
        &mut self,
        kind: u8,
        value: &[u8],
        strength: u8,
        scope: Option<&str>,
        created_at: i64,
        expires_at: Option<i64>,
        shadow_until: Option<i64>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO override_list
                 (kind, value, strength, scope, created_at, expires_at, shadow_until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                kind,
                value,
                strength,
                scope,
                created_at,
                expires_at,
                shadow_until
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn remove_override(&mut self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM override_list WHERE id = ?1", [id])?
            > 0)
    }

    pub fn add_exception(&mut self, rule: &str, kind: u8, value: &[u8]) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO filter_exceptions (rule, kind, value) VALUES (?1, ?2, ?3)",
            params![rule, kind, value],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn remove_exception(&mut self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM filter_exceptions WHERE id = ?1", [id])?
            > 0)
    }

    pub fn add_allow(&mut self, query: &str) -> Result<i64> {
        self.conn
            .execute("INSERT INTO filter_allow (query) VALUES (?1)", [query])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn remove_allow(&mut self, id: i64) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM filter_allow WHERE id = ?1", [id])?
            > 0)
    }

    pub fn add_fixture(&mut self, rule: &str, must_match: bool, tokens: &[u8]) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO filter_fixtures (rule, must_match, tokens) VALUES (?1, ?2, ?3)",
            params![rule, must_match, tokens],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `(id, must_match, tokens)` for one rule.
    pub fn fixtures(&self, rule: &str) -> Result<Vec<(i64, bool, Vec<u8>)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, must_match, tokens FROM filter_fixtures WHERE rule = ?1")?;
        let rows = stmt.query_map([rule], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces every rule-layer table, and the pack-rule override rows,
    /// with `set` (one-step Undo). The
    /// pack-version rows are left as they are: Undo restores rule-set
    /// edits, never versions, so it can't undo an acceptance or accept a
    /// pending version.
    pub fn replace_rule_set(&mut self, set: &RuleSet) -> Result<()> {
        let tx = self.conn.transaction()?;
        for table in [
            "filter_rules",
            "pack_switches",
            "override_list",
            "filter_exceptions",
            "filter_allow",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        tx.execute(
            "DELETE FROM settings WHERE substr(key, 1, ?1) = ?2",
            params![PACK_OVERRIDE_PREFIX.len() as i64, PACK_OVERRIDE_PREFIX],
        )?;
        for (rule, value) in &set.pack_overrides {
            put_pack_override(&tx, rule, value)?;
        }
        for (i, r) in set.rules.iter().enumerate() {
            tx.execute(
                "INSERT INTO filter_rules
                     (id, position, query, action, enabled, shadow_until, unless_query)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    r.id,
                    i as i64 + 1,
                    r.query,
                    r.action,
                    r.enabled,
                    r.shadow_until,
                    r.unless_query
                ],
            )?;
        }
        for (id, on) in &set.pack_switches {
            tx.execute(
                "INSERT INTO pack_switches (id, enabled) VALUES (?1, ?2)",
                params![id, on],
            )?;
        }
        for o in &set.overrides {
            tx.execute(
                "INSERT INTO override_list (id, kind, value, strength, scope, created_at,
                     expires_at, shadow_until, hits)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    o.id,
                    o.kind,
                    o.value,
                    o.strength,
                    o.scope,
                    o.created_at,
                    o.expires_at,
                    o.shadow_until,
                    o.hits
                ],
            )?;
        }
        for e in &set.exceptions {
            tx.execute(
                "INSERT INTO filter_exceptions (id, rule, kind, value) VALUES (?1, ?2, ?3, ?4)",
                params![e.id, e.rule, e.kind, e.value],
            )?;
        }
        for (id, q) in &set.allow {
            tx.execute(
                "INSERT INTO filter_allow (id, query) VALUES (?1, ?2)",
                params![id, q],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Keeps a snapshot of the rule set for one-step Undo, replacing the
    /// last one.
    pub fn save_previous(&mut self, data: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO rule_set_prev (id, data) VALUES (1, ?1)",
            [data],
        )?;
        Ok(())
    }

    pub fn previous(&self) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT data FROM rule_set_prev WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?)
    }
}

/// Your correction to one torrent's classification (§20.9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassCorrection {
    /// `Some(true)`: Is adult. `Some(false)`: Not adult.
    pub adult: Option<bool>,
    pub not_junk: bool,
}

impl Store {
    /// Sets or clears (`ClassCorrection::default()`) a torrent's
    /// correction. Returns false when the torrent isn't stored. The caller
    /// re-enriches the torrent, which logs it for re-indexing.
    pub fn set_correction(&mut self, id: i64, c: ClassCorrection) -> Result<bool> {
        let tx = crate::begin_write(&mut self.conn)?;
        let exists = tx
            .query_row("SELECT 1 FROM torrents WHERE id = ?1", [id], |_| Ok(()))
            .optional()?
            .is_some();
        if exists {
            if c == ClassCorrection::default() {
                tx.execute("DELETE FROM class_corrections WHERE torrent_id = ?1", [id])?;
            } else {
                tx.execute(
                    "INSERT INTO class_corrections (torrent_id, adult, not_junk)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT (torrent_id) DO UPDATE
                     SET adult = excluded.adult, not_junk = excluded.not_junk",
                    params![id, c.adult, c.not_junk],
                )?;
            }
            // Enrichment classified under the old correction is no longer
            // written over it (§20.9).
            bump_class_gen(&tx)?;
        }
        tx.commit()?;
        Ok(exists)
    }

    pub fn correction(&self, id: i64) -> Result<Option<ClassCorrection>> {
        Ok(self
            .conn
            .prepare_cached("SELECT adult, not_junk FROM class_corrections WHERE torrent_id = ?1")?
            .query_row([id], |r| {
                Ok(ClassCorrection {
                    adult: r.get(0)?,
                    not_junk: r.get(1)?,
                })
            })
            .optional()?)
    }

    /// Your corrections, by torrent id, at most `limit`.
    pub fn corrections(&self, limit: usize) -> Result<Vec<(i64, ClassCorrection)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, adult, not_junk FROM class_corrections
             ORDER BY torrent_id LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok((
                r.get(0)?,
                ClassCorrection {
                    adult: r.get(1)?,
                    not_junk: r.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Adds a classification rule: `(query, adult)`.
    pub fn add_class_rule(&mut self, query: &str, adult: bool) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO class_rules (query, adult) VALUES (?1, ?2)",
            params![query, adult],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Removes a classification rule, returning its query.
    pub fn remove_class_rule(&mut self, id: i64) -> Result<Option<String>> {
        let tx = crate::begin_write(&mut self.conn)?;
        let query: Option<String> = tx
            .query_row("SELECT query FROM class_rules WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        tx.execute("DELETE FROM class_rules WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(query)
    }

    /// Classification rules, oldest first: `(id, query, adult)`.
    pub fn class_rules(&self) -> Result<Vec<(i64, String, bool)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, query, adult FROM class_rules ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Marks stored torrents' enrichment as outdated, so background
    /// enrichment classifies them again. Returns how many were marked.
    ///
    /// Written in transactions of [`MARK_CHUNK`] rows, so the ingest
    /// writer never waits long for the database.
    pub fn mark_unenriched(&mut self, ids: &[i64]) -> Result<usize> {
        let mut n = 0;
        for chunk in ids.chunks(MARK_CHUNK) {
            let tx = self.conn.transaction()?;
            {
                let mut stmt = tx.prepare_cached(
                    "UPDATE torrent_parsed SET parser_ver = 0 WHERE torrent_id = ?1",
                )?;
                for id in chunk {
                    n += stmt.execute([id])?;
                }
            }
            tx.commit()?;
        }
        Ok(n)
    }

    /// Marks every stored torrent after id `after` for background
    /// enrichment: those the search index hasn't reached yet.
    ///
    /// In transactions of [`MARK_CHUNK`] ids, since an index that's new or
    /// rebuilding can be far behind.
    pub fn mark_unenriched_after(&mut self, after: i64) -> Result<usize> {
        let last: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(torrent_id), 0) FROM torrent_parsed",
            [],
            |r| r.get(0),
        )?;
        let mut n = 0;
        let mut lo = after;
        while lo < last {
            let hi = lo.saturating_add(MARK_CHUNK as i64);
            n += self.conn.execute(
                "UPDATE torrent_parsed SET parser_ver = 0
                 WHERE torrent_id > ?1 AND torrent_id <= ?2",
                params![lo, hi],
            )?;
            lo = hi;
        }
        Ok(n)
    }

    /// The classification rules' generation (§20.9): bumped by every rule
    /// change, checked by every enrichment write.
    pub fn class_gen(&self) -> Result<i64> {
        class_gen(&self.conn)
    }

    /// Moves the classification rules to a new generation, so enrichment
    /// made under the old rules is no longer written. Returns it.
    pub fn bump_class_gen(&mut self) -> Result<i64> {
        let tx = crate::begin_write(&mut self.conn)?;
        let next = bump_class_gen(&tx)?;
        tx.commit()?;
        Ok(next)
    }

    /// Your corrections after torrent id `after`, at most `limit`.
    pub fn corrections_after(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, ClassCorrection)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, adult, not_junk FROM class_corrections
             WHERE torrent_id > ?1 ORDER BY torrent_id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after, limit as i64], |r| {
            Ok((
                r.get(0)?,
                ClassCorrection {
                    adult: r.get(1)?,
                    not_junk: r.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Rows marked per transaction by [`Store::mark_unenriched`].
pub const MARK_CHUNK: usize = 10_000;

const CLASS_GEN: &str = "class_rules_gen";

fn bump_class_gen(conn: &rusqlite::Connection) -> Result<i64> {
    let next = class_gen(conn)? + 1;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![CLASS_GEN, next],
    )?;
    Ok(next)
}

pub(crate) fn class_gen(conn: &rusqlite::Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [CLASS_GEN],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoreConfig;

    fn store() -> Store {
        Store::open_in_memory(StoreConfig::default()).unwrap()
    }

    #[test]
    fn pack_version_rows_read_text_unreadable_and_absent() {
        let s = store();
        assert_eq!(s.pack_version_rows().unwrap(), PackVersionRows::default());
        assert!(!s.pack_version_rows().unwrap().any());
        s.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, 'MW-1@1'), (?2, x'ff'), (?3, 7)",
                params![
                    PACKS_APPLIED_KEYS[0],
                    PACKS_APPLIED_KEYS[1],
                    PACKS_SEEDED_KEY
                ],
            )
            .unwrap();
        let rows = s.rule_set().unwrap().pack_versions;
        assert_eq!(rows.applied[0], Some(Ok("MW-1@1".to_owned())));
        assert_eq!(rows.applied[1], Some(Err(vec![0xff])));
        assert_eq!(rows.seeded, Some(Err(b"7".to_vec())));
        assert!(rows.any());
    }

    fn has(s: &Store, key: &str) -> bool {
        s.setting_text(key).unwrap().is_some()
    }

    #[test]
    fn pack_defaults_on_a_fresh_store_switch_on_and_consume_the_marker() {
        let mut s = store();
        assert!(has(&s, crate::PACKS_FRESH_KEY), "a new store is fresh");
        s.set_pack_switch("lures", false).unwrap();
        // The existing-store step refuses while the store is fresh.
        assert!(!s.apply_pack_defaults(false, &["malware"], None).unwrap());
        assert!(
            s.apply_pack_defaults(true, &["malware", "lures"], Some(["MW-1@2", "MW-1@2"]))
                .unwrap()
        );
        let set = s.rule_set().unwrap();
        assert_eq!(
            set.pack_switches,
            [("lures".to_owned(), false), ("malware".to_owned(), true)],
            "a row already there is a choice, and kept"
        );
        assert_eq!(set.pack_versions.applied[0], Some(Ok("MW-1@2".into())));
        assert_eq!(set.pack_versions.applied[1], Some(Ok("MW-1@2".into())));
        assert_eq!(set.pack_versions.seeded, Some(Ok("1".into())));
        assert!(!has(&s, crate::PACKS_FRESH_KEY));
        assert_eq!(
            s.setting_text(crate::PACKS_DEFAULTS_KEY)
                .unwrap()
                .as_deref(),
            Some("1")
        );
        // Once only, either way.
        assert!(!s.apply_pack_defaults(true, &["spam"], None).unwrap());
        assert!(!s.apply_pack_defaults(false, &["spam"], None).unwrap());
        assert_eq!(s.rule_set().unwrap().pack_switches.len(), 2);
    }

    #[test]
    fn pack_defaults_on_an_existing_store_write_off_rows() {
        let mut s = store();
        s.update_settings_text(&[], &[crate::PACKS_FRESH_KEY])
            .unwrap();
        s.set_pack_switch("malware", true).unwrap();
        assert!(
            !s.apply_pack_defaults(true, &["spam"], None).unwrap(),
            "not fresh"
        );
        assert!(
            s.apply_pack_defaults(false, &["malware", "spam"], None)
                .unwrap()
        );
        let set = s.rule_set().unwrap();
        assert_eq!(
            set.pack_switches,
            [("malware".to_owned(), true), ("spam".to_owned(), false)]
        );
        assert!(!set.pack_versions.any(), "no version rows written");
        assert!(has(&s, crate::PACKS_DEFAULTS_KEY));
        assert!(!s.apply_pack_defaults(false, &["fakes"], None).unwrap());
    }

    #[test]
    fn seeding_is_one_transaction_and_only_on_a_store_without_rows() {
        let mut s = store();
        s.set_pack_switch("SC-1", false).unwrap();
        s.add_exception("SC-1", 2, b"q").unwrap();
        s.add_exception("MW-1", 2, b"r").unwrap();
        let seed = s
            .seed_pack_versions("MW-1@1", "SC-1", "SC-2", "rules@9")
            .unwrap()
            .unwrap();
        assert_eq!(
            s.setting_text(PACKS_SEEDED_KEY).unwrap().as_deref(),
            Some("rules@9")
        );
        assert_eq!(
            seed,
            PackSeed {
                rule_set_nonempty: true,
                switch_copied: true,
                exceptions_copied: 1,
            }
        );
        let set = s.rule_set().unwrap();
        assert_eq!(set.pack_versions.applied[0], Some(Ok("MW-1@1".into())));
        assert_eq!(set.pack_versions.applied[0], set.pack_versions.applied[1]);
        assert_eq!(set.pack_versions.seeded, Some(Ok("rules@9".into())));
        assert!(set.pack_switches.contains(&("SC-2".into(), false)));
        assert_eq!(
            set.exceptions.iter().filter(|e| e.rule == "SC-2").count(),
            1
        );
        // Never again.
        assert_eq!(
            s.seed_pack_versions("MW-1@2", "SC-1", "SC-2", "rules@1")
                .unwrap(),
            None
        );
        assert_eq!(s.rule_set().unwrap().exceptions.len(), 3);
        // An empty store seeds quietly.
        let mut s = store();
        let seed = s
            .seed_pack_versions("", "SC-1", "SC-2", "rules@9")
            .unwrap()
            .unwrap();
        assert_eq!(seed, PackSeed::default());
        assert_eq!(
            s.setting_text(PACKS_SEEDED_KEY).unwrap().as_deref(),
            Some("1"),
            "quiet"
        );
    }

    #[test]
    fn pack_overrides_read_write_and_undo_with_the_rule_set() {
        let mut s = store();
        assert!(s.rule_set().unwrap().pack_overrides.is_empty());
        s.set_pack_override("MW-1", Some("1\n1\ndemote")).unwrap();
        s.set_pack_override("SP-1", Some("1\n1\nhide")).unwrap();
        // A key that only looks like the prefix through LIKE wildcards.
        s.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES ('filter.pack-override:X', 'no'),
                 (?1, x'ff')",
                [format!("{PACK_OVERRIDE_PREFIX}LU-1")],
            )
            .unwrap();
        let set = s.rule_set().unwrap();
        assert_eq!(
            set.pack_overrides,
            [
                ("LU-1".to_owned(), Err(vec![0xff])),
                ("MW-1".to_owned(), Ok("1\n1\ndemote".to_owned())),
                ("SP-1".to_owned(), Ok("1\n1\nhide".to_owned())),
            ]
        );
        s.set_pack_override("SP-1", None).unwrap();
        s.set_pack_override("SP-1", None).unwrap();
        assert_eq!(s.rule_set().unwrap().pack_overrides.len(), 2);
        // Undo: replaced with the snapshot's, an unreadable one kept as bytes.
        s.replace_rule_set(&set).unwrap();
        assert_eq!(s.rule_set().unwrap().pack_overrides, set.pack_overrides);
        s.replace_rule_set(&RuleSet::default()).unwrap();
        assert!(s.rule_set().unwrap().pack_overrides.is_empty());
        assert_eq!(
            s.setting_text("filter.pack-override:X").unwrap().as_deref(),
            Some("no"),
            "other settings are left"
        );
    }

    #[test]
    fn version_writes_carry_the_marker_and_the_switch_and_undo_leaves_them() {
        let mut s = store();
        s.write_pack_versions([Some("A@1"), None], Some(("malware", true)))
            .unwrap();
        let set = s.rule_set().unwrap();
        assert_eq!(set.pack_versions.applied, [Some(Ok("A@1".into())), None]);
        assert!(set.pack_versions.seeded.is_some());
        assert_eq!(set.pack_switches, [("malware".to_owned(), true)]);
        s.replace_rule_set(&RuleSet::default()).unwrap();
        let after = s.rule_set().unwrap();
        assert!(after.pack_switches.is_empty());
        assert_eq!(
            after.pack_versions, set.pack_versions,
            "Undo never restores versions"
        );
    }
}
