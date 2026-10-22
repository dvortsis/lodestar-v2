//! Typed access to the `settings` table for the runtime settings
//! registry (architecture §11): values are JSON text under keys the
//! registry owns, and a change to several keys is one transaction.
//!
//! The table also holds other subsystems' state (the active file
//! dictionary, the floor manifests, the first-run time); these helpers
//! only read and write the keys they're given, so a caller that keeps to
//! its own prefix can't reach anyone else's.

use rusqlite::{OptionalExtension, params};

use crate::{Error, Result, Store};

impl Store {
    /// The text stored under `key`, if any. A non-text value is corrupt
    /// for a text key.
    pub fn setting_text(&self, key: &str) -> Result<Option<String>> {
        let value: Option<rusqlite::types::Value> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        match value {
            None => Ok(None),
            Some(rusqlite::types::Value::Text(s)) => Ok(Some(s)),
            Some(_) => Err(Error::Corrupt("setting is not text")),
        }
    }

    /// Every text setting whose key starts with `prefix`, sorted by key.
    /// The prefix is matched literally (no `LIKE` wildcards).
    pub fn settings_with_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT key, value FROM settings WHERE substr(key, 1, ?2) = ?1 ORDER BY key",
        )?;
        let rows = stmt.query_map(params![prefix, prefix.chars().count() as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, rusqlite::types::Value>(1)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            match row? {
                (key, rusqlite::types::Value::Text(value)) => out.push((key, value)),
                _ => return Err(Error::Corrupt("setting is not text")),
            }
        }
        Ok(out)
    }

    /// Stores every `(key, text)` pair in one transaction: all of them
    /// are written, or none is.
    pub fn set_settings_text(&mut self, entries: &[(&str, &str)]) -> Result<()> {
        self.update_settings_text(entries, &[])
    }

    /// Stores every `(key, text)` pair in `set` and deletes every key in
    /// `remove`, in one transaction: all of it happens, or none does.
    pub fn update_settings_text(&mut self, set: &[(&str, &str)], remove: &[&str]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )?;
            for (key, value) in set {
                upsert.execute(params![key, value])?;
            }
            let mut delete = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
            for key in remove {
                delete.execute([key])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{Store, StoreConfig};

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("t.db"), StoreConfig::default()).unwrap();
        (dir, store)
    }

    #[test]
    fn text_round_trips_and_overwrites() {
        let (_dir, mut s) = store();
        assert_eq!(s.setting_text("config.a.x").unwrap(), None);
        s.set_settings_text(&[("config.a.x", "1"), ("config.a.y", "\"two\"")])
            .unwrap();
        s.set_settings_text(&[("config.a.x", "3")]).unwrap();
        assert_eq!(s.setting_text("config.a.x").unwrap().as_deref(), Some("3"));
        assert_eq!(
            s.settings_with_prefix("config.a.").unwrap(),
            vec![
                ("config.a.x".to_string(), "3".to_string()),
                ("config.a.y".to_string(), "\"two\"".to_string()),
            ]
        );
    }

    #[test]
    fn prefixes_are_literal_and_skip_other_keys() {
        let (_dir, mut s) = store();
        s.set_setting_i64("first_run_at", 5).unwrap();
        s.set_settings_text(&[("config_x", "1"), ("configXa", "2"), ("config.b", "3")])
            .unwrap();
        // `_` and `%` would be wildcards to LIKE.
        assert_eq!(
            s.settings_with_prefix("config.").unwrap(),
            vec![("config.b".to_string(), "3".to_string())]
        );
        assert!(s.settings_with_prefix("config%").unwrap().is_empty());
    }

    #[test]
    fn updates_set_and_remove_together() {
        let (_dir, mut s) = store();
        s.set_settings_text(&[("config.a", "1"), ("config.b", "2")])
            .unwrap();
        s.update_settings_text(&[("config.c", "3")], &["config.a", "config.missing"])
            .unwrap();
        assert_eq!(
            s.settings_with_prefix("config.").unwrap(),
            vec![
                ("config.b".to_string(), "2".to_string()),
                ("config.c".to_string(), "3".to_string()),
            ]
        );
    }

    #[test]
    fn a_non_text_value_is_corrupt() {
        let (_dir, mut s) = store();
        s.set_setting_i64("config.n", 5).unwrap();
        assert!(s.setting_text("config.n").is_err());
        assert!(s.settings_with_prefix("config.").is_err());
    }

    #[test]
    fn a_failed_write_writes_nothing() {
        let (_dir, mut s) = store();
        s.set_settings_text(&[("config.a", "old")]).unwrap();
        s.conn
            .execute_batch(
                "CREATE TEMP TRIGGER fail BEFORE INSERT ON main.settings
                 WHEN NEW.key = 'config.boom' BEGIN SELECT RAISE(ABORT, 'boom'); END;",
            )
            .unwrap();
        assert!(
            s.set_settings_text(&[("config.a", "new"), ("config.boom", "1")])
                .is_err()
        );
        assert_eq!(s.setting_text("config.a").unwrap().as_deref(), Some("old"));
        assert_eq!(s.setting_text("config.boom").unwrap(), None);
        // A failed write rolls back the deletes beside it too.
        assert!(
            s.update_settings_text(&[("config.boom", "1")], &["config.a"])
                .is_err()
        );
        assert_eq!(s.setting_text("config.a").unwrap().as_deref(), Some("old"));
    }
}
