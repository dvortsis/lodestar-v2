//! What a remote worker judges with (WK-8, architecture §19.4, §20): the
//! filter snapshot, the dictionary snapshot and their chunk assembly, all
//! decoded from bytes the coordinator sent. Arbitrary bytes never panic,
//! never decode past the caps, and whatever decodes is canonical (it
//! re-encodes to the same bytes).
//!
//! The first byte picks the case: 0 the filter snapshot, 1 the dictionary,
//! 2 a chunk sequence (each chunk a header byte, its sequence number, and a
//! length byte, with the payload from the rest; a chunk is padded to the
//! full chunk size unless the header's top bit marks it the last, so a
//! transfer can complete and its payload is decoded). Run it with a small
//! allocation limit to check the caps hold:
//! `cargo +nightly fuzz run cluster_filter_snapshot -- -malloc_limit_mb=128`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_cluster::proto::{MAX_CHUNK, SnapshotChunk, SnapshotKind};
use lodestar_pipeline::wire::filter_snapshot::{
    DictSnapshot, FilterSnapshot, Incoming, MAX_DICT, MAX_SNAPSHOT, Step,
};

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    match pick % 3 {
        0 => {
            if let Ok(s) = FilterSnapshot::decode(body) {
                assert!(body.len() <= MAX_SNAPSHOT);
                assert!(s.filter.state.held.is_empty());
                assert_eq!(s.encode().expect("a decoded snapshot encodes"), body);
                // Whatever decodes builds an engine without panicking.
                let _ = s.engine();
            }
        }
        1 => {
            if let Ok(d) = DictSnapshot::decode(body) {
                assert!(d.bytes.len() <= MAX_DICT && d.id >= 1);
                assert_eq!(d.encode().expect("a decoded dictionary encodes"), body);
            }
        }
        _ => {
            // Chunks are padded to `MAX_CHUNK` unless the header's top bit
            // marks the last, so a multi-chunk transfer can complete, and
            // what completes is decoded.
            let mut inc = Incoming::new(SnapshotKind::Filter);
            let mut rest = body;
            while let [head, seq, len, tail @ ..] = rest {
                let n = usize::from(*len).min(tail.len());
                let (bytes, after) = tail.split_at(n);
                rest = after;
                let mut bytes = bytes.to_vec();
                if head & 0x80 == 0 {
                    bytes.resize(MAX_CHUNK, 0);
                }
                let c = SnapshotChunk {
                    kind: if head & 0x40 == 0 {
                        SnapshotKind::Filter
                    } else {
                        SnapshotKind::Dictionary
                    },
                    version: u64::from(head & 0x03),
                    seq: u32::from(*seq),
                    total: u32::from((head >> 2) & 0x07),
                    bytes,
                };
                match inc.take(c) {
                    Ok(Step::Done(_, done)) => {
                        assert!(done.bytes.len() <= MAX_SNAPSHOT);
                        let _ = done.filter();
                    }
                    Ok(Step::Ack(a)) => assert_eq!(a.kind, SnapshotKind::Filter),
                    Err(_) => {}
                }
            }
        }
    }
});
