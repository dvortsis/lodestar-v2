//! Cluster protocol frames (architecture §19.2): arbitrary bytes never
//! panic, never decode past the size caps, and whatever decodes is a valid
//! message that encodes and decodes back to itself.
//!
//! Run it with a small allocation limit to check the caps hold:
//! `cargo +nightly fuzz run cluster_frame -- -malloc_limit_mb=8`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_cluster::frame::{
    self, MAX_JOIN_REPLY, MAX_JOIN_REQUEST, MAX_MESSAGE, decode_join_reply, decode_join_request,
    decode_to_coordinator, decode_to_worker,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(msg) = decode_to_coordinator(data) {
        assert!(msg.validate());
        let again = frame::encode(&msg).expect("a decoded message encodes");
        assert!(again.len() <= MAX_MESSAGE + 1);
        assert_eq!(decode_to_coordinator(&again).expect("round trip"), msg);
    }
    if let Ok(msg) = decode_to_worker(data) {
        assert!(msg.validate());
        let again = frame::encode(&msg).expect("a decoded message encodes");
        assert_eq!(decode_to_worker(&again).expect("round trip"), msg);
    }
    if let Ok(req) = decode_join_request(data) {
        assert!(data.len() <= MAX_JOIN_REQUEST);
        let again = frame::encode(&req).expect("a decoded request encodes");
        assert_eq!(decode_join_request(&again).expect("round trip"), req);
    }
    if let Ok(reply) = decode_join_reply(data) {
        assert!(data.len() <= MAX_JOIN_REPLY);
        let again = frame::encode(&reply).expect("a decoded reply encodes");
        assert_eq!(decode_join_reply(&again).expect("round trip"), reply);
    }
});
