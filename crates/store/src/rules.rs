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
