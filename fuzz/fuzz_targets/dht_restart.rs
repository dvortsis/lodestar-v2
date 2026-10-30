//! CR-0d's restart files (`crates/dht/src/restart.rs`): untrusted bytes on
//! disk. The fuzzer can't forge the container's SHA-256, so the target
//! seals the input itself: the first byte picks the file kind, a snapshot
//! takes the next 8 bytes as the wall-clock reading for validation, and
//! the rest is the body. Nothing may panic, whatever decodes is canonical
//! (re-encodes to the same bytes), and a decoded snapshot that resumes
//! never seeds a deadline earlier than the one it records.
#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use lodestar_dht::restart::{
    self, Decision, Found, Here, Loaded, Marker, MarkerState, Snapshot, Witness, WitnessFound,
};
use sha2::{Digest, Sha256};

fn seal(magic: &[u8; 8], body: &[u8]) -> Vec<u8> {
    let mut out = magic.to_vec();
    out.extend_from_slice(&restart::VERSION.to_le_bytes());
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
            let Some((clock, body)) = body.split_first_chunk::<8>() else {
                return;
            };
            snapshot(&seal(b"LDSRSNAP", body), u64::from_le_bytes(*clock));
        }
    }
});

/// `clock`: how far after the snapshot `wall0` is, in nanoseconds,
/// including sub-millisecond offsets.
fn snapshot(bytes: &[u8], clock: u64) {
    let Ok((s, sum)) = Snapshot::decode(bytes) else {
        return;
    };
    assert_eq!(s.encode(), bytes);
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
    if let Decision::Resume(seeded) = restart::plan(&loaded, &here).decision {
        for (addr, until) in &seeded.entries {
            let e = s.entries.iter().find(|e| e.addr == *addr).expect("seeded from the file");
            let recorded = Duration::from_millis(s.at_ms + u64::from(e.remaining_s) * 1000);
            assert!(wall0 + until.duration_since(here.mono0) >= recorded);
        }
        if let Some(g) = seeded.gate_until {
            let recorded = Duration::from_millis(s.at_ms + u64::from(s.gate_s) * 1000);
            assert!(wall0 + g.duration_since(here.mono0) >= recorded);
        }
    }
}
