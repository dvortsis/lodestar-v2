//! CR-0d's restart files (`crates/dht/src/restart.rs`): untrusted bytes on
//! disk. Nothing may panic, whatever decodes is canonical (re-encodes to
//! the same bytes), and validation plus seeding of a decoded snapshot is
//! total for any wall-clock reading.
#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use lodestar_dht::restart::{
    self, Found, Here, Loaded, Marker, MarkerState, Snapshot, Witness, WitnessFound,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(w) = Witness::decode(data) {
        assert_eq!(w.encode(), data);
    }
    if let Ok(m) = Marker::decode(data) {
        assert_eq!(m.encode(), data);
    }
    let Ok((s, sum)) = Snapshot::decode(data) else {
        return;
    };
    assert_eq!(s.encode(), data);
    // A marker and witness that agree with it, so validation reaches the
    // clock rules and seeding.
    let marker = Marker {
        state: MarkerState::Clean {
            snap: sum,
            state_bin: [0; 32],
        },
        run: s.run,
        dir_path: [0; 32],
        at_ms: s.at_ms,
    };
    let witness = Witness {
        generation: s.run.generation,
        dir_id: s.run.dir_id,
        at_ms: 0,
        path: String::new(),
    };
    let loaded = Loaded {
        marker: Found::Ok(marker),
        snapshot: Found::Ok((s.clone(), sum)),
        witness: WitnessFound::Ok(witness),
        state_bin: Some([0; 32]),
    };
    let seed = data.iter().fold(0u64, |a, &b| a.rotate_left(5) ^ u64::from(b));
    let here = Here {
        path: String::new(),
        dir_path: [0; 32],
        ids: s.run.ids,
        horizon_s: s.horizon_s,
        wall0: Duration::from_millis(s.at_ms.saturating_add(seed % 30_000_000)),
        mono0: Instant::now(),
    };
    let _ = restart::plan(&loaded, &here);
});
