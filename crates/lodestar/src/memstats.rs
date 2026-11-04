//! Memory attribution gauges (X-mem-gauges): which heap the process's
//! anonymous memory is in, so a soak's memory verdict can say where a ramp
//! is.
//!
//! - **mimalloc**, the Rust global allocator (not `override`): its own
//!   committed bytes, from `mi_stats_get_json` through the `mimalloc`
//!   crate's safe `MiMalloc::stats_json` (`extended` feature).
//! - **SQLite**, which counts its own heap: `sqlite3_memory_used` and its
//!   high-water mark, through `lodestar_store::memstats`.
//! - **The kernel's view** (Linux only): `RssAnon` and `RssFile` from
//!   `/proc/self/status`, and `AnonHugePages` from
//!   `/proc/self/smaps_rollup`.
//!
//! C code (SQLite, zstd) allocates through the C library's `malloc`, which
//! mimalloc doesn't count. `RssAnon` minus mimalloc's committed bytes and
//! SQLite's own approximates the rest, which the analyzer reports as "other
//! heap". It's neither an upper nor a lower bound:
//! - it also holds thread stacks and every other anonymous mapping;
//! - transparent huge pages can fault whole 2 MiB pages into mimalloc's
//!   arenas beyond the slices mimalloc counts, which lands here (read it
//!   with `AnonHugePages`);
//! - mimalloc counts committed slices whether or not they're resident,
//!   which can make it too small.
//!
//! The storage sampler (`storage.rs`) calls [`sample`] every 60 s on a
//! blocking thread, and `/metrics` reads the last values. Each value is on
//! its own: one that can't be read (another OS, a field missing or odd, a
//! failed read) is absent from `/metrics`, never 0 and never a panic.
//!
//! **Cost per sample:** the SQLite reads take its memory-statistics mutex
//! for one load each. mimalloc copies its statistics block and formats
//! about 15 KB of JSON (mimalloc-allocated, freed at once), then this
//! parses it: about 0.2 ms, measured in the decision. The two `/proc` reads
//! are small text files; `smaps_rollup` walks the process's mappings in
//! the kernel, which is why it runs on the 60 s sampler and never in a
//! scrape. None of it takes a lock of the search index, the store or the
//! writer.

use std::sync::atomic::{AtomicU64, Ordering};

/// A gauge with nothing sampled, or nothing readable.
const ABSENT: u64 = u64::MAX;

/// The last sampled values; each `None` while absent.
#[derive(Debug)]
pub struct Gauges {
    mimalloc_committed: AtomicU64,
    mimalloc_peak_committed: AtomicU64,
    sqlite: AtomicU64,
    sqlite_highwater: AtomicU64,
    rss_anon: AtomicU64,
    rss_file: AtomicU64,
    anon_huge: AtomicU64,
}

impl Default for Gauges {
    fn default() -> Gauges {
        Gauges {
            mimalloc_committed: AtomicU64::new(ABSENT),
            mimalloc_peak_committed: AtomicU64::new(ABSENT),
            sqlite: AtomicU64::new(ABSENT),
            sqlite_highwater: AtomicU64::new(ABSENT),
            rss_anon: AtomicU64::new(ABSENT),
            rss_file: AtomicU64::new(ABSENT),
            anon_huge: AtomicU64::new(ABSENT),
        }
    }
}

/// One reading, as `/metrics` renders it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub mimalloc_committed: Option<u64>,
    pub mimalloc_peak_committed: Option<u64>,
    pub sqlite: Option<u64>,
    pub sqlite_highwater: Option<u64>,
    pub rss_anon: Option<u64>,
    pub rss_file: Option<u64>,
    pub anon_huge: Option<u64>,
}

impl Gauges {
    pub fn snapshot(&self) -> Snapshot {
        let get = |a: &AtomicU64| Some(a.load(Ordering::Relaxed)).filter(|&v| v != ABSENT);
        Snapshot {
            mimalloc_committed: get(&self.mimalloc_committed),
            mimalloc_peak_committed: get(&self.mimalloc_peak_committed),
            sqlite: get(&self.sqlite),
            sqlite_highwater: get(&self.sqlite_highwater),
            rss_anon: get(&self.rss_anon),
            rss_file: get(&self.rss_file),
            anon_huge: get(&self.anon_huge),
        }
    }

    /// Stores every value of `s`: an absent one clears its gauge, so a
    /// field that stops reading drops out rather than going stale.
    pub fn store(&self, s: Snapshot) {
        for (gauge, v) in [
            (&self.mimalloc_committed, s.mimalloc_committed),
            (&self.mimalloc_peak_committed, s.mimalloc_peak_committed),
            (&self.sqlite, s.sqlite),
            (&self.sqlite_highwater, s.sqlite_highwater),
            (&self.rss_anon, s.rss_anon),
            (&self.rss_file, s.rss_file),
            (&self.anon_huge, s.anon_huge),
        ] {
            // A real value of u64::MAX bytes can't happen; clamp it anyway
            // so it never reads as absent.
            gauge.store(v.map_or(ABSENT, |v| v.min(ABSENT - 1)), Ordering::Relaxed);
        }
    }
}

/// Reads every value now (blocking: the `/proc` reads).
pub fn sample(g: &Gauges) {
    g.store(read());
}

fn read() -> Snapshot {
    let (mimalloc_committed, mimalloc_peak_committed) = mimalloc_commit();
    let (rss_anon, rss_file, anon_huge) = proc_self();
    Snapshot {
        mimalloc_committed,
        mimalloc_peak_committed,
        sqlite: Some(lodestar_store::memstats::sqlite_memory_used()),
        sqlite_highwater: Some(lodestar_store::memstats::sqlite_memory_highwater()),
        rss_anon,
        rss_file,
        anon_huge,
    }
}

/// mimalloc's committed bytes now and at their peak, process-wide.
fn mimalloc_commit() -> (Option<u64>, Option<u64>) {
    match mimalloc::MiMalloc::stats_json() {
        Ok(json) => match json.to_str() {
            Ok(text) => parse_mimalloc_json(text),
            Err(_) => (None, None),
        },
        Err(_) => (None, None),
    }
}

/// `process.commit_current` and `process.commit_peak` from
/// `mi_stats_get_json`'s output; each `None` when missing or not a
/// non-negative integer.
pub fn parse_mimalloc_json(text: &str) -> (Option<u64>, Option<u64>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return (None, None);
    };
    let field = |name: &str| v.get("process")?.get(name)?.as_u64();
    (field("commit_current"), field("commit_peak"))
}

#[cfg(target_os = "linux")]
fn proc_self() -> (Option<u64>, Option<u64>, Option<u64>) {
    let status = std::fs::read_to_string("/proc/self/status").ok();
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").ok();
    let field = |text: &Option<String>, key| text.as_deref().and_then(|t| kb_field(t, key));
    (
        field(&status, "RssAnon"),
        field(&status, "RssFile"),
        field(&rollup, "AnonHugePages"),
    )
}

#[cfg(not(target_os = "linux"))]
fn proc_self() -> (Option<u64>, Option<u64>, Option<u64>) {
    (None, None, None)
}

/// The bytes in a `/proc` line `<key>: <n> kB`. `None` when the key is
/// missing, appears twice, or its value isn't digits followed by `kB`
/// (or overflows): an odd field is absent, never a guess. Only the Linux
/// sampler (and the tests) call it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn kb_field(text: &str, key: &str) -> Option<u64> {
    let mut found = None;
    for line in text.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name != key {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let mut parts = rest.split_ascii_whitespace();
        let (Some(n), Some("kB"), None) = (parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        found = Some(n.parse::<u64>().ok()?.checked_mul(1024)?);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any text, any key: an answer or `None`, never a panic.
        #[test]
        fn kb_field_never_panics(text in "\\PC{0,200}", key in "[A-Za-z]{0,12}") {
            let _ = kb_field(&text, &key);
        }

        /// A well-formed line, with any spacing around the number and unit,
        /// reads back as its kilobytes in bytes (`None` past u64).
        #[test]
        fn a_well_formed_line_reads_back(
            n in any::<u64>(),
            a in "[ \t]{0,4}",
            b in "[ \t]{1,4}",
            c in "[ \t]{0,4}",
            before in "([a-z]{1,8}:[ \t]*[0-9]{1,6} kB\n){0,3}",
        ) {
            let text = format!("{before}RssAnon:{a}{n}{b}kB{c}\nThreads:\t9\n");
            prop_assert_eq!(kb_field(&text, "RssAnon"), n.checked_mul(1024));
        }

        /// Any JSON-ish text never panics, and a well-formed one reads back.
        #[test]
        fn mimalloc_json_reads_back_or_is_absent(
            now in any::<u64>(), peak in any::<u64>(), junk in "\\PC{0,80}",
        ) {
            let _ = parse_mimalloc_json(&junk);
            let json = format!(
                r#"{{"process": {{"commit_current": {now}, "commit_peak": {peak}}}}}"#
            );
            prop_assert_eq!(parse_mimalloc_json(&json), (Some(now), Some(peak)));
        }
    }

    const STATUS: &str = "Name:\tlodestar\nVmRSS:\t  612340 kB\nRssAnon:\t  503808 kB\n\
        RssFile:\t  108000 kB\nRssShmem:\t       0 kB\nThreads:\t42\n";

    #[test]
    fn status_fields_read_in_bytes() {
        assert_eq!(kb_field(STATUS, "RssAnon"), Some(503_808 * 1024));
        assert_eq!(kb_field(STATUS, "RssFile"), Some(108_000 * 1024));
        assert_eq!(kb_field(STATUS, "RssShmem"), Some(0));
    }

    #[test]
    fn a_missing_field_is_absent() {
        assert_eq!(kb_field(STATUS, "AnonHugePages"), None);
        assert_eq!(kb_field("", "RssAnon"), None);
        // A prefix of another key isn't the key.
        assert_eq!(kb_field("RssAnonX:\t1 kB\n", "RssAnon"), None);
        assert_eq!(kb_field(" RssAnon:\t1 kB\n", "RssAnon"), None);
    }

    #[test]
    fn spacing_varies() {
        assert_eq!(kb_field("RssAnon:1 kB", "RssAnon"), Some(1024));
        assert_eq!(
            kb_field("RssAnon:      7     kB   \n", "RssAnon"),
            Some(7 * 1024)
        );
        assert_eq!(kb_field("RssAnon:\t\t7\tkB\r\n", "RssAnon"), Some(7 * 1024));
    }

    #[test]
    fn odd_values_are_absent() {
        for line in [
            "RssAnon:",
            "RssAnon:\t\n",
            "RssAnon:\t12\n",
            "RssAnon:\t12 MB\n",
            "RssAnon:\t12 kb\n",
            "RssAnon:\t-12 kB\n",
            "RssAnon:\t+12 kB\n",
            "RssAnon:\t1.5 kB\n",
            "RssAnon:\t12 kB extra\n",
            "RssAnon:\tlots kB\n",
            "RssAnon:\t١٢ kB\n",
            "Threads:\t42\nRssAnon:\t1 kB\nRssAnon:\t2 kB\n",
        ] {
            assert_eq!(kb_field(line, "RssAnon"), None, "{line:?}");
        }
    }

    #[test]
    fn huge_values_read_or_overflow_to_absent() {
        let max_kb = u64::MAX / 1024;
        let line = format!("AnonHugePages:\t{max_kb} kB\n");
        assert_eq!(kb_field(&line, "AnonHugePages"), Some(max_kb * 1024));
        let over = format!("AnonHugePages:\t{} kB\n", max_kb + 1);
        assert_eq!(kb_field(&over, "AnonHugePages"), None);
        let digits = format!("AnonHugePages:\t{} kB\n", "9".repeat(40));
        assert_eq!(kb_field(&digits, "AnonHugePages"), None);
    }

    #[test]
    fn smaps_rollup_reads_anon_huge_pages() {
        let rollup = "55d0c0000000-7ffc00000000 ---p 00000000 00:00 0  [rollup]\n\
            Rss:              612340 kB\nAnonymous:        503808 kB\n\
            AnonHugePages:    217088 kB\nShmemPmdMapped:        0 kB\n";
        assert_eq!(kb_field(rollup, "AnonHugePages"), Some(217_088 * 1024));
        assert_eq!(kb_field(rollup, "Anonymous"), Some(503_808 * 1024));
    }

    #[test]
    fn mimalloc_json_reads_the_process_commit() {
        let json = r#"{"stat_version": 2, "process": {"elapsed_msecs": 10,
            "commit_current": 4194304, "commit_peak": 8388608}, "malloc_bins": []}"#;
        assert_eq!(
            parse_mimalloc_json(json),
            (Some(4_194_304), Some(8_388_608))
        );
        let partial = r#"{"process": {"commit_current": 4096, "commit_peak": -1}}"#;
        assert_eq!(parse_mimalloc_json(partial), (Some(4096), None));
        assert_eq!(parse_mimalloc_json(r#"{"process": 3}"#), (None, None));
        assert_eq!(parse_mimalloc_json("not json"), (None, None));
        assert_eq!(parse_mimalloc_json(""), (None, None));
    }

    #[test]
    fn absent_values_clear_and_present_ones_store() {
        let g = Gauges::default();
        assert_eq!(g.snapshot(), Snapshot::default());
        let full = Snapshot {
            mimalloc_committed: Some(1),
            mimalloc_peak_committed: Some(2),
            sqlite: Some(5),
            sqlite_highwater: Some(6),
            rss_anon: Some(3),
            rss_file: Some(0),
            anon_huge: Some(u64::MAX),
        };
        g.store(full);
        let got = g.snapshot();
        assert_eq!(got.rss_file, Some(0), "0 is a value, not absent");
        assert_eq!(got.anon_huge, Some(u64::MAX - 1));
        g.store(Snapshot {
            rss_anon: None,
            ..full
        });
        assert_eq!(g.snapshot().rss_anon, None);
        assert_eq!(g.snapshot().mimalloc_committed, Some(1));
    }

    /// The test binary runs on mimalloc (main.rs's global allocator).
    #[test]
    fn mimalloc_reports_committed_memory() {
        let held = vec![1u8; 4 << 20];
        let (now, peak) = mimalloc_commit();
        let now = now.expect("mimalloc's stats JSON has commit_current");
        assert!(now >= 4 << 20, "committed {now} with 4 MiB held");
        assert!(peak.expect("commit_peak") >= now);
        drop(held);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_reads_this_process() {
        let g = Gauges::default();
        sample(&g);
        let s = g.snapshot();
        assert!(s.rss_anon.is_some_and(|v| v > 0), "{s:?}");
        assert!(s.rss_file.is_some(), "{s:?}");
        // AnonHugePages is 0 without THP, but the field is always there.
        assert!(s.anon_huge.is_some(), "{s:?}");
    }

    #[test]
    fn the_sqlite_gauge_rises_after_opening_a_store_and_writing() {
        let store = lodestar_store::Store::open_in_memory(Default::default()).unwrap();
        store
            .connection()
            .execute_batch(
                "CREATE TABLE pad(x BLOB);
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
                 INSERT INTO pad SELECT randomblob(256) FROM n;",
            )
            .unwrap();
        let g = Gauges::default();
        sample(&g);
        let s = g.snapshot();
        // This store alone holds over 500 KB; other tests only add to it.
        assert!(s.sqlite.is_some_and(|v| v >= 500_000), "{s:?}");
        assert!(s.sqlite_highwater >= s.sqlite, "{s:?}");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn other_systems_leave_the_proc_gauges_absent() {
        let g = Gauges::default();
        sample(&g);
        let s = g.snapshot();
        assert_eq!((s.rss_anon, s.rss_file, s.anon_huge), (None, None, None));
        assert!(s.mimalloc_committed.is_some());
    }
}
