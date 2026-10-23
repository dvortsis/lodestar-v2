//! The update trust core (§12.2): the envelope, manifest, key statement,
//! freshness, minisign and trust-state parsers, and `verify::check`
//! against a fixed TEST ONLY trust root. Nothing may panic, and a
//! document that verifies re-verifies identically, also as a refetch on
//! the state it produced.
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use lodestar_update::testing::{TestTrust, envelope};
use lodestar_update::{
    Channel, Embedded, Envelope, FreshnessBody, ManifestBody, PublicKey, Signature, StatementBody,
    TrustState, check,
};

const NOW: u64 = 1_790_000_000;

fn keys() -> &'static (TestTrust, Embedded) {
    static KEYS: OnceLock<(TestTrust, Embedded)> = OnceLock::new();
    KEYS.get_or_init(|| {
        let t = TestTrust::default();
        let e = t.embedded(vec![]);
        (t, e)
    })
}

fn verify_twice(env: &[u8], channel: Channel) {
    let (_, emb) = keys();
    let state = TrustState::default();
    let first = check(env, channel, NOW, emb, &state);
    assert_eq!(first, check(env, channel, NOW, emb, &state));
    if let Ok(c) = first {
        let again = check(env, channel, NOW, emb, &c.next).expect("a refetch verifies");
        assert_eq!(again.manifest, c.manifest);
        assert_eq!(TrustState::merge(&c.next, again.next.clone()), again.next);
        let reloaded = TrustState::load(&c.next.to_bytes(), emb).expect("a written state loads");
        assert_eq!(reloaded, c.next);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let (t, emb) = keys();
    let _ = Envelope::parse(rest);
    let _ = ManifestBody::parse(rest);
    let _ = StatementBody::parse(rest);
    let _ = FreshnessBody::parse(rest);
    let _ = TrustState::load(rest, emb);
    if let Ok(text) = std::str::from_utf8(rest) {
        let _ = Signature::parse(text);
        let _ = PublicKey::from_file(text);
    }
    let channel = Channel::ALL[usize::from(mode >> 6) % 3];
    match mode & 3 {
        // The bytes as a fetched envelope.
        0 => verify_twice(rest, channel),
        // The bytes as a manifest body signed by the release key, so the
        // parser and the rules past the signature are reached.
        1 => verify_twice(&envelope(&t.release.signed(rest), None), channel),
        2 => verify_twice(&envelope(&t.nightly.signed(rest), None), channel),
        // Split in two: a manifest body and a freshness body.
        _ => {
            let cut = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            let (m, f) = rest.split_at(cut);
            let f = f.get(1..).unwrap_or_default();
            let env = envelope(&t.release.signed(m), Some(&t.freshness.signed(f)));
            verify_twice(&env, channel);
        }
    }
});
