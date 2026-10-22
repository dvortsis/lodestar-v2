//! Your rules, pack switches, the CSX-5 override list, exceptions and
//! *Always allow* (architecture §20.7), as plain rows. Their meaning
//! belongs to the filter engine; queries are kept as query text and
//! parsed by it.

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
        Ok(set)
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

    /// Replaces every rule-layer table with `set` (one-step Undo).
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
        let tx = self.conn.transaction()?;
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
        let tx = self.conn.transaction()?;
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
    pub fn mark_unenriched_after(&mut self, after: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE torrent_parsed SET parser_ver = 0 WHERE torrent_id > ?1",
            [after],
        )?)
    }

    /// The classification rules' generation (§20.9): bumped by every rule
    /// change, checked by every enrichment write.
    pub fn class_gen(&self) -> Result<i64> {
        class_gen(&self.conn)
    }

    /// Moves the classification rules to a new generation, so enrichment
    /// made under the old rules is no longer written. Returns it.
    pub fn bump_class_gen(&mut self) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let next = class_gen(&tx)? + 1;
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![CLASS_GEN, next],
        )?;
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
