//! CR-0d's restart files (`crates/dht/src/restart.rs`): untrusted bytes on
//! disk. The fuzzer can't forge the container's SHA-256, so the target
//! seals the input itself: the first byte picks the file kind. For a
//! snapshot the next byte picks the form (bit 0: version 2, the spill
//! snapshot of X-split-resume-gate; bit 1, with bit 0: the spill section
//! is expanded from a sparse list), the next 8 bytes give the wall clock
//! for validation, and the rest is the body.
//!
//! An expanded body is `cut` (2 bytes LE) ‖ `n` (2 bytes LE) ‖ `n` bytes
//! of sparse list ‖ a template. The template is cut at `cut % (len + 1)`
//! and the section goes between, as a bencode string. The list's records
//! are a block (3 bytes LE, modulo `SPILL_BLOCKS`), a count byte and up
//! to 4 × count bytes of words, all verbatim; a later record for a block
//! replaces an earlier one, and every other block is a 0 count. A section
//! is at least 128 KiB, past what libFuzzer would grow by itself, so this
//! is how version 2 is reached. `crates/dht/src/restart/tests.rs` builds
//! the seeds in `seeds/dht_restart/` with the same layout.
//!
//! Nothing may panic, whatever decodes is canonical (re-encodes to the
//! same bytes) and has the version it was sealed at (a body never decodes
//! at both), and a decoded snapshot that resumes never seeds a deadline
//! earlier than the one it records, nor a spilled cooldown's end earlier
//! than its record (or the horizon, which its rounded-up minutes may pass).
//! A version 2 snapshot holds a slot, and without its section it's
//! written as version 1 and decodes to the same snapshot.
#![no_main]

use std::collections::BTreeMap;
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use lodestar_dht::restart::{
    self, Decision, Found, Here, Instant, Loaded, Marker, MarkerState, SPILL_BLOCKS,
    SPILL_MINUTE_BITS, Snapshot, Witness, WitnessFound,
};
use sha2::{Digest, Sha256};

fn seal(magic: &[u8; 8], body: &[u8]) -> Vec<u8> {
    seal_at(magic, restart::VERSION, body)
}

fn seal_at(magic: &[u8; 8], version: u32, body: &[u8]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    let sum = Sha256::digest(&out);
    out.extend_from_slice(&sum);
    out
}

fuzz_target!(|data: &[u8]| {
    let Some((&kind, body)) = data.split_first() else {
        return;
    };
    // The raw bytes too, for the container checks themselves.
    let _ = Snapshot::decode(data);
    let _ = Marker::decode(data);
    let _ = Witness::decode(data);
    match kind % 3 {
        0 => {
            let bytes = seal(b"LDSRWITN", body);
            if let Ok(w) = Witness::decode(&bytes) {
                assert_eq!(w.encode(), bytes);
            }
        }
        1 => {
            let bytes = seal(b"LDSRMARK", body);
            if let Ok(m) = Marker::decode(&bytes) {
                assert_eq!(m.encode(), bytes);
            }
        }
        _ => {
            let Some((&form, rest)) = body.split_first() else {
                return;
            };
            let Some((clock, body)) = rest.split_first_chunk::<8>() else {
                return;
            };
            let clock = u64::from_le_bytes(*clock);
            match form % 4 {
                0 | 2 => snapshot(body, restart::VERSION, clock),
                1 => snapshot(body, restart::SNAPSHOT_SPILL_VERSION, clock),
                _ => {
                    if let Some(body) = expand(body) {
                        snapshot(&body, restart::SNAPSHOT_SPILL_VERSION, clock);
                    }
                }
            }
        }
    }
});

/// The body an expanded input stands for (the layout is in the header).
fn expand(input: &[u8]) -> Option<Vec<u8>> {
    let (cut, input) = input.split_first_chunk::<2>()?;
    let (n, input) = input.split_first_chunk::<2>()?;
    let n = usize::from(u16::from_le_bytes(*n)).min(input.len());
    let (mut sparse, template) = input.split_at(n);
    let mut blocks = BTreeMap::new();
    while let Some((b, rest)) = sparse.split_first_chunk::<3>() {
        let Some((&count, rest)) = rest.split_first() else {
            break;
        };
        let block = (u32::from_le_bytes([b[0], b[1], b[2], 0]) as usize) % SPILL_BLOCKS;
        let (words, rest) = rest.split_at((4 * usize::from(count)).min(rest.len()));
        blocks.insert(block, (count, words));
        sparse = rest;
    }
    let mut raw =
        Vec::with_capacity(SPILL_BLOCKS + blocks.values().map(|w| w.1.len()).sum::<usize>());
    for b in 0..SPILL_BLOCKS {
        match blocks.get(&b) {
            Some((count, words)) => {
                raw.push(*count);
                raw.extend_from_slice(words);
            }
            None => raw.push(0),
        }
    }
    let cut = usize::from(u16::from_le_bytes(*cut)) % (template.len() + 1);
    let mut body = template[..cut].to_vec();
    body.extend_from_slice(format!("{}:", raw.len()).as_bytes());
    body.extend_from_slice(&raw);
    body.extend_from_slice(&template[cut..]);
    Some(body)
}

/// `clock`: how far after the snapshot `wall0` is, in nanoseconds,
/// including sub-millisecond offsets.
fn snapshot(body: &[u8], version: u32, clock: u64) {
    let bytes = seal_at(b"LDSRSNAP", version, body);
    let Ok((s, sum)) = Snapshot::decode(&bytes) else {
        return;
    };
    assert_eq!(s.encode(), bytes);
    // Each version has its own dictionary keys, so no body decodes at both.
    let other = if version == restart::VERSION {
        restart::SNAPSHOT_SPILL_VERSION
    } else {
        restart::VERSION
    };
    assert!(Snapshot::decode(&seal_at(b"LDSRSNAP", other, body)).is_err());
    match &s.spill {
        None => assert_eq!(version, restart::VERSION),
        Some(sp) => {
            assert_eq!(version, restart::SNAPSHOT_SPILL_VERSION);
            // Never empty: a snapshot with nothing spilled is version 1.
            assert!(!words(&sp.raw).is_empty());
            // Without its section it's written as version 1, and the rest
            // is unchanged.
            let v1 = Snapshot {
                run: s.run,
                at_ms: s.at_ms,
                horizon_s: s.horizon_s,
                gate_s: s.gate_s,
                entries: s.entries.clone(),
                spill: None,
            };
            let bytes = v1.encode();
            assert_eq!(&bytes[8..12], &restart::VERSION.to_le_bytes());
            assert_eq!(Snapshot::decode(&bytes).map(|d| d.0), Ok(v1));
        }
    }
    // A marker and witness that agree with it, so validation reaches the
    // clock rules and seeding.
    let loaded = Loaded {
        marker: Found::Ok(Marker {
            state: MarkerState::Clean {
                snap: sum,
                state_bin: [0; 32],
            },
            run: s.run,
            dir_path: restart::path_digest(""),
            at_ms: s.at_ms,
        }),
        snapshot: Found::Ok((s.clone(), sum)),
        witness: WitnessFound::Ok(Witness {
            generation: s.run.generation,
            dir_id: s.run.dir_id,
            at_ms: 0,
            path: String::new(),
        }),
        state_bin: Some([0; 32]),
    };
    let ahead_ns = clock % ((u64::from(s.horizon_s) * 1000 + 2) * 1_000_000);
    let wall0 = Duration::from_millis(s.at_ms) + Duration::from_nanos(ahead_ns);
    let here = Here {
        path: String::new(),
        dir_path: restart::path_digest(""),
        ids: s.run.ids,
        horizon_s: s.horizon_s,
        wall0,
        mono0: Instant::now(),
    };
    if let Decision::Resume(seeded) = restart::plan(loaded, &here).decision {
        for (addr, until) in &seeded.entries {
            let e = s
                .entries
                .iter()
                .find(|e| e.addr == *addr)
                .expect("seeded from the file");
            let recorded = Duration::from_millis(s.at_ms + u64::from(e.remaining_s) * 1000);
            assert!(wall0 + until.duration_since(here.mono0) >= recorded);
        }
        if let Some(g) = seeded.gate_until {
            let recorded = Duration::from_millis(s.at_ms + u64::from(s.gate_s) * 1000);
            assert!(wall0 + g.duration_since(here.mono0) >= recorded);
        }
        assert_eq!(seeded.spill.is_some(), s.spill.is_some());
        if let (Some(sp), Some(file)) = (&seeded.spill, &s.spill) {
            // The slots still cooling, in order, each with its block and
            // tag, never ending before its record (capped at the horizon)
            // and never more than the horizon away.
            let now = restart::floor_ms(wall0);
            let h_ms = u64::from(s.horizon_s) * 1000;
            let mask = (1u32 << SPILL_MINUTE_BITS) - 1;
            let live: Vec<_> = words(&file.raw)
                .into_iter()
                .map(|(b, w)| (b, w, s.at_ms + u64::from(w & mask) * 60_000))
                .filter(|&(_, _, recorded)| recorded > now)
                .collect();
            let mut n = 0;
            for ((block, tag, end), (b, w, recorded)) in sp.slots().zip(&live) {
                assert_eq!((block, tag), (*b, w & !mask));
                let left = end.duration_since(here.mono0);
                assert!(left <= Duration::from_millis(h_ms));
                let floor = Duration::from_millis((*recorded).min(s.at_ms + h_ms));
                assert!(wall0 + left >= floor);
                n += 1;
            }
            assert_eq!(n, live.len());
            assert_eq!(sp.slots().count(), live.len());
        }
    }
}

/// A checked section's slots, as (block, word). Its own reader on purpose
/// (`Spilled::words` is crate-private), so the check doesn't trust it.
fn words(raw: &[u8]) -> Vec<(usize, u32)> {
    let mut out = Vec::new();
    let mut at = 0;
    for b in 0..SPILL_BLOCKS {
        let n = usize::from(raw[at]);
        for w in raw[at + 1..at + 1 + 4 * n].chunks_exact(4) {
            out.push((b, u32::from_be_bytes(w.try_into().unwrap())));
        }
        at += 1 + 4 * n;
    }
    out
}
