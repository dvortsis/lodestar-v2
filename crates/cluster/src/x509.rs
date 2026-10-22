//! Ed25519 key pairs and the three certificate shapes the cluster issues.
//!
//! Lodestar only ever *writes* certificates; reading and verifying them is
//! rustls-webpki's job. So this is a DER encoder for one fixed layout, not
//! a general X.509 library:
//! - **CA:** `basicConstraints` CA:TRUE, pathlen 0 (critical) and
//!   `keyUsage` keyCertSign + cRLSign (critical).
//! - **Coordinator:** `keyUsage` digitalSignature, EKU serverAuth, and the
//!   DNS name [`COORDINATOR_NAME`].
//! - **Worker:** `keyUsage` digitalSignature, EKU clientAuth, and the DNS
//!   name `<name>.worker.cluster.lodestar.invalid`.
//!
//! The EKUs keep the roles apart: webpki refuses a worker certificate where
//! a server certificate is expected, and the reverse.

use std::fmt;

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use zeroize::Zeroizing;

use crate::ClusterError;

/// The name the coordinator's certificate carries, and the only name a
/// worker checks it against. Workers reach the coordinator by IP, LAN
/// name or overlay name alike, so the pin is the cluster CA, not the host.
pub const COORDINATOR_NAME: &str = "coordinator.cluster.lodestar.invalid";

/// Clock skew a certificate tolerates: it's valid from five minutes before
/// it was issued. §19.3 flags a worker past 60 s, so this only absorbs
/// clocks that are already being reported.
pub const CLOCK_SKEW_SECS: i64 = 5 * 60;

/// A SHA-256 fingerprint of a DER certificate. Not a secret.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of(der: &[u8]) -> Fingerprint {
        let digest = ring::digest::digest(&ring::digest::SHA256, der);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Fingerprint(out)
    }

    pub(crate) fn from_hex(text: &str) -> Option<Fingerprint> {
        let bytes = crate::hex::decode(text)?;
        Some(Fingerprint(bytes.try_into().ok()?))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&crate::hex::encode(&self.0))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// An Ed25519 key pair. The PKCS#8 bytes are zeroed on drop; ring's own
/// expanded key isn't, which is as far as ring lets us go.
pub struct KeyPair {
    pkcs8: Zeroizing<Vec<u8>>,
    pair: Ed25519KeyPair,
}

impl KeyPair {
    pub fn generate() -> Result<KeyPair, ClusterError> {
        let doc = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| ClusterError::Random)?;
        KeyPair::from_pkcs8(Zeroizing::new(doc.as_ref().to_vec()))
    }

    pub(crate) fn from_pkcs8(pkcs8: Zeroizing<Vec<u8>>) -> Result<KeyPair, ClusterError> {
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&pkcs8)
            .map_err(|_| ClusterError::Corrupt("key pair"))?;
        Ok(KeyPair { pkcs8, pair })
    }

    pub fn public_key(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(self.pair.public_key().as_ref());
        out
    }

    pub(crate) fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(message).as_ref().to_vec()
    }
}

impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeyPair(***)")
    }
}

/// What a certificate is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role<'a> {
    Ca,
    Coordinator,
    Worker(&'a str),
}

pub(crate) struct Spec<'a> {
    pub role: Role<'a>,
    pub subject: &'a str,
    pub issuer: &'a str,
    pub public_key: &'a [u8; 32],
    pub not_before: i64,
    pub not_after: i64,
}

/// A fresh positive 16-byte serial, minimally encodable as a DER INTEGER
/// (its first byte is 0x40..=0x7f).
pub(crate) fn random_serial() -> Result<[u8; 16], ClusterError> {
    let mut serial = [0u8; 16];
    SystemRandom::new()
        .fill(&mut serial)
        .map_err(|_| ClusterError::Random)?;
    serial[0] = (serial[0] & 0x3f) | 0x40;
    Ok(serial)
}

/// The DER certificate for `spec`, signed by `issuer_key`.
pub(crate) fn certificate(spec: &Spec<'_>, serial: &[u8; 16], issuer_key: &KeyPair) -> Vec<u8> {
    let tbs = tbs_certificate(spec, serial);
    let signature = issuer_key.sign(&tbs);
    seq(&[tbs, ed25519_algorithm(), bit_string(&signature)].concat())
}

/// Whether `cert` (one Lodestar wrote) certifies `public_key`. Its
/// SubjectPublicKeyInfo has one fixed encoding, so this is a byte search;
/// the chain itself is checked by webpki.
pub(crate) fn certifies(cert: &[u8], public_key: &[u8; 32]) -> bool {
    let spki = subject_public_key_info(public_key);
    cert.windows(spki.len()).any(|w| w == spki.as_slice())
}

/// Checks an Ed25519 signature (tests use it on certificates).
#[cfg(test)]
pub(crate) fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    use ring::signature::{ED25519, UnparsedPublicKey};
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(message, signature)
        .is_ok()
}

fn tbs_certificate(spec: &Spec<'_>, serial: &[u8; 16]) -> Vec<u8> {
    let version = tlv(0xa0, &tlv(0x02, &[2]));
    let validity = seq(&[time(spec.not_before), time(spec.not_after)].concat());
    seq(&[
        version,
        tlv(0x02, serial),
        ed25519_algorithm(),
        name(spec.issuer),
        validity,
        name(spec.subject),
        subject_public_key_info(spec.public_key),
        tlv(0xa3, &seq(&extensions(spec.role).concat())),
    ]
    .concat())
}

// OIDs, as their DER contents.
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_EKU: &[u8] = &[0x55, 0x1d, 0x25];
const OID_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const OID_CLIENT_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

fn extensions(role: Role<'_>) -> Vec<Vec<u8>> {
    // keyUsage bits: digitalSignature is bit 0 (0x80, 7 unused bits);
    // keyCertSign and cRLSign are bits 5 and 6 (0x06, 1 unused bit).
    let leaf_usage = extension(OID_KEY_USAGE, true, &tlv(0x03, &[7, 0x80]));
    match role {
        Role::Ca => vec![
            extension(
                OID_BASIC_CONSTRAINTS,
                true,
                &seq(&[tlv(0x01, &[0xff]), tlv(0x02, &[0])].concat()),
            ),
            extension(OID_KEY_USAGE, true, &tlv(0x03, &[1, 0x06])),
        ],
        Role::Coordinator => vec![
            leaf_usage,
            extension(OID_EKU, false, &seq(&tlv(0x06, OID_SERVER_AUTH))),
            extension(
                OID_SAN,
                false,
                &seq(&tlv(0x82, COORDINATOR_NAME.as_bytes())),
            ),
        ],
        Role::Worker(name) => vec![
            leaf_usage,
            extension(OID_EKU, false, &seq(&tlv(0x06, OID_CLIENT_AUTH))),
            extension(
                OID_SAN,
                false,
                &seq(&tlv(
                    0x82,
                    format!("{name}.worker.cluster.lodestar.invalid").as_bytes(),
                )),
            ),
        ],
    }
}

fn extension(oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut body = tlv(0x06, oid);
    if critical {
        body.extend(tlv(0x01, &[0xff]));
    }
    body.extend(tlv(0x04, value));
    seq(&body)
}

fn ed25519_algorithm() -> Vec<u8> {
    seq(&tlv(0x06, OID_ED25519))
}

fn subject_public_key_info(public_key: &[u8; 32]) -> Vec<u8> {
    seq(&[ed25519_algorithm(), bit_string(public_key)].concat())
}

fn name(common_name: &str) -> Vec<u8> {
    let attribute = seq(&[tlv(0x06, OID_CN), tlv(0x0c, common_name.as_bytes())].concat());
    seq(&tlv(0x31, &attribute))
}

fn bit_string(bytes: &[u8]) -> Vec<u8> {
    tlv(0x03, &[&[0u8][..], bytes].concat())
}

/// UTCTime through 2049, GeneralizedTime after (RFC 5280 §4.1.2.5).
fn time(unix: i64) -> Vec<u8> {
    let (y, mo, d, h, mi, s) = civil(unix);
    if (1950..2050).contains(&y) {
        let text = format!("{:02}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z", y % 100);
        tlv(0x17, text.as_bytes())
    } else {
        let text = format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z");
        tlv(0x18, text.as_bytes())
    }
}

/// Unix seconds to a UTC civil date and time (H. Hinnant's algorithm).
fn civil(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

fn seq(contents: &[u8]) -> Vec<u8> {
    tlv(0x30, contents)
}

fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
    let len = contents.len();
    let mut out = Vec::with_capacity(len + 4);
    out.push(tag);
    match len {
        0..=0x7f => out.push(len as u8),
        0x80..=0xff => out.extend([0x81, len as u8]),
        _ => {
            assert!(len <= 0xffff, "certificate fields are small");
            out.extend([0x82, (len >> 8) as u8, len as u8]);
        }
    }
    out.extend_from_slice(contents);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil(1_792_627_199), (2026, 10, 21, 23, 59, 59));
        assert_eq!(civil(2_524_608_000), (2050, 1, 1, 0, 0, 0));
    }

    #[test]
    fn times_switch_to_generalized_in_2050() {
        assert_eq!(time(0), tlv(0x17, b"700101000000Z"));
        assert_eq!(time(2_524_607_999), tlv(0x17, b"491231235959Z"));
        assert_eq!(time(2_524_608_000), tlv(0x18, b"20500101000000Z"));
    }

    #[test]
    fn lengths_use_the_long_form_past_127() {
        assert_eq!(&tlv(0x04, &[0; 127])[..2], &[0x04, 0x7f]);
        assert_eq!(&tlv(0x04, &[0; 128])[..3], &[0x04, 0x81, 0x80]);
        assert_eq!(&tlv(0x04, &[0; 300])[..4], &[0x04, 0x82, 0x01, 0x2c]);
    }

    #[test]
    fn certificates_are_signed_by_the_issuer_and_certify_the_subject() {
        let ca = KeyPair::generate().unwrap();
        let leaf = KeyPair::generate().unwrap();
        let spec = Spec {
            role: Role::Worker("w1"),
            subject: "w1",
            issuer: "ca",
            public_key: &leaf.public_key(),
            not_before: 1_700_000_000,
            not_after: 1_700_086_400,
        };
        let serial = random_serial().unwrap();
        assert!((0x40..=0x7f).contains(&serial[0]));
        let cert = certificate(&spec, &serial, &ca);
        assert!(certifies(&cert, &leaf.public_key()));
        assert!(!certifies(&cert, &ca.public_key()));
        // The last 64 bytes are the signature over the TBS that follows the
        // outer header.
        let tbs = tbs_certificate(&spec, &serial);
        let signature = &cert[cert.len() - 64..];
        assert!(verify(&ca.public_key(), &tbs, signature));
        assert!(!verify(&leaf.public_key(), &tbs, signature));
    }

    #[test]
    fn key_pairs_never_print() {
        let key = KeyPair::generate().unwrap();
        assert_eq!(format!("{key:?}"), "KeyPair(***)");
    }
}
