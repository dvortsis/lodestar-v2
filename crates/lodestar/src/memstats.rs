//! Memory attribution gauges (X-mem-gauges): which heap the process's
//! anonymous memory is in, so a soak's memory verdict can say where a ramp
//! is.
//!
//! - **mimalloc**, the Rust global allocator, and in musl builds the C
//!   library's `malloc` too (below): its own committed bytes, from
//!   `mi_stats_get_json` through the `mimalloc` crate's safe
//!   `MiMalloc::stats_json` (`extended` feature).
//! - **SQLite**, which counts its own heap: `sqlite3_memory_used` and its
//!   high-water mark, through `lodestar_store::memstats`.
//! - **The kernel's view** (Linux only): `RssAnon` and `RssFile` from
//!   `/proc/self/status`, `AnonHugePages` from `/proc/self/smaps_rollup`,
//!   and the anonymous mappings outside the allocator's arenas by size
//!   class from `/proc/self/maps` (X-soak-screen: soak r4's growth was in
//!   128-512 KiB ones; [`anon_mappings`]).
//!
//! Where C code (SQLite, zstd) allocates depends on the build
//! (`docs/metrics.md`, "Memory attribution"):
//! - **musl builds** (the release builds: the Docker image and the
//!   tarballs): mimalloc's `override` feature makes it the C library's
//!   `malloc` as well (X-mimalloc-override, `Cargo.toml`). Its committed
//!   bytes then include C allocations, and SQLite's count is part of them,
//!   not beside them. `RssAnon` minus mimalloc's committed bytes
//!   approximates the rest.
//! - **Every other build** (Windows, glibc): C code allocates through the C
//!   library's own `malloc`, which mimalloc doesn't count. `RssAnon` minus
//!   mimalloc's committed bytes and SQLite's own approximates the rest.
//!
//! The analyzer reports that rest as "other heap", choosing the formula by
//! the build target the collector records. It's neither an upper nor a
//! lower bound:
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
//! parses it: about 0.2 ms, measured in the decision. `status` is a small
//! text file; `maps` is one line per mapping, read line by line, and costs
//! what `docs/decisions/2026-11-07-X-soak-screen.md` measures at about
//! 3,000 mappings; `smaps_rollup` walks the process's mappings in
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
    anon_mappings: [AtomicU64; MAPPING_CLASSES.len()],
}

/// The size classes of the anonymous-mapping gauge, by the kernel's own
/// mapping size: each class's upper bound (exclusive) and label, the last
/// unbounded. The same classes as `bench/examples/mem-cheap.rs`, with its
/// two largest joined.
pub const MAPPING_CLASSES: [(u64, &str); 4] = [
    (128 << 10, "lt_128k"),
    (512 << 10, "128k_512k"),
    (2 << 20, "512k_2m"),
    (u64::MAX, "ge_2m"),
];

/// A run of adjacent anonymous mappings at least this big is taken to be
/// an allocator arena (mimalloc reserves 1 GiB at a time) and isn't
/// counted, as `mem-cheap` decides it.
const ARENA_MIN: u64 = 256 << 20;

/// More lines than this and the read gives up (absent): the kernel's
/// default `vm.max_map_count` is 65,530, so this bounds the sample's cost.
const MAX_MAPPINGS: usize = 1 << 17;

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
            anon_mappings: std::array::from_fn(|_| AtomicU64::new(ABSENT)),
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
    /// Anonymous mappings outside the arenas, by [`MAPPING_CLASSES`].
    pub anon_mappings: Option<[u64; MAPPING_CLASSES.len()]>,
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
            anon_mappings: {
                let v: Vec<u64> = self.anon_mappings.iter().filter_map(get).collect();
                v.try_into().ok()
            },
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
        for (i, gauge) in self.anon_mappings.iter().enumerate() {
            let v = s.anon_mappings.map(|m| m[i].min(ABSENT - 1));
            gauge.store(v.unwrap_or(ABSENT), Ordering::Relaxed);
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
    #[cfg(target_os = "linux")]
    let anon_mappings = std::fs::File::open("/proc/self/maps")
        .ok()
        .and_then(|f| anon_mappings(std::io::BufReader::with_capacity(64 << 10, f)));
    #[cfg(not(target_os = "linux"))]
    let anon_mappings = None;
    Snapshot {
        mimalloc_committed,
        mimalloc_peak_committed,
        sqlite: lodestar_store::memstats::sqlite_memory_used(),
        sqlite_highwater: lodestar_store::memstats::sqlite_memory_highwater(),
        rss_anon,
        rss_file,
        anon_huge,
        anon_mappings,
    }
}

/// Anonymous mappings outside allocator arenas, by [`MAPPING_CLASSES`],
/// from `/proc/<pid>/maps` text.
///
/// A mapping is anonymous when its inode is 0 and it has no path (so not
/// `[heap]`, `[stack]` or a named one). Adjacent anonymous mappings form a
/// run, whatever their permissions; a run spanning [`ARENA_MIN`] or more is
/// an arena and none of its mappings count. Of the rest, only mappings
/// that can be read, written or run count: a `---p` one is a guard page or
/// reserved address space, not memory. Read one line at a time, so the
/// cost is one line buffer whatever the count. `None` (absent) for any
/// line that doesn't parse, a read error, or more than [`MAX_MAPPINGS`]
/// lines: an odd file is absent, never a partial count.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn anon_mappings(maps: impl std::io::BufRead) -> Option<[u64; MAPPING_CLASSES.len()]> {
    let mut total = [0u64; MAPPING_CLASSES.len()];
    let mut run = total;
    let mut span: Option<(u64, u64)> = None;
    let mut flush = |run: &mut [u64; 4], span: Option<(u64, u64)>| {
        if span.is_some_and(|(a, b)| b - a < ARENA_MIN) {
            for (t, r) in total.iter_mut().zip(run.iter()) {
                *t += r;
            }
        }
        *run = [0; 4];
    };
    for (n, line) in maps.lines().enumerate() {
        let line = line.ok()?;
        if n >= MAX_MAPPINGS {
            return None;
        }
        let mut f = line.split_ascii_whitespace();
        let (Some(range), Some(perms), Some(_), Some(_), Some(inode)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            return None;
        };
        let (a, b) = range.split_once('-')?;
        let (start, end) = (
            u64::from_str_radix(a, 16).ok()?,
            u64::from_str_radix(b, 16).ok()?,
        );
        if end <= start || perms.len() != 4 || !inode.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if inode != "0" || f.next().is_some() {
            continue;
        }
        match span {
            Some((first, last)) if last == start => span = Some((first, end)),
            _ => {
                flush(&mut run, span);
                span = Some((start, end));
            }
        }
        if !perms.starts_with("---") {
            let size = end - start;
            let class = MAPPING_CLASSES
                .iter()
                .position(|(max, _)| size < *max)
                .unwrap_or(MAPPING_CLASSES.len() - 1);
            run[class] += 1;
        }
    }
    flush(&mut run, span);
    Some(total)
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
            anon_mappings: Some([0, 7, u64::MAX, 2]),
        };
        g.store(full);
        let got = g.snapshot();
        assert_eq!(got.rss_file, Some(0), "0 is a value, not absent");
        assert_eq!(got.anon_huge, Some(u64::MAX - 1));
        assert_eq!(got.anon_mappings, Some([0, 7, u64::MAX - 1, 2]));
        g.store(Snapshot {
            rss_anon: None,
            ..full
        });
        assert_eq!(g.snapshot().rss_anon, None);
        assert_eq!(g.snapshot().mimalloc_committed, Some(1));
        g.store(Snapshot {
            anon_mappings: None,
            ..full
        });
        assert_eq!(g.snapshot().anon_mappings, None);
    }

    /// One `/proc/<pid>/maps` line: `start`, `len` in KiB, perms, inode, path.
    fn map(start: u64, kib: u64, perms: &str, inode: u64, path: &str) -> String {
        format!(
            "{start:x}-{:x} {perms} 00000000 00:00 {inode}    {path}\n",
            start + (kib << 10)
        )
    }

    fn mappings(lines: &[String]) -> Option<[u64; 4]> {
        anon_mappings(lines.concat().as_bytes())
    }

    #[test]
    fn anonymous_mappings_count_by_size_class() {
        let g = 1 << 40;
        let lines = [
            // Each class's edges: just below, at (the next class), just above.
            map(g, 124, "rw-p", 0, ""),
            map(g + (1 << 30), 128, "rw-p", 0, ""),
            map(g + (2 << 30), 508, "rw-p", 0, ""),
            map(g + (3 << 30), 512, "rw-p", 0, ""),
            map(g + (4 << 30), 2044, "r--p", 0, ""),
            map(g + (5 << 30), 2048, "rw-p", 0, ""),
            // Not anonymous: a path, a pseudo-path, an inode.
            map(g + (6 << 30), 256, "rw-p", 0, "/data/index/x.idx"),
            map(g + (7 << 30), 256, "rw-p", 0, "[heap]"),
            map(g + (8 << 30), 256, "rw-p", 41, ""),
            // A guard page or reserved space isn't memory.
            map(g + (9 << 30), 256, "---p", 0, ""),
        ];
        assert_eq!(mappings(&lines), Some([1, 2, 2, 1]));
        assert_eq!(mappings(&[]), Some([0; 4]));
    }

    #[test]
    fn arena_runs_are_left_out_and_small_runs_are_not() {
        let g = 1 << 40;
        // A 1 GiB arena split by permissions, then a 256 KiB mapping right
        // after it (part of the run, so left out), then a gap.
        let arena = [
            map(g, 64 << 10, "rw-p", 0, ""),
            map(g + (64 << 20), 256, "rw-p", 0, ""),
            map(
                g + (64 << 20) + (256 << 10),
                (1 << 20) - (64 << 10) - 256,
                "---p",
                0,
                "",
            ),
        ];
        assert_eq!(mappings(&arena), Some([0; 4]));
        // Adjacent small mappings under the arena size still count, each
        // in its own class (as r4 counted the kernel's mappings).
        let small = [
            map(g, 256, "rw-p", 0, ""),
            map(g + (256 << 10), 256, "rw-p", 0, ""),
            map(g + (512 << 10), 64, "rw-p", 0, ""),
        ];
        assert_eq!(mappings(&small), Some([1, 2, 0, 0]));
        // Just under the arena size: counted.
        let under = [
            map(g, (256 << 10) - 256, "rw-p", 0, ""),
            map(g + (256 << 20) - (256 << 10), 128, "rw-p", 0, ""),
        ];
        assert_eq!(mappings(&under), Some([0, 1, 0, 1]));
        // A file mapping between two anonymous ones breaks the run.
        let split = [
            map(g, 200 << 10, "rw-p", 0, ""),
            map(g + (200 << 20), 4, "r--p", 7, "/lib/x.so"),
            map(g + (200 << 20) + (4 << 10), 200 << 10, "rw-p", 0, ""),
        ];
        assert_eq!(mappings(&split), Some([0, 0, 0, 2]));
    }

    #[test]
    fn an_odd_maps_line_makes_the_gauge_absent() {
        let good = map(1 << 40, 256, "rw-p", 0, "");
        for bad in [
            "zz-10 rw-p 00000000 00:00 0\n",
            "10-zz rw-p 00000000 00:00 0\n",
            "20-10 rw-p 00000000 00:00 0\n",
            "10-20 rw- 00000000 00:00 0\n",
            "10-20 rw-p 00000000 00:00\n",
            "10-20 rw-p 00000000 00:00 x\n",
            "garbage\n",
        ] {
            assert_eq!(mappings(&[good.clone(), bad.into()]), None, "{bad:?}");
        }
        let many = "1000-2000 rw-p 00000000 00:00 0\n".repeat(MAX_MAPPINGS + 1);
        assert_eq!(anon_mappings(many.as_bytes()), None);
    }

    proptest! {
        #[test]
        fn anon_mappings_never_panics(text in "\\PC{0,400}") {
            let _ = anon_mappings(text.as_bytes());
        }
    }

    /// The sample's cost at about 3,000 mappings (X-soak-screen): 750
    /// parked threads with small stacks, each its stack, guard page and
    /// thread-local mappings.
    /// Ignored in the workspace run; the soak-screen job runs it.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "spawns 750 threads; run by the soak-screen CI job"]
    fn anon_mappings_cost_at_3000_mappings() {
        use std::sync::{Arc, Barrier};
        let threads = 750;
        let barrier = Arc::new(Barrier::new(threads + 1));
        let ready = Arc::new(Barrier::new(threads + 1));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (b, r) = (barrier.clone(), ready.clone());
                std::thread::Builder::new()
                    .stack_size(64 << 10)
                    .spawn(move || {
                        r.wait();
                        b.wait();
                    })
                    .expect("spawn")
            })
            .collect();
        ready.wait();
        let lines = std::fs::read_to_string("/proc/self/maps")
            .unwrap()
            .lines()
            .count();
        let mut took: Vec<std::time::Duration> = (0..51)
            .map(|_| {
                let started = std::time::Instant::now();
                let f = std::fs::File::open("/proc/self/maps").unwrap();
                let counts = anon_mappings(std::io::BufReader::with_capacity(64 << 10, f));
                assert!(counts.is_some());
                started.elapsed()
            })
            .collect();
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }
        took.sort();
        let median = took[took.len() / 2];
        println!(
            "SCREEN-COST anon_mappings: {lines} mappings, median {:.0} us, max {:.0} us per read",
            median.as_secs_f64() * 1e6,
            took[took.len() - 1].as_secs_f64() * 1e6
        );
        assert!(lines >= 2_500, "only {lines} mappings");
        // Generous: a 60 s sampler can afford far more; this catches a
        // quadratic read, not a slow runner.
        assert!(median.as_millis() < 100, "median {median:?}");
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
        // Thread stacks alone are anonymous mappings outside any arena.
        assert!(
            s.anon_mappings.is_some_and(|m| m.iter().sum::<u64>() > 0),
            "{s:?}"
        );
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
        assert_eq!(s.anon_mappings, None);
        assert!(s.mimalloc_committed.is_some());
    }
}
