//! The update trust core (§12.2): the envelope, manifest, key statement,
//! freshness, minisign and trust-state parsers, `verify::check` against
//! a fixed TEST ONLY trust root, and `verify::overlay`. Nothing may
//! panic, and a document that verifies re-verifies identically, also as
//! a refetch on the state it produced.
#![no_main]

use std::sync::OnceLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use libfuzzer_sys::fuzz_target;
use lodestar_update::testing::{TestTrust, envelope};
use lodestar_update::{
    Channel, Embedded, Envelope, FreshnessBody, ManifestBody, PublicKey, Sha256, Signature,
    SignedStatement, StatementBody, Target, TrustState, check, overlay,
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
        let merged = TrustState::merge(&c.next, again.next.clone());
        assert_eq!(merged.seq, again.next.seq);
        let reloaded = TrustState::load(&c.next.to_bytes().expect("serialises"), emb)
            .expect("a written state loads");
        assert_eq!(reloaded, c.next);
    }
}

/// Fuzz-chosen statement bodies, each signed by the test recovery key,
/// inside a release-signed manifest: reaches chain acceptance, gaps,
/// equivocation and the rotation resets.
fn with_statements(data: &[u8], channel: Channel) {
    let (t, _) = keys();
    let statements: Vec<SignedStatement> = data
        .split(|&b| b == 0)
        .take(8)
        .map(|body| {
            let s = t.recovery.signed(body);
            SignedStatement {
                body: s.body,
                minisig: s.minisig,
                cosig: None,
            }
        })
        .collect();
    let manifest = serde_json::json!({
        "type": "lodestar-manifest",
        "format": 1,
        "channel": channel,
        "timestamp": NOW,
        "expires": NOW + 86_400,
        "statements": statements,
        "releases": [],
    });
    let body = serde_json::to_vec(&manifest).expect("json");
    verify_twice(&envelope(&t.release.signed(&body), None), channel);
}

/// The bytes as an overlay manifest signed by the release key, with the
/// binary pinned or not.
fn as_overlay(data: &[u8]) {
    let (t, emb) = keys();
    let signed = t.release.signed(data);
    let Ok(body) = STANDARD.decode(&signed.body) else {
        return;
    };
    let v = semver::Version::new(0, 5, 3);
    let first = overlay(
        &body,
        &signed.minisig,
        Sha256::of(b""),
        0,
        &v,
        Target::X86_64LinuxMusl,
        emb,
    );
    let again = overlay(
        &body,
        &signed.minisig,
        Sha256::of(b""),
        0,
        &v,
        Target::X86_64LinuxMusl,
        emb,
    );
    assert_eq!(first, again);
    let _ = overlay(
        data,
        &String::from_utf8_lossy(data),
        Sha256::of(data),
        1,
        &v,
        Target::X86_64LinuxMusl,
        emb,
    );
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
    match mode & 7 {
        // The bytes as a fetched envelope.
        0 => verify_twice(rest, channel),
        // The bytes as a manifest body signed by the release key, so the
        // parser and the rules past the signature are reached.
        1 => verify_twice(&envelope(&t.release.signed(rest), None), channel),
        2 => verify_twice(&envelope(&t.nightly.signed(rest), None), channel),
        // Split in two: a manifest body and a freshness body.
        3 => {
            let cut = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            let (m, f) = rest.split_at(cut);
            let f = f.get(1..).unwrap_or_default();
            let env = envelope(&t.release.signed(m), Some(&t.freshness.signed(f)));
            verify_twice(&env, channel);
        }
        4 | 5 => with_statements(rest, channel),
        _ => as_overlay(rest),
    }
});
