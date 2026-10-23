//! What the M5 user-data modules share: input checks and the call made
//! after a delete.

use crate::{Error, Result, Store};

/// A `usize` count as an SQL `LIMIT`/`OFFSET`, saturating: a plain cast
/// turns `usize::MAX` into -1, which SQLite reads as "no limit" for LIMIT
/// but as 0 for OFFSET.
pub(crate) fn sql_limit(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// A name (saved search, collection, key): 1–200 characters, no control
/// characters, no surrounding whitespace. The API trims before calling.
pub(crate) fn check_name(name: &str) -> Result<()> {
    check_text(
        name,
        200,
        "name must be 1-200 characters, trimmed, no control characters",
    )
}

/// A note: 1–4,000 characters, otherwise as [`check_name`] but line
/// breaks (`\n`) and tabs are allowed. A carriage return is refused: the
/// API normalizes CRLF to LF first.
pub(crate) fn check_note(note: &str) -> Result<()> {
    let n = note.chars().count();
    let ok = (1..=4000).contains(&n)
        && note.trim() == note
        && note
            .chars()
            .all(|c| !c.is_control() || c == '\n' || c == '\t');
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(
            "note must be 1-4000 characters, trimmed, no control characters but line breaks",
        ))
    }
}

pub(crate) fn check_text(s: &str, max: usize, what: &'static str) -> Result<()> {
    let n = s.chars().count();
    if (1..=max).contains(&n) && s.trim() == s && !s.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(Error::Invalid(what))
    }
}

impl Store {
    /// Called after every committed delete of user data (a saved search
    /// and its alerts, a collection, a key, a user item, pruning), and
    /// after an update that clears or replaces stored text (a name, query,
    /// note or labels), whose old rows `secure_delete` zeroes in the main
    /// file.
    ///
    /// The WAL still holds the frames those rows were written in, so this
    /// runs #81's scrub ([`Store::scrub_wal`]): best effort, never failing
    /// the change that already committed, and retried by the next delete
    /// or the hourly purge when it can't finish (`scrub_pending`).
    pub(crate) fn after_user_delete(&mut self) {
        #[cfg(test)]
        SCRUBS.with(|n| n.set(n.get() + 1));
        self.scrub_wal();
    }
}

#[cfg(test)]
thread_local! {
    /// How often [`Store::after_user_delete`] ran on this thread.
    static SCRUBS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Runs `f` and returns how many times it called the scrub hook.
#[cfg(test)]
pub(crate) fn scrubs_during(f: impl FnOnce()) -> u64 {
    let before = SCRUBS.with(std::cell::Cell::get);
    f();
    SCRUBS.with(std::cell::Cell::get) - before
}

/// A store with torrents `1..=n`, for the M5 modules' tests.
#[cfg(test)]
pub(crate) fn test_store(n: i64) -> Store {
    let store = Store::open_in_memory(crate::StoreConfig::default()).unwrap();
    for id in 1..=n {
        store
            .conn
            .execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                 VALUES (?1, ?2, 'x', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                rusqlite::params![id, id.to_be_bytes().repeat(3)[..20].to_vec()],
            )
            .unwrap();
    }
    store
}

/// Deletes a torrent the way retention or a relabel does.
#[cfg(test)]
pub(crate) fn delete_torrent(store: &Store, id: i64) {
    store
        .conn
        .execute("DELETE FROM torrents WHERE id = ?1", [id])
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// A read-first write waits for another connection's write lock
    /// instead of failing with SQLITE_BUSY (the read-to-write upgrade a
    /// deferred transaction would attempt never runs the busy handler).
    #[test]
    fn writes_wait_for_another_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut store = Store::open(&path, crate::StoreConfig::default()).unwrap();
        let id = store.create_collection("A", None, 0).unwrap();
        let other = Connection::open(&path).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            other.execute_batch("COMMIT").unwrap();
        });
        // Reads the collection first, then writes.
        assert_eq!(store.add_to_collection(id, &[], 1).unwrap(), 0);
        store.reorder_collection(id, &[], 2).unwrap();
        holder.join().unwrap();
    }

    /// Deleted rows and replaced text leave nothing in the database file
    /// or the WAL (owner decision §2.17): `secure_delete` zeroes the main
    /// file, and the scrub truncates the WAL.
    #[test]
    fn deleted_and_replaced_text_leave_no_trace_on_disk() {
        use crate::{SavedSearchInput, UserItemUpdate};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let mut s = Store::open(&path, crate::StoreConfig::default()).unwrap();
        s.conn
            .execute(
                "INSERT INTO torrents (id, info_hash, name, size, piece_len, piece_count,
                     file_count, info_size, flags, discovered_at, metadata_at, last_seen_at)
                 VALUES (1, zeroblob(20), 'x', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                [],
            )
            .unwrap();
        let on_disk = || {
            let mut all = std::fs::read(&path).unwrap();
            all.extend(std::fs::read(dir.path().join("t.db-wal")).unwrap_or_default());
            all
        };
        let has = |m: &str| on_disk().windows(m.len()).any(|w| w == m.as_bytes());

        let c = s
            .create_collection("coll-marker-5e1", Some("note-marker-8a2"), 0)
            .unwrap();
        let note = UserItemUpdate {
            note: Some(Some("item-marker-3f7")),
            favourite: Some(true),
            ..Default::default()
        };
        s.set_user_item(1, &note, 0).unwrap();
        let mut q = SavedSearchInput {
            name: "a",
            query: "query-marker-c44",
            ast: "{}",
            sort: None,
            alert: true,
            rss: false,
            channels: 0,
            paused: false,
        };
        let id = s.create_saved_search(&q, 0, 0).unwrap();
        for m in [
            "coll-marker-5e1",
            "note-marker-8a2",
            "item-marker-3f7",
            "query-marker-c44",
        ] {
            assert!(has(m), "{m} was written");
        }

        s.delete_collection(c).unwrap();
        s.set_user_item(
            1,
            &UserItemUpdate {
                note: Some(None),
                ..Default::default()
            },
            1,
        )
        .unwrap();
        q.query = "other";
        s.update_saved_search(id, &q, 0, 1).unwrap();
        for m in [
            "coll-marker-5e1",
            "note-marker-8a2",
            "item-marker-3f7",
            "query-marker-c44",
        ] {
            assert!(!has(m), "{m} is gone from the file and the WAL");
        }
        assert!(!s.scrub_pending);
    }

    /// Deletes and text that's replaced or cleared reach the scrub hook;
    /// updates that leave every old text in place don't.
    #[test]
    fn replaced_text_is_scrubbed() {
        use crate::{SavedSearchInput, UserItemUpdate};
        let mut s = test_store(2);
        let c = s.create_collection("A", Some("n"), 0).unwrap();
        assert_eq!(
            scrubs_during(|| assert!(s.update_collection(c, "A", Some("n"), 1).unwrap())),
            0
        );
        assert_eq!(
            scrubs_during(|| assert!(s.update_collection(c, "B", Some("n"), 2).unwrap())),
            1
        );
        assert_eq!(
            scrubs_during(|| assert!(s.update_collection(c, "B", None, 3).unwrap())),
            1
        );
        assert_eq!(
            scrubs_during(|| assert!(s.update_collection(c, "B", Some("new"), 4).unwrap())),
            0
        );

        let note = |n: Option<&'static str>| UserItemUpdate {
            note: Some(n),
            ..Default::default()
        };
        let fav = UserItemUpdate {
            favourite: Some(true),
            ..Default::default()
        };
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &note(Some("a")), 0).unwrap();
            }),
            0
        );
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &fav, 1).unwrap();
            }),
            0
        );
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &note(Some("b")), 2).unwrap();
            }),
            1
        );
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &note(None), 3).unwrap();
            }),
            1
        );
        let l1 = ["x"];
        let labels = UserItemUpdate {
            labels: Some(&l1),
            ..Default::default()
        };
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &labels, 4).unwrap();
            }),
            0
        );
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &labels, 5).unwrap();
            }),
            0
        );
        let none = UserItemUpdate {
            labels: Some(&[]),
            favourite: Some(false),
            ..Default::default()
        };
        assert_eq!(
            scrubs_during(|| {
                s.set_user_item(1, &none, 6).unwrap();
            }),
            1
        );

        let mut q = SavedSearchInput {
            name: "a",
            query: "dune",
            ast: "{}",
            sort: None,
            alert: true,
            rss: false,
            channels: 0,
            paused: false,
        };
        let id = s.create_saved_search(&q, 0, 0).unwrap();
        q.paused = true;
        assert_eq!(
            scrubs_during(|| {
                s.update_saved_search(id, &q, 0, 1).unwrap();
            }),
            0
        );
        q.query = "dune 2160p";
        assert_eq!(
            scrubs_during(|| {
                s.update_saved_search(id, &q, 0, 2).unwrap();
            }),
            1
        );
        assert_eq!(
            scrubs_during(|| {
                s.delete_saved_search(id).unwrap();
            }),
            1
        );
        assert_eq!(
            scrubs_during(|| {
                s.delete_collection(c).unwrap();
            }),
            1
        );
    }

    #[test]
    fn names_and_notes() {
        assert!(check_name("4K nature docs").is_ok());
        assert!(check_name(&"x".repeat(200)).is_ok());
        for bad in ["", " x", "x ", "a\nb", "a\u{7}b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name(&"é".repeat(201)).is_err());
        assert!(check_note("line one\nline two\tok").is_ok());
        for bad in ["", "\nx", "a\u{0}b", &"x".repeat(4001)] {
            assert!(check_note(bad).is_err(), "{bad:?}");
        }
    }
}
