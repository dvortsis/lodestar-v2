//! Collections (architecture §7): named, ordered lists of torrents.
//!
//! Positions are integers within a collection, ascending; appends go after
//! the last one, and a reorder rewrites them densely. A torrent is in a
//! collection at most once. Deleting a collection or a torrent removes its
//! entries.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::user_data::{check_name, check_note, write_tx};
use crate::{Error, Result, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub note: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub items: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionItem {
    pub torrent_id: i64,
    pub position: i64,
    pub added_at: i64,
}

const COLUMNS: &str = "c.id, c.name, c.note, c.created_at, c.updated_at,
    (SELECT count(*) FROM collection_items i WHERE i.collection_id = c.id)";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: r.get(0)?,
        name: r.get(1)?,
        note: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        items: crate::get_u64(r, 5)?,
    })
}

fn check(name: &str, note: Option<&str>) -> Result<()> {
    check_name(name)?;
    note.map(check_note).transpose()?;
    Ok(())
}

fn exists(tx: &Transaction<'_>, id: i64) -> Result<()> {
    let known: bool = tx
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM collections WHERE id = ?1)")?
        .query_row([id], |r| r.get(0))?;
    if known {
        Ok(())
    } else {
        Err(Error::NotFound("collection"))
    }
}

fn touch(tx: &Transaction<'_>, id: i64, now: i64) -> Result<()> {
    tx.prepare_cached("UPDATE collections SET updated_at = ?2 WHERE id = ?1")?
        .execute(params![id, now])?;
    Ok(())
}

impl Store {
    pub fn create_collection(&mut self, name: &str, note: Option<&str>, now: i64) -> Result<i64> {
        check(name, note)?;
        self.conn.execute(
            "INSERT INTO collections (name, note, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![name, note, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Renames a collection and sets its note; `false` if it doesn't exist.
    pub fn update_collection(
        &mut self,
        id: i64,
        name: &str,
        note: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        check(name, note)?;
        let changed = self.conn.execute(
            "UPDATE collections SET name = ?2, note = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, name, note, now],
        )?;
        Ok(changed > 0)
    }

    /// Deletes a collection and its entries (not the torrents).
    pub fn delete_collection(&mut self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM collections WHERE id = ?1", [id])?;
        if deleted > 0 {
            self.after_user_delete();
        }
        Ok(deleted > 0)
    }

    pub fn collection(&self, id: i64) -> Result<Option<Collection>> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM collections c WHERE c.id = ?1"
            ))?
            .query_row([id], row)
            .optional()?)
    }

    /// Every collection, most recently changed first.
    pub fn collections(&self) -> Result<Vec<Collection>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM collections c ORDER BY c.updated_at DESC, c.id DESC"
        ))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The ids of the collections a torrent is in, in id order (the
    /// drawer's "In collections").
    pub fn collections_of(&self, torrent_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT collection_id FROM collection_items WHERE torrent_id = ?1
             ORDER BY collection_id",
        )?;
        let rows = stmt.query_map([torrent_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A collection's entries in order: up to `limit` after position
    /// `after` (the previous page's last), or from the start.
    pub fn collection_items(
        &self,
        id: i64,
        after: Option<i64>,
        limit: usize,
    ) -> Result<Vec<CollectionItem>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT torrent_id, position, added_at FROM collection_items
             WHERE collection_id = ?1 AND position > ?2 ORDER BY position LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![id, after.unwrap_or(i64::MIN), limit as i64], |r| {
            Ok(CollectionItem {
                torrent_id: r.get(0)?,
                position: r.get(1)?,
                added_at: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Appends torrents to a collection in the order given; ones already
    /// in it stay where they are. All or nothing: an unknown collection or
    /// torrent is [`Error::NotFound`] and adds none. Returns how many were
    /// added.
    pub fn add_to_collection(&mut self, id: i64, torrent_ids: &[i64], now: i64) -> Result<usize> {
        let tx = write_tx(&mut self.conn)?;
        exists(&tx, id)?;
        let mut next: i64 = tx
            .prepare_cached(
                "SELECT coalesce(max(position), 0) + 1 FROM collection_items
                 WHERE collection_id = ?1",
            )?
            .query_row([id], |r| r.get(0))?;
        let mut added = 0;
        {
            let mut known =
                tx.prepare_cached("SELECT EXISTS (SELECT 1 FROM torrents WHERE id = ?1)")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO collection_items (collection_id, torrent_id, position, added_at)
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            )?;
            for &t in torrent_ids {
                if !known.query_row([t], |r| r.get::<_, bool>(0))? {
                    return Err(Error::NotFound("torrent"));
                }
                if insert.execute(params![id, t, next, now])? > 0 {
                    next += 1;
                    added += 1;
                }
            }
        }
        if added > 0 {
            touch(&tx, id, now)?;
        }
        tx.commit()?;
        Ok(added)
    }

    /// Removes torrents from a collection; returns how many were in it.
    pub fn remove_from_collection(
        &mut self,
        id: i64,
        torrent_ids: &[i64],
        now: i64,
    ) -> Result<usize> {
        let tx = write_tx(&mut self.conn)?;
        let mut removed = 0;
        {
            let mut stmt = tx.prepare_cached(
                "DELETE FROM collection_items WHERE collection_id = ?1 AND torrent_id = ?2",
            )?;
            for &t in torrent_ids {
                removed += stmt.execute(params![id, t])?;
            }
        }
        if removed > 0 {
            touch(&tx, id, now)?;
        }
        tx.commit()?;
        if removed > 0 {
            self.after_user_delete();
        }
        Ok(removed)
    }

    /// Puts a collection's entries in the order given, which must name
    /// each of its torrents exactly once ([`Error::Invalid`] otherwise, so
    /// a reorder from a stale page can't drop or duplicate entries).
    pub fn reorder_collection(&mut self, id: i64, torrent_ids: &[i64], now: i64) -> Result<()> {
        let tx = write_tx(&mut self.conn)?;
        exists(&tx, id)?;
        let mut current: Vec<i64> = {
            let mut stmt = tx.prepare_cached(
                "SELECT torrent_id FROM collection_items WHERE collection_id = ?1",
            )?;
            stmt.query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        current.sort_unstable();
        let mut given = torrent_ids.to_vec();
        given.sort_unstable();
        if current != given {
            return Err(Error::Invalid("a reorder must list every entry once"));
        }
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE collection_items SET position = ?3
                 WHERE collection_id = ?1 AND torrent_id = ?2",
            )?;
            for (i, &t) in torrent_ids.iter().enumerate() {
                stmt.execute(params![id, t, i as i64 + 1])?;
            }
        }
        touch(&tx, id, now)?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::{delete_torrent, test_store};

    fn order(s: &Store, id: i64) -> Vec<i64> {
        s.collection_items(id, None, 100)
            .unwrap()
            .iter()
            .map(|i| i.torrent_id)
            .collect()
    }

    #[test]
    fn lifecycle() {
        let mut s = test_store(10);
        let id = s.create_collection("Road trip", None, 10).unwrap();
        assert_eq!(s.add_to_collection(id, &[5, 2, 5, 9], 20).unwrap(), 3);
        assert_eq!(order(&s, id), [5, 2, 9]);
        // Already there: stays put; new ones go last.
        assert_eq!(s.add_to_collection(id, &[2, 1], 30).unwrap(), 1);
        assert_eq!(order(&s, id), [5, 2, 9, 1]);
        let c = s.collection(id).unwrap().unwrap();
        assert_eq!((c.items, c.created_at, c.updated_at), (4, 10, 30));

        let page = s.collection_items(id, None, 2).unwrap();
        let rest = s.collection_items(id, Some(page[1].position), 10).unwrap();
        assert_eq!(
            rest.iter().map(|i| i.torrent_id).collect::<Vec<_>>(),
            [9, 1]
        );

        s.reorder_collection(id, &[1, 9, 5, 2], 40).unwrap();
        assert_eq!(order(&s, id), [1, 9, 5, 2]);
        assert_eq!(s.add_to_collection(id, &[3], 50).unwrap(), 1);
        assert_eq!(order(&s, id), [1, 9, 5, 2, 3]);

        assert_eq!(s.remove_from_collection(id, &[9, 7], 60).unwrap(), 1);
        assert_eq!(order(&s, id), [1, 5, 2, 3]);
        assert_eq!(s.collections_of(5).unwrap(), [id]);

        assert!(s.update_collection(id, "Trip", Some("summer"), 70).unwrap());
        let c = s.collection(id).unwrap().unwrap();
        assert_eq!(
            (c.name.as_str(), c.note.as_deref()),
            ("Trip", Some("summer"))
        );
        assert!(!s.update_collection(999, "x", None, 0).unwrap());
    }

    #[test]
    fn refusals_change_nothing() {
        let mut s = test_store(5);
        let id = s.create_collection("A", None, 0).unwrap();
        s.add_to_collection(id, &[1, 2], 0).unwrap();
        assert!(matches!(
            s.add_to_collection(id, &[3, 99], 1),
            Err(Error::NotFound("torrent"))
        ));
        assert!(matches!(
            s.add_to_collection(999, &[1], 1),
            Err(Error::NotFound("collection"))
        ));
        assert_eq!(order(&s, id), [1, 2], "all or nothing");
        for bad in [&[2][..], &[2, 1, 1], &[2, 1, 3], &[]] {
            assert!(matches!(
                s.reorder_collection(id, bad, 1),
                Err(Error::Invalid(_))
            ));
        }
        assert_eq!(order(&s, id), [1, 2]);
        assert!(s.create_collection("", None, 0).is_err());
        assert!(s.create_collection("B", Some(""), 0).is_err());
        assert_eq!(s.collections().unwrap().len(), 1);
    }

    #[test]
    fn deletes_cascade_and_ids_are_never_reused() {
        let mut s = test_store(5);
        let a = s.create_collection("A", None, 0).unwrap();
        let b = s.create_collection("B", None, 1).unwrap();
        s.add_to_collection(a, &[1, 2], 0).unwrap();
        s.add_to_collection(b, &[2], 0).unwrap();
        assert_eq!(
            s.collections()
                .unwrap()
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [b, a]
        );

        delete_torrent(&s, 2);
        assert_eq!(order(&s, a), [1]);
        assert_eq!(s.collection(b).unwrap().unwrap().items, 0);

        assert!(s.delete_collection(a).unwrap());
        assert!(!s.delete_collection(a).unwrap());
        assert!(s.collections_of(1).unwrap().is_empty());
        let entries: i64 = s
            .conn
            .query_row("SELECT count(*) FROM collection_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(entries, 0);
        s.delete_collection(b).unwrap();
        assert!(s.create_collection("C", None, 2).unwrap() > b);
    }
}
