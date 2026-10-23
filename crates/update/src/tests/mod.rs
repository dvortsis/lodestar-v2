//! Cross-module tests: the golden vectors, key changes and the floor,
//! and property tests.

mod keys;
mod props;
mod vectors;

use serde_json::Value;

use crate::testing::{self, TestKey, TestTrust};
use crate::{Channel, Signed, SignedStatement};

/// The vectors' base time (2026-09-21T13:46:40Z).
pub(crate) const T: u64 = 1_790_000_000;

/// The Linux binary every test release pins.
pub(crate) const BINARY: &[u8] = b"\x7fELF lodestar test binary";

/// A signed stable manifest dated `ts` with one release of `version`.
pub(crate) fn signed_manifest(key: &TestKey, channel: Channel, ts: u64, version: &str) -> Signed {
    key.signed(&testing::bytes(&testing::manifest(
        channel, ts, version, BINARY,
    )))
}

/// A manifest body edited by `f`, signed by `key`.
pub(crate) fn manifest_with(
    key: &TestKey,
    channel: Channel,
    ts: u64,
    f: impl FnOnce(&mut Value),
) -> (Vec<u8>, Signed) {
    let mut v = testing::manifest(channel, ts, "0.5.3", BINARY);
    f(&mut v);
    let body = testing::bytes(&v);
    let signed = key.signed(&body);
    (body, signed)
}

/// Sets a manifest's statements.
pub(crate) fn with_statements(v: &mut Value, statements: &[SignedStatement]) {
    v["statements"] = serde_json::to_value(statements).unwrap();
}

pub(crate) fn trust() -> TestTrust {
    TestTrust::default()
}
