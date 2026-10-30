//! The keys and key statements this build embeds (generation 0).
//!
//! `crates/update/keys/` holds copies of `packaging/keys/*.pub`, because
//! the Docker build context has only `crates/`; a test fails on any
//! difference. The freshness key is generation 0 (`FCA9AFC5C8D864FE`).
//! The nightly key doesn't exist yet (`docs/owner-decisions.md` §2.21):
//! until the owner creates it, the Nightly channel verifies nothing,
//! reported as a reason. Either its `.pub` is copied in here and added
//! below, or a recovery-signed statement introduces the role.

use crate::Error;
use crate::doc::{self, SignedStatement};
use crate::minisign::PublicKey;

const RECOVERY: &str = include_str!("../keys/lodestar-recovery.pub");
const RELEASE: &str = include_str!("../keys/lodestar-release.pub");
const NIGHTLY: Option<&str> = None;
const FRESHNESS: Option<&str> = Some(include_str!("../keys/lodestar-freshness.pub"));
const STATEMENTS: &str = include_str!("../keys/statements.json");

/// The trust root a verification starts from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Embedded {
    pub recovery: PublicKey,
    pub release: Option<PublicKey>,
    pub nightly: Option<PublicKey>,
    pub freshness: Option<PublicKey>,
    /// The key statements known when this build was made: the floor's
    /// first layer.
    pub statements: Vec<SignedStatement>,
}

impl Embedded {
    /// This build's own keys and statements.
    pub fn builtin() -> Result<Self, Error> {
        let opt = |k: Option<&str>| k.map(PublicKey::from_file).transpose();
        Ok(Self {
            recovery: PublicKey::from_file(RECOVERY)?,
            release: Some(PublicKey::from_file(RELEASE)?),
            nightly: opt(NIGHTLY)?,
            freshness: opt(FRESHNESS)?,
            statements: doc::strict(STATEMENTS.as_bytes())?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chain, Role};

    #[test]
    fn builtin_parses_and_its_chain_verifies() {
        let e = Embedded::builtin().unwrap();
        assert_eq!(e.recovery.id(), "67922DDF005FD310");
        assert_eq!(e.release.unwrap().id(), "02910813433E81E0");
        assert_eq!(e.freshness.unwrap().id(), "FCA9AFC5C8D864FE");
        let chain = Chain::embedded(&e).unwrap();
        assert_eq!(chain.seq() as usize, e.statements.len());
    }

    /// `packaging/keys/` holds each role's current public key. It must be
    /// the embedded generation-0 copy, or the key this build's embedded
    /// statements lead to (generation 0 never changes after a rotation:
    /// the statements carry the change). The recovery key never changes.
    #[test]
    fn embedded_keys_match_packaging() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/keys");
        let read = |name: &str| std::fs::read_to_string(format!("{dir}/{name}")).ok();
        let Some(recovery) = read("lodestar-recovery.pub") else {
            return; // Not in a full checkout (the Docker context has only crates/).
        };
        assert_eq!(
            recovery, RECOVERY,
            "crates/update/keys/lodestar-recovery.pub differs"
        );
        let e = Embedded::builtin().unwrap();
        let chain = Chain::embedded(&e).unwrap();
        for (role, file) in [
            (Role::Release, "lodestar-release.pub"),
            (Role::Nightly, "lodestar-nightly.pub"),
            (Role::Freshness, "lodestar-freshness.pub"),
        ] {
            let Some(text) = read(file) else {
                assert!(
                    chain.current(role).is_none(),
                    "this build has a {role:?} key but packaging/keys/{file} is missing"
                );
                continue;
            };
            if role == Role::Release && chain.keys(role).len() == 1 {
                // No rotation yet: the copy must be the same file.
                assert_eq!(
                    text, RELEASE,
                    "crates/update/keys/lodestar-release.pub differs"
                );
            }
            if role == Role::Freshness && chain.keys(role).len() == 1 {
                assert_eq!(
                    Some(text.as_str()),
                    FRESHNESS,
                    "crates/update/keys/lodestar-freshness.pub differs"
                );
            }
            let published = PublicKey::from_file(&text).unwrap();
            assert_eq!(
                chain.current(role),
                Some(published),
                "packaging/keys/{file} is neither embedded in crates/update/keys/ nor introduced \
                 by a statement in crates/update/keys/statements.json"
            );
        }
    }
}
