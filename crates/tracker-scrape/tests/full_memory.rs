//! A 50 MB full-scrape reply parses with flat memory (SRC-12 gate).
//!
//! The reply is generated as it is fed, 64 KiB at a time, so neither the
//! test nor the parser ever holds it whole. Two measures, since the
//! workspace forbids `unsafe` and so a counting allocator:
//!
//! - **The parser's own heap** ([`Parser::heap_bytes`]) after every chunk
//!   must equal what it held before the first: nothing it keeps grows with
//!   the reply. This holds on every platform and is exact.
//! - **The process's resident memory** (Linux, `/proc/self/status`) is
//!   sampled after every chunk; its growth over the parse must stay under
//!   [`RSS_BOUND`]. A parser that buffered the reply or kept every key
//!   would add tens of megabytes.
//!
//! This file holds one test, so no other test's allocations land in the
//! process count.

use lodestar_metainfo::InfoHash;
use lodestar_tracker_scrape::full::{LIMITS, Parser};

const TARGET: usize = 50 * 1000 * 1000;
const CHUNK: usize = 64 * 1024;
/// Resident-memory growth allowed over the parse: the chunk buffer and
/// allocator slack. A buffered reply would be 50 MB; the keys alone
/// (about 640,000 of them) 12.8 MB.
const RSS_BOUND: u64 = 4 * 1024 * 1024;

/// The hash of entry `i`: distinct, and recomputable for the check.
fn hash(i: u64) -> [u8; 20] {
    let mut h = [0u8; 20];
    let x = i.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for (j, b) in h.iter_mut().enumerate() {
        *b = (x >> ((j % 8) * 8)) as u8 ^ j as u8;
    }
    h
}

fn word(h: &[u8]) -> u64 {
    u64::from_le_bytes(h[..8].try_into().unwrap())
}

/// Writes entry `i` into `out`. Every 97th is malformed (a 19-byte key),
/// so the skip path runs at scale too.
fn write_entry(i: u64, out: &mut Vec<u8>) {
    if i.is_multiple_of(97) {
        out.extend_from_slice(b"19:");
        out.extend_from_slice(&hash(i)[..19]);
    } else {
        out.extend_from_slice(b"20:");
        out.extend_from_slice(&hash(i));
    }
    let s = (i % 5000) as u32;
    out.extend_from_slice(
        format!(
            "d8:completei{s}e10:downloadedi{}e10:incompletei{}ee",
            s * 3,
            s / 2
        )
        .as_bytes(),
    );
}

/// Resident bytes, where the platform tells us (Linux's `VmRSS`, in kB,
/// whatever the page size).
fn rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[test]
fn a_50_mb_reply_parses_in_flat_memory() {
    let mut parser = Parser::new(LIMITS);
    let heap = parser.heap_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK + 256);
    let (mut keys, mut sum) = (0u64, 0u64);
    let mut emit = |b: &[InfoHash]| {
        keys += b.len() as u64;
        // A checksum over every emitted hash, in order, to compare with
        // the generator's without storing either list.
        sum = b
            .iter()
            .fold(sum, |s, h| s.wrapping_mul(31).wrapping_add(word(&h.0)));
    };

    let base = rss();
    let mut peak = base;
    let (mut fed, mut entries) = (0usize, 0u64);
    buf.extend_from_slice(b"d5:filesd");
    while fed + buf.len() < TARGET {
        write_entry(entries, &mut buf);
        entries += 1;
        if buf.len() >= CHUNK {
            parser.feed(&buf, &mut emit).unwrap();
            fed += buf.len();
            buf.clear();
            assert_eq!(parser.heap_bytes(), heap, "the parser's heap grew");
            peak = peak.max(rss());
        }
    }
    buf.extend_from_slice(b"ee");
    fed += buf.len();
    parser.feed(&buf, &mut emit).unwrap();
    assert_eq!(parser.heap_bytes(), heap, "the parser's heap grew");
    let summary = parser.finish(&mut emit).unwrap();

    assert!(fed >= TARGET, "generated only {fed} bytes");
    assert_eq!(summary.bytes, fed as u64);
    let malformed = entries.div_ceil(97);
    assert_eq!(summary.malformed, malformed);
    assert_eq!(summary.keys, entries - malformed);
    assert_eq!(keys, summary.keys);
    let want = (0..entries)
        .filter(|i| !i.is_multiple_of(97))
        .fold(0u64, |s, i| s.wrapping_mul(31).wrapping_add(word(&hash(i))));
    assert_eq!(
        sum, want,
        "the emitted hashes differ from the generated ones"
    );

    if let (Some(base), Some(peak)) = (base, peak) {
        let growth = peak.saturating_sub(base);
        assert!(
            growth <= RSS_BOUND,
            "resident memory grew by {growth} bytes parsing {fed} bytes (bound {RSS_BOUND})"
        );
    }
}
