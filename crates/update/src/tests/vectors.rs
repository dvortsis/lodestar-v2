//! Golden vectors: each envelope in `tests/vectors/check/` is generated
//! from the TEST ONLY keys and checked against its expected outcome.
//! `LODESTAR_BLESS=1 cargo test -p lodestar-update vectors` rewrites them;
//! otherwise a generated envelope that differs from its file fails, so
//! the format can't drift unnoticed.

use semver::Version;
use serde_json::json;

use super::*;
use crate::testing::{envelope, freshness, statement};
use crate::{
    ArtifactKind, Checked, DAY, Embedded, Error, Freshness, Notice, Role, Sha256, Target,
    TrustState, check, install,
};

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/check");

enum Expect {
    Ok {
        current: bool,
        freshness: Option<Result<(), Error>>,
    },
    Err(Error),
}

struct Case {
    name: &'static str,
    envelope: Vec<u8>,
    channel: Channel,
    now: u64,
    embedded: Embedded,
    state: TrustState,
    expect: Expect,
    /// A further assertion on a successful check.
    then: Option<fn(&Checked)>,
}

fn case(name: &'static str, envelope: Vec<u8>, expect: Expect) -> Case {
    Case {
        name,
        envelope,
        channel: Channel::Stable,
        now: T + 100,
        embedded: trust().embedded(vec![]),
        state: TrustState::default(),
        expect,
        then: None,
    }
}

const OK: Expect = Expect::Ok {
    current: true,
    freshness: None,
};

fn marks(channel: Channel, f: impl FnOnce(&mut crate::Marks)) -> TrustState {
    let mut s = TrustState::default();
    f(s.channels.entry(channel).or_default());
    s
}

fn cases() -> Vec<Case> {
    let t = trust();
    let good = signed_manifest(&t.release, Channel::Stable, T, "0.5.3");
    let good_body = testing::bytes(&testing::manifest(Channel::Stable, T, "0.5.3", BINARY));
    let k5 = TestKey::new(5);
    let k6 = TestKey::new(6);
    let k7 = TestKey::new(7);
    let k8 = TestKey::new(8);
    let rot1 = statement(&t.recovery, 1, Role::Release, &k5.public(), T - 50);
    let rot2 = statement(&t.recovery, 2, Role::Release, &k6.public(), T - 40);
    let fresh = |body: &[u8], ts: u64, key: &TestKey| {
        key.signed(&testing::bytes(&freshness(
            Channel::Stable,
            body,
            ts,
            ts + 20 * DAY,
        )))
    };
    let mut v = Vec::new();

    v.push(case("good", envelope(&good, None), OK));
    v.push(case(
        "bad_signature",
        envelope(
            &Signed {
                body: t.release.signed(&good_body[..good_body.len() - 1]).body,
                minisig: good.minisig.clone(),
            },
            None,
        ),
        Expect::Err(Error::BadSignature),
    ));
    v.push(case(
        "bad_global_signature",
        envelope(
            &Signed {
                body: good.body.clone(),
                minisig: good
                    .minisig
                    .replace("trusted comment: test", "trusted comment: tesT"),
            },
            None,
        ),
        Expect::Err(Error::BadGlobalSignature),
    ));
    v.push(case(
        "legacy_ed",
        envelope(
            &Signed {
                body: good.body.clone(),
                minisig: t.release.sign_legacy(&good_body),
            },
            None,
        ),
        Expect::Err(Error::LegacySignature),
    ));
    v.push(case(
        "unknown_key",
        envelope(
            &signed_manifest(&TestKey::new(9), Channel::Stable, T, "0.5.3"),
            None,
        ),
        Expect::Err(Error::UnknownKey),
    ));
    v.push(case(
        "another_channel",
        envelope(
            &signed_manifest(&t.release, Channel::Beta, T, "0.5.3"),
            None,
        ),
        Expect::Err(Error::WrongChannel),
    ));
    v.push(Case {
        channel: Channel::Nightly,
        ..case(
            "nightly_by_release_key",
            envelope(
                &signed_manifest(&t.release, Channel::Nightly, T, "0.5.3"),
                None,
            ),
            Expect::Err(Error::UnknownKey),
        )
    });
    v.push(case(
        "stable_by_nightly_key",
        envelope(
            &signed_manifest(&t.nightly, Channel::Stable, T, "0.5.3"),
            None,
        ),
        Expect::Err(Error::UnknownKey),
    ));
    v.push(Case {
        now: T + 8 * DAY,
        then: Some(|c| {
            assert!(c.notices.contains(&Notice::ClockAhead));
            let v = Version::new(0, 5, 3);
            assert_eq!(
                install(c, &v, Target::X86_64LinuxMusl, ArtifactKind::Binary),
                Err(Error::NotCurrent)
            );
        }),
        ..case(
            "expired",
            envelope(&good, None),
            Expect::Ok {
                current: false,
                freshness: None,
            },
        )
    });
    v.push(case(
        "expiry_over_30_days",
        envelope(
            &manifest_with(&t.release, Channel::Stable, T, |m| {
                m["expires"] = json!(T + 30 * DAY + 1)
            })
            .1,
            None,
        ),
        Expect::Err(Error::BadExpiry),
    ));
    v.push(Case {
        now: T - DAY - 1,
        ..case("ahead", envelope(&good, None), Expect::Err(Error::Ahead))
    });
    v.push(Case {
        state: marks(Channel::Stable, |m| m.hwm_ts = T + 1),
        ..case(
            "older_than_mark",
            envelope(&good, None),
            Expect::Err(Error::Older),
        )
    });
    v.push(Case {
        state: marks(Channel::Stable, |m| {
            m.hwm_ts = T;
            m.hwm_sha256 = Some(Sha256::of(b"another body"));
        }),
        ..case(
            "same_timestamp_other_body",
            envelope(&good, None),
            Expect::Err(Error::SameTimestamp),
        )
    });
    v.push(Case {
        state: marks(Channel::Stable, |m| {
            m.hwm_ts = T;
            m.hwm_sha256 = Some(Sha256::of(&good_body));
        }),
        ..case("refetch_same_body", envelope(&good, None), OK)
    });
    v.push(case(
        "freshness_good",
        envelope(&good, Some(&fresh(&good_body, T + 60, &t.freshness))),
        Expect::Ok {
            current: true,
            freshness: Some(Ok(())),
        },
    ));
    v.push(Case {
        now: T + 10 * DAY,
        then: Some(|c| assert_eq!(c.expires, T + 26 * DAY)),
        ..case(
            "freshness_extends_expiry",
            envelope(&good, Some(&fresh(&good_body, T + 6 * DAY, &t.freshness))),
            Expect::Ok {
                current: true,
                freshness: Some(Ok(())),
            },
        )
    });
    v.push(Case {
        state: marks(Channel::Stable, |m| {
            m.fresh_ts = T + 6 * DAY;
            m.fresh_sha256 = Some(Sha256::of(b"another freshness"));
        }),
        now: T + 6 * DAY,
        ..case(
            "replayed_freshness",
            envelope(&good, Some(&fresh(&good_body, T + 6 * DAY, &t.freshness))),
            Expect::Ok {
                current: true,
                freshness: Some(Err(Error::Replayed)),
            },
        )
    });
    let older_body = testing::bytes(&testing::manifest(Channel::Stable, T - 10, "0.5.2", BINARY));
    v.push(case(
        "freshness_for_older_manifest",
        envelope(&good, Some(&fresh(&older_body, T + 60, &t.freshness))),
        Expect::Ok {
            current: true,
            freshness: Some(Err(Error::ManifestMismatch)),
        },
    ));
    v.push(Case {
        embedded: t.embedded(vec![statement(
            &t.recovery,
            1,
            Role::Freshness,
            &k7.public(),
            T - 10,
        )]),
        ..case(
            "freshness_by_revoked_key",
            envelope(&good, Some(&fresh(&good_body, T + 60, &t.freshness))),
            Expect::Ok {
                current: true,
                freshness: Some(Err(Error::RevokedKey)),
            },
        )
    });
    v.push(Case {
        embedded: Embedded {
            freshness: None,
            ..t.embedded(vec![])
        },
        then: Some(|c| assert!(c.notices.contains(&Notice::FreshnessUnavailable))),
        ..case(
            "freshness_without_a_key",
            envelope(&good, Some(&fresh(&good_body, T + 60, &t.freshness))),
            Expect::Ok {
                current: true,
                freshness: Some(Err(Error::NoKey(Role::Freshness))),
            },
        )
    });
    v.push(Case {
        then: Some(|c| {
            let v = Version::new(0, 5, 3);
            assert_eq!(
                install(c, &v, Target::X86_64LinuxMusl, ArtifactKind::Binary),
                Err(Error::Withdrawn)
            );
        }),
        ..case(
            "withdrawn",
            envelope(
                &manifest_with(&t.release, Channel::Stable, T, |m| {
                    m["releases"][0]["withdrawn"] = json!(true)
                })
                .1,
                None,
            ),
            OK,
        )
    });
    v.push(Case {
        then: Some(|c| {
            assert!(c.manifest.releases[0].manual_steps);
            let v = Version::new(0, 5, 3);
            let pin = install(c, &v, Target::X86_64LinuxMusl, ArtifactKind::Binary).unwrap();
            assert_eq!(pin.sha256, Sha256::of(BINARY));
            assert_eq!(
                install(c, &v, Target::Aarch64LinuxMusl, ArtifactKind::Binary),
                Err(Error::NoArtifact)
            );
        }),
        ..case(
            "needs_manual",
            envelope(
                &manifest_with(&t.release, Channel::Stable, T, |m| {
                    m["releases"][0]["manual_steps"] = json!(true)
                })
                .1,
                None,
            ),
            OK,
        )
    });
    v.push(Case {
        then: Some(|c| {
            let versions: Vec<String> = c
                .manifest
                .releases
                .iter()
                .map(|r| r.version.to_string())
                .collect();
            assert_eq!(versions, ["0.5.1", "0.5.2", "0.6.0"]);
            assert_eq!(c.manifest.releases[2].min_from, Some(Version::new(0, 5, 2)));
        }),
        ..case(
            "skipped_version_chain",
            envelope(
                &manifest_with(&t.release, Channel::Stable, T, |m| {
                    let mut r2 = testing::release("0.5.2", T - 20, BINARY);
                    r2["min_from"] = json!("0.5.1");
                    let mut r3 = testing::release("0.6.0", T - 10, BINARY);
                    r3["min_from"] = json!("0.5.2");
                    r3["level"] = json!("feature");
                    m["releases"] = json!([testing::release("0.5.1", T - 30, BINARY), r2, r3]);
                })
                .1,
                None,
            ),
            OK,
        )
    });
    v.push(Case {
        then: Some(|c| {
            assert_eq!(c.next.seq, 2);
            assert_eq!(c.next.revoked.len(), 2);
        }),
        ..case(
            "two_rotation_chain",
            envelope(
                &manifest_with(&k6, Channel::Stable, T, |m| {
                    with_statements(m, &[rot1.clone(), rot2.clone()])
                })
                .1,
                None,
            ),
            OK,
        )
    });
    v.push(case(
        "gap",
        envelope(
            &manifest_with(&k6, Channel::Stable, T, |m| {
                with_statements(m, std::slice::from_ref(&rot2))
            })
            .1,
            None,
        ),
        Expect::Err(Error::Gap),
    ));
    v.push(Case {
        embedded: t.embedded(vec![rot1.clone()]),
        then: None,
        ..case(
            "equivocating_statement",
            envelope(
                &manifest_with(&k8, Channel::Stable, T, |m| {
                    with_statements(
                        m,
                        &[statement(
                            &t.recovery,
                            1,
                            Role::Release,
                            &k8.public(),
                            T - 50,
                        )],
                    )
                })
                .1,
                None,
            ),
            Expect::Err(Error::Equivocation),
        )
    });
    v.push(Case {
        channel: Channel::Nightly,
        embedded: Embedded {
            nightly: None,
            ..t.embedded(vec![])
        },
        then: Some(|c| assert_eq!(c.next.seq, 1)),
        ..case(
            "introduction_statement",
            envelope(
                &manifest_with(&k8, Channel::Nightly, T, |m| {
                    with_statements(
                        m,
                        &[statement(
                            &t.recovery,
                            1,
                            Role::Nightly,
                            &k8.public(),
                            T - 50,
                        )],
                    )
                })
                .1,
                None,
            ),
            OK,
        )
    });
    v.push(Case {
        channel: Channel::Nightly,
        embedded: Embedded {
            nightly: None,
            ..t.embedded(vec![])
        },
        then: None,
        ..case(
            "nightly_without_a_key",
            envelope(
                &signed_manifest(&t.nightly, Channel::Nightly, T, "0.5.3"),
                None,
            ),
            Expect::Err(Error::NoKey(Role::Nightly)),
        )
    });
    v.push(case(
        "release_key_only_statement",
        envelope(
            &manifest_with(&k5, Channel::Stable, T, |m| {
                with_statements(
                    m,
                    &[statement(
                        &t.release,
                        1,
                        Role::Release,
                        &k5.public(),
                        T - 50,
                    )],
                )
            })
            .1,
            None,
        ),
        Expect::Err(Error::NotRecoverySigned),
    ));
    v.push(case(
        "statement_ahead",
        envelope(
            &manifest_with(&k5, Channel::Stable, T, |m| {
                with_statements(
                    m,
                    &[statement(
                        &t.recovery,
                        1,
                        Role::Release,
                        &k5.public(),
                        T + 100 + DAY + 1,
                    )],
                )
            })
            .1,
            None,
        ),
        Expect::Err(Error::Ahead),
    ));
    v.push(case(
        "newer_format",
        envelope(
            &manifest_with(&t.release, Channel::Stable, T, |m| {
                m["format"] = json!(2);
                m["new_field"] = json!(true);
            })
            .1,
            None,
        ),
        Expect::Err(Error::NewerFormat),
    ));
    v.push(case(
        "checksums_signature_is_not_a_manifest",
        envelope(&t.release.signed(b"0123  lodestar\n"), None),
        Expect::Err(Error::BadDocument),
    ));
    v.push(case(
        "statement_as_manifest",
        envelope(
            &Signed {
                body: rot1.body.clone(),
                minisig: t.release.sign(
                    &crate::doc::decode(&rot1.body, &rot1.minisig, 4096)
                        .unwrap()
                        .0,
                ),
            },
            None,
        ),
        Expect::Err(Error::WrongType),
    ));
    v
}

#[test]
fn vectors() {
    let bless = std::env::var_os("LODESTAR_BLESS").is_some();
    std::fs::create_dir_all(DIR).unwrap();
    let cases = cases();
    let mut names: Vec<&str> = cases.iter().map(|c| c.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), cases.len(), "duplicate vector names");
    for c in cases {
        let path = format!("{DIR}/{}.json", c.name);
        if bless {
            std::fs::write(&path, &c.envelope).unwrap();
        }
        let file = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{path}: {e} (bless with LODESTAR_BLESS=1)"));
        assert_eq!(
            file, c.envelope,
            "{}: vector differs (bless with LODESTAR_BLESS=1)",
            c.name
        );
        let got = check(&file, c.channel, c.now, &c.embedded, &c.state);
        match (&c.expect, &got) {
            (Expect::Err(e), Err(g)) => assert_eq!(e, g, "{}", c.name),
            (Expect::Ok { current, freshness }, Ok(g)) => {
                assert_eq!(*current, g.current, "{}: current", c.name);
                let f = match &g.freshness {
                    Freshness::Absent => None,
                    Freshness::Accepted { .. } => Some(Ok(())),
                    Freshness::Refused(e) => Some(Err(*e)),
                };
                assert_eq!(*freshness, f, "{}: freshness", c.name);
                // A check that succeeded succeeds again on its own next
                // state (a refetch), with the same result.
                let again = check(&file, c.channel, c.now, &c.embedded, &g.next).unwrap();
                assert_eq!(again.manifest, g.manifest, "{}", c.name);
                assert_eq!(
                    TrustState::merge(&g.next, again.next.clone()),
                    again.next,
                    "{}",
                    c.name
                );
                if let Some(then) = c.then {
                    then(g);
                }
            }
            _ => panic!("{}: expected {}, got {got:?}", c.name, describe(&c.expect)),
        }
    }
}

fn describe(e: &Expect) -> String {
    match e {
        Expect::Ok { .. } => "Ok".into(),
        Expect::Err(e) => e.code().into(),
    }
}
