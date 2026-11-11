//! A worker's spool segments (WK-11, architecture §19.6): read back from
//! a disk the worker doesn't fully control (a crash mid-write, damage, a
//! copied folder). Arbitrary bytes never panic and never allocate past the
//! caps; the frames found are whole and in order, and each is one the
//! encoder would write; a record's plaintext that decodes re-encodes to
//! the same bytes.
//!
//! The first byte picks the decoder (even: a segment, odd: a record's
//! plaintext). Run it with a small allocation limit to check the caps:
//! `cargo +nightly fuzz run worker_spool -- -malloc_limit_mb=64`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_pipeline::spool::{
    HEADER_LEN, MAX_FRAME, MAX_LEASES, Record, Tail, decode_segment, encode_frame,
};
use lodestar_pipeline::wire::MAX_PAYLOAD;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, body)) = data.split_first() else {
        return;
    };
    if pick % 2 == 0 {
        let Ok(d) = decode_segment(body) else {
            return;
        };
        assert_eq!(d.header.encode().as_slice(), &body[..HEADER_LEN]);
        let mut at = HEADER_LEN;
        for f in &d.frames {
            assert!(!f.is_empty() && f.len() <= MAX_FRAME);
            let framed = encode_frame(f).unwrap();
            assert_eq!(&body[at..at + framed.len()], framed.as_slice());
            at += framed.len();
        }
        if d.tail == Tail::Clean {
            assert_eq!(at, body.len());
        } else {
            assert!(at < body.len());
        }
    } else if let Ok(r) = Record::decode(body) {
        assert!(r.batch != 0);
        assert!(r.leases.len() <= MAX_LEASES);
        assert!(r.payload.len() <= MAX_PAYLOAD);
        assert_eq!(r.encode().unwrap(), body);
    }
});
