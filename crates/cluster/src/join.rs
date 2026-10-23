//! One-time join tokens (§19.6).
//!
//! A token is `lsj1` followed by 122 lowercase base32 characters
//! (RFC 4648 alphabet, no padding) encoding 76 bytes:
//!
//! | Bytes | Field |
//! |---|---|
//! | 8 | token id: which record to check, not secret |
//! | 32 | the secret, from the system's CSPRNG |
//! | 32 | SHA-256 of the cluster CA certificate the coordinator presents |
//! | 4 | checksum: the first 4 bytes of SHA-256 over the 72 bytes above |
//!
//! - **The coordinator keeps only a hash** of the id and secret, compared in
//!   constant time. The token text exists once, in what
//!   [`Coordinator::create_join_token`](crate::Coordinator::create_join_token)
//!   returns.
//! - **The fingerprint** is how a new worker, which has nothing else yet,
//!   recognises the real coordinator on its first connection: anything
//!   else on the path can't present a chain to that CA.
//! - **The checksum** turns a mistyped token into "mistyped" instead of a
//!   confusing refusal or an untrusted coordinator.

use std::fmt;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::Fingerprint;

pub(crate) const PREFIX: &str = "lsj1";
const ID_LEN: usize = 8;
const SECRET_LEN: usize = 32;
const PAYLOAD_LEN: usize = ID_LEN + SECRET_LEN + 32;
const RAW_LEN: usize = PAYLOAD_LEN + 4;
/// 76 bytes are 608 bits: 122 base32 characters, the last carrying 3
/// data bits and 2 zero bits.
const TEXT_LEN: usize = 122;
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// A join token. It never prints; [`JoinToken::expose`] renders it for the
/// one screen that shows it.
pub struct JoinToken {
    raw: Zeroizing<[u8; RAW_LEN]>,
}

/// Why a token's text was refused before any lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFormatError {
    /// It doesn't start with `lsj1`.
    Prefix,
    Length,
    /// A character outside the base32 alphabet, or non-zero padding bits.
    Encoding,
    /// The checksum doesn't match: a typo or a truncated copy.
    Checksum,
}

impl fmt::Display for TokenFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TokenFormatError::Prefix => "this isn't a Lodestar join token",
            TokenFormatError::Length => "the join token is the wrong length; copy it again",
            TokenFormatError::Encoding => "the join token has a character it can't contain",
            TokenFormatError::Checksum => "the join token is mistyped; copy it again",
        })
    }
}

impl std::error::Error for TokenFormatError {}

impl JoinToken {
    pub(crate) fn new(id: [u8; ID_LEN], secret: &[u8; SECRET_LEN], ca: Fingerprint) -> JoinToken {
        let mut raw = Zeroizing::new([0u8; RAW_LEN]);
        raw[..ID_LEN].copy_from_slice(&id);
        raw[ID_LEN..ID_LEN + SECRET_LEN].copy_from_slice(secret);
        raw[ID_LEN + SECRET_LEN..PAYLOAD_LEN].copy_from_slice(&ca.0);
        let check = checksum(&raw[..PAYLOAD_LEN]);
        raw[PAYLOAD_LEN..].copy_from_slice(&check);
        JoinToken { raw }
    }

    /// Parses a token as pasted: surrounding whitespace is ignored and
    /// either case is accepted.
    pub fn parse(text: &str) -> Result<JoinToken, TokenFormatError> {
        let text = text.trim();
        let body = text
            .get(..PREFIX.len())
            .filter(|p| p.eq_ignore_ascii_case(PREFIX))
            .map(|_| &text.as_bytes()[PREFIX.len()..])
            .ok_or(TokenFormatError::Prefix)?;
        if body.len() != TEXT_LEN {
            return Err(TokenFormatError::Length);
        }
        let mut raw = Zeroizing::new([0u8; RAW_LEN]);
        let mut acc: u16 = 0;
        let mut bits = 0u32;
        let mut at = 0;
        for &c in body {
            let lower = c.to_ascii_lowercase();
            let v = ALPHABET
                .iter()
                .position(|&a| a == lower)
                .ok_or(TokenFormatError::Encoding)? as u16;
            acc = (acc << 5) | v;
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                raw[at] = (acc >> bits) as u8;
                at += 1;
                acc &= (1 << bits) - 1;
            }
        }
        // The two leftover bits must be zero, so every token has exactly
        // one spelling (besides case).
        if at != RAW_LEN || acc != 0 {
            return Err(TokenFormatError::Encoding);
        }
        let expected = checksum(&raw[..PAYLOAD_LEN]);
        if !bool::from(raw[PAYLOAD_LEN..].ct_eq(&expected)) {
            return Err(TokenFormatError::Checksum);
        }
        Ok(JoinToken { raw })
    }

    /// The token's text. Show it once, and don't log it.
    pub fn expose(&self) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::with_capacity(PREFIX.len() + TEXT_LEN));
        out.push_str(PREFIX);
        let mut acc: u16 = 0;
        let mut bits = 0u32;
        for &b in self.raw.iter() {
            acc = (acc << 8) | u16::from(b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[usize::from((acc >> bits) & 31)] as char);
            }
            acc &= (1 << bits) - 1;
        }
        if bits > 0 {
            out.push(ALPHABET[usize::from((acc << (5 - bits)) & 31)] as char);
        }
        out
    }

    /// The fingerprint of the cluster CA the coordinator must present.
    pub fn ca_fingerprint(&self) -> Fingerprint {
        let mut fp = [0u8; 32];
        fp.copy_from_slice(&self.raw[ID_LEN + SECRET_LEN..PAYLOAD_LEN]);
        Fingerprint(fp)
    }

    pub(crate) fn id(&self) -> [u8; ID_LEN] {
        let mut id = [0u8; ID_LEN];
        id.copy_from_slice(&self.raw[..ID_LEN]);
        id
    }

    /// What the coordinator stores and compares: SHA-256 over a domain
    /// label, the id and the secret. The secret is 256 random bits, so a
    /// fast hash is enough; nothing here is a password.
    pub(crate) fn hash(&self) -> [u8; 32] {
        let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
        ctx.update(b"lodestar-join-hash-v1");
        ctx.update(&self.raw[..ID_LEN + SECRET_LEN]);
        let mut out = [0u8; 32];
        out.copy_from_slice(ctx.finish().as_ref());
        out
    }
}

impl fmt::Debug for JoinToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JoinToken(***)")
    }
}

/// Constant-time equality of two token hashes.
pub(crate) fn hashes_match(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.ct_eq(b).into()
}

fn checksum(payload: &[u8]) -> [u8; 4] {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(b"lodestar-join-v1");
    ctx.update(payload);
    let digest = ctx.finish();
    let mut out = [0u8; 4];
    out.copy_from_slice(&digest.as_ref()[..4]);
    out
}

/// A worker name: one DNS label of lowercase letters, digits and hyphens,
/// 1–63 characters, not starting or ending with a hyphen. It goes into the
/// worker's certificate.
pub fn valid_worker_name(name: &str) -> bool {
    (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> JoinToken {
        JoinToken::new([1; 8], &[2; 32], Fingerprint([3; 32]))
    }

    #[test]
    fn round_trips_in_either_case_with_whitespace() {
        let text = token().expose();
        assert_eq!(text.len(), PREFIX.len() + TEXT_LEN);
        assert!(text.starts_with("lsj1"));
        let back = JoinToken::parse(&text).unwrap();
        assert_eq!(back.raw.as_slice(), token().raw.as_slice());
        let shouted = format!("  {}\n", text.to_ascii_uppercase());
        assert_eq!(JoinToken::parse(&shouted).unwrap().id(), [1; 8]);
        assert_eq!(back.ca_fingerprint(), Fingerprint([3; 32]));
    }

    #[test]
    fn refuses_malformed_text() {
        let text = token().expose();
        assert_eq!(JoinToken::parse("").unwrap_err(), TokenFormatError::Prefix);
        assert_eq!(
            JoinToken::parse(&text.replacen("lsj1", "lsj2", 1)).unwrap_err(),
            TokenFormatError::Prefix
        );
        assert_eq!(
            JoinToken::parse(&text[..text.len() - 1]).unwrap_err(),
            TokenFormatError::Length
        );
        assert_eq!(
            JoinToken::parse(&format!("{}a", text.as_str())).unwrap_err(),
            TokenFormatError::Length
        );
        let mut bad = text.to_string();
        bad.replace_range(10..11, "1");
        assert_eq!(
            JoinToken::parse(&bad).unwrap_err(),
            TokenFormatError::Encoding
        );
        // A non-zero padding bit in the last character.
        let mut padded = text.to_string();
        let last = padded.pop().unwrap();
        let v = ALPHABET.iter().position(|&a| a as char == last).unwrap();
        padded.push(ALPHABET[v | 1] as char);
        assert_eq!(
            JoinToken::parse(&padded).unwrap_err(),
            TokenFormatError::Encoding
        );
        // A typo in the body.
        let mut typo = text.to_string();
        let c = typo.as_bytes()[20];
        let swap = if c == b'a' { "b" } else { "a" };
        typo.replace_range(20..21, swap);
        assert_eq!(
            JoinToken::parse(&typo).unwrap_err(),
            TokenFormatError::Checksum
        );
        // Multi-byte text where the prefix would be.
        assert_eq!(
            JoinToken::parse("ls\u{e9}1").unwrap_err(),
            TokenFormatError::Prefix
        );
    }

    #[test]
    fn never_prints() {
        let t = token();
        assert_eq!(format!("{t:?}"), "JoinToken(***)");
    }

    #[test]
    fn hashes_bind_the_secret() {
        let other = JoinToken::new([1; 8], &[9; 32], Fingerprint([3; 32]));
        assert!(hashes_match(&token().hash(), &token().hash()));
        assert!(!hashes_match(&token().hash(), &other.hash()));
    }

    #[test]
    fn worker_names() {
        for ok in ["a", "nas-2", "w0", &"x".repeat(63)] {
            assert!(valid_worker_name(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "A", "a.b", "a b", "é", &"x".repeat(64)] {
            assert!(!valid_worker_name(bad), "{bad}");
        }
    }

    mod props {
        use proptest::prelude::*;

        use super::super::*;

        fn body() -> impl Strategy<Value = String> {
            proptest::collection::vec(proptest::sample::select(ALPHABET.to_vec()), TEXT_LEN)
                .prop_map(|chars| format!("{PREFIX}{}", String::from_utf8(chars).unwrap()))
        }

        proptest! {
            #[test]
            fn arbitrary_text_never_panics(text in ".{0,200}") {
                let _ = JoinToken::parse(&text);
            }

            #[test]
            fn well_formed_noise_is_refused_by_the_checksum(text in body()) {
                // Random base32 of the right length: only the checksum (or
                // the padding bits) stands between it and a token.
                prop_assert!(JoinToken::parse(&text).is_err());
            }

            #[test]
            fn tokens_round_trip(id: [u8; 8], secret: [u8; 32], fp: [u8; 32]) {
                let token = JoinToken::new(id, &secret, Fingerprint(fp));
                let back = JoinToken::parse(&token.expose()).unwrap();
                prop_assert_eq!(back.raw.as_slice(), token.raw.as_slice());
                prop_assert_eq!(back.hash(), token.hash());
            }

            #[test]
            fn any_one_character_change_is_refused(
                id: [u8; 8],
                secret: [u8; 32],
                at in 0..TEXT_LEN,
                to in proptest::sample::select(ALPHABET.to_vec()),
            ) {
                let text = JoinToken::new(id, &secret, Fingerprint([7; 32])).expose();
                let mut bytes = text.as_bytes().to_vec();
                prop_assume!(bytes[PREFIX.len() + at] != to);
                bytes[PREFIX.len() + at] = to;
                prop_assert!(JoinToken::parse(std::str::from_utf8(&bytes).unwrap()).is_err());
            }
        }
    }
}
