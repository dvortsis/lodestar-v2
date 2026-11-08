//! Checking a downloaded artifact (plan 2026-11-06-v06-companions §4 CMP-1,
//! Q2; architecture §18.10).
//!
//! - **The pin is the run-time gate, always** (Q2, owner cq2): the size and
//!   SHA-256 must be the catalogue's ([`matches`]). The comparison runs in
//!   constant time: a hash isn't secret, but there's no reason to leak how
//!   much of one matched.
//! - **Upstream OpenPGP** signatures are checked when the pin is made, by
//!   CMP-13's tool; the catalogue records it as
//!   [`UpstreamSig::VerifiedAtPin`], and nothing more is checked here.
//! - **Upstream minisign** signatures, if a companion ever publishes them,
//!   are checked at run time with [`lodestar_update::minisign`]
//!   ([`minisign`]), after the pin matched. The catalogue can't name a
//!   minisign key yet; `docs/decisions/2026-11-08-CMP-1.md` records what
//!   its `upstream_sig` needs to carry.

use std::io::{self, Read, Seek, SeekFrom};

use lodestar_update::minisign::{PublicKey, Signature};

use crate::catalogue::{Pin, UpstreamSig};
use crate::fetch::Verified;

/// Why a check failed: stable codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The size or SHA-256 isn't the pin's.
    Mismatch,
    /// The signature file isn't a valid prehashed minisign signature.
    SignatureFile,
    /// The signature is by another key.
    WrongKey,
    /// The signature doesn't verify.
    BadSignature,
    /// The file couldn't be read.
    Io,
}

impl VerifyError {
    pub fn code(self) -> &'static str {
        match self {
            VerifyError::Mismatch => "download_mismatch",
            VerifyError::SignatureFile => "signature_file",
            VerifyError::WrongKey => "signature_wrong_key",
            VerifyError::BadSignature => "signature_bad",
            VerifyError::Io => "download_io",
        }
    }
}

/// Whether `size` bytes hashing to `sha256` are what `pin` names. Both
/// are always compared; the hash in constant time.
pub fn matches(pin: &Pin, size: u64, sha256: &[u8; 32]) -> Result<(), VerifyError> {
    let hash_ok = ct_eq(pin.sha256.as_bytes(), sha256);
    let size_ok = pin.size == size;
    if hash_ok & size_ok {
        Ok(())
    } else {
        Err(VerifyError::Mismatch)
    }
}

/// Equal in time that doesn't depend on where they differ.
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// What a pin's upstream signature needs at run time, beyond the SHA-256
/// [`matches`] already checked. CMP-5 asks this for every pin before
/// extraction and refuses one it can't satisfy. The match is exhaustive
/// on purpose: a new [`UpstreamSig`] variant (a minisign key, say) is a
/// compile error here until its run-time check is wired in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// Nothing more: upstream doesn't sign it, or CMP-13 checked its
    /// OpenPGP signature when the pin was made (Q2).
    PinOnly,
}

pub fn upstream(pin: &Pin) -> Upstream {
    match pin.upstream_sig {
        UpstreamSig::None | UpstreamSig::VerifiedAtPin { .. } => Upstream::PinOnly,
    }
}

/// Verifies upstream's minisign signature `sig` (a `.minisig` file's text)
/// over `file` with `key`, the key pinned for the entry. Call it after the
/// pin matched. The file, at most the pin's size, is read whole: minisign
/// signs its BLAKE2b-512 prehash, which `lodestar_update` takes over the
/// message. The handle is left at its start.
pub fn minisign(file: &mut Verified, key: &PublicKey, sig: &str) -> Result<(), VerifyError> {
    let sig = Signature::parse(sig).map_err(|_| VerifyError::SignatureFile)?;
    if sig.key_number() != key.number() {
        return Err(VerifyError::WrongKey);
    }
    let message = read_all(file).map_err(|_| VerifyError::Io)?;
    key.verify(&message, &sig).map_err(|e| match e {
        lodestar_update::Error::WrongKey => VerifyError::WrongKey,
        _ => VerifyError::BadSignature,
    })
}

fn read_all(file: &mut Verified) -> io::Result<Vec<u8>> {
    let size = usize::try_from(file.size()).map_err(|_| io::Error::other("too large"))?;
    file.seek(SeekFrom::Start(0))?;
    let mut message = Vec::with_capacity(size);
    let limit = file.size() + 1;
    Read::take(&mut *file, limit).read_to_end(&mut message)?;
    file.seek(SeekFrom::Start(0))?;
    if message.len() != size {
        return Err(io::Error::other("the file changed"));
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_equality() {
        let a = [7u8; 32];
        let mut b = a;
        assert!(ct_eq(&a, &b));
        b[31] ^= 1;
        assert!(!ct_eq(&a, &b));
        b = a;
        b[0] ^= 0x80;
        assert!(!ct_eq(&a, &b));
    }

    #[test]
    fn codes_are_stable() {
        assert_eq!(VerifyError::BadSignature.code(), "signature_bad");
        assert_eq!(VerifyError::WrongKey.code(), "signature_wrong_key");
        assert_eq!(VerifyError::Mismatch.code(), "download_mismatch");
    }
}
