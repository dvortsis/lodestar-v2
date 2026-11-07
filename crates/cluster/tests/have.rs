//! The have-filter's wire format (WK-3): property tests over the header,
//! deltas and chunk assembly.

use lodestar_cluster::have::{
    Assembly, HEADER_V1_LEN, HaveDelta, HaveError, HaveHeader, HaveKey, MAX_DELTA_KEYS,
    MAX_FILTER_BITS, chunk_range, chunks_for,
};
use lodestar_cluster::proto::{MAX_CHUNK, SnapshotChunk, SnapshotKind};
use proptest::prelude::*;

fn header() -> impl Strategy<Value = HaveHeader> {
    (
        any::<u64>(),
        1u64..,
        any::<u64>(),
        any::<[u8; 16]>(),
        1u64..=MAX_FILTER_BITS / 64,
        1u8..=16,
        any::<u64>(),
    )
        .prop_flat_map(
            |(generation, serial, next_delta, key, words, hashes, count)| {
                let bits = words * 64;
                (1..=bits).prop_map(move |capacity| HaveHeader {
                    generation,
                    serial,
                    next_delta,
                    key: HaveKey::from_bytes(key),
                    bits,
                    hashes,
                    capacity,
                    count,
                    chunks: chunks_for(bits),
                })
            },
        )
}

fn chunk(h: &HaveHeader, seq: u32) -> SnapshotChunk {
    SnapshotChunk {
        kind: SnapshotKind::HaveFilter,
        version: h.serial,
        seq,
        total: h.chunks,
        bytes: vec![0xFF; chunk_range(h.bits, seq).len()],
    }
}

proptest! {
    #[test]
    fn headers_round_trip(h in header()) {
        let b = h.encode();
        prop_assert_eq!(b.len(), HEADER_V1_LEN);
        prop_assert_eq!(HaveHeader::decode(&b), Ok(h));
    }

    /// Whatever the bytes, decoding returns; what decodes encodes back to
    /// the same bytes.
    #[test]
    fn any_bytes_decode_safely(b in proptest::collection::vec(any::<u8>(), 0..200)) {
        if let Ok(h) = HaveHeader::decode(&b) {
            prop_assert_eq!(h.encode(), b.clone());
        }
        if let Ok(d) = HaveDelta::decode(&b) {
            prop_assert_eq!(d.encode(), Some(b));
        }
    }

    #[test]
    fn deltas_round_trip(
        generation in any::<u64>(),
        seq in any::<u64>(),
        keys in proptest::collection::vec(any::<[u8; 20]>(), 1..300),
    ) {
        let d = HaveDelta { generation, seq, keys };
        let b = d.encode().unwrap();
        prop_assert_eq!(HaveDelta::decode(&b), Ok(d));
        for cut in [0, 1, b.len() - 1] {
            prop_assert!(HaveDelta::decode(&b[..cut]).is_err());
        }
    }

    /// Only the chunks in order complete an assembly; the first one out of
    /// order is refused.
    #[test]
    fn only_in_order_chunks_complete(
        words in 1u64..(4 * MAX_CHUNK as u64 / 8),
        order in proptest::collection::vec(0u32..5, 1..8),
    ) {
        let h = HaveHeader {
            generation: 1,
            serial: 1,
            next_delta: 0,
            key: HaveKey::from_bytes([0; 16]),
            bits: words * 64,
            hashes: 7,
            capacity: 1,
            count: 0,
            chunks: chunks_for(words * 64),
        };
        let mut a = Assembly::new(h);
        let mut covered = 0;
        for (i, seq) in order.iter().enumerate() {
            let c = chunk(&h, (*seq).min(h.chunks));
            let r = a.accept(&c);
            if i < h.chunks as usize && *seq == i as u32 {
                let p = r.unwrap();
                prop_assert_eq!(p.offset, covered);
                covered += p.bytes.len();
                prop_assert_eq!(p.last, i + 1 == h.chunks as usize);
            } else {
                prop_assert!(r.is_err());
                break;
            }
        }
        prop_assert_eq!(a.is_complete(), covered as u64 == h.bits / 8);
    }
}

#[test]
fn a_missing_last_chunk_never_completes() {
    let h = HaveHeader {
        generation: 1,
        serial: 3,
        next_delta: 0,
        key: HaveKey::from_bytes([1; 16]),
        bits: 3 * 8 * MAX_CHUNK as u64,
        hashes: 7,
        capacity: 100,
        count: 0,
        chunks: 3,
    };
    let mut a = Assembly::new(h);
    a.accept(&chunk(&h, 0)).unwrap();
    a.accept(&chunk(&h, 1)).unwrap();
    assert!(!a.is_complete());
    let mut other = chunk(&h, 2);
    other.version = 4;
    assert_eq!(a.accept(&other), Err(HaveError::OtherTransfer));
    other.version = 3;
    other.kind = SnapshotKind::Dictionary;
    assert_eq!(a.accept(&other), Err(HaveError::OtherTransfer));
    assert!(a.accept(&chunk(&h, 2)).unwrap().last);
    assert_eq!(a.accept(&chunk(&h, 2)), Err(HaveError::Complete));
}

#[test]
fn the_largest_delta_fits_a_message() {
    let d = HaveDelta {
        generation: u64::MAX,
        seq: u64::MAX,
        keys: vec![[0xEE; 20]; MAX_DELTA_KEYS],
    };
    let b = d.encode().unwrap();
    assert!(b.len() <= lodestar_cluster::proto::MAX_OPAQUE);
    assert!(lodestar_cluster::proto::ToWorker::HaveDelta(b).validate());
}

#[test]
fn a_header_fits_a_message_and_the_largest_filter_fits_the_chunk_limit() {
    let h = HaveHeader {
        generation: 1,
        serial: 1,
        next_delta: 0,
        key: HaveKey::from_bytes([1; 16]),
        bits: MAX_FILTER_BITS,
        hashes: 7,
        capacity: 1,
        count: 0,
        chunks: chunks_for(MAX_FILTER_BITS),
    };
    assert!(HaveHeader::decode(&h.encode()).is_ok());
    assert!(lodestar_cluster::proto::ToWorker::HaveSnapshot(h.encode()).validate());
    assert!(h.chunks <= lodestar_cluster::proto::MAX_CHUNKS);
}

/// A header's key never reaches `Debug` of the message carrying it, nor
/// do the bytes of any opaque payload.
#[test]
fn messages_print_lengths_not_payloads() {
    use lodestar_cluster::proto::{Assign, ToWorker};
    let h = HaveHeader {
        generation: 2,
        serial: 3,
        next_delta: 0,
        key: HaveKey::from_bytes([0xA7; 16]),
        bits: 1 << 20,
        hashes: 7,
        capacity: 1000,
        count: 4,
        chunks: chunks_for(1 << 20),
    };
    let header = h.encode();
    let msgs = [
        ToWorker::HaveSnapshot(header.clone()),
        ToWorker::HaveDelta(
            HaveDelta {
                generation: 2,
                seq: 0,
                keys: vec![[0xA7; 20]],
            }
            .encode()
            .unwrap(),
        ),
        ToWorker::Update(vec![0xA7; 9]),
        ToWorker::Assign(Assign {
            revision: 1,
            payload: vec![0xA7; 9],
        }),
        ToWorker::Snapshot(SnapshotChunk {
            kind: SnapshotKind::HaveFilter,
            version: 3,
            seq: 0,
            total: 1,
            bytes: vec![0xA7; 64],
        }),
    ];
    for m in &msgs {
        let shown = format!("{m:?} {m:#?}");
        for needle in ["167", "a7", "A7", "[", "0x"] {
            assert!(!shown.contains(needle), "{needle} in {shown}");
        }
    }
    assert_eq!(format!("{:?}", msgs[0]), "HaveSnapshot(70 bytes)");
}
