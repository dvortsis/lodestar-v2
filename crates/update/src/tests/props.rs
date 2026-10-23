//! Property tests: no parser panics, any mutation of a signed body or
//! signature fails, the trust state round-trips, merges are monotonic.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use proptest::prelude::*;

use super::*;
use crate::testing::envelope;
use crate::{
    Envelope, FreshnessBody, ManifestBody, Marks, PublicKey, Signature, StatementBody, TrustState,
    check,
};

fn embedded() -> crate::Embedded {
    trust().embedded(vec![])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn parsers_never_panic(data in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let _ = Envelope::parse(&data);
        let _ = ManifestBody::parse(&data);
        let _ = StatementBody::parse(&data);
        let _ = FreshnessBody::parse(&data);
        let _ = TrustState::load(&data, &embedded());
        let text = String::from_utf8_lossy(&data);
        let _ = Signature::parse(&text);
        let _ = PublicKey::from_file(&text);
        let _ = PublicKey::from_base64(&text);
        let _ = check(&data, Channel::Stable, T, &embedded(), &TrustState::default());
    }

    #[test]
    fn json_shaped_input_never_panics(s in r#"\{("[a-z_]{1,10}":(\[\]|null|[0-9]{1,3}|"[a-z0-9-]{0,8}"|\{\}),?){0,8}\}"#) {
        let _ = Envelope::parse(s.as_bytes());
        let _ = ManifestBody::parse(s.as_bytes());
        let _ = StatementBody::parse(s.as_bytes());
        let _ = FreshnessBody::parse(s.as_bytes());
        let _ = TrustState::load(s.as_bytes(), &embedded());
    }

    #[test]
    fn mutating_the_signed_body_fails(i in any::<prop::sample::Index>(), bit in 0u8..8) {
        let t = trust();
        let m = signed_manifest(&t.release, Channel::Stable, T, "0.5.3");
        let mut body = STANDARD.decode(&m.body).unwrap();
        let i = i.index(body.len());
        body[i] ^= 1 << bit;
        let env = envelope(&crate::Signed { body: STANDARD.encode(&body), minisig: m.minisig }, None);
        prop_assert!(check(&env, Channel::Stable, T + 1, &embedded(), &TrustState::default()).is_err());
    }

    #[test]
    fn mutating_the_signature_fails(i in any::<prop::sample::Index>(), c in "[A-Za-z0-9+/= a-z]") {
        let t = trust();
        let m = signed_manifest(&t.release, Channel::Stable, T, "0.5.3");
        // The untrusted comment (line 1) isn't signed; everything after it is.
        let start = m.minisig.find('\n').unwrap() + 1;
        let end = m.minisig.len() - 1;
        let i = start + i.index(end - start);
        let mut sig = m.minisig.clone().into_bytes();
        prop_assume!(sig[i] != b'\n' && sig[i] != c.as_bytes()[0]);
        sig[i] = c.as_bytes()[0];
        let env = envelope(&crate::Signed { body: m.body, minisig: String::from_utf8(sig).unwrap() }, None);
        prop_assert!(check(&env, Channel::Stable, T + 1, &embedded(), &TrustState::default()).is_err());
    }

    #[test]
    fn trust_state_round_trips(marks in proptest::collection::vec((0u64..1 << 40, any::<bool>(), 0u64..1 << 40), 0..4)) {
        let mut s = TrustState::default();
        for (c, (h, sha, f)) in Channel::ALL.into_iter().zip(marks) {
            s.channels.insert(c, Marks {
                hwm_ts: h,
                hwm_sha256: sha.then(|| crate::Sha256::of(&h.to_le_bytes())),
                fresh_ts: f,
                fresh_sha256: None,
            });
        }
        prop_assert_eq!(TrustState::load(&s.to_bytes().unwrap(), &embedded()).unwrap(), s);
    }

    #[test]
    fn merges_are_monotonic(a in states(), b in states()) {
        let m = TrustState::merge(&a, b.clone());
        prop_assert!(m == a || m == b);
        prop_assert!(m.seq >= a.seq);
        if m.seq == a.seq {
            for (c, old) in &a.channels {
                let new = m.marks(*c);
                prop_assert!(new.hwm_ts >= old.hwm_ts && new.fresh_ts >= old.fresh_ts);
            }
        }
    }
}

fn states() -> impl Strategy<Value = TrustState> {
    (0u64..3, proptest::collection::vec((0u64..5, 0u64..5), 3)).prop_map(|(seq, marks)| {
        let mut s = TrustState {
            seq,
            ..TrustState::default()
        };
        for (c, (h, f)) in Channel::ALL.into_iter().zip(marks) {
            s.channels.insert(
                c,
                Marks {
                    hwm_ts: h,
                    fresh_ts: f,
                    ..Marks::default()
                },
            );
        }
        s
    })
}
