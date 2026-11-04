//! SQLite's own heap, for the memory attribution gauges (X-mem-gauges).
//!
//! The bundled SQLite allocates through the C library's `malloc` (musl's
//! mallocng in the Docker image), not through the Rust global allocator,
//! so mimalloc's counters never see it. SQLite counts its own bytes
//! (`SQLITE_DEFAULT_MEMSTATUS` is on in the bundled build): these read
//! those process-wide counters.
//!
//! **Cost:** each call takes SQLite's memory-statistics mutex for a load
//! of one integer, the same mutex every SQLite allocation already takes
//! for nanoseconds. It allocates nothing and touches no connection, page
//! or lock of ours.
//!
//! **`unsafe`:** rusqlite wraps neither function, so these are the two
//! reads owner-decisions §2.35 allows in this crate (and the reason its
//! `Cargo.toml` sets `unsafe_code = "deny"`, not the workspace's
//! "forbid"). Each `allow` covers one call and nothing else.

use rusqlite::ffi;

/// Bytes SQLite has allocated and not yet freed, process-wide.
pub fn sqlite_memory_used() -> u64 {
    // SAFETY: `sqlite3_memory_used` takes no arguments, dereferences no
    // caller pointer and only reads a global counter under SQLite's own
    // mutex. SQLite documents it as callable from any thread at any time,
    // before or after any connection opens
    // (https://sqlite.org/c3ref/memory_highwater.html).
    #[allow(unsafe_code)]
    let used = unsafe { ffi::sqlite3_memory_used() };
    u64::try_from(used).unwrap_or(0)
}

/// The most [`sqlite_memory_used`] has been since the process started.
pub fn sqlite_memory_highwater() -> u64 {
    // SAFETY: as above. Its one argument is a plain integer flag; 0 only
    // reads the high-water mark and never resets it.
    #[allow(unsafe_code)]
    let high = unsafe { ffi::sqlite3_memory_highwater(0) };
    u64::try_from(high).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_a_store_and_writing_raises_sqlite_memory() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t(x BLOB);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
             INSERT INTO t SELECT randomblob(256) FROM n;",
        )
        .unwrap();
        // Other tests' connections share the counter and can free theirs
        // at any moment, so compare against this connection's own floor:
        // 2,000 rows of 256 random bytes held in memory are over 500 KB.
        let used = sqlite_memory_used();
        assert!(used >= 500_000, "SQLite reports {used} bytes");
        assert!(sqlite_memory_highwater() >= used);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2000);
    }

    #[test]
    fn a_real_store_counts_in_sqlite_memory() {
        let store = crate::Store::open_in_memory(crate::StoreConfig::default()).unwrap();
        let used = sqlite_memory_used();
        assert!(used > 0, "an open store holds SQLite memory");
        assert!(sqlite_memory_highwater() >= used);
        drop(store);
    }
}
