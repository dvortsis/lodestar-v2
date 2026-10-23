//! The key chain: generation 0 (the embedded keys) plus the accepted
//! recovery-signed key statements, in sequence.
//!
//! A statement is accepted only with a valid recovery signature, a
//! `timestamp` at most a day ahead (when a clock is given) and a `seq`
//! exactly one above the current sequence (one sequence for all roles).
//! Accepting it revokes the previous key of that role only. A document's
//! statements at or below the current sequence must equal the accepted
//! ones byte for byte; a different one is equivocation, and a skip (or a
//! list that doesn't start at 1) is a gap. Both refuse the whole extension.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::doc::{self, MAX_STATEMENT, MAX_STATEMENTS, SignedStatement, StatementBody};
use crate::keys::Embedded;
use crate::minisign::{PublicKey, Signature};
use crate::{Error, MAX_AHEAD, Role};

/// An accepted key statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub seq: u64,
    pub role: Role,
    pub key: PublicKey,
    pub timestamp: u64,
    /// The statement, normalised: the body re-encoded, the recovery
    /// signature with a fixed untrusted comment, no cosig. What a state
    /// stores is never padded by whoever carried it.
    pub signed: SignedStatement,
    body: Vec<u8>,
}

/// The chain of keys from the embedded generation 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    recovery: PublicKey,
    gen0: [Option<PublicKey>; 3],
    accepted: Vec<Accepted>,
}

impl Chain {
    /// Generation 0 alone: the embedded keys, no statements.
    pub fn genesis(embedded: &Embedded) -> Self {
        Self {
            recovery: embedded.recovery,
            gen0: [embedded.release, embedded.nightly, embedded.freshness],
            accepted: Vec::new(),
        }
    }

    /// The embedded keys and statements: this build's own floor.
    pub fn embedded(embedded: &Embedded) -> Result<Self, Error> {
        let mut chain = Self::genesis(embedded);
        chain.extend(&embedded.statements, None)?;
        Ok(chain)
    }

    /// Generation 0 extended by each source in turn (shortest first, so
    /// a shorter source never looks like a gap after a longer one). The
    /// sources are already trusted copies (embedded, `trust.json`,
    /// verified overlays), so no clock applies.
    pub fn from_sources(
        embedded: &Embedded,
        sources: &[&[SignedStatement]],
    ) -> Result<Self, Error> {
        let mut chain = Self::genesis(embedded);
        let mut sources: Vec<&[SignedStatement]> = sources.to_vec();
        sources.push(&embedded.statements);
        sources.sort_by_key(|s| s.len());
        for s in sources {
            chain.extend(s, None)?;
        }
        Ok(chain)
    }

    /// The current sequence number (0 with no statements).
    pub fn seq(&self) -> u64 {
        self.accepted.len() as u64
    }

    pub fn recovery(&self) -> PublicKey {
        self.recovery
    }

    pub fn statements(&self) -> &[Accepted] {
        &self.accepted
    }

    /// The accepted statements as carried.
    pub fn signed(&self) -> Vec<SignedStatement> {
        self.accepted.iter().map(|a| a.signed.clone()).collect()
    }

    /// The current key of `role`, if it has one.
    pub fn current(&self, role: Role) -> Option<PublicKey> {
        self.accepted
            .iter()
            .rev()
            .find(|a| a.role == role)
            .map(|a| a.key)
            .or(self.gen0[role.index()])
    }

    /// Every key `role` has had, oldest first.
    pub fn keys(&self, role: Role) -> Vec<PublicKey> {
        self.gen0[role.index()]
            .into_iter()
            .chain(
                self.accepted
                    .iter()
                    .filter(|a| a.role == role)
                    .map(|a| a.key),
            )
            .collect()
    }

    /// The role a key has had, if any.
    pub fn role_of(&self, key: &PublicKey) -> Option<Role> {
        Role::ALL.into_iter().find(|r| self.keys(*r).contains(key))
    }

    /// Whether `key` was a key of some role and has been replaced.
    pub fn is_revoked(&self, key: &PublicKey) -> bool {
        self.revoked().contains(key)
    }

    /// Every replaced key, in the order they were replaced.
    pub fn revoked(&self) -> Vec<PublicKey> {
        let mut out = Vec::new();
        let mut current = self.gen0;
        for a in &self.accepted {
            if let Some(old) = current[a.role.index()] {
                out.push(old);
            }
            current[a.role.index()] = Some(a.key);
        }
        out
    }

    /// The statements accepted after sequence `seq`.
    pub fn since(&self, seq: u64) -> &[Accepted] {
        let from = usize::try_from(seq)
            .unwrap_or(usize::MAX)
            .min(self.accepted.len());
        &self.accepted[from..]
    }

    /// Extends the chain with a document's statements: the whole chain,
    /// from `seq` 1 in ascending order (so any verifier can follow it,
    /// whatever its own sequence). All or nothing. Returns how many were
    /// new.
    pub fn extend(
        &mut self,
        statements: &[SignedStatement],
        now: Option<u64>,
    ) -> Result<usize, Error> {
        if statements.len() > MAX_STATEMENTS {
            return Err(Error::BadDocument);
        }
        let mut next = self.clone();
        let mut last = 0;
        let mut added = 0;
        for s in statements {
            let (body, sig) = doc::decode(&s.body, &s.minisig, MAX_STATEMENT)?;
            if let Some(c) = &s.cosig {
                // Optional and never sufficient: only its form is checked.
                Signature::parse(c)?;
            }
            // A statement we already hold is known by its bytes.
            if let Some(known) = next.accepted.iter().find(|a| a.body == body) {
                last = in_order(last, known.seq)?;
                continue;
            }
            if sig.key_number() != next.recovery.number() {
                return Err(Error::NotRecoverySigned);
            }
            next.recovery.verify(&body, &sig).map_err(|e| match e {
                Error::WrongKey => Error::NotRecoverySigned,
                e => e,
            })?;
            let parsed = StatementBody::parse(&body)?;
            last = in_order(last, parsed.seq)?;
            if parsed.seq <= next.seq() {
                return Err(Error::Equivocation);
            }
            if parsed.seq != next.seq() + 1 {
                return Err(Error::Gap);
            }
            if next.accepted.len() >= MAX_CHAIN {
                return Err(Error::BadDocument);
            }
            if now.is_some_and(|now| parsed.timestamp > now.saturating_add(MAX_AHEAD)) {
                return Err(Error::Ahead);
            }
            let key = PublicKey::from_base64(&parsed.key)?;
            if next.used(&key) {
                return Err(Error::KeyReused);
            }
            next.accepted.push(Accepted {
                seq: parsed.seq,
                role: parsed.role,
                key,
                timestamp: parsed.timestamp,
                signed: SignedStatement {
                    body: STANDARD.encode(&body),
                    minisig: sig.to_file("signature from the Lodestar recovery key"),
                    cosig: None,
                },
                body,
            });
            added += 1;
        }
        *self = next;
        Ok(added)
    }

    /// Whether a key, or its key number, has ever been in the chain.
    fn used(&self, key: &PublicKey) -> bool {
        std::iter::once(Some(self.recovery))
            .chain(self.gen0)
            .flatten()
            .chain(self.accepted.iter().map(|a| a.key))
            .any(|k| k.key() == key.key() || k.number() == key.number())
    }
}

/// The most statements a chain holds over its life (one per rotation of
/// any role). A document can't carry more, and `trust.json` always fits.
pub const MAX_CHAIN: usize = MAX_STATEMENTS;

/// The next position in a document's list: exactly one after the last,
/// starting at 1. Going back is malformed; skipping ahead is a gap.
fn in_order(last: u64, seq: u64) -> Result<u64, Error> {
    if seq <= last {
        return Err(Error::BadDocument);
    }
    if seq != last + 1 {
        return Err(Error::Gap);
    }
    Ok(seq)
}
