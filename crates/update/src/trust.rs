//! The trust state (`<data>/bin/trust.json`; its file I/O is the
//! updater's): the accepted key statements, the sequence, the revoked
//! keys and each channel's marks. It lives outside the store and every
//! snapshot, so no rollback undoes a revocation.
//!
//! **Forward only:** a new state replaces the old only if its sequence is
//! higher, or equal with the same statements and no channel's marks
//! lower. `merge` enforces it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::chain::Chain;
use crate::doc::{self, Sha256, SignedStatement};
use crate::keys::Embedded;
use crate::minisign::PublicKey;
use crate::{Channel, Error, Role};

/// `trust.json` is at most 256 KiB.
pub const MAX_TRUST: usize = 256 * 1024;
/// The trust state's format.
pub const TRUST_FORMAT: u64 = 1;

/// One channel's marks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marks {
    /// The high-water mark: the newest accepted manifest's own timestamp
    /// (or a rotation's, which resets it). 0 is none.
    pub hwm_ts: u64,
    /// The SHA-256 of the manifest body at the mark; null after a reset.
    pub hwm_sha256: Option<Sha256>,
    /// The freshness mark: the newest accepted freshness timestamp.
    pub fresh_ts: u64,
    /// The SHA-256 of the freshness body at the mark; null after a reset.
    pub fresh_sha256: Option<Sha256>,
}

impl Marks {
    /// Whether `ts` with body digest `sha` may pass a mark: newer, or the
    /// same timestamp with the same body (a refetch). After a reset there
    /// is no digest, so the reset time itself passes.
    pub(crate) fn passes(mark: u64, mark_sha: Option<Sha256>, ts: u64, sha: Sha256) -> bool {
        ts > mark || (ts == mark && mark_sha.is_none_or(|m| m == sha))
    }

    fn not_lower_than(&self, old: &Marks) -> bool {
        self.hwm_ts >= old.hwm_ts && self.fresh_ts >= old.fresh_ts
    }
}

/// The trust state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustState {
    pub format: u64,
    pub seq: u64,
    pub statements: Vec<SignedStatement>,
    /// The revoked keys (minisign base64 lines), derived from the
    /// statements; kept for people reading the file and checked on load.
    pub revoked: Vec<String>,
    pub channels: BTreeMap<Channel, Marks>,
}

impl Default for TrustState {
    fn default() -> Self {
        Self {
            format: TRUST_FORMAT,
            seq: 0,
            statements: Vec::new(),
            revoked: Vec::new(),
            channels: BTreeMap::new(),
        }
    }
}

/// A verified overlay's contribution to a rebuilt state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayMark {
    pub channel: Channel,
    pub timestamp: u64,
    pub sha256: Sha256,
    pub statements: Vec<SignedStatement>,
}

impl TrustState {
    /// Parses `trust.json` and checks it against the embedded root: every
    /// statement verifies in sequence and `seq` and `revoked` agree with
    /// them. Anything else is `TrustDamaged`, which blocks installs.
    pub fn load(bytes: &[u8], embedded: &Embedded) -> Result<Self, Error> {
        if bytes.len() > MAX_TRUST {
            return Err(Error::TrustDamaged);
        }
        let state: TrustState = doc::strict(bytes).map_err(|_| Error::TrustDamaged)?;
        state.chain(embedded)?;
        Ok(state)
    }

    /// The bytes to write (pretty JSON, with a final newline).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec_pretty(self).unwrap_or_default();
        out.push(b'\n');
        out
    }

    /// The chain these statements make from the embedded generation 0
    /// (statements alone, without the embedded ones), checked against
    /// `seq` and `revoked`.
    pub fn chain(&self, embedded: &Embedded) -> Result<Chain, Error> {
        if self.format != TRUST_FORMAT {
            return Err(Error::TrustDamaged);
        }
        let mut chain = Chain::genesis(embedded);
        chain
            .extend(&self.statements, None)
            .map_err(|_| Error::TrustDamaged)?;
        if chain.seq() != self.seq || revoked_lines(&chain) != self.revoked {
            return Err(Error::TrustDamaged);
        }
        Ok(chain)
    }

    /// The state for a chain that extends this one: the statements past
    /// this state's sequence are applied, each rotation resetting the
    /// marks its role governs (a release or nightly rotation the
    /// high-water mark of the channels that key signs, a freshness
    /// rotation every freshness mark) to the statement's timestamp.
    pub fn advance(&self, chain: &Chain) -> TrustState {
        let mut next = self.clone();
        for a in chain.since(self.seq) {
            match a.role {
                Role::Release | Role::Nightly => {
                    for c in a.role.channels() {
                        let m = next.channels.entry(*c).or_default();
                        m.hwm_ts = a.timestamp;
                        m.hwm_sha256 = None;
                    }
                }
                Role::Freshness => {
                    for c in Channel::ALL {
                        let m = next.channels.entry(c).or_default();
                        m.fresh_ts = a.timestamp;
                        m.fresh_sha256 = None;
                    }
                }
            }
        }
        if chain.seq() > self.seq {
            next.seq = chain.seq();
            next.statements = chain.signed();
            next.revoked = revoked_lines(chain);
        }
        next
    }

    /// A state rebuilt when `trust.json` is missing: the floor (embedded
    /// statements, then those of every verified overlay), with each
    /// channel's high-water mark at its newest verified overlay manifest.
    pub fn rebuild(embedded: &Embedded, overlays: &[OverlayMark]) -> Result<TrustState, Error> {
        let sources: Vec<&[SignedStatement]> = overlays.iter().map(|o| &o.statements[..]).collect();
        let chain = Chain::from_sources(embedded, &sources)?;
        let mut state = TrustState::default().advance(&chain);
        for o in overlays {
            let m = state.channels.entry(o.channel).or_default();
            if o.timestamp > m.hwm_ts {
                m.hwm_ts = o.timestamp;
                m.hwm_sha256 = Some(o.sha256);
            }
        }
        Ok(state)
    }

    /// The forward-only merge: `new` replaces `old` only if its sequence
    /// is higher, or equal with the same statements and no channel's
    /// marks lower. Otherwise `old` stays.
    pub fn merge(old: &TrustState, new: TrustState) -> TrustState {
        let forward = new.seq > old.seq
            || (new.seq == old.seq
                && new.statements == old.statements
                && old
                    .channels
                    .iter()
                    .all(|(c, m)| new.channels.get(c).is_some_and(|n| n.not_lower_than(m))));
        if forward { new } else { old.clone() }
    }

    /// Whether this state's statements revoke `key`.
    pub fn revokes(&self, key: &PublicKey) -> bool {
        self.revoked.contains(&key.to_base64())
    }

    /// The marks for a channel (all zero if none yet).
    pub fn marks(&self, channel: Channel) -> Marks {
        self.channels.get(&channel).copied().unwrap_or_default()
    }
}

fn revoked_lines(chain: &Chain) -> Vec<String> {
    chain.revoked().iter().map(PublicKey::to_base64).collect()
}
