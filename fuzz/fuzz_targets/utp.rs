//! uTP packet parsing never panics and round-trips; a connection fed
//! hostile packets and local events never panics, keeps its buffers
//! bounded and only emits packets that parse.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_utp::fuzzing;

fuzz_target!(|data: &[u8]| {
    fuzzing::packet(data);
    fuzzing::connection(data);
});
