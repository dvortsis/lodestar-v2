//! BIP-340 Schnorr signature checks for Nostr events (NIP-01), for the
//! Nostr discovery source (plan `docs/plan/2026-11-06-v08.md` Q-r2 and
//! NOS-1, architecture §18.5).
//!
//! A relay can send any event under any publisher's key; only the
//! signature ties an event to its key. Without the check, a relay could
//! forge a publisher and fool per-publisher rules and the claims'
//! publisher. So an event whose signature fails is dropped, like one whose
//! id fails ([`nostr::Refused::IdMismatch`]).
//!
//! The curve maths is RustCrypto's `k256` (pure Rust, `schnorr` only); this
//! module only adapts it. [`verify`] is BIP-340's verification over any
//! message; [`event_signed`] recomputes an event's id and checks it; [`parse_signed`]
//! is [`nostr::parse`] with the check, the entry point for relay messages.
//! None of them panic on any input: a key that isn't an x coordinate on
//! the curve, an `r` at or above the field size or an `s` at or above the
//! curve order is simply a failed check.
//!
//! The choices made here are recorded in `docs/decisions/2026-11-23-NOS-1.md`.

use crate::nostr::{self, Event, Limits, Message, Refused};
use k256::schnorr::{Signature, VerifyingKey};

/// Why a relay message was dropped by [`parse_signed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// [`nostr::parse`] refused it.
    Refused(Refused),
    /// An `EVENT` whose BIP-340 signature doesn't verify against its
    /// public key and id.
    BadSignature,
}

impl From<Refused> for Rejected {
    fn from(r: Refused) -> Self {
        Rejected::Refused(r)
    }
}

/// True only when `sig` is a valid BIP-340 signature of `msg` under the
/// x-only public key `pubkey`. False for anything else, including a key
/// that isn't on the curve and a signature with `r` ≥ p or `s` ≥ n.
#[must_use]
pub fn verify(pubkey: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(key) = VerifyingKey::from_slice(pubkey) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(sig) else {
        return false;
    };
    key.verify_raw(msg, &sig).is_ok()
}

/// True only when the event's id is the hash of its fields and its
/// signature verifies over that id (NIP-01: the signed message is the
/// 32-byte id itself).
///
/// The id is recomputed here rather than trusted, because [`Event`]'s
/// fields are public: an event edited or rebuilt after parsing, still
/// carrying its old id and signature, is refused. That costs one SHA-256,
/// small beside the signature check.
#[must_use]
pub fn event_signed(e: &Event) -> bool {
    nostr::event_id(&e.pubkey, e.created_at, e.kind, &e.tags, &e.content) == e.id
        && verify(&e.pubkey, &e.id, &e.sig)
}

/// One relay message, as [`nostr::parse`] reads it, with an `EVENT`'s
/// signature checked as well: an event that fails is
/// [`Rejected::BadSignature`] and nothing from it is kept. Other messages
/// carry no signature and pass through unchanged.
pub fn parse_signed(msg: &[u8], limits: &Limits) -> Result<Message, Rejected> {
    let m = nostr::parse(msg, limits)?;
    if let Message::Event { event, .. } = &m
        && !event_signed(event)
    {
        return Err(Rejected::BadSignature);
    }
    Ok(m)
}
