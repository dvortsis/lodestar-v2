//! KRPC parsing never panics, and whatever parses is stable under
//! encode → parse.
#![no_main]

use lodestar_dht::krpc::parse;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(m) = parse(data) {
        let bytes = m.encode();
        assert_eq!(parse(&bytes).expect("re-encoded message must parse"), m);
    }
});
