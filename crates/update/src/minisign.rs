//! minisign public keys and signatures, written to the format
//! (<https://jedisct1.github.io/minisign/>), verified with `ring`'s Ed25519.
//!
//! Only prehashed signatures (`ED`: Ed25519 over BLAKE2b-512 of the
//! message, minisign ≥ 0.10's default) are accepted; the legacy `Ed`
//! form is refused. A signature verifies only when the key number
//! matches, the signature over the prehash verifies and the global
//! signature over `signature ‖ trusted comment` verifies.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use blake2::{Blake2b512, Digest as _};
use ring::signature::{ED25519, UnparsedPublicKey};

use crate::Error;

/// A signature file is at most this long.
pub const MAX_SIGNATURE_FILE: usize = 1024;
/// Each comment line's text is at most this many bytes.
pub const MAX_COMMENT: usize = 512;

const UNTRUSTED: &str = "untrusted comment: ";
const TRUSTED: &str = "trusted comment: ";

/// An Ed25519 public key in minisign's form: an 8-byte key number and the key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey {
    number: [u8; 8],
    key: [u8; 32],
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({})", self.id())
    }
}

impl PublicKey {
    /// Builds a key from its parts.
    pub fn from_parts(number: [u8; 8], key: [u8; 32]) -> Self {
        Self { number, key }
    }

    /// Parses the base64 line of a public key: 42 bytes, `Ed`, the key
    /// number, the Ed25519 key.
    pub fn from_base64(line: &str) -> Result<Self, Error> {
        if line.len() > 64 {
            return Err(Error::BadKey);
        }
        let raw = STANDARD.decode(line).map_err(|_| Error::BadKey)?;
        let raw: [u8; 42] = raw.try_into().map_err(|_| Error::BadKey)?;
        if &raw[..2] != b"Ed" {
            return Err(Error::BadKey);
        }
        let mut number = [0; 8];
        number.copy_from_slice(&raw[2..10]);
        let mut key = [0; 32];
        key.copy_from_slice(&raw[10..]);
        Ok(Self { number, key })
    }

    /// Parses a `.pub` file: an untrusted comment line and the key line,
    /// with an optional final newline.
    pub fn from_file(text: &str) -> Result<Self, Error> {
        let lines = split_lines(text, 2, 256).ok_or(Error::BadKey)?;
        let comment = lines[0].strip_prefix(UNTRUSTED).ok_or(Error::BadKey)?;
        if !comment_ok(comment) {
            return Err(Error::BadKey);
        }
        Self::from_base64(lines[1])
    }

    /// The base64 line, as `from_base64` reads it.
    pub fn to_base64(&self) -> String {
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(b"Ed");
        raw.extend_from_slice(&self.number);
        raw.extend_from_slice(&self.key);
        STANDARD.encode(raw)
    }

    /// The key ID minisign shows: the key number as a little-endian `u64`,
    /// in upper-case hex.
    pub fn id(&self) -> String {
        format!("{:016X}", u64::from_le_bytes(self.number))
    }

    /// The 8-byte key number.
    pub fn number(&self) -> [u8; 8] {
        self.number
    }

    /// The raw Ed25519 key.
    pub fn key(&self) -> [u8; 32] {
        self.key
    }

    /// Verifies `sig` over `message`: key number, signature over the
    /// BLAKE2b-512 prehash, then the global signature. All or nothing.
    pub fn verify(&self, message: &[u8], sig: &Signature) -> Result<(), Error> {
        if sig.number != self.number {
            return Err(Error::WrongKey);
        }
        let ed = UnparsedPublicKey::new(&ED25519, &self.key[..]);
        let prehash = Blake2b512::digest(message);
        ed.verify(prehash.as_slice(), &sig.signature)
            .map_err(|_| Error::BadSignature)?;
        let mut global = Vec::with_capacity(64 + sig.trusted.len());
        global.extend_from_slice(&sig.signature);
        global.extend_from_slice(sig.trusted.as_bytes());
        ed.verify(&global, &sig.global)
            .map_err(|_| Error::BadGlobalSignature)
    }
}

/// A parsed minisign signature file.
#[derive(Clone, PartialEq, Eq)]
pub struct Signature {
    number: [u8; 8],
    signature: [u8; 64],
    trusted: String,
    global: [u8; 64],
}

impl std::fmt::Debug for Signature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let id = PublicKey::from_parts(self.number, [0; 32]).id();
        write!(f, "Signature(key {id})")
    }
}

impl Signature {
    /// Parses a signature file: exactly four lines (LF or CRLF, an
    /// optional final line ending, nothing else), each comment at most
    /// 512 bytes of printable UTF-8, the whole at most 1 KiB.
    pub fn parse(text: &str) -> Result<Self, Error> {
        if text.len() > MAX_SIGNATURE_FILE {
            return Err(Error::BadSignatureFile);
        }
        let lines = split_lines(text, 4, MAX_SIGNATURE_FILE).ok_or(Error::BadSignatureFile)?;
        let untrusted = lines[0]
            .strip_prefix(UNTRUSTED)
            .ok_or(Error::BadSignatureFile)?;
        let trusted = lines[2]
            .strip_prefix(TRUSTED)
            .ok_or(Error::BadSignatureFile)?;
        if !comment_ok(untrusted) || !comment_ok(trusted) {
            return Err(Error::BadSignatureFile);
        }
        if lines[1].len() > 100 || lines[3].len() > 88 {
            return Err(Error::BadSignatureFile);
        }
        let sig = STANDARD
            .decode(lines[1])
            .map_err(|_| Error::BadSignatureFile)?;
        let sig: [u8; 74] = sig.try_into().map_err(|_| Error::BadSignatureFile)?;
        match &sig[..2] {
            b"ED" => {}
            b"Ed" => return Err(Error::LegacySignature),
            _ => return Err(Error::BadSignatureFile),
        }
        let global = STANDARD
            .decode(lines[3])
            .map_err(|_| Error::BadSignatureFile)?;
        let global: [u8; 64] = global.try_into().map_err(|_| Error::BadSignatureFile)?;
        let mut number = [0; 8];
        number.copy_from_slice(&sig[2..10]);
        let mut signature = [0; 64];
        signature.copy_from_slice(&sig[10..]);
        Ok(Self {
            number,
            signature,
            trusted: trusted.to_owned(),
            global,
        })
    }

    /// The key number of the key that made it.
    pub fn key_number(&self) -> [u8; 8] {
        self.number
    }

    /// The trusted comment (covered by the global signature).
    pub fn trusted_comment(&self) -> &str {
        &self.trusted
    }

    /// Builds a signature from its parts (used by test signing).
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn from_parts(
        number: [u8; 8],
        signature: [u8; 64],
        trusted: String,
        global: [u8; 64],
    ) -> Self {
        Self {
            number,
            signature,
            trusted,
            global,
        }
    }

    /// The signature file, as minisign writes it.
    pub(crate) fn to_file(&self, untrusted: &str) -> String {
        let mut sig = Vec::with_capacity(74);
        sig.extend_from_slice(b"ED");
        sig.extend_from_slice(&self.number);
        sig.extend_from_slice(&self.signature);
        format!(
            "{UNTRUSTED}{untrusted}\n{}\n{TRUSTED}{}\n{}\n",
            STANDARD.encode(sig),
            self.trusted,
            STANDARD.encode(self.global)
        )
    }
}

/// Splits `text` into exactly `n` lines ending in LF or CRLF, the last
/// one's ending optional. Any other CR, or more lines, is refused.
fn split_lines(text: &str, n: usize, max: usize) -> Option<Vec<&str>> {
    if text.len() > max {
        return None;
    }
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if body.ends_with('\r') {
        return None;
    }
    let lines: Vec<&str> = body
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if lines.len() != n || lines.iter().any(|l| l.contains('\r')) {
        return None;
    }
    Some(lines)
}

/// A comment is at most 512 bytes of printable text (tab allowed, as
/// minisign's default trusted comment uses one).
fn comment_ok(comment: &str) -> bool {
    comment.len() <= MAX_COMMENT && comment.chars().all(|c| c == '\t' || !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VECTORS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors/minisign");

    fn read(name: &str) -> String {
        std::fs::read_to_string(format!("{VECTORS}/{name}")).unwrap()
    }

    #[test]
    fn real_tool_prehashed_signature_verifies() {
        // Made with minisign 0.11 (`minisign -S`); see the folder's README.
        let key = PublicKey::from_file(&read("tool.pub")).unwrap();
        assert_eq!(key.id(), "CBE3DCC7E80F961D");
        let sig = Signature::parse(&read("prehashed.minisig")).unwrap();
        assert_eq!(sig.trusted_comment(), "lodestar test vector");
        let msg = read("message.txt");
        key.verify(msg.as_bytes(), &sig).unwrap();
        assert_eq!(
            key.verify(b"Lodestar minisign interop vector!\n", &sig),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn real_tool_legacy_signature_is_refused() {
        assert_eq!(
            Signature::parse(&read("legacy.minisig")),
            Err(Error::LegacySignature)
        );
    }

    #[test]
    fn trusted_comment_is_covered_by_the_global_signature() {
        let key = PublicKey::from_file(&read("tool.pub")).unwrap();
        let text =
            read("prehashed.minisig").replace("lodestar test vector", "lodestar test vectoR");
        let sig = Signature::parse(&text).unwrap();
        assert_eq!(
            key.verify(read("message.txt").as_bytes(), &sig),
            Err(Error::BadGlobalSignature)
        );
    }

    #[test]
    fn real_public_keys_decode_to_their_documented_ids() {
        let release = PublicKey::from_file(include_str!("../keys/lodestar-release.pub")).unwrap();
        assert_eq!(release.id(), "02910813433E81E0");
        let recovery = PublicKey::from_file(include_str!("../keys/lodestar-recovery.pub")).unwrap();
        assert_eq!(recovery.id(), "67922DDF005FD310");
        assert_eq!(
            PublicKey::from_base64(&release.to_base64()).unwrap(),
            release
        );
    }

    #[test]
    fn crlf_and_missing_final_newline_are_accepted() {
        let text = read("prehashed.minisig");
        let crlf = text.replace('\n', "\r\n");
        assert!(Signature::parse(&crlf).is_ok());
        assert!(Signature::parse(text.trim_end_matches('\n')).is_ok());
    }

    #[test]
    fn malformed_signature_files_are_refused() {
        let text = read("prehashed.minisig");
        let lines: Vec<&str> = text.lines().collect();
        let bad = [
            format!("{text}\n"),                                    // a fifth, empty line
            format!("{text}extra\n"),                               // a fifth line
            lines[..3].join("\n"),                                  // three lines
            text.replacen("untrusted comment: ", "untrusted: ", 1), // wrong prefix
            text.replacen("trusted comment: lodestar", "trusted comment:lodestar", 1),
            text.replacen('\n', "\r", 1), // a bare CR
            text.replacen(lines[1], &format!(" {}", lines[1]), 1), // whitespace in base64
            text.replacen(lines[1], lines[1].trim_end_matches('='), 1), // no padding
            text.replacen("lodestar test vector", &"x".repeat(513), 1),
            text.replacen("lodestar test vector", "bell\u{7}", 1),
            text.replacen(lines[3], &lines[3][4..], 1), // short global signature
            format!("{text}{}", " ".repeat(1024)),
        ];
        for b in bad {
            assert!(Signature::parse(&b).is_err(), "accepted {b:?}");
        }
    }

    #[test]
    fn wrong_key_number_is_refused_before_any_curve_work() {
        let key = PublicKey::from_file(&read("tool.pub")).unwrap();
        let other = PublicKey::from_parts([9; 8], key.key());
        let sig = Signature::parse(&read("prehashed.minisig")).unwrap();
        assert_eq!(
            other.verify(read("message.txt").as_bytes(), &sig),
            Err(Error::WrongKey)
        );
    }

    #[test]
    fn malformed_public_keys_are_refused() {
        let line = "RWQdlg/ox9zjy/7VTPq0yawuKuleo4sUji9Twi18XJ/zDNv4K4WAfSDy";
        assert!(PublicKey::from_base64(line).is_ok());
        for bad in [
            &line[..line.len() - 4],
            "RUQdlg/ox9zjy/7VTPq0yawuKuleo4sUji9Twi18XJ/zDNv4K4WAfSDy", // "ED"
            "",
            "RWQdlg/ox9zjy/7VTPq0yawuKuleo4sUji9Twi18XJ/zDNv4K4WAfSD",
        ] {
            assert!(PublicKey::from_base64(bad).is_err(), "{bad}");
        }
        assert!(PublicKey::from_file(line).is_err());
    }
}
