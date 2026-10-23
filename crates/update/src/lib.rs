//! The update trust core (architecture §12.2): minisign verification,
//! the signed manifest, key statements and the key chain, the trust
//! state and its forward-only rules, and the three verification entry
//! points the updater and the supervisor use.
//!
//! The crate is pure: no I/O, no clock, no network, no logging. Callers
//! pass bytes and the time, and write whatever trust state it returns.
//! The formats are a contract with the release workflow; README.md in
//! this crate is their specification.
//!
//! **Verify, then parse.** Every signed document is carried as the exact
//! bytes that were signed. Nothing in a body is used before its signature
//! verifies under a key allowed for it. The one read ahead is a
//! document's `statements` array when its signer isn't known yet: each
//! statement is itself signed by the recovery key and is verified before
//! its own body is read (see `verify`).

pub mod chain;
pub mod doc;
pub mod keys;
pub mod minisign;
pub mod trust;
pub mod verify;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

pub use chain::Chain;
pub use doc::{
    Artifact, ArtifactKind, Envelope, FreshnessBody, Level, ManifestBody, Release, Sha256, Signed,
    SignedStatement, StatementBody, Target,
};
pub use keys::Embedded;
pub use minisign::{PublicKey, Signature};
pub use trust::{Marks, TrustState};
pub use verify::{Checked, Freshness, Manifest, Overlay, Pin, check, install, overlay};

use serde::{Deserialize, Serialize};

/// One day, in seconds.
pub const DAY: u64 = 86_400;
/// A manifest, freshness or key statement dated further ahead of the
/// local clock than this is refused.
pub const MAX_AHEAD: u64 = DAY;
/// `expires` is at most this long after a document's own `timestamp`.
pub const MAX_LIFETIME: u64 = 30 * DAY;

/// An update channel. A manifest names its channel inside its signed body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
    Nightly,
}

impl Channel {
    pub const ALL: [Channel; 3] = [Channel::Stable, Channel::Beta, Channel::Nightly];

    /// The key role that signs this channel's manifests.
    pub fn role(self) -> Role {
        match self {
            Channel::Stable | Channel::Beta => Role::Release,
            Channel::Nightly => Role::Nightly,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
            Channel::Nightly => "nightly",
        }
    }
}

/// A signing key's role (`docs/owner-decisions.md` §2.21). The recovery
/// key has no role: it signs key statements only and is never rotated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Release,
    Nightly,
    Freshness,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Release, Role::Nightly, Role::Freshness];

    /// The channels whose manifests this role signs (none for freshness).
    pub fn channels(self) -> &'static [Channel] {
        match self {
            Role::Release => &[Channel::Stable, Channel::Beta],
            Role::Nightly => &[Channel::Nightly],
            Role::Freshness => &[],
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// Something the Updates screen should say, raised by a check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Notice {
    /// Genuine documents keep arriving from more than a day ahead.
    ClockBehind,
    /// The clock is past the newest accepted manifest's expiry while
    /// checks succeed.
    ClockAhead,
    /// A recovery-signed statement disagrees with one already accepted.
    Equivocation,
    /// No key is known for the chosen channel's role (Nightly until the
    /// owner creates the nightly key).
    ChannelUnavailable,
    /// A freshness statement was present but no freshness key exists yet.
    FreshnessUnavailable,
    /// A freshness statement was present and refused.
    FreshnessRefused,
    /// The stored trust state doesn't verify.
    TrustDamaged,
}

impl Notice {
    /// The stable code the API and UI use.
    pub fn code(self) -> &'static str {
        match self {
            Notice::ClockBehind => "clock_behind",
            Notice::ClockAhead => "clock_ahead",
            Notice::Equivocation => "equivocation",
            Notice::ChannelUnavailable => "channel_unavailable",
            Notice::FreshnessUnavailable => "freshness_unavailable",
            Notice::FreshnessRefused => "freshness_refused",
            Notice::TrustDamaged => "trust_damaged",
        }
    }
}

/// Why a document, key or artifact was refused. Each has a stable code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Error {
    // Encoding and signatures.
    /// A public key isn't a valid minisign key.
    BadKey,
    /// A signature file isn't four valid lines.
    BadSignatureFile,
    /// A legacy (not prehashed) minisign signature.
    LegacySignature,
    /// The signature's key number isn't the key's.
    WrongKey,
    /// The signature over the message doesn't verify.
    BadSignature,
    /// The global signature over the trusted comment doesn't verify.
    BadGlobalSignature,
    /// A document is larger than its cap.
    TooLarge,
    /// A body isn't valid base64.
    BadEncoding,
    /// A body isn't strict JSON of the expected shape, or breaks a cap.
    BadDocument,
    /// A body's `type` isn't the document kind expected here.
    WrongType,
    /// A manifest format this build can't read: it needs a newer Lodestar.
    NewerFormat,
    // Keys and the chain.
    /// No key exists for this role yet.
    NoKey(Role),
    /// The signing key isn't the current key of the role allowed here.
    UnknownKey,
    /// The signing key has been revoked.
    RevokedKey,
    /// A key statement isn't signed by the recovery key.
    NotRecoverySigned,
    /// A key statement's `seq` skips ahead of the current sequence.
    Gap,
    /// A statement with an accepted `seq` differs from the accepted one.
    Equivocation,
    /// A key statement introduces a key that's already been used.
    KeyReused,
    // Time and marks.
    /// Dated more than a day ahead of the local clock.
    Ahead,
    /// `expires` isn't after `timestamp`, or is more than 30 days after it.
    BadExpiry,
    /// The manifest names another channel than the one asked for.
    WrongChannel,
    /// Older than the channel's high-water mark.
    Older,
    /// The same timestamp as the high-water mark with another body.
    SameTimestamp,
    /// A freshness statement no newer than the channel's freshness mark.
    Replayed,
    /// A freshness statement for another manifest.
    ManifestMismatch,
    /// The stored trust state doesn't verify or is inconsistent.
    TrustDamaged,
    // Installs and overlays.
    /// The manifest isn't current, so nothing new installs.
    NotCurrent,
    /// No release with this version.
    UnknownVersion,
    /// The release has been withdrawn.
    Withdrawn,
    /// The release has no artifact for this target and kind.
    NoArtifact,
    /// The binary's SHA-256 or size doesn't match its pin.
    ArtifactMismatch,
}

impl Error {
    /// The stable code the API, logs and vectors use.
    pub fn code(self) -> &'static str {
        match self {
            Error::BadKey => "bad_key",
            Error::BadSignatureFile => "bad_signature_file",
            Error::LegacySignature => "legacy_signature",
            Error::WrongKey => "wrong_key",
            Error::BadSignature => "bad_signature",
            Error::BadGlobalSignature => "bad_global_signature",
            Error::TooLarge => "too_large",
            Error::BadEncoding => "bad_encoding",
            Error::BadDocument => "bad_document",
            Error::WrongType => "wrong_type",
            Error::NewerFormat => "newer_format",
            Error::NoKey(Role::Release) => "no_release_key",
            Error::NoKey(Role::Nightly) => "no_nightly_key",
            Error::NoKey(Role::Freshness) => "no_freshness_key",
            Error::UnknownKey => "unknown_key",
            Error::RevokedKey => "revoked_key",
            Error::NotRecoverySigned => "not_recovery_signed",
            Error::Gap => "statement_gap",
            Error::Equivocation => "equivocation",
            Error::KeyReused => "key_reused",
            Error::Ahead => "ahead_of_clock",
            Error::BadExpiry => "bad_expiry",
            Error::WrongChannel => "wrong_channel",
            Error::Older => "older_than_mark",
            Error::SameTimestamp => "same_timestamp",
            Error::Replayed => "replayed",
            Error::ManifestMismatch => "manifest_mismatch",
            Error::TrustDamaged => "trust_damaged",
            Error::NotCurrent => "not_current",
            Error::UnknownVersion => "unknown_version",
            Error::Withdrawn => "withdrawn",
            Error::NoArtifact => "no_artifact",
            Error::ArtifactMismatch => "artifact_mismatch",
        }
    }

    /// The notice a refusal raises, if any. `Ahead` is only returned for
    /// documents whose signature already verified, so it means the local
    /// clock looks behind.
    pub fn notice(self) -> Option<Notice> {
        match self {
            Error::Ahead => Some(Notice::ClockBehind),
            Error::Equivocation => Some(Notice::Equivocation),
            Error::NoKey(Role::Freshness) => Some(Notice::FreshnessUnavailable),
            Error::NoKey(_) => Some(Notice::ChannelUnavailable),
            Error::TrustDamaged => Some(Notice::TrustDamaged),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for Error {}
