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
use crate::{Channel, Error, MAX_AHEAD, Role};

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
}

/// The trust state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustState {
    pub format: u64,
    pub seq: u64,
    pub statements: Vec<SignedStatement>,
    /// The revoked keys (minisign base64 lines). Each build derives the
    /// revocations its own generation 0 implies and keeps these as well,
    /// so a build with other generation-0 keys (an older image, a
    /// rollback) still honours every revocation a newer one recorded.
    /// Extra entries only add revocations.
    pub revoked: Vec<String>,
    #[serde(deserialize_with = "unique_channels")]
    pub channels: BTreeMap<Channel, Marks>,
}

/// The channel map, refusing a channel named twice.
fn unique_channels<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<Channel, Marks>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<Channel, Marks>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map of channels")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((c, m)) = map.next_entry::<Channel, Marks>()? {
                if out.insert(c, m).is_some() {
                    return Err(serde::de::Error::custom("duplicate channel"));
                }
            }
            Ok(out)
        }
    }
    d.deserialize_map(Unique)
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
    /// statement verifies in sequence, `seq` agrees with them and each
    /// `revoked` entry is a key. Anything else is `TrustDamaged`, which
    /// blocks installs.
    pub fn load(bytes: &[u8], embedded: &Embedded) -> Result<Self, Error> {
        if bytes.len() > MAX_TRUST {
            return Err(Error::TrustDamaged);
        }
        let mut state: TrustState = doc::strict(bytes).map_err(|_| Error::TrustDamaged)?;
        // Normalised, so a file written by hand (or by another layout)
        // merges like one this build wrote.
        state.statements = state.chain(embedded)?.signed();
        Ok(state)
    }

    /// The bytes to write (pretty JSON, with a final newline).
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut out = serde_json::to_vec_pretty(self).map_err(|_| Error::TrustDamaged)?;
        out.push(b'\n');
        Ok(out)
    }

    /// The chain these statements make from the embedded generation 0
    /// (statements alone, without the embedded ones), checked against
    /// `seq`.
    pub fn chain(&self, embedded: &Embedded) -> Result<Chain, Error> {
        if self.format != TRUST_FORMAT
            || self.revoked.len() > MAX_REVOKED
            || self
                .revoked
                .iter()
                .any(|k| PublicKey::from_base64(k).is_err())
        {
            return Err(Error::TrustDamaged);
        }
        let mut chain = Chain::genesis(embedded);
        chain
            .extend(&self.statements, None)
            .map_err(|_| Error::TrustDamaged)?;
        if chain.seq() != self.seq {
            return Err(Error::TrustDamaged);
        }
        Ok(chain)
    }

    /// Marks more than a day ahead of `now` mean the clock is behind:
    /// every mark is a verified timestamp, accepted at most a day ahead.
    /// That's `Ahead` (notice `clock_behind`); the file is left alone, so
    /// the marks still hold once the clock is right.
    pub fn marks_plausible(&self, now: u64) -> Result<(), Error> {
        let limit = now.saturating_add(MAX_AHEAD);
        if self
            .channels
            .values()
            .any(|m| m.hwm_ts > limit || m.fresh_ts > limit)
        {
            return Err(Error::Ahead);
        }
        Ok(())
    }

    fn reset(&mut self, role: Role, timestamp: u64) {
        match role {
            Role::Release | Role::Nightly => {
                for c in role.channels() {
                    let m = self.channels.entry(*c).or_default();
                    m.hwm_ts = timestamp;
                    m.hwm_sha256 = None;
                }
            }
            Role::Freshness => {
                for c in Channel::ALL {
                    let m = self.channels.entry(c).or_default();
                    m.fresh_ts = timestamp;
                    m.fresh_sha256 = None;
                }
            }
        }
    }

    /// The state for a chain that extends this one: the statements past
    /// this state's sequence are applied, each rotation resetting the
    /// marks its role governs (a release or nightly rotation the
    /// high-water mark of the channels that key signs, a freshness
    /// rotation every freshness mark) to the statement's timestamp.
    pub fn advance(&self, chain: &Chain) -> TrustState {
        let mut next = self.clone();
        for a in chain.since(self.seq) {
            next.reset(a.role, a.timestamp);
        }
        if chain.seq() > self.seq {
            next.seq = chain.seq();
            next.statements = chain.signed();
        }
        next.revoked = union(&self.revoked, &revoked_lines(chain));
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

    /// The forward-only merge of a new state (from a check) into the one
    /// on disk. Statements must extend the old ones. With a higher
    /// sequence, the old marks take only the resets of the new statements
    /// (a rotation's reset is intended: it lets a client a leaked key froze
    /// recover), then each mark is the later of the two, so a check that
    /// started from a stale state can't lower a channel the rotation
    /// doesn't govern. With the same sequence (and the same statements),
    /// each mark is likewise the later of the two. On a tie a stored
    /// digest is kept. Statements compare by their signed bodies, so a
    /// file in another textual form still merges. Revocations are the
    /// union.
    pub fn merge(old: &TrustState, new: TrustState) -> TrustState {
        // Statements compare by their signed bodies, not their text.
        let (Some(old_bodies), Some(new_bodies)) =
            (bodies(&old.statements), bodies(&new.statements))
        else {
            return old.clone();
        };
        // A state whose `seq` disagrees with its statements is never written.
        if new.seq < old.seq
            || new.seq != new_bodies.len() as u64
            || !new_bodies.starts_with(&old_bodies)
            || (new.seq == old.seq && new_bodies != old_bodies)
        {
            return old.clone();
        }
        let mut base = old.clone();
        for body in &new_bodies[old_bodies.len()..] {
            base.reset(body.role, body.timestamp);
        }
        for (c, n) in &new.channels {
            let m = base.channels.entry(*c).or_default();
            (m.hwm_ts, m.hwm_sha256) = later((m.hwm_ts, m.hwm_sha256), (n.hwm_ts, n.hwm_sha256));
            (m.fresh_ts, m.fresh_sha256) =
                later((m.fresh_ts, m.fresh_sha256), (n.fresh_ts, n.fresh_sha256));
        }
        if new.seq > old.seq {
            base.seq = new.seq;
            base.statements = new.statements;
        }
        base.revoked = union(&old.revoked, &new.revoked);
        base
    }

    /// Whether this state revokes `key` (its stored revocations; `check`
    /// also refuses keys the embedded chain revokes).
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

/// At most this many revoked keys: one per statement, and a few more a
/// build with other generation-0 keys may have recorded.
pub const MAX_REVOKED: usize = crate::chain::MAX_CHAIN + 3;

fn union(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = a.to_vec();
    for k in b {
        if !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// Stored statements' bodies (their signatures were verified when they
/// were accepted, and are again on load).
fn bodies(statements: &[SignedStatement]) -> Option<Vec<crate::doc::StatementBody>> {
    statements
        .iter()
        .map(|s| {
            let (body, _) = doc::decode(&s.body, &s.minisig, crate::doc::MAX_STATEMENT).ok()?;
            crate::doc::StatementBody::parse(&body).ok()
        })
        .collect()
}

/// The later of two marks; on a tie, a stored digest is kept.
fn later(old: (u64, Option<Sha256>), new: (u64, Option<Sha256>)) -> (u64, Option<Sha256>) {
    match new.0.cmp(&old.0) {
        std::cmp::Ordering::Greater => new,
        std::cmp::Ordering::Equal if old.1.is_none() => new,
        _ => old,
    }
}
