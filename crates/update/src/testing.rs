//! TEST ONLY signing with fixed seeds (Ed25519 is deterministic, so the
//! vectors are stable). Built for this crate's tests and, with the
//! `testing` feature, for the fuzz target and other crates' tests. No
//! shipped build enables it, and none of these keys is trusted outside a
//! test's own `Embedded`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use blake2::{Blake2b512, Digest as _};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::{Value, json};

use crate::doc::{Signed, SignedStatement};
use crate::keys::Embedded;
use crate::minisign::{PublicKey, Signature};
use crate::{Channel, Role};

/// A TEST ONLY key pair from a fixed seed.
#[derive(Debug)]
pub struct TestKey {
    pair: Ed25519KeyPair,
    number: [u8; 8],
}

impl TestKey {
    /// TEST ONLY: the key whose seed is 32 copies of `seed`.
    pub fn new(seed: u8) -> Self {
        let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("valid seed");
        Self {
            pair,
            number: [seed, 0x7e, 0x57, 0, 0, 0, 0, seed],
        }
    }

    pub fn public(&self) -> PublicKey {
        let mut key = [0; 32];
        key.copy_from_slice(self.pair.public_key().as_ref());
        PublicKey::from_parts(self.number, key)
    }

    /// A prehashed (`ED`) signature file over `message`.
    pub fn sign(&self, message: &[u8]) -> String {
        self.signature(message, "test")
            .to_file("signature from a TEST ONLY key")
    }

    /// A legacy (`Ed`) signature file over `message`.
    pub fn sign_legacy(&self, message: &[u8]) -> String {
        let sig = self.pair.sign(message);
        let trusted = "test";
        let mut global = sig.as_ref().to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global = self.pair.sign(&global);
        let mut line = b"Ed".to_vec();
        line.extend_from_slice(&self.number);
        line.extend_from_slice(sig.as_ref());
        format!(
            "untrusted comment: legacy\n{}\ntrusted comment: {trusted}\n{}\n",
            STANDARD.encode(line),
            STANDARD.encode(global.as_ref())
        )
    }

    fn signature(&self, message: &[u8], trusted: &str) -> Signature {
        let prehash = Blake2b512::digest(message);
        let sig: [u8; 64] = self
            .pair
            .sign(prehash.as_slice())
            .as_ref()
            .try_into()
            .expect("64");
        let mut global = sig.to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global: [u8; 64] = self.pair.sign(&global).as_ref().try_into().expect("64");
        Signature::from_parts(self.number, sig, trusted.to_owned(), global)
    }

    /// A signed document over `body`'s bytes.
    pub fn signed(&self, body: &[u8]) -> Signed {
        Signed {
            body: STANDARD.encode(body),
            minisig: self.sign(body),
        }
    }
}

/// The JSON bytes of a value (compact, as a publisher might write them).
pub fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).expect("json")
}

/// A key statement signed by `recovery`.
pub fn statement(
    recovery: &TestKey,
    seq: u64,
    role: Role,
    key: &PublicKey,
    timestamp: u64,
) -> SignedStatement {
    let body = bytes(&json!({
        "type": "lodestar-key",
        "seq": seq,
        "role": role,
        "key": key.to_base64(),
        "timestamp": timestamp,
    }));
    let s = recovery.signed(&body);
    SignedStatement {
        body: s.body,
        minisig: s.minisig,
        cosig: None,
    }
}

/// A TEST ONLY set of keys: recovery, release, nightly and freshness,
/// plus spares for rotations.
#[derive(Debug)]
pub struct TestTrust {
    pub recovery: TestKey,
    pub release: TestKey,
    pub nightly: TestKey,
    pub freshness: TestKey,
}

impl Default for TestTrust {
    fn default() -> Self {
        Self {
            recovery: TestKey::new(1),
            release: TestKey::new(2),
            nightly: TestKey::new(3),
            freshness: TestKey::new(4),
        }
    }
}

impl TestTrust {
    /// The embedded root with all four keys and `statements`.
    pub fn embedded(&self, statements: Vec<SignedStatement>) -> Embedded {
        Embedded {
            recovery: self.recovery.public(),
            release: Some(self.release.public()),
            nightly: Some(self.nightly.public()),
            freshness: Some(self.freshness.public()),
            statements,
        }
    }
}

/// A manifest body with one release of `version` (a Linux binary
/// pinned to `binary`), dated `timestamp`, expiring in 7 days.
pub fn manifest(channel: Channel, timestamp: u64, version: &str, binary: &[u8]) -> Value {
    json!({
        "type": "lodestar-manifest",
        "format": 1,
        "channel": channel,
        "timestamp": timestamp,
        "expires": timestamp + 7 * crate::DAY,
        "statements": [],
        "releases": [release(version, timestamp, binary)],
    })
}

/// One release entry.
pub fn release(version: &str, date: u64, binary: &[u8]) -> Value {
    json!({
        "version": version,
        "date": date,
        "level": "fix",
        "security": false,
        "manual_steps": false,
        "min_from": null,
        "withdrawn": false,
        "migrates": false,
        "reindex": false,
        "reindex_bytes": 0,
        "notes": "Fixes.",
        "notes_url": null,
        "artifacts": [{
            "target": "x86_64-unknown-linux-musl",
            "kind": "binary",
            "url": format!("https://github.com/dvortsis/lodestar-v2/releases/download/v{version}/lodestar"),
            "sha256": crate::Sha256::of(binary).to_hex(),
            "size": binary.len(),
        }],
    })
}

/// A freshness body for a manifest body.
pub fn freshness(channel: Channel, manifest_body: &[u8], timestamp: u64, expires: u64) -> Value {
    json!({
        "type": "lodestar-freshness",
        "channel": channel,
        "manifest_sha256": crate::Sha256::of(manifest_body).to_hex(),
        "timestamp": timestamp,
        "expires": expires,
        "statements": [],
    })
}

/// An envelope over a signed manifest and an optional freshness.
pub fn envelope(manifest: &Signed, freshness: Option<&Signed>) -> Vec<u8> {
    bytes(&json!({ "manifest": manifest, "freshness": freshness }))
}
