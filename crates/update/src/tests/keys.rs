//! Key changes, the trust floor and overlays.

use semver::Version;

use super::*;
use crate::testing::{envelope, statement};
use crate::trust::OverlayMark;
use crate::{Chain, DAY, Error, Marks, Role, Sha256, Target, TrustState, check, overlay};

fn checked_state(
    env: &[u8],
    embedded: &crate::Embedded,
    state: &TrustState,
    now: u64,
) -> TrustState {
    check(env, Channel::Stable, now, embedded, state)
        .unwrap()
        .next
}

#[test]
fn a_recovery_signed_rotation_shuts_the_old_key_out() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let k5 = TestKey::new(5);
    let rot = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    let (_, m) = manifest_with(&k5, Channel::Stable, T + 10, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let state = checked_state(&envelope(&m, None), &emb, &TrustState::default(), T + 20);
    assert_eq!(state.seq, 1);
    assert!(state.revokes(&t.release.public()));
    assert!(!state.revokes(&k5.public()));

    // The old key's manifests are refused afterwards, however new.
    let old = signed_manifest(&t.release, Channel::Stable, T + 100, "0.5.4");
    assert_eq!(
        check(
            &envelope(&old, None),
            Channel::Stable,
            T + 200,
            &emb,
            &state
        ),
        Err(Error::RevokedKey)
    );
    // The new key's later manifests don't need to carry the statement.
    let next = signed_manifest(&k5, Channel::Stable, T + 100, "0.5.4");
    assert!(
        check(
            &envelope(&next, None),
            Channel::Stable,
            T + 200,
            &emb,
            &state
        )
        .is_ok()
    );
}

#[test]
fn lower_or_equal_sequence_statements_are_refused_unless_identical() {
    let t = trust();
    let (k5, k6, k7) = (TestKey::new(5), TestKey::new(6), TestKey::new(7));
    let s1 = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    let s2 = statement(&t.recovery, 2, Role::Nightly, &k6.public(), T);
    let emb = t.embedded(vec![s1.clone(), s2.clone()]);
    let mut chain = Chain::embedded(&emb).unwrap();
    assert_eq!(chain.seq(), 2);
    // Identical copies of accepted statements are fine (a full chain).
    assert_eq!(chain.extend(&[s1.clone(), s2.clone()], Some(T)), Ok(0));
    // A different statement with an accepted sequence number is refused.
    let lower = statement(&t.recovery, 1, Role::Release, &k7.public(), T + 1);
    let equal = statement(&t.recovery, 2, Role::Release, &k7.public(), T + 1);
    assert_eq!(
        chain.clone().extend(&[lower], Some(T)),
        Err(Error::Equivocation)
    );
    assert_eq!(
        chain.clone().extend(&[equal], Some(T)),
        Err(Error::Equivocation)
    );
    // Out of order is refused, and so is a reused key.
    assert_eq!(
        chain.clone().extend(&[s2.clone(), s1.clone()], Some(T)),
        Err(Error::BadDocument)
    );
    let reuse = statement(&t.recovery, 3, Role::Freshness, &t.release.public(), T);
    assert_eq!(
        chain.clone().extend(&[reuse], Some(T)),
        Err(Error::KeyReused)
    );
    // A failed extension leaves the chain unchanged.
    let s3 = statement(&t.recovery, 3, Role::Freshness, &k7.public(), T);
    let bad = statement(&t.release, 4, Role::Release, &TestKey::new(9).public(), T);
    assert_eq!(
        chain.extend(&[s3, bad], Some(T)),
        Err(Error::NotRecoverySigned)
    );
    assert_eq!(chain.seq(), 2);
}

#[test]
fn a_cosignature_is_never_sufficient() {
    let t = trust();
    let k5 = TestKey::new(5);
    // Signed only by the old release key, which also cosigns.
    let mut s = statement(&t.release, 1, Role::Release, &k5.public(), T);
    s.cosig = Some(s.minisig.clone());
    let mut chain = Chain::genesis(&t.embedded(vec![]));
    assert_eq!(
        chain.extend(std::slice::from_ref(&s), None),
        Err(Error::NotRecoverySigned)
    );
    // With the recovery signature, a cosig is accepted but must be well formed.
    let mut s = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    s.cosig = Some("not a signature".into());
    assert_eq!(
        chain.extend(std::slice::from_ref(&s), None),
        Err(Error::BadSignatureFile)
    );
    s.cosig = Some(t.release.sign(b"anything"));
    assert_eq!(chain.extend(&[s], None), Ok(1));
}

#[test]
fn a_rotation_resets_only_the_marks_its_role_governs() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let mut state = TrustState::default();
    let high = Marks {
        hwm_ts: T + 5000,
        hwm_sha256: Some(Sha256::of(b"x")),
        fresh_ts: T + 5000,
        fresh_sha256: Some(Sha256::of(b"y")),
    };
    for c in Channel::ALL {
        state.channels.insert(c, high);
    }
    // A leaked release key pushed the marks ahead; the rotation, dated
    // T + 10, brings Stable and Beta back to it, and the new key's
    // manifest at T + 20 is accepted.
    let k5 = TestKey::new(5);
    let rot = statement(&t.recovery, 1, Role::Release, &k5.public(), T + 10);
    let (body, m) = manifest_with(&k5, Channel::Stable, T + 20, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let next = checked_state(&envelope(&m, None), &emb, &state, T + 30);
    assert_eq!(next.marks(Channel::Stable).hwm_ts, T + 20);
    assert_eq!(
        next.marks(Channel::Stable).hwm_sha256,
        Some(Sha256::of(&body))
    );
    assert_eq!(next.marks(Channel::Beta).hwm_ts, T + 10);
    assert_eq!(next.marks(Channel::Beta).hwm_sha256, None);
    assert_eq!(next.marks(Channel::Nightly), high);
    for c in Channel::ALL {
        assert_eq!(next.marks(c).fresh_ts, T + 5000, "{c:?}");
    }

    // A freshness rotation resets every freshness mark and nothing else.
    let k6 = TestKey::new(6);
    let frot = statement(&t.recovery, 2, Role::Freshness, &k6.public(), T + 40);
    let f = k6.signed(&crate::testing::bytes(&crate::testing::freshness(
        Channel::Stable,
        &body,
        T + 50,
        T + 20 * DAY,
    )));
    let f = {
        let mut v: serde_json::Value =
            serde_json::from_slice(&crate::doc::decode(&f.body, &f.minisig, 65536).unwrap().0)
                .unwrap();
        with_statements(&mut v, &[rot.clone(), frot]);
        k6.signed(&crate::testing::bytes(&v))
    };
    let c = check(
        &envelope(&m, Some(&f)),
        Channel::Stable,
        T + 60,
        &emb,
        &next,
    )
    .unwrap();
    assert_eq!(
        c.freshness,
        crate::Freshness::Accepted {
            timestamp: T + 50,
            expires: T + 20 * DAY
        }
    );
    let after = c.next;
    assert_eq!(after.seq, 2);
    assert_eq!(after.marks(Channel::Stable).fresh_ts, T + 50);
    assert_eq!(after.marks(Channel::Beta).fresh_ts, T + 40);
    assert_eq!(after.marks(Channel::Nightly).fresh_ts, T + 40);
    assert_eq!(after.marks(Channel::Nightly).hwm_ts, T + 5000);
    assert_eq!(after.marks(Channel::Stable).hwm_ts, T + 20);
}

#[test]
fn the_floor_refuses_a_revoked_key_without_trust_json() {
    let t = trust();
    let k5 = TestKey::new(5);
    let rot = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    let old = envelope(
        &signed_manifest(&t.release, Channel::Stable, T + 100, "0.5.4"),
        None,
    );

    // Embedded statements: a rebuilt state already has the rotation.
    let emb = t.embedded(vec![rot.clone()]);
    let rebuilt = TrustState::rebuild(&emb, &[]).unwrap();
    assert_eq!(
        check(&old, Channel::Stable, T + 200, &emb, &rebuilt),
        Err(Error::RevokedKey)
    );
    // Even an empty state is lifted to the embedded floor.
    assert_eq!(
        check(&old, Channel::Stable, T + 200, &emb, &TrustState::default()),
        Err(Error::RevokedKey)
    );

    // Verified overlays: an older build learns the rotation from one.
    let emb = t.embedded(vec![]);
    let (body, m) = manifest_with(&k5, Channel::Stable, T + 50, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let ov = overlay(
        &body,
        &m.minisig,
        Sha256::of(BINARY),
        BINARY.len() as u64,
        &Version::new(0, 5, 3),
        Target::X86_64LinuxMusl,
        &emb,
    )
    .unwrap();
    let rebuilt = TrustState::rebuild(&emb, std::slice::from_ref(&ov.mark)).unwrap();
    assert_eq!(rebuilt.seq, 1);
    assert_eq!(rebuilt.marks(Channel::Stable).hwm_ts, T + 50);
    assert_eq!(
        check(&old, Channel::Stable, T + 200, &emb, &rebuilt),
        Err(Error::RevokedKey)
    );
}

#[test]
fn trust_survives_and_rejects_tampering() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let k5 = TestKey::new(5);
    let rot = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    let (_, m) = manifest_with(&k5, Channel::Stable, T + 10, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let state = checked_state(&envelope(&m, None), &emb, &TrustState::default(), T + 20);
    let bytes = state.to_bytes();
    assert_eq!(TrustState::load(&bytes, &emb).unwrap(), state);

    // Dropping the statement, editing `revoked` or `seq`, or an unknown
    // field: all damage.
    let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for edit in [
        |v: &mut serde_json::Value| v["statements"] = serde_json::json!([]),
        |v: &mut serde_json::Value| v["revoked"] = serde_json::json!([]),
        |v: &mut serde_json::Value| v["seq"] = serde_json::json!(2),
        |v: &mut serde_json::Value| v["extra"] = serde_json::json!(1),
        |v: &mut serde_json::Value| v["format"] = serde_json::json!(2),
    ] {
        let mut e = v.clone();
        edit(&mut e);
        assert_eq!(
            TrustState::load(&crate::testing::bytes(&e), &emb),
            Err(Error::TrustDamaged)
        );
    }
    v["statements"][0]["minisig"] =
        serde_json::json!(statement(&t.release, 1, Role::Release, &k5.public(), T).minisig);
    assert_eq!(
        TrustState::load(&crate::testing::bytes(&v), &emb),
        Err(Error::TrustDamaged)
    );
    assert_eq!(TrustState::load(b"{", &emb), Err(Error::TrustDamaged));
}

#[test]
fn merge_is_forward_only() {
    let mut old = TrustState::default();
    old.channels.insert(
        Channel::Stable,
        Marks {
            hwm_ts: 10,
            ..Marks::default()
        },
    );
    let mut lower = old.clone();
    lower.channels.insert(
        Channel::Stable,
        Marks {
            hwm_ts: 9,
            ..Marks::default()
        },
    );
    assert_eq!(TrustState::merge(&old, lower), old);
    let mut missing = old.clone();
    missing.channels.clear();
    assert_eq!(TrustState::merge(&old, missing), old);
    let mut higher = old.clone();
    higher.channels.insert(
        Channel::Beta,
        Marks {
            fresh_ts: 3,
            ..Marks::default()
        },
    );
    assert_eq!(TrustState::merge(&old, higher.clone()), higher);
    // A higher sequence wins even with lower marks (a rotation's reset).
    let mut rotated = lower_marks(&old);
    rotated.seq = 1;
    assert_eq!(TrustState::merge(&old, rotated.clone()), rotated);
    assert_eq!(TrustState::merge(&rotated, old.clone()), rotated);
}

fn lower_marks(s: &TrustState) -> TrustState {
    let mut s = s.clone();
    s.channels.insert(Channel::Stable, Marks::default());
    s
}

#[test]
fn an_older_supervisor_starts_an_overlay_signed_by_a_newer_key() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let k5 = TestKey::new(5);
    let rot = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    let (body, m) = manifest_with(&k5, Channel::Stable, T, |m| {
        with_statements(m, std::slice::from_ref(&rot));
        // Expired long ago: no clock or expiry applies at start.
        m["expires"] = serde_json::json!(T + 1);
    });
    let v = Version::new(0, 5, 3);
    let target = Target::X86_64LinuxMusl;
    let len = BINARY.len() as u64;
    let ov = overlay(&body, &m.minisig, Sha256::of(BINARY), len, &v, target, &emb).unwrap();
    assert_eq!(ov.signer, k5.public());
    assert!(!ov.revoked_later);

    // A tampered binary, or a different version or target, is refused.
    assert_eq!(
        overlay(&body, &m.minisig, Sha256::of(b"x"), len, &v, target, &emb),
        Err(Error::ArtifactMismatch)
    );
    assert_eq!(
        overlay(
            &body,
            &m.minisig,
            Sha256::of(BINARY),
            len + 1,
            &v,
            target,
            &emb
        ),
        Err(Error::ArtifactMismatch)
    );
    assert_eq!(
        overlay(
            &body,
            &m.minisig,
            Sha256::of(BINARY),
            len,
            &Version::new(0, 5, 4),
            target,
            &emb
        ),
        Err(Error::UnknownVersion)
    );
    assert_eq!(
        overlay(
            &body,
            &m.minisig,
            Sha256::of(BINARY),
            len,
            &v,
            Target::Aarch64LinuxMusl,
            &emb
        ),
        Err(Error::NoArtifact)
    );
    let mut tampered = body.clone();
    tampered[10] ^= 1;
    assert!(
        overlay(
            &tampered,
            &m.minisig,
            Sha256::of(BINARY),
            len,
            &v,
            target,
            &emb
        )
        .is_err()
    );

    // A build whose embedded statements revoke the signer refuses it.
    let k6 = TestKey::new(6);
    let rot2 = statement(&t.recovery, 2, Role::Release, &k6.public(), T + 5);
    let newer = t.embedded(vec![rot.clone(), rot2]);
    assert_eq!(
        overlay(
            &body,
            &m.minisig,
            Sha256::of(BINARY),
            len,
            &v,
            target,
            &newer
        ),
        Err(Error::RevokedKey)
    );

    // Revoked only by a later statement (here the overlay's own): starts,
    // with the notice.
    let (body, m) = manifest_with(&t.release, Channel::Stable, T, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let ov = overlay(&body, &m.minisig, Sha256::of(BINARY), len, &v, target, &emb).unwrap();
    assert!(ov.revoked_later);

    // The nightly key can't sign a stable overlay.
    let (body, m) = manifest_with(&t.nightly, Channel::Stable, T, |_| {});
    assert_eq!(
        overlay(&body, &m.minisig, Sha256::of(BINARY), len, &v, target, &emb),
        Err(Error::UnknownKey)
    );
}

#[test]
fn rebuild_keeps_the_newest_overlay_mark() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let mark = |ts| OverlayMark {
        channel: Channel::Beta,
        timestamp: ts,
        sha256: Sha256::of(&ts.to_le_bytes()),
        statements: vec![],
    };
    let s = TrustState::rebuild(&emb, &[mark(T + 5), mark(T + 9), mark(T + 1)]).unwrap();
    assert_eq!(s.marks(Channel::Beta).hwm_ts, T + 9);
    assert_eq!(
        s.marks(Channel::Beta).hwm_sha256,
        Some(Sha256::of(&(T + 9).to_le_bytes()))
    );
}

#[test]
fn a_statement_made_with_the_minisign_cli_is_accepted() {
    use base64::Engine as _;
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/minisign");
    let read = |n: &str| std::fs::read(format!("{dir}/{n}")).unwrap();
    let recovery =
        crate::PublicKey::from_file(std::str::from_utf8(&read("tool.pub")).unwrap()).unwrap();
    let emb = crate::Embedded {
        recovery,
        ..trust().embedded(vec![])
    };
    let s = SignedStatement {
        body: base64::engine::general_purpose::STANDARD.encode(read("statement.json")),
        minisig: String::from_utf8(read("statement.json.minisig")).unwrap(),
        cosig: None,
    };
    let mut chain = Chain::genesis(&emb);
    assert_eq!(chain.extend(&[s], Some(T)), Ok(1));
    assert_eq!(
        chain.current(Role::Release).unwrap().id(),
        "0500000066577E05"
    );
    assert!(chain.is_revoked(&trust().release.public()));
}

#[test]
fn a_freshness_statement_cannot_revoke_its_manifests_key() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let (body, m) = manifest_with(&t.release, Channel::Stable, T, |_| {});
    let rot = statement(&t.recovery, 1, Role::Release, &TestKey::new(5).public(), T);
    let mut f = crate::testing::freshness(Channel::Stable, &body, T + 50, T + 20 * DAY);
    with_statements(&mut f, &[rot]);
    let f = t.freshness.signed(&crate::testing::bytes(&f));
    let c = check(
        &envelope(&m, Some(&f)),
        Channel::Stable,
        T + 60,
        &emb,
        &TrustState::default(),
    )
    .unwrap();
    assert_eq!(c.freshness, crate::Freshness::Refused(Error::RevokedKey));
    assert_eq!(c.next.seq, 0);
}
