//! The replicated have-filter's wire format (WK-3, architecture §19.3):
//! the snapshot header, deltas and chunk assembly. Arbitrary bytes never
//! panic; an unknown version or a wrong length is refused; whatever decodes
//! encodes back to the same bytes; and an assembly completes only from
//! every chunk in order, each of its exact length.
//!
//! The first byte picks the case: 0 a header, 1 a delta, anything else an
//! assembly, whose header (70 bytes, version forced to 1) is followed by
//! chunks given as a sequence byte (its top bit: another transfer's chunk)
//! and a length byte (odd: the exact length; even: in 32 KiB steps).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_cluster::have::{
    Assembly, HAVE_V1, HEADER_V1_LEN, HaveDelta, HaveHeader, chunk_range,
};
use lodestar_cluster::proto::{SnapshotChunk, SnapshotKind};
use lodestar_metainfo::InfoHash;
use lodestar_pipeline::Bloom;

/// Keeps a run fast: filters up to 2 MiB are assembled for real.
const MAX_FUZZ_BITS: u64 = 1 << 24;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    match pick {
        0 => {
            if let Ok(h) = HaveHeader::decode(rest) {
                assert_eq!(rest.len(), HEADER_V1_LEN);
                assert_eq!(h.encode(), rest);
                assert!(h.check().is_ok());
            }
        }
        1 => {
            if let Ok(d) = HaveDelta::decode(rest) {
                assert_eq!(d.encode().as_deref(), Some(rest));
            }
        }
        _ => assemble(rest),
    }
});

fn assemble(data: &[u8]) {
    if data.len() < HEADER_V1_LEN {
        return;
    }
    let mut head = data[..HEADER_V1_LEN].to_vec();
    head[0] = HAVE_V1;
    let Ok(h) = HaveHeader::decode(&head) else {
        return;
    };
    if h.bits > MAX_FUZZ_BITS {
        return;
    }
    let bloom = Bloom::for_header(&h);
    let mut a = Assembly::new(h);
    let mut expected = 0u32;
    for pair in data[HEADER_V1_LEN..].chunks_exact(2) {
        let seq = u32::from(pair[0] & 0x7F);
        let other = pair[0] & 0x80 != 0;
        let exact = chunk_range(h.bits, seq).len();
        let len = if pair[1] % 2 == 1 {
            exact
        } else {
            usize::from(pair[1]) * 64 * 1024 / 2
        };
        let c = SnapshotChunk {
            kind: SnapshotKind::HaveFilter,
            version: if other { h.serial.wrapping_add(1) } else { h.serial },
            seq,
            total: h.chunks,
            bytes: vec![pair[1]; len],
        };
        let owned = a.owns(&c);
        assert_eq!(owned, !other);
        match a.accept(&c) {
            Ok(p) => {
                assert!(owned);
                assert_eq!(seq, expected);
                assert_eq!(p.bytes.len(), exact);
                assert_eq!(p.offset, chunk_range(h.bits, seq).start);
                bloom.write_bytes(p.offset, p.bytes);
                expected += 1;
                assert_eq!(p.last, expected == h.chunks);
            }
            Err(_) => {
                assert!(!owned || seq != expected || len != exact || expected == h.chunks);
                // A chunk of another transfer is skipped; anything else
                // ends the assembly, as the worker drops it.
                if !owned {
                    continue;
                }
                break;
            }
        }
    }
    assert_eq!(a.is_complete(), expected == h.chunks);
    if a.is_complete() {
        let _ = bloom.contains(&InfoHash([pick_key(data); 20]));
    }
}

fn pick_key(data: &[u8]) -> u8 {
    data.last().copied().unwrap_or(0)
}
