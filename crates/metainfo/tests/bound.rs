//! The combined cost of a parsed file tree is bounded whatever the shape:
//! memory and time stay within fixed ceilings at the default limits, and an
//! over-budget tree is refused with the limit error, never a partial `Info`.
//!
//! Memory is the process's peak resident set (`VmHWM`), reset before each
//! parse where Linux allows it. Measuring the process needs no allocator
//! hook and no new dependency for this crate. The peak is absolute (the
//! test binary and the input included), so it can only overstate the parse.
//! Other platforms check time and results only. This binary holds only
//! these tests and runs them one at a time, so their peaks don't mix.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use lodestar_metainfo::{Error, Info, Limits, ParseOptions};

/// One test at a time, so peaks don't mix.
static SERIAL: Mutex<()> = Mutex::new(());

const MIB: usize = 1 << 20;

/// Ceiling on the whole process's peak resident set during any parse at
/// the default limits; see the decisions file for how it fits the 1 GB RSS
/// budget.
const MAX_RSS: usize = 512 * MIB;
/// Generous so the slowest CI runner passes; measured parses take under a
/// second.
const MAX_TIME: Duration = Duration::from_secs(60);

/// The process's peak resident set in bytes, where the platform reports it.
fn peak_rss() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: usize = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// Resets the peak to the current resident set (Linux 4.0+). If that's
/// refused the peak stays the lifetime one, which still bounds this parse.
fn reset_peak_rss() {
    if std::path::Path::new("/proc/self/clear_refs").exists()
        && let Err(e) = std::fs::write("/proc/self/clear_refs", "5")
    {
        eprintln!("peak RSS not reset ({e}); the lifetime peak is checked");
    }
}

/// Held for a whole test, input building included: the peak is the
/// process's, so nothing else may allocate while one is measured.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Parses `raw` and returns the result with the peak resident set and time.
/// The caller holds [`serial`].
fn measure(raw: &[u8], opts: &ParseOptions) -> (Result<Info, Error>, Option<usize>, Duration) {
    reset_peak_rss();
    let start = Instant::now();
    let parsed = Info::parse_with(raw, opts);
    let took = start.elapsed();
    (parsed, peak_rss(), took)
}

fn within_ceilings(what: &str, peak: Option<usize>, took: Duration) {
    eprintln!("{what}: peak RSS {:?} MiB, {took:?}", peak.map(|p| p / MIB));
    if let Some(peak) = peak {
        assert!(
            peak <= MAX_RSS,
            "{what}: peak RSS {} MiB is over {} MiB",
            peak / MIB,
            MAX_RSS / MIB
        );
    }
    assert!(took <= MAX_TIME, "{what}: took {took:?}, over {MAX_TIME:?}");
}

fn bstr(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(s.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(s);
}

/// A zero-length v2 file entry: `<name>d0:d6:lengthi0eee`.
fn v2_file(out: &mut Vec<u8>, name: &[u8]) {
    bstr(out, name);
    out.extend_from_slice(b"d0:d6:lengthi0eee");
}

/// A pure v2 info dict around an already encoded `file tree` value.
fn v2_info(tree: &[u8]) -> Vec<u8> {
    let mut out = b"d9:file tree".to_vec();
    out.extend_from_slice(tree);
    out.extend_from_slice(b"12:meta versioni2e4:name4:tree12:piece lengthi16384ee");
    out
}

/// `depth` nested one-entry directories, each named `dir`, holding `files`
/// zero-length files with six-byte names in the deepest.
fn nested(depth: usize, dir: &[u8], files: usize) -> Vec<u8> {
    let mut tree = Vec::new();
    for _ in 0..depth {
        tree.push(b'd');
        bstr(&mut tree, dir);
    }
    tree.push(b'd');
    for i in 0..files {
        v2_file(&mut tree, format!("{i:06}").as_bytes());
    }
    tree.extend(std::iter::repeat_n(b'e', depth + 1));
    v2_info(&tree)
}

/// A v1 file list whose every path has `segments` one-byte segments.
fn v1_deep_paths(files: usize, segments: usize) -> Vec<u8> {
    let mut out = b"d5:filesl".to_vec();
    for _ in 0..files {
        out.extend_from_slice(b"d6:lengthi0e4:pathl");
        for _ in 0..segments {
            out.extend_from_slice(b"1:a");
        }
        out.extend_from_slice(b"ee");
    }
    out.extend_from_slice(b"e4:name4:tree12:piece lengthi16384e6:pieces0:e");
    out
}

/// A pure v2 tree of `shared.len()` nested shared directories holding 400
/// subdirectories (8-byte names) of 500 zero-length files each, 200,000
/// files in all, with file names padded to `file_name` bytes.
fn wide_tree(shared: &[String], file_name: usize) -> Vec<u8> {
    let mut tree = Vec::new();
    for dir in shared {
        tree.push(b'd');
        bstr(&mut tree, dir.as_bytes());
    }
    tree.push(b'd');
    for sub in 0..400 {
        bstr(&mut tree, format!("sub-{sub:04}").as_bytes());
        tree.push(b'd');
        for file in 0..500 {
            let name = format!("{sub:04}-{file:04}-{:x<w$}", "", w = file_name - 10);
            v2_file(&mut tree, name.as_bytes());
        }
        tree.push(b'e');
    }
    tree.extend(std::iter::repeat_n(b'e', shared.len() + 1));
    v2_info(&tree)
}

/// 200,000 files, 8 segments and 100 bytes per path: six 12-byte shared
/// directories, then an 8-byte subdirectory and a 20-byte file name.
fn realistic_large() -> Vec<u8> {
    let shared: Vec<String> = (0..6).map(|l| format!("directory-{l:02}")).collect();
    wide_tree(&shared, 20)
}

/// The most an accepted tree may cost: 2,000,000 segments (200,000 files of
/// 10) and 32,000,000 path bytes (160 per file), just inside both budgets.
fn full_budget() -> Vec<u8> {
    let shared: Vec<String> = (0..8).map(|l| format!("directory-{l:05}")).collect();
    wide_tree(&shared, 32)
}

fn expect_limit(result: Result<Info, Error>, what: &str) {
    match result {
        Err(Error::LimitExceeded { what: got, .. }) => assert_eq!(got, what),
        Err(e) => panic!("expected the {what:?} limit, got {e}"),
        Ok(info) => panic!(
            "expected the {what:?} limit, parsed {} files",
            info.files.len()
        ),
    }
}

/// What `limits` should make of `files` paths of `segments` segments and
/// `bytes` bytes each: the first budget a file runs out of, if any.
fn expected(limits: &Limits, files: usize, segments: usize, bytes: usize) -> Option<&'static str> {
    // The 1-based file at which each running total first goes over.
    let over_segments = limits.max_total_path_segments / segments + 1;
    let over_bytes = limits.max_total_path_bytes / bytes + 1;
    if over_segments.min(over_bytes) > files {
        None
    } else if over_segments <= over_bytes {
        Some("total path segments")
    } else {
        Some("total path bytes")
    }
}

fn check(what: &str, raw: &[u8], opts: &ParseOptions, files: usize, want: Option<&str>) {
    let (parsed, peak, took) = measure(raw, opts);
    match want {
        Some(limit) => expect_limit(parsed, limit),
        None => assert_eq!(parsed.expect(what).files.len(), files, "{what}"),
    }
    within_ceilings(what, peak, took);
}

#[test]
fn v2_trees_at_the_per_path_limits_parse_or_are_refused() {
    let _serial = serial();
    // Every mix of depth and directory-name length up to the per-path
    // limits, at the file-count limit: each either parses whole or is
    // refused with the budget it ran out of first. The file count and both
    // budgets are a tenth of the defaults, so the sweep stays quick; a
    // refusal stops at the budget, so the trees at the full budget below
    // bound its cost at the defaults.
    let limits = Limits {
        max_files: Limits::DEFAULT.max_files / 10,
        max_total_path_segments: Limits::DEFAULT.max_total_path_segments / 10,
        max_total_path_bytes: Limits::DEFAULT.max_total_path_bytes / 10,
        ..Limits::DEFAULT
    };
    let opts = ParseOptions {
        limits,
        encoding_hint: None,
    };
    let files = limits.max_files;
    for depth in [0, 1, 8, limits.max_path_depth - 1] {
        for dir_len in [1, 16, 63] {
            let raw = nested(depth, &vec![b'a'; dir_len], files);
            let want = expected(&limits, files, depth + 1, depth * dir_len + 6);
            let what = format!("v2 depth {depth}, names {dir_len}");
            check(&what, &raw, &opts, files, want);
        }
    }
}

#[test]
fn v1_lists_at_the_per_path_limits_stay_within_bounds() {
    let _serial = serial();
    // As many paths of each depth as the bencode item budget lets through.
    for segments in [1, 8, Limits::DEFAULT.max_path_depth] {
        let files = (Limits::DEFAULT.max_items - 16) / (segments + 5);
        let files = files.min(Limits::DEFAULT.max_files);
        let raw = v1_deep_paths(files, segments);
        let want = expected(&Limits::DEFAULT, files, segments, segments);
        let opts = ParseOptions::default();
        check(&format!("v1 depth {segments}"), &raw, &opts, files, want);
    }

    // Over a smaller budget the same list is refused, not cut short.
    let raw = v1_deep_paths(28_900, 64);
    let opts = ParseOptions {
        limits: Limits {
            max_total_path_segments: 64 * 10_000,
            ..Limits::DEFAULT
        },
        encoding_hint: None,
    };
    let (parsed, peak, took) = measure(&raw, &opts);
    expect_limit(parsed, "total path segments");
    within_ceilings("v1 over a smaller budget", peak, took);
}

#[test]
fn large_realistic_torrent_parses_within_bounds() {
    let _serial = serial();
    let raw = realistic_large();
    assert!(raw.len() <= Limits::DEFAULT.max_info_bytes);
    let (parsed, peak, took) = measure(&raw, &ParseOptions::default());
    let info = parsed.expect("a large real-world shape parses");
    assert_eq!(info.files.len(), 200_000);
    assert!(info.files.iter().all(|f| f.path.len() == 8));
    assert_eq!(info.path_string(&info.files[0]).len(), 100 + 7);
    within_ceilings("realistic", peak, took);
}

#[test]
fn tree_at_the_full_budget_parses_within_bounds() {
    let _serial = serial();
    let raw = full_budget();
    assert!(raw.len() <= Limits::DEFAULT.max_info_bytes);
    let (parsed, peak, took) = measure(&raw, &ParseOptions::default());
    let info = parsed.expect("a tree just inside the budget parses");
    let segments: usize = info.files.iter().map(|f| f.path.len()).sum();
    let bytes: usize = info.files.iter().flat_map(|f| &f.path).map(Vec::len).sum();
    assert_eq!(segments, Limits::DEFAULT.max_total_path_segments);
    assert!(bytes <= Limits::DEFAULT.max_total_path_bytes);
    assert_eq!(bytes, 32_000_000);
    within_ceilings("full budget", peak, took);
}
