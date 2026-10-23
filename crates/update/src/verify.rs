//! The three verification entry points.
//!
//! - [`check`]: a fetched envelope against the trust state and the clock.
//!   Returns the manifest, whether it's current, the next trust state and
//!   the notices.
//! - [`install`]: a checked manifest's pin for one release and target.
//! - [`overlay`]: an installed overlay, from the built-in recovery key
//!   alone: no clock, no expiry, no high-water mark (§12.2), so an
//!   installed version keeps starting offline.
//!
//! Role rules: Stable and Beta manifests need the current release key,
//! Nightly the current nightly key, freshness statements the current
//! freshness key.

use semver::Version;

use crate::chain::Chain;
use crate::doc::{
    self, ArtifactKind, Envelope, FreshnessBody, MAX_FRESHNESS, MAX_MANIFEST, ManifestBody,
    Release, Sha256, Signed, SignedStatement, Target,
};
use crate::keys::Embedded;
use crate::minisign::{PublicKey, Signature};
use crate::trust::{Marks, OverlayMark, TrustState};
use crate::{Channel, Error, MAX_AHEAD, Notice, Role};

/// A verified manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub channel: Channel,
    pub timestamp: u64,
    pub expires: u64,
    pub releases: Vec<Release>,
    /// The SHA-256 of the signed body.
    pub sha256: Sha256,
    /// The signed body and its signature file, as fetched: an overlay
    /// keeps both.
    pub body: Vec<u8>,
    pub minisig: String,
    /// The key that signed it.
    pub signer: PublicKey,
    /// The body's own key statements.
    pub statements: Vec<SignedStatement>,
}

/// What became of the envelope's freshness statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Freshness {
    Absent,
    /// Accepted; extends the manifest's expiry to `expires`.
    Accepted {
        timestamp: u64,
        expires: u64,
    },
    /// Refused; the manifest itself still counts.
    Refused(Error),
}

/// The result of a successful check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub manifest: Manifest,
    pub freshness: Freshness,
    /// The later of the manifest's and the accepted freshness expiry.
    pub expires: u64,
    /// Whether the manifest (or its freshness) hasn't expired. One that
    /// isn't current blocks installs only.
    pub current: bool,
    /// The trust state to write (through `TrustState::merge`).
    pub next: TrustState,
    pub notices: Vec<Notice>,
}

/// One artifact to download, from a current manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub version: Version,
    pub url: String,
    pub sha256: Sha256,
    pub size: u64,
}

/// A verified overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    pub version: Version,
    pub channel: Channel,
    pub signer: PublicKey,
    /// Revoked by a statement after this build's embedded ones (the
    /// overlay's own, here; the updater also checks `trust.json`): it
    /// starts, with a notice.
    pub revoked_later: bool,
    pub withdrawn: bool,
    pub migrates: bool,
    /// For a rebuilt trust state.
    pub mark: OverlayMark,
}

/// Verifies a fetched envelope for `channel` at `now` (Unix seconds).
pub fn check(
    envelope: &[u8],
    channel: Channel,
    now: u64,
    embedded: &Embedded,
    state: &TrustState,
) -> Result<Checked, Error> {
    let envelope = Envelope::parse(envelope)?;
    let mut chain = state.chain(embedded)?;
    chain.extend(&embedded.statements, None)?;
    let role = channel.role();

    // The manifest: signature first, then its body.
    let (body, sig) = doc::decode(
        &envelope.manifest.body,
        &envelope.manifest.minisig,
        MAX_MANIFEST,
    )?;
    let signer = signer(&mut chain, role, &body, &sig, Some(now))?;
    signer.verify(&body, &sig)?;
    let parsed = ManifestBody::parse(&body)?;
    chain.extend(&parsed.statements, Some(now))?;
    if chain.current(role) != Some(signer) {
        return Err(Error::RevokedKey);
    }
    if parsed.channel != channel {
        return Err(Error::WrongChannel);
    }
    if parsed.timestamp > now.saturating_add(MAX_AHEAD) {
        return Err(Error::Ahead);
    }
    let mut next = state.advance(&chain);
    let sha256 = Sha256::of(&body);
    let marks = next.marks(channel);
    if parsed.timestamp < marks.hwm_ts {
        return Err(Error::Older);
    }
    if !Marks::passes(marks.hwm_ts, marks.hwm_sha256, parsed.timestamp, sha256) {
        return Err(Error::SameTimestamp);
    }
    let m = next.channels.entry(channel).or_default();
    m.hwm_ts = parsed.timestamp;
    m.hwm_sha256 = Some(sha256);

    let mut notices = Vec::new();
    let mut expires = parsed.expires;
    let freshness = match &envelope.freshness {
        None => Freshness::Absent,
        Some(f) => match freshness(f, channel, sha256, signer, now, embedded, &next) {
            Ok((accepted, fresh_chain)) => {
                next = next.advance(&fresh_chain);
                let m = next.channels.entry(channel).or_default();
                m.fresh_ts = accepted.timestamp;
                m.fresh_sha256 = Some(accepted.sha256);
                expires = expires.max(accepted.expires);
                Freshness::Accepted {
                    timestamp: accepted.timestamp,
                    expires: accepted.expires,
                }
            }
            Err(e) => {
                notices.push(e.notice().unwrap_or(Notice::FreshnessRefused));
                Freshness::Refused(e)
            }
        },
    };
    let current = now < expires;
    if !current {
        notices.push(Notice::ClockAhead);
    }
    Ok(Checked {
        manifest: Manifest {
            channel,
            timestamp: parsed.timestamp,
            expires: parsed.expires,
            releases: parsed.releases,
            sha256,
            body,
            minisig: envelope.manifest.minisig,
            signer,
            statements: parsed.statements,
        },
        freshness,
        expires,
        current,
        next,
        notices,
    })
}

struct AcceptedFreshness {
    timestamp: u64,
    expires: u64,
    sha256: Sha256,
}

/// Verifies a freshness statement against the state after its manifest
/// was accepted.
fn freshness(
    f: &Signed,
    channel: Channel,
    manifest_sha256: Sha256,
    manifest_signer: PublicKey,
    now: u64,
    embedded: &Embedded,
    state: &TrustState,
) -> Result<(AcceptedFreshness, Chain), Error> {
    let mut chain = state.chain(embedded)?;
    chain.extend(&embedded.statements, None)?;
    let (body, sig) = doc::decode(&f.body, &f.minisig, MAX_FRESHNESS)?;
    let signer = signer(&mut chain, Role::Freshness, &body, &sig, Some(now))?;
    signer.verify(&body, &sig)?;
    let parsed = FreshnessBody::parse(&body)?;
    chain.extend(&parsed.statements, Some(now))?;
    // Its statements may not revoke its own key, nor the manifest's.
    if chain.current(Role::Freshness) != Some(signer)
        || chain.current(channel.role()) != Some(manifest_signer)
    {
        return Err(Error::RevokedKey);
    }
    // A freshness rotation in this statement resets the freshness marks.
    let state = state.advance(&chain);
    if parsed.channel != channel || parsed.manifest_sha256 != manifest_sha256 {
        return Err(Error::ManifestMismatch);
    }
    if parsed.timestamp > now.saturating_add(MAX_AHEAD) {
        return Err(Error::Ahead);
    }
    let marks = state.marks(channel);
    let sha256 = Sha256::of(&body);
    if !Marks::passes(marks.fresh_ts, marks.fresh_sha256, parsed.timestamp, sha256) {
        return Err(Error::Replayed);
    }
    Ok((
        AcceptedFreshness {
            timestamp: parsed.timestamp,
            expires: parsed.expires,
            sha256,
        },
        chain,
    ))
}

/// The key a document for `role` must verify under. When the signature
/// names a key the chain doesn't know as current, the body's statements
/// are read ahead (each verified under the recovery key before its own
/// body is read) in case they introduce it.
fn signer(
    chain: &mut Chain,
    role: Role,
    body: &[u8],
    sig: &Signature,
    now: Option<u64>,
) -> Result<PublicKey, Error> {
    let named = |chain: &Chain| {
        chain
            .current(role)
            .filter(|k| k.number() == sig.key_number())
    };
    if let Some(key) = named(chain) {
        return Ok(key);
    }
    chain.extend(&doc::statements_ahead(body)?, now)?;
    if let Some(key) = named(chain) {
        return Ok(key);
    }
    let revoked = chain
        .keys(role)
        .iter()
        .any(|k| k.number() == sig.key_number());
    Err(match chain.current(role) {
        _ if revoked => Error::RevokedKey,
        None => Error::NoKey(role),
        Some(_) => Error::UnknownKey,
    })
}

/// The pin for `version` on `target`, from a current manifest. Manual
/// steps and the level are policy, not trust, and aren't checked here.
pub fn install(
    checked: &Checked,
    version: &Version,
    target: Target,
    kind: ArtifactKind,
) -> Result<Pin, Error> {
    if !checked.current {
        return Err(Error::NotCurrent);
    }
    let release = find(&checked.manifest.releases, version)?;
    if release.withdrawn {
        return Err(Error::Withdrawn);
    }
    let a = release
        .artifacts
        .iter()
        .find(|a| a.target == target && a.kind == kind)
        .ok_or(Error::NoArtifact)?;
    Ok(Pin {
        version: release.version.clone(),
        url: a.url.clone(),
        sha256: a.sha256,
        size: a.size,
    })
}

fn find<'a>(releases: &'a [Release], version: &Version) -> Result<&'a Release, Error> {
    releases
        .iter()
        .find(|r| r.version == *version)
        .ok_or(Error::UnknownVersion)
}

/// Verifies an installed overlay from the embedded root alone: the key
/// statements (embedded, then the overlay manifest's own), the manifest's
/// signature, then the binary's SHA-256 and size for `target`. No clock,
/// no expiry, no high-water mark. A key the embedded statements revoke is
/// refused; one revoked only by a later statement starts, with a notice.
pub fn overlay(
    manifest: &[u8],
    minisig: &str,
    binary_sha256: Sha256,
    binary_len: u64,
    version: &Version,
    target: Target,
    embedded: &Embedded,
) -> Result<Overlay, Error> {
    if manifest.len() > MAX_MANIFEST {
        return Err(Error::TooLarge);
    }
    let sig = Signature::parse(minisig)?;
    let floor = Chain::embedded(embedded)?;
    let mut chain = floor.clone();
    // The signer is any key that has signed manifests, current or not;
    // revocation is judged below.
    let signing = |chain: &Chain| {
        [Role::Release, Role::Nightly]
            .into_iter()
            .flat_map(|r| chain.keys(r))
            .find(|k| k.number() == sig.key_number())
    };
    let signer = match signing(&chain) {
        Some(k) => k,
        None => {
            chain.extend(&doc::statements_ahead(manifest)?, None)?;
            signing(&chain).ok_or(Error::UnknownKey)?
        }
    };
    signer.verify(manifest, &sig)?;
    let parsed = ManifestBody::parse(manifest)?;
    chain.extend(&parsed.statements, None)?;
    if chain.role_of(&signer) != Some(parsed.channel.role()) {
        return Err(Error::UnknownKey);
    }
    if floor.is_revoked(&signer) {
        return Err(Error::RevokedKey);
    }
    let release = find(&parsed.releases, version)?;
    let artifact = release
        .artifacts
        .iter()
        .find(|a| a.target == target && a.kind == ArtifactKind::Binary)
        .ok_or(Error::NoArtifact)?;
    if artifact.sha256 != binary_sha256 || artifact.size != binary_len {
        return Err(Error::ArtifactMismatch);
    }
    Ok(Overlay {
        version: release.version.clone(),
        channel: parsed.channel,
        signer,
        revoked_later: chain.is_revoked(&signer),
        withdrawn: release.withdrawn,
        migrates: release.migrates,
        mark: OverlayMark {
            channel: parsed.channel,
            timestamp: parsed.timestamp,
            sha256: Sha256::of(manifest),
            statements: parsed.statements,
        },
    })
}
