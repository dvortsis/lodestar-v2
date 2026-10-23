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
        chain.clone().extend(&[s1.clone(), equal], Some(T)),
        Err(Error::Equivocation)
    );
    // Out of order is refused, a tail that doesn't start at 1 is a gap,
    // and a reused key is refused.
    assert_eq!(
        chain
            .clone()
            .extend(&[s1.clone(), s2.clone(), s1.clone()], Some(T)),
        Err(Error::BadDocument)
    );
    assert_eq!(
        chain.clone().extend(std::slice::from_ref(&s2), Some(T)),
        Err(Error::Gap)
    );
    let reuse = statement(&t.recovery, 3, Role::Freshness, &t.release.public(), T);
    assert_eq!(
        chain
            .clone()
            .extend(&[s1.clone(), s2.clone(), reuse], Some(T)),
        Err(Error::KeyReused)
    );
    // A failed extension leaves the chain unchanged.
    let s3 = statement(&t.recovery, 3, Role::Freshness, &k7.public(), T);
    let bad = statement(&t.release, 4, Role::Release, &TestKey::new(9).public(), T);
    assert_eq!(
        chain.extend(&[s1, s2, s3, bad], Some(T)),
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
    let bytes = state.to_bytes().unwrap();
    assert_eq!(TrustState::load(&bytes, &emb).unwrap(), state);

    // Dropping the statement, editing `revoked` or `seq`, or an unknown
    // field: all damage.
    let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for edit in [
        |v: &mut serde_json::Value| v["statements"] = serde_json::json!([]),
        |v: &mut serde_json::Value| v["revoked"] = serde_json::json!(["not a key"]),
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
    let mark = |h, f| Marks {
        hwm_ts: h,
        fresh_ts: f,
        ..Marks::default()
    };
    let mut old = TrustState::default();
    old.channels.insert(Channel::Stable, mark(10, 0));
    let mut lower = old.clone();
    lower.channels.insert(Channel::Stable, mark(9, 0));
    assert_eq!(TrustState::merge(&old, lower), old);
    let mut missing = old.clone();
    missing.channels.clear();
    assert_eq!(TrustState::merge(&old, missing), old);
    let mut higher = old.clone();
    higher.channels.insert(Channel::Beta, mark(0, 3));
    assert_eq!(TrustState::merge(&old, higher.clone()), higher);
    // A lower sequence never wins.
    let t = trust();
    let s1 = statement(
        &t.recovery,
        1,
        Role::Freshness,
        &TestKey::new(5).public(),
        4,
    );
    let ahead = TrustState {
        seq: 1,
        statements: vec![s1.clone()],
        ..TrustState::default()
    };
    assert_eq!(TrustState::merge(&ahead, old.clone()), ahead);
}

/// The runner's case: Beta's mark is T + 5000 on disk; a check built
/// from a stale state (Beta at T) accepts a manifest carrying a freshness
/// rotation. The rotation resets only freshness marks, so Beta stays.
#[test]
fn a_stale_check_with_a_rotation_cannot_lower_other_marks() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let mark = |h, f| Marks {
        hwm_ts: h,
        fresh_ts: f,
        ..Marks::default()
    };
    let mut disk = TrustState::default();
    disk.channels
        .insert(Channel::Beta, mark(T + 5000, T + 5000));
    disk.channels.insert(Channel::Stable, mark(T, T + 5000));
    let mut stale = TrustState::default();
    stale.channels.insert(Channel::Beta, mark(T, 0));
    let frot = statement(
        &t.recovery,
        1,
        Role::Freshness,
        &TestKey::new(6).public(),
        T + 40,
    );
    let (_, m) = manifest_with(&t.release, Channel::Stable, T + 50, |m| {
        with_statements(m, std::slice::from_ref(&frot))
    });
    let next = check(&envelope(&m, None), Channel::Stable, T + 60, &emb, &stale)
        .unwrap()
        .next;
    let merged = TrustState::merge(&disk, next);
    assert_eq!(merged.seq, 1);
    assert_eq!(merged.marks(Channel::Beta).hwm_ts, T + 5000);
    assert_eq!(merged.marks(Channel::Stable).hwm_ts, T + 50);
    // The freshness rotation's reset applies to every channel.
    for c in Channel::ALL {
        assert_eq!(merged.marks(c).fresh_ts, T + 40, "{c:?}");
    }
    // A release rotation from a stale check resets Stable and Beta only.
    let mut disk = TrustState::default();
    disk.channels.insert(Channel::Nightly, mark(T + 9000, 0));
    disk.channels.insert(Channel::Beta, mark(T + 9000, 0));
    let k7 = TestKey::new(7);
    let rot = statement(&t.recovery, 1, Role::Release, &k7.public(), T + 10);
    let (_, m) = manifest_with(&k7, Channel::Stable, T + 50, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let next = check(
        &envelope(&m, None),
        Channel::Stable,
        T + 60,
        &emb,
        &TrustState::default(),
    )
    .unwrap()
    .next;
    let merged = TrustState::merge(&disk, next);
    assert_eq!(merged.marks(Channel::Nightly).hwm_ts, T + 9000);
    assert_eq!(merged.marks(Channel::Beta).hwm_ts, T + 10);
    assert_eq!(merged.marks(Channel::Stable).hwm_ts, T + 50);
}

/// Two builds whose generation 0 differs only in the nightly key: each
/// loads the other's `trust.json`, and the revocation the build with the
/// key recorded survives in the one without it.
#[test]
fn trust_json_loads_across_different_generation_zero_keys() {
    let t = trust();
    let with = t.embedded(vec![]);
    let without = crate::Embedded {
        nightly: None,
        ..t.embedded(vec![])
    };
    let n2 = TestKey::new(9);
    let rot = statement(&t.recovery, 1, Role::Nightly, &n2.public(), T);
    let (_, m) = manifest_with(&n2, Channel::Nightly, T + 10, |m| {
        with_statements(m, std::slice::from_ref(&rot))
    });
    let env = envelope(&m, None);
    let a = check(
        &env,
        Channel::Nightly,
        T + 20,
        &with,
        &TrustState::default(),
    )
    .unwrap()
    .next;
    let b = check(
        &env,
        Channel::Nightly,
        T + 20,
        &without,
        &TrustState::default(),
    )
    .unwrap()
    .next;
    assert!(a.revokes(&t.nightly.public()));
    assert!(!b.revokes(&t.nightly.public()));
    let a_in_b = TrustState::load(&a.to_bytes().unwrap(), &without).unwrap();
    let b_in_a = TrustState::load(&b.to_bytes().unwrap(), &with).unwrap();
    assert!(a_in_b.revokes(&t.nightly.public()));
    // The older build checks again and keeps the newer one's revocation.
    let again = check(&env, Channel::Nightly, T + 30, &without, &a_in_b)
        .unwrap()
        .next;
    assert!(TrustState::merge(&a_in_b, again).revokes(&t.nightly.public()));
    // The newer build reading the older file derives its own revocation.
    let again = check(&env, Channel::Nightly, T + 30, &with, &b_in_a)
        .unwrap()
        .next;
    assert!(again.revokes(&t.nightly.public()));
    // The old nightly key stays refused in the build that knew it.
    let old = signed_manifest(&t.nightly, Channel::Nightly, T + 100, "0.5.4");
    assert_eq!(
        check(
            &envelope(&old, None),
            Channel::Nightly,
            T + 200,
            &with,
            &b_in_a
        ),
        Err(Error::RevokedKey)
    );
}

#[test]
fn marks_far_ahead_of_the_clock_mean_the_clock_is_behind() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let mut s = TrustState::default();
    s.channels.insert(
        Channel::Beta,
        Marks {
            hwm_ts: T + 3 * DAY,
            ..Marks::default()
        },
    );
    let m = signed_manifest(&t.release, Channel::Stable, T, "0.5.3");
    assert_eq!(
        check(&envelope(&m, None), Channel::Stable, T + 100, &emb, &s),
        Err(Error::Ahead)
    );
    assert_eq!(Error::Ahead.notice(), Some(crate::Notice::ClockBehind));
    assert!(
        check(
            &envelope(&m, None),
            Channel::Stable,
            T + 2 * DAY + 1,
            &emb,
            &s
        )
        .is_ok()
    );
}

#[test]
fn stored_statements_are_normalised_and_the_chain_is_capped() {
    let t = trust();
    let k5 = TestKey::new(5);
    let mut padded = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    padded.minisig = padded
        .minisig
        .replace("signature from a TEST ONLY key", &"x".repeat(400));
    padded.cosig = Some(t.release.sign(b"padding"));
    let mut chain = Chain::genesis(&t.embedded(vec![]));
    chain.extend(std::slice::from_ref(&padded), None).unwrap();
    let stored = &chain.signed()[0];
    assert_eq!(stored.cosig, None);
    assert!(!stored.minisig.contains("xxxx"));
    // A known statement followed by a skip is a gap.
    let s3 = statement(&t.recovery, 3, Role::Release, &TestKey::new(6).public(), T);
    assert_eq!(
        chain.clone().extend(&[padded.clone(), s3], None),
        Err(Error::Gap)
    );

    // The chain holds at most MAX_CHAIN statements over its life.
    let max = crate::chain::MAX_CHAIN as u64;
    let all: Vec<SignedStatement> = (1..=max + 1)
        .map(|i| {
            statement(
                &t.recovery,
                i,
                Role::Nightly,
                &TestKey::new(100 + i as u8).public(),
                T,
            )
        })
        .collect();
    let mut chain = Chain::genesis(&t.embedded(vec![]));
    assert_eq!(chain.extend(&all[..max as usize], None), Ok(max as usize));
    let state = TrustState::default().advance(&chain);
    assert!(state.to_bytes().unwrap().len() <= crate::trust::MAX_TRUST);
    assert!(TrustState::load(&state.to_bytes().unwrap(), &t.embedded(vec![])).is_ok());
    let mut chain = Chain::genesis(&t.embedded(vec![]));
    let mut long = all[..max as usize].to_vec();
    long.push(all[max as usize].clone());
    assert_eq!(chain.extend(&long, None), Err(Error::BadDocument));
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

#[test]
fn merge_and_load_refuse_what_does_not_extend() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let s = |seq, k: u8| {
        statement(
            &t.recovery,
            seq,
            Role::Release,
            &TestKey::new(k).public(),
            T,
        )
    };
    let old = TrustState {
        seq: 1,
        statements: vec![s(1, 5)],
        ..TrustState::default()
    };
    // A higher sequence whose statements don't extend the old ones loses.
    let other = TrustState {
        seq: 2,
        statements: vec![s(1, 6), s(2, 7)],
        ..TrustState::default()
    };
    assert_eq!(TrustState::merge(&old, other), old);
    let longer = TrustState {
        seq: 2,
        statements: vec![s(1, 5), s(2, 7)],
        ..TrustState::default()
    };
    let merged = TrustState::merge(&old, longer.clone());
    assert_eq!((merged.seq, &merged.statements), (2, &longer.statements));
    // A channel named twice is damage.
    let dup = br#"{"format":1,"seq":0,"statements":[],"revoked":[],"channels":{"stable":{"hwm_ts":5,"hwm_sha256":null,"fresh_ts":0,"fresh_sha256":null},"stable":{"hwm_ts":1,"hwm_sha256":null,"fresh_ts":0,"fresh_sha256":null}}}"#;
    assert_eq!(TrustState::load(dup, &emb), Err(Error::TrustDamaged));
    assert!(TrustState::load(&dup.replace_second_stable(), &emb).is_ok());
}

trait ReplaceSecond {
    fn replace_second_stable(&self) -> Vec<u8>;
}

impl ReplaceSecond for [u8] {
    fn replace_second_stable(&self) -> Vec<u8> {
        let s = std::str::from_utf8(self).unwrap();
        let i = s.rfind("\"stable\"").unwrap();
        format!("{}\"beta\"{}", &s[..i], &s[i + 8..]).into_bytes()
    }
}

/// An honest state checked on a host whose clock is two days behind (a
/// NAS before time sync): `clock_behind`, never damage, and the marks
/// still hold once the clock is right.
#[test]
fn a_clock_behind_is_not_damage() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let m = signed_manifest(&t.release, Channel::Stable, T, "0.5.3");
    let env = envelope(&m, None);
    let state = check(&env, Channel::Stable, T + 10, &emb, &TrustState::default())
        .unwrap()
        .next;
    assert_eq!(
        check(&env, Channel::Stable, T - 2 * DAY, &emb, &state),
        Err(Error::Ahead)
    );
    let older = signed_manifest(&t.release, Channel::Stable, T - 5, "0.5.2");
    assert_eq!(
        check(
            &envelope(&older, None),
            Channel::Stable,
            T + 20,
            &emb,
            &state
        ),
        Err(Error::Older)
    );
}

/// A valid `trust.json` whose statements are in another textual form
/// (another untrusted comment, a cosig) loads normalised and keeps
/// merging forward.
#[test]
fn an_unnormalised_trust_json_keeps_updating() {
    let t = trust();
    let emb = t.embedded(vec![]);
    let k5 = TestKey::new(5);
    let mut s1 = statement(&t.recovery, 1, Role::Release, &k5.public(), T);
    s1.minisig = s1
        .minisig
        .replace("signature from a TEST ONLY key", "hand made");
    s1.cosig = Some(t.release.sign(b"x"));
    let mut disk = TrustState {
        seq: 1,
        statements: vec![s1.clone()],
        ..TrustState::default()
    };
    disk.channels.insert(
        Channel::Stable,
        Marks {
            hwm_ts: T + 10,
            ..Marks::default()
        },
    );
    let loaded = TrustState::load(&disk.to_bytes().unwrap(), &emb).unwrap();
    assert_ne!(loaded.statements, disk.statements);
    let k6 = TestKey::new(6);
    let s2 = statement(&t.recovery, 2, Role::Nightly, &k6.public(), T);
    let (_, m) = manifest_with(&k5, Channel::Stable, T + 30, |m| {
        with_statements(m, &[s1.clone(), s2])
    });
    let next = check(&envelope(&m, None), Channel::Stable, T + 40, &emb, &loaded)
        .unwrap()
        .next;
    // Merging into the file exactly as stored works too.
    for old in [&loaded, &disk] {
        let merged = TrustState::merge(old, next.clone());
        assert_eq!(merged.seq, 2);
        assert_eq!(merged.marks(Channel::Stable).hwm_ts, T + 30);
    }
}

#[test]
fn merge_keeps_a_stored_digest_and_takes_later_marks_at_the_same_sequence() {
    let sha = Some(Sha256::of(b"stored"));
    let mut old = TrustState::default();
    old.channels.insert(
        Channel::Stable,
        Marks {
            hwm_ts: 10,
            hwm_sha256: sha,
            fresh_ts: 5,
            ..Marks::default()
        },
    );
    old.channels.insert(
        Channel::Beta,
        Marks {
            hwm_ts: 7,
            ..Marks::default()
        },
    );
    let mut new = TrustState::default();
    new.channels.insert(
        Channel::Stable,
        Marks {
            hwm_ts: 10,
            hwm_sha256: None,
            fresh_ts: 9,
            ..Marks::default()
        },
    );
    new.channels.insert(
        Channel::Beta,
        Marks {
            hwm_ts: 3,
            ..Marks::default()
        },
    );
    let merged = TrustState::merge(&old, new);
    let stable = merged.marks(Channel::Stable);
    assert_eq!(
        (stable.hwm_ts, stable.hwm_sha256, stable.fresh_ts),
        (10, sha, 9)
    );
    assert_eq!(merged.marks(Channel::Beta).hwm_ts, 7);
}
