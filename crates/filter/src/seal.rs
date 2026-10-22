//! Sealed names (§20.3): a quarantined torrent's name is kept encrypted
//! at rest and opened only on an explicit, password-confirmed reveal. The
//! key lives outside the database, so a copy of the database alone (a
//! backup, a support bundle) doesn't reveal names.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

const NONCE: usize = 24;

pub struct Sealer(XChaCha20Poly1305);

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealError;

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sealing failed: no randomness or a damaged seal")
    }
}

impl std::error::Error for SealError {}

impl Sealer {
    pub fn new(key: &[u8; 32]) -> Sealer {
        Sealer(XChaCha20Poly1305::new(key.into()))
    }

    /// A fresh random key.
    pub fn generate_key() -> Result<[u8; 32], SealError> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|_| SealError)?;
        Ok(key)
    }

    /// The nonce, then the ciphertext with its tag.
    pub fn seal(&self, text: &str) -> Result<Vec<u8>, SealError> {
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce).map_err(|_| SealError)?;
        let ct = self
            .0
            .encrypt(XNonce::from_slice(&nonce), text.as_bytes())
            .map_err(|_| SealError)?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        Ok(out)
    }

    pub fn open(&self, sealed: &[u8]) -> Result<String, SealError> {
        if sealed.len() < NONCE {
            return Err(SealError);
        }
        let (nonce, ct) = sealed.split_at(NONCE);
        let pt = self
            .0
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|_| SealError)?;
        String::from_utf8(pt).map_err(|_| SealError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = Sealer::new(&Sealer::generate_key().unwrap());
        let a = s.seal("A Name").unwrap();
        let b = s.seal("A Name").unwrap();
        assert_ne!(a, b, "fresh nonce each time");
        assert!(!a.windows(6).any(|w| w == b"A Name"));
        assert_eq!(s.open(&a).unwrap(), "A Name");
        let other = Sealer::new(&[7; 32]);
        assert_eq!(other.open(&a), Err(SealError));
        let mut bad = a.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(s.open(&bad), Err(SealError));
        assert_eq!(s.open(&a[..10]), Err(SealError));
    }
}
