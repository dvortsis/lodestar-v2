//! Tracker reply parsing never panics. A UDP scrape reply that parses
//! never claims more entries than one packet may carry, and an HTTP reply
//! that parses answers exactly the hashes asked about.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_metainfo::InfoHash;
use lodestar_tracker_scrape::{http, udp};

fuzz_target!(|data: &[u8]| {
    if let Ok(udp::Packet::Scrape { counts, .. }) = udp::parse_response(data) {
        assert!(counts.len() <= udp::MAX_HASHES);
    }
    // The first byte picks how many hashes the "request" had; hash i is
    // twenty bytes of i, which fuzzed bodies can name.
    let (n, body) = data.split_first().map_or((0, data), |(n, b)| (*n % 8, b));
    let hashes: Vec<InfoHash> = (0..n).map(|i| InfoHash([i; 20])).collect();
    if let Ok(reply) = http::parse_scrape(body, &hashes) {
        assert_eq!(reply.counts.len(), hashes.len());
    }
});
