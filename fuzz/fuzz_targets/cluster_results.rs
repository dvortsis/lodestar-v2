//! The `Results` and `Counters` payloads a worker sends (WK-6,
//! architecture §19.2, §19.4): the coordinator decodes them from bytes it
//! didn't choose. Arbitrary bytes never panic and never decode past the
//! caps; whatever decodes is canonical (it re-encodes to the same bytes),
//! carries no `pieces`, and keys every record by one of its swarm hashes.
//! Sealed records (WK-8) decode with `decode_with` and re-encode too.
//!
//! The first byte picks the decoder (even: `Results`, odd: `Counters`), so
//! each sees well-formed headers often. Run it with a small allocation
//! limit to check the caps hold:
//! `cargo +nightly fuzz run cluster_results -- -malloc_limit_mb=64`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_pipeline::wire::results::MAX_RECORDS;
use lodestar_pipeline::wire::{CountersBatch, MAX_PAYLOAD, ResultsBatch};

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    if pick % 2 == 0 {
        if let Ok((batch, sealed)) = ResultsBatch::decode_with(body) {
            assert!(body.len() <= MAX_PAYLOAD);
            assert!(batch.records.len() + sealed.len() <= MAX_RECORDS);
            // Sealed records (WK-8) are left out of a plain decode.
            assert_eq!(ResultsBatch::decode(body).unwrap(), batch);
            for r in &batch.records {
                assert!(r.torrent.swarm_hashes().any(|h| h == r.key));
                for blob in [&r.torrent.raw, &r.torrent.extra].into_iter().flatten() {
                    let stripped = lodestar_metainfo::info_without_pieces(blob).unwrap();
                    assert_eq!(&stripped, blob, "a blob with pieces decoded");
                }
            }
            let again = batch.encode_with(&sealed).expect("a decoded batch encodes");
            assert_eq!(again, body);
        }
    } else if let Ok(batch) = CountersBatch::decode(body) {
        assert!(batch.counters.windows(2).all(|w| {
            (w[0].hour, w[0].metric, w[0].dim, w[0].key) < (w[1].hour, w[1].metric, w[1].dim, w[1].key)
        }));
        assert_eq!(batch.encode().expect("a decoded batch encodes"), body);
    }
});
