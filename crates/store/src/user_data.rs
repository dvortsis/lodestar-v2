//! What the M5 user-data modules share: input checks and the call made
//! after a delete.

use crate::{Error, Result, Store};

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
/// breaks and tabs are allowed.
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
    /// and its alerts, a collection, a key, a user item), whose rows
    /// `secure_delete` zeroes in the main file.
    ///
    /// TODO(#81): the WAL still holds the frames those rows were written
    /// in until a checkpoint truncates it. When #81 merges, call its WAL
    /// scrub here (best effort, as its other delete paths do).
    pub(crate) fn after_user_delete(&self) {}
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
