//! What the M5 user-data modules share: input checks and the call made
//! after a delete.

use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::{Error, Result, Store};

/// Opens a write transaction with `BEGIN IMMEDIATE`. A deferred one that
/// reads first fails at once with SQLITE_BUSY when another connection
/// holds the write lock, because SQLite doesn't run the busy handler for a
/// read-to-write upgrade; IMMEDIATE takes the lock up front, waiting out
/// the busy timeout.
///
/// TODO(#81): becomes `crate::begin_write` once #81 is on main.
pub(crate) fn write_tx(conn: &mut Connection) -> Result<Transaction<'_>> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

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
    /// TODO(#81): the WAL still holds the frames those rows were written
    /// in until a checkpoint truncates it. When #81 merges, call its WAL
    /// scrub here (best effort, as its other delete paths do).
    pub(crate) fn after_user_delete(&self) {
        #[cfg(test)]
        SCRUBS.with(|n| n.set(n.get() + 1));
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
